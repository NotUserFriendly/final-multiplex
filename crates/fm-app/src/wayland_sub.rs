//! Wayland wl_subsurface dedicated present loop (Phase 3, ADR-0026).
//!
//! Creates a desync'd wl_subsurface under iced's parent window and drives a
//! dedicated wgpu render thread that composites N video sources at Fifo vsync,
//! independent of iced's event loop.
//!
//! # Thread safety
//!
//! `create_subsurface()` MUST be called from iced's event-loop thread (the same
//! thread that drives the Wayland socket).  We use an isolated `wl_event_queue`
//! for the one-time setup roundtrip so we never race with winit's own dispatch.
//! After setup the render thread owns the wgpu surface exclusively.

use std::ffi::c_void;
use std::os::raw::{c_char, c_int};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle,
};

use wayland_client::protocol::__interfaces::{
    wl_compositor_interface, wl_registry_interface, wl_subcompositor_interface,
    wl_subsurface_interface, wl_surface_interface,
};
use wayland_sys::{
    client::{wayland_client_handle, wl_display, wl_event_queue, wl_proxy},
    common::wl_argument,
    ffi_dispatch,
};

use crate::gpu_path::{GpuFrameStore, TimedFrame};

// ── Globals discovery ─────────────────────────────────────────────────────────

struct GlobalsState {
    compositor: *mut wl_proxy,
    compositor_ver: u32,
    subcompositor: *mut wl_proxy,
    subcompositor_ver: u32,
}

#[repr(C)]
struct RegistryListener {
    global: unsafe extern "C" fn(*mut c_void, *mut wl_proxy, u32, *const c_char, u32),
    global_remove: unsafe extern "C" fn(*mut c_void, *mut wl_proxy, u32),
}

unsafe extern "C" fn on_global(
    data: *mut c_void,
    registry: *mut wl_proxy,
    name: u32,
    interface: *const c_char,
    _version: u32,
) {
    let state = &mut *(data as *mut GlobalsState);
    let iface = std::ffi::CStr::from_ptr(interface).to_bytes();

    if iface == b"wl_compositor" && state.compositor.is_null() {
        // wl_registry.bind signature is "usun": uint name, string iface_name, uint
        // version, new_id. The array variant requires all four args explicitly; the
        // preceding EINVAL was caused by version > registry version, and the SIGSEGV
        // was caused by passing only 2 args so args[1].s was read as NULL.
        let mut args = [
            wl_argument { u: name },
            wl_argument {
                s: wl_compositor_interface.name,
            },
            wl_argument { u: 1 },
            wl_argument { n: 0 },
        ];
        state.compositor = ffi_dispatch!(
            wayland_client_handle(),
            wl_proxy_marshal_array_constructor_versioned,
            registry,
            0, // WL_REGISTRY_BIND
            args.as_mut_ptr(),
            &wl_compositor_interface,
            1
        );
        state.compositor_ver = 1;
    } else if iface == b"wl_subcompositor" && state.subcompositor.is_null() {
        let mut args = [
            wl_argument { u: name },
            wl_argument {
                s: wl_subcompositor_interface.name,
            },
            wl_argument { u: 1 },
            wl_argument { n: 0 },
        ];
        state.subcompositor = ffi_dispatch!(
            wayland_client_handle(),
            wl_proxy_marshal_array_constructor_versioned,
            registry,
            0, // WL_REGISTRY_BIND
            args.as_mut_ptr(),
            &wl_subcompositor_interface,
            1
        );
        state.subcompositor_ver = 1;
    }
}

unsafe extern "C" fn on_global_remove(_data: *mut c_void, _registry: *mut wl_proxy, _name: u32) {}

static REGISTRY_LISTENER: RegistryListener = RegistryListener {
    global: on_global,
    global_remove: on_global_remove,
};

// ── Shared render state ───────────────────────────────────────────────────────

/// One entry per video source: the frame ring, current offset, and NDC rect.
pub struct RenderSlot {
    pub store: GpuFrameStore,
    /// Offset to subtract from running_time_ns before selecting a frame.
    pub offset_ns: i64,
    /// NDC rect [x0, y0, x1, y1]; Y=-1 = bottom, Y=+1 = top.
    pub rect: [f32; 4],
}

/// Shared between iced's event loop (writer) and the render thread (reader).
pub type RenderShared = Arc<Mutex<Vec<RenderSlot>>>;

/// Pipeline running time in nanoseconds, updated by the event loop each vsync.
pub type RunningTime = Arc<AtomicU64>;

