// Persistent-texture wgpu shader widget for video display (ADR-0006 bridge
// replacement). Creates a GPU texture once via Pipeline::new and updates it
// in-place with queue.write_texture — no delete/re-upload cycle, no flicker.

use crate::bridge::FrameData;
use iced::widget::shader::{self, Viewport};
use iced::{mouse, Rectangle};
use std::sync::Arc;

// ── Public program (one per view call) ───────────────────────────────────────

#[derive(Debug)]
pub struct VideoProg {
    pub frame: Option<Arc<FrameData>>,
}

impl<Msg> shader::Program<Msg> for VideoProg
where
    Msg: Clone + std::fmt::Debug + Send + 'static,
{
    type State = ();
    type Primitive = VideoPrim;

    fn draw(&self, _state: &(), _cursor: mouse::Cursor, _bounds: Rectangle) -> VideoPrim {
        VideoPrim {
            frame: self.frame.clone(),
        }
    }
}

// ── Primitive (CPU snapshot carried into prepare/draw) ────────────────────────

#[derive(Debug)]
pub struct VideoPrim {
    frame: Option<Arc<FrameData>>,
}

impl shader::Primitive for VideoPrim {
    type Pipeline = GpuState;

    fn prepare(
        &self,
        pipeline: &mut GpuState,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bounds: &Rectangle,
        _viewport: &Viewport,
    ) {
        let frame = match &self.frame {
            Some(f) => f,
            None => return,
        };
        let (w, h) = (frame.width, frame.height);

        // Rebuild GPU resources when video dimensions change (first frame, or
        // if the compositor is rebuilt with different grid geometry).
        let rebuild = pipeline
            .inner
            .as_ref()
            .map(|i| i.tex_w != w || i.tex_h != h)
            .unwrap_or(true);

        if rebuild {
            pipeline.inner = Some(GpuInner::new(device, pipeline.format, w, h));
        }

        let inner = pipeline.inner.as_mut().unwrap();

        // Compute letterbox / pillarbox scale so the video always fills the
        // widget at its native aspect ratio with black bars in the leftover
        // space.  Updated every prepare call so window resizes take effect
        // immediately, not just when a new video frame arrives.
        let (sx, sy) = if bounds.width > 0.0 && bounds.height > 0.0 {
            let tex_ar = inner.tex_w as f32 / inner.tex_h as f32;
            let disp_ar = bounds.width / bounds.height;
            if disp_ar > tex_ar {
                (tex_ar / disp_ar, 1.0f32) // pillarbox: bars on left/right
            } else {
                (1.0f32, disp_ar / tex_ar) // letterbox: bars on top/bottom
            }
        } else {
            (1.0f32, 1.0f32)
        };
        let mut scale_bytes = [0u8; 8];
        scale_bytes[0..4].copy_from_slice(&sx.to_le_bytes());
        scale_bytes[4..8].copy_from_slice(&sy.to_le_bytes());
        queue.write_buffer(&inner.scale_buf, 0, &scale_bytes);

        if frame.generation != inner.last_gen {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &inner.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &frame.rgba,
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
            inner.last_gen = frame.generation;
        }
    }

    fn draw(&self, pipeline: &GpuState, render_pass: &mut wgpu::RenderPass<'_>) -> bool {
        let Some(inner) = &pipeline.inner else {
            return false;
        };
        render_pass.set_pipeline(&inner.pipeline);
        render_pass.set_bind_group(0, &inner.bind_group, &[]);
        render_pass.draw(0..4, 0..1);
        true
    }
}

// ── GPU state (owns the persistent texture; one instance per app) ─────────────

pub struct GpuState {
    format: wgpu::TextureFormat,
    inner: Option<GpuInner>,
}

impl shader::Pipeline for GpuState {
    fn new(_device: &wgpu::Device, _queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        // Defer texture creation until the first frame arrives so we know the
        // video dimensions.
        Self {
            format,
            inner: None,
        }
    }
}

// ── Inner GPU resources (texture + bind group + render pipeline) ──────────────

struct GpuInner {
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    pipeline: wgpu::RenderPipeline,
    scale_buf: wgpu::Buffer,
    last_gen: u64,
    tex_w: u32,
    tex_h: u32,
}

impl GpuInner {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat, w: u32, h: u32) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("fm_video_tex"),
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
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let scale_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fm_scale_buf"),
            size: 8,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("fm_video_bgl"),
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

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("fm_video_bg"),
            layout: &bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&tex_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: scale_buf.as_entire_binding(),
                },
            ],
        });

        let shader_mod = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("fm_video_shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("fm_video_pl"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("fm_video_rp"),
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
                    format,
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
            texture,
            bind_group,
            pipeline,
            scale_buf,
            last_gen: 0,
            tex_w: w,
            tex_h: h,
        }
    }
}

// ── WGSL blit shader ──────────────────────────────────────────────────────────

const SHADER: &str = r#"
struct V { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> }

@group(0) @binding(2) var<uniform> scale: vec2<f32>;

@vertex fn vs_main(@builtin(vertex_index) i: u32) -> V {
    var p = array<vec2<f32>,4>(
        vec2(-1.0,-1.0), vec2(1.0,-1.0),
        vec2(-1.0, 1.0), vec2(1.0, 1.0));
    var u = array<vec2<f32>,4>(
        vec2(0.0,1.0), vec2(1.0,1.0),
        vec2(0.0,0.0), vec2(1.0,0.0));
    return V(vec4(p[i] * scale, 0.0, 1.0), u[i]);
}

@group(0) @binding(0) var t: texture_2d<f32>;
@group(0) @binding(1) var s: sampler;

@fragment fn fs_main(v: V) -> @location(0) vec4<f32> {
    return textureSample(t, s, v.uv);
}
"#;