/// Window pixel dimensions packed as `(width as u64) << 32 | height as u64`.
/// Written by the event loop on every `Message::Resized`; read by the render
/// thread each frame to reconfigure the surface when the window is resized.
pub type WindowSize = Arc<AtomicU64>;

pub fn pack_window_size(w: u32, h: u32) -> u64 {
    ((w as u64) << 32) | (h as u64)
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Raw Wayland handles for the subsurface.
pub struct SubsurfaceHandles {
    pub display: *mut wl_display,
    pub surface: *mut wl_proxy,
}

// SAFETY: set up before the render thread starts; never touched by the
// event-loop thread after the render thread is spawned.
unsafe impl Send for SubsurfaceHandles {}
unsafe impl Sync for SubsurfaceHandles {}

/// Create a wl_subsurface under iced's window.
///
/// # Safety
///
/// Must be called from iced's event-loop thread.
/// `display_ptr` is the `wl_display*`; `parent_surface_ptr` is iced's `wl_surface*`.
/// Both must remain valid for the lifetime of the returned handles.
pub unsafe fn create_subsurface(
    display_ptr: *mut c_void,
    parent_surface_ptr: *mut c_void,
    pos: (i32, i32),
) -> Option<SubsurfaceHandles> {
    let display = display_ptr as *mut wl_display;
    let parent = parent_surface_ptr as *mut wl_proxy;

    // Isolated event queue — our roundtrip never races with winit's dispatch.
    let our_queue: *mut wl_event_queue =
        ffi_dispatch!(wayland_client_handle(), wl_display_create_queue, display);
    if our_queue.is_null() {
        return None;
    }

    // Wrap display proxy so new objects land on our queue.
    let display_wrapper: *mut wl_proxy = ffi_dispatch!(
        wayland_client_handle(),
        wl_proxy_create_wrapper,
        display as *mut wl_proxy
    );
    if display_wrapper.is_null() {
        return None;
    }
    ffi_dispatch!(
        wayland_client_handle(),
        wl_proxy_set_queue,
        display_wrapper,
        our_queue
    );

    // GET_REGISTRY (wl_display opcode 1) → registry on our queue.
    let mut reg_args = [wl_argument { n: 0 }];
    let registry: *mut wl_proxy = ffi_dispatch!(
        wayland_client_handle(),
        wl_proxy_marshal_array_constructor_versioned,
        display_wrapper,
        1,
        reg_args.as_mut_ptr(),
        &wl_registry_interface,
        1
    );
    ffi_dispatch!(
        wayland_client_handle(),
        wl_proxy_wrapper_destroy,
        display_wrapper
    );
    if registry.is_null() {
        return None;
    }

    // Install listener and roundtrip to collect globals.
    let mut globals = GlobalsState {
        compositor: std::ptr::null_mut(),
        compositor_ver: 0,
        subcompositor: std::ptr::null_mut(),
        subcompositor_ver: 0,
    };
    let ret: c_int = ffi_dispatch!(
        wayland_client_handle(),
        wl_proxy_add_listener,
        registry,
        &REGISTRY_LISTENER as *const RegistryListener as *mut extern "C" fn(),
        &mut globals as *mut _ as *mut c_void
    );
    if ret < 0 {
        return None;
    }

    // Roundtrip dispatches only our isolated queue.
    let rt: c_int = ffi_dispatch!(
        wayland_client_handle(),
        wl_display_roundtrip_queue,
        display,
        our_queue
    );
    if rt < 0 {
        return None;
    }

    if globals.compositor.is_null() || globals.subcompositor.is_null() {
        return None;
    }

    // wl_compositor.create_surface (opcode 0) → our wl_surface.
    let mut surf_args = [wl_argument { n: 0 }];
    let our_surface: *mut wl_proxy = ffi_dispatch!(
        wayland_client_handle(),
        wl_proxy_marshal_array_constructor_versioned,
        globals.compositor,
        0,
        surf_args.as_mut_ptr(),
        &wl_surface_interface,
        globals.compositor_ver
    );
    if our_surface.is_null() {
        return None;
    }

    // wl_subcompositor.get_subsurface (opcode 1).
    // args: [new_id: wl_subsurface, surface: our_surface, parent: iced_surface]
    let mut sub_args = [
        wl_argument { n: 0 },
        wl_argument {
            o: our_surface as *const c_void,
        },
        wl_argument {
            o: parent as *const c_void,
        },
    ];
    let subsurface: *mut wl_proxy = ffi_dispatch!(
        wayland_client_handle(),
        wl_proxy_marshal_array_constructor_versioned,
        globals.subcompositor,
        1,
        sub_args.as_mut_ptr(),
        &wl_subsurface_interface,
        globals.subcompositor_ver
    );
    if subsurface.is_null() {
        return None;
    }

    // wl_subsurface.place_below (opcode 3): place behind the parent surface so
    // iced's tile overlay layer (offset controls) renders on top of our video.
    let mut below_args = [wl_argument {
        o: parent as *const c_void,
    }];
    ffi_dispatch!(
        wayland_client_handle(),
        wl_proxy_marshal_array,
        subsurface,
        3,
        below_args.as_mut_ptr()
    );

    // wl_subsurface.set_position (opcode 1): place in parent coords.
    let mut pos_args = [wl_argument { i: pos.0 }, wl_argument { i: pos.1 }];
    ffi_dispatch!(
        wayland_client_handle(),
        wl_proxy_marshal_array,
        subsurface,
        1,
        pos_args.as_mut_ptr()
    );

    // wl_subsurface.set_desync (opcode 5): present independent of parent commits.
    ffi_dispatch!(
        wayland_client_handle(),
        wl_proxy_marshal_array,
        subsurface,
        5,
        std::ptr::null_mut()
    );

    // wl_surface.commit on parent (opcode 6): make subsurface visible.
    ffi_dispatch!(
        wayland_client_handle(),
        wl_proxy_marshal_array,
        parent,
        6,
        std::ptr::null_mut()
    );

    ffi_dispatch!(wayland_client_handle(), wl_display_flush, display);

    Some(SubsurfaceHandles {
        display,
        surface: our_surface,
    })
}

/// Spawn the dedicated render thread.
///
/// The render thread composites `render_shared`'s sources at Fifo vsync.
/// `running_time` is updated by iced's event loop each frame.
pub fn spawn_render_thread(
    handles: SubsurfaceHandles,
    width: u32,
    height: u32,
    render_shared: RenderShared,
    running_time: RunningTime,
    window_size: WindowSize,
) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("wgpu-subsurface".into())
        .spawn(move || {
            render_loop(
                handles,
                width,
                height,
                render_shared,
                running_time,
                window_size,
            )
        })
        .expect("spawn render thread")
}

// ── GPU resource types (private to render thread) ─────────────────────────────

struct SharedPipeline {
    pipeline: wgpu::RenderPipeline,
    bgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

impl SharedPipeline {
    fn new(device: &wgpu::Device, surface_format: wgpu::TextureFormat) -> Self {
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sub_rect_bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let shader_mod = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("sub_rect_shader"),
            source: wgpu::ShaderSource::Wgsl(RECT_SHADER.into()),
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sub_rect_pl"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("sub_rect_rp"),
            layout: Some(&pl),
            vertex: wgpu::VertexState {
                module: &shader_mod,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader_mod,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });
        Self {
            pipeline,
            bgl,
            sampler,
        }
    }
}

struct SlotGpu {
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    rect_buf: wgpu::Buffer,
    last_pts: u64,
    tex_w: u32,
    tex_h: u32,
    /// Letterboxed NDC rect last written to rect_buf; compared each frame to
    /// skip redundant buffer writes.
    last_lb_rect: [f32; 4],
}

impl SlotGpu {
    fn new(device: &wgpu::Device, shared: &SharedPipeline, w: u32, h: u32) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("sub_slot_tex"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let tex_view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let rect_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sub_slot_rect"),
            size: 16, // vec4<f32>
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("sub_slot_bg"),
            layout: &shared.bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&tex_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&shared.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: rect_buf.as_entire_binding(),
                },
            ],
        });
        Self {
            texture,
            bind_group,
            rect_buf,
            last_pts: u64::MAX,
            tex_w: w,
            tex_h: h,
            last_lb_rect: [0.0; 4],
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Compute the largest axis-aligned rect within `cell` (NDC) that preserves
/// the video aspect ratio `vw/vh`, centered within the cell.  `win_w/win_h`
/// are the subsurface pixel dimensions used to convert NDC extents to pixels
/// for the AR comparison.
fn letterbox_rect(cell: [f32; 4], vw: u32, vh: u32, win_w: u32, win_h: u32) -> [f32; 4] {
    let [x0, y0, x1, y1] = cell;
    // Convert NDC extents to pixel extents so AR comparison is meaningful.
    let cell_px_w = (x1 - x0) * win_w as f32 * 0.5;
    let cell_px_h = (y1 - y0) * win_h as f32 * 0.5;
    let cell_ar = cell_px_w / cell_px_h;
    let video_ar = vw as f32 / vh as f32;
    // Pillarbox when cell is wider, letterbox when cell is taller.
    let (sx, sy) = if cell_ar > video_ar {
        (video_ar / cell_ar, 1.0f32)
    } else {
        (1.0f32, cell_ar / video_ar)
    };
    let cx = (x0 + x1) * 0.5;
    let cy = (y0 + y1) * 0.5;
    let hw = (x1 - x0) * 0.5 * sx;
    let hh = (y1 - y0) * 0.5 * sy;
    [cx - hw, cy - hh, cx + hw, cy + hh]
}

// ── Render loop ───────────────────────────────────────────────────────────────

fn render_loop(
    handles: SubsurfaceHandles,
    width: u32,
    height: u32,
    render_shared: RenderShared,
    running_time: RunningTime,
    window_size: WindowSize,
) {
    let mut width = width;
    let mut height = height;
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..Default::default()
    });

    let surface = unsafe {
        instance
            .create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_window_handle: RawWindowHandle::Wayland(WaylandWindowHandle::new(
                    NonNull::new(handles.surface as *mut c_void).expect("subsurface non-null"),
                )),
                raw_display_handle: RawDisplayHandle::Wayland(WaylandDisplayHandle::new(
                    NonNull::new(handles.display as *mut c_void).expect("display non-null"),
                )),
            })
            .expect("[sub] create_surface_unsafe failed")
    };

    let adapter = instance
        .enumerate_adapters(wgpu::Backends::VULKAN)
        .into_iter()
        .find(|a| a.is_surface_supported(&surface))
        .expect("[sub] no Vulkan adapter supports the subsurface");

    eprintln!("[sub] adapter: {}", adapter.get_info().name);

    let (device, queue) = block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("sub-device"),
        ..Default::default()
    }))
    .expect("[sub] request_device failed");

    let caps = surface.get_capabilities(&adapter);
    // Prefer a non-sRGB surface: video frames from GStreamer are already
    // gamma-encoded for display.  Writing through an sRGB surface would apply
    // a second gamma pass and wash out mid-tones.
    let format = caps
        .formats
        .iter()
        .copied()
        .find(|f| !f.is_srgb())
        .or_else(|| caps.formats.first().copied())
        .expect("[sub] no surface format");

    let mut config = wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format,
        width: width.max(1),
        height: height.max(1),
        present_mode: wgpu::PresentMode::Fifo,
        alpha_mode: wgpu::CompositeAlphaMode::Opaque,
        view_formats: vec![],
        desired_maximum_frame_latency: 2,
    };
    surface.configure(&device, &config);
    eprintln!(
        "[sub] surface {}×{} {:?} Fifo — video compositor active",
        config.width, config.height, format
    );

    let shared_pipeline = SharedPipeline::new(&device, format);
    let mut slots: Vec<Option<SlotGpu>> = Vec::new();

    // Step 3 instrumentation: log present rate every 5 s.
    let mut fps_frames: u32 = 0;
    let mut fps_tick = std::time::Instant::now();

    loop {
        // Reconfigure BEFORE acquiring the swapchain texture; wgpu forbids
        // calling configure while a SurfaceTexture is live.
        let packed = window_size.load(Ordering::Relaxed);
        let (new_w, new_h) = ((packed >> 32) as u32, (packed & 0xFFFF_FFFF) as u32);
        if new_w != config.width || new_h != config.height {
            config.width = new_w.max(1);
            config.height = new_h.max(1);
            surface.configure(&device, &config);
            width = config.width;
            height = config.height;
            // Reset lb_rects so letterboxing recomputes with new pixel dims.
            for slot_opt in slots.iter_mut().flatten() {
                slot_opt.last_lb_rect = [0.0; 4];
            }
        }

        let frame = match surface.get_current_texture() {
            Ok(f) => f,
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                surface.configure(&device, &config);
                continue;
            }
            Err(wgpu::SurfaceError::Timeout) => continue,
            Err(e) => {
                eprintln!("[sub] surface error: {e:?} — exiting");
                break;
            }
        };

        let now_ns = running_time.load(Ordering::Relaxed);

        // Snapshot (store Arc, offset, rect) per slot — brief lock, no pixel work.
        let slot_meta: Vec<(GpuFrameStore, i64, [f32; 4])> = {
            let guard = render_shared.lock().unwrap();
            guard
                .iter()
                .map(|s| (Arc::clone(&s.store), s.offset_ns, s.rect))
                .collect()
        };

        // Select frames from each ring without holding render_shared.
        let snapshots: Vec<(Option<Arc<TimedFrame>>, [f32; 4])> = slot_meta
            .iter()
            .map(|(store, offset_ns, rect)| {
                let target_ns = (now_ns as i64 - offset_ns).max(0) as u64;
                let tframe = store.lock().unwrap().select(target_ns);
                (tframe, *rect)
            })
            .collect();

        // Grow slot vec to match source count.
        if slots.len() < snapshots.len() {
            slots.resize_with(snapshots.len(), || None);
        }

        // Upload changed textures, rebuild slots when dimensions change.
        for (i, (frame_opt, rect)) in snapshots.iter().enumerate() {
            let Some(tframe) = frame_opt else { continue };
            let (w, h) = (tframe.width, tframe.height);

            let rebuild = slots[i]
                .as_ref()
                .map(|s| s.tex_w != w || s.tex_h != h)
                .unwrap_or(true);
            if rebuild {
                slots[i] = Some(SlotGpu::new(&device, &shared_pipeline, w, h));
            }
            let slot = slots[i].as_mut().unwrap();

            // Compute letterboxed sub-rect within the cell and write to uniform
            // only when it changes (dims change or cell layout updated).
            let lb = letterbox_rect(*rect, w, h, width, height);
            if lb != slot.last_lb_rect {
                let mut bytes = [0u8; 16];
                for (j, v) in lb.iter().enumerate() {
                    bytes[j * 4..(j + 1) * 4].copy_from_slice(&v.to_le_bytes());
                }
                queue.write_buffer(&slot.rect_buf, 0, &bytes);
                slot.last_lb_rect = lb;
            }

            if tframe.pts_ns != slot.last_pts {
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &slot.texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    &tframe.rgba,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(w * 4),
                        rows_per_image: None,
                    },
                    wgpu::Extent3d {
                        width: w,
                        height: h,
                        depth_or_array_layers: 1,
                    },
                );
                slot.last_pts = tframe.pts_ns;
            }
        }

        // Composite pass: clear to black, draw each source in its NDC rect.
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            pass.set_pipeline(&shared_pipeline.pipeline);
            for slot_opt in slots.iter().flatten() {
                pass.set_bind_group(0, &slot_opt.bind_group, &[]);
                pass.draw(0..4, 0..1);
            }
        }
        queue.submit([encoder.finish()]);
        frame.present();

        fps_frames += 1;
        let elapsed = fps_tick.elapsed();
        if elapsed >= std::time::Duration::from_secs(5) {
            let fps = fps_frames as f64 / elapsed.as_secs_f64();
            eprintln!("[sub] present fps={:.1}", fps);
            fps_frames = 0;
            fps_tick = std::time::Instant::now();
        }

        // Update surface config if window was resized (reported by the next acquire).
        // Handled by the Lost/Outdated branch above; store current config for reconfigure.
        let _ = &mut config;
    }
}

// ── WGSL shader ───────────────────────────────────────────────────────────────

const RECT_SHADER: &str = r#"
struct Vert { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> }

// [x0, y0, x1, y1] in NDC; Y=-1=bottom, Y=+1=top.
@group(0) @binding(2) var<uniform> rect: vec4<f32>;

@vertex fn vs_main(@builtin(vertex_index) i: u32) -> Vert {
    let px = array<f32,4>(rect.x, rect.z, rect.x, rect.z);
    let py = array<f32,4>(rect.y, rect.y, rect.w, rect.w);
    let pu = array<f32,4>(0.0, 1.0, 0.0, 1.0);
    let pv = array<f32,4>(1.0, 1.0, 0.0, 0.0);
    return Vert(vec4(px[i], py[i], 0.0, 1.0), vec2(pu[i], pv[i]));
}

@group(0) @binding(0) var vid_tex: texture_2d<f32>;
@group(0) @binding(1) var vid_samp: sampler;

@fragment fn fs_main(v: Vert) -> @location(0) vec4<f32> {
    return textureSample(vid_tex, vid_samp, v.uv);
}
"#;

// ── Minimal block_on ──────────────────────────────────────────────────────────

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    use std::sync::Arc;
    use std::task::{Context, Poll, Wake, Waker};
    struct NoopWake;
    impl Wake for NoopWake {
        fn wake(self: Arc<Self>) {}
    }
    let waker = Waker::from(Arc::new(NoopWake));
    let mut ctx = Context::from_waker(&waker);
    let mut pinned = std::pin::pin!(f);
    loop {
        if let Poll::Ready(val) = pinned.as_mut().poll(&mut ctx) {
            return val;
        }
        std::hint::spin_loop();
    }
}
