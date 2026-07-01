use crate::bridge::{self, FrameData};
use crate::gpu_path::{self, GpuFrameStore, TimedFrame};
use crate::video::{GpuRectProg, VideoProg};
use fm_adapter_sdk::metrics::SourceMetrics;
use gstreamer::prelude::{ElementExt, GstBinExt, ObjectExt as GstObjectExt, PipelineExt};
use iced::widget::{button, column, container, row, shader, stack, text, text_input};
use iced::{Background, Color, Element, Length, Subscription, Task};
#[cfg(target_os = "linux")]
#[allow(unused_imports)]
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

const MIN_OFFSET_MS: i32 = -60_000;
const MAX_OFFSET_MS: i32 = 60_000;
pub(crate) const CHROME_H: f32 = 50.0;
/// Width of the GPU-path side panel (Block 1 proof display).
pub(crate) const GPU_PANEL_W: f32 = 480.0;

struct SourceRow {
    id: String,
    offset_ms: i32,
    /// Live editing buffer; only committed to offset_ms on valid parse.
    offset_buf: String,
    muted: bool,
    /// Truncated uri basename for display.
    display_name: String,
    /// Effective per-source offset bounds derived from adapter capability + core ceiling.
    /// File sources: ±60 000 ms; live sources: [0, min(declared_max, ceiling)].
    min_offset_ms: i32,
    max_offset_ms: i32,
    /// True for out-of-process (RTSP) sources; false for in-process file sources.
    is_external: bool,
    /// External source: adapter not delivering (Restarting/Failed or is_reconnecting).
    signal_lost: bool,
    /// File source: set once the first frame has arrived; used to gate FILE TERMINATED.
    has_ever_had_frames: bool,
    /// Reboot or Kill pressed and still in progress; disables both buttons.
    transitioning: bool,
    /// Kill-button cooldown: disabled until this instant to prevent spam.
    kill_cooldown_until: Option<Instant>,
}

pub struct App {
    transport: Option<fm_core::transport::Transport>,
    metrics: Option<fm_core::metrics::MetricsCollector>,
    /// Out-of-process adapter supervisor (Phase 2). None if no external sources.
    supervisor: Option<fm_core::supervisor::Supervisor>,
    /// Net-time provider — kept alive for the session so adapters can re-sync.
    /// Switched to the audio hardware clock after the pipeline reaches PLAYING
    /// (ADR-0027).  None if there are no external sources.
    #[allow(dead_code)]
    net_clock: Option<fm_core::net_clock::NetClock>,
    frame_store: bridge::FrameStore,
    current_frame: Option<Arc<FrameData>>,
    frame_gen: u64,
    playing: bool,
    sources: Vec<SourceRow>,
    source_metrics: Vec<SourceMetrics>,
    grid_cols: u32,
    grid_rows: u32,
    grid_ar: f32,
    win_w: f32,
    win_h: f32,
    error: Option<String>,
    config_persist: Option<fm_core::persist::ConfigPersist>,
    /// Set on every committed offset change; cleared after a 500 ms idle flush.
    last_offset_change: Option<Instant>,
    /// IDs of external sources — used to query chain state for delivery watchdog.
    external_source_ids: Vec<String>,
    /// GPU presentation path (ADR-0024, Phase 3 Block 2).
    /// One FrameStore per source that has a video pad.
    gpu_stores: HashMap<String, GpuFrameStore>,
    /// Most recently scheduler-selected frame per GPU-pathed source.
    current_gpu_frames: HashMap<String, Arc<TimedFrame>>,
    /// Phase 3 dedicated present loop (ADR-0026): handle to the render thread.
    #[cfg(target_os = "linux")]
    spike_thread: Option<std::thread::JoinHandle<()>>,
    /// Shared source list for the render thread; updated when layout/offsets change.
    #[cfg(target_os = "linux")]
    render_shared: Option<crate::wayland_sub::RenderShared>,
    /// Pipeline running time fed to the render thread each vsync.
    #[cfg(target_os = "linux")]
    render_running_time: Option<crate::wayland_sub::RunningTime>,
    /// Window pixel dims (packed u64) fed to the render thread on each resize.
    #[cfg(target_os = "linux")]
    window_size: Option<crate::wayland_sub::WindowSize>,
    /// Process start time — drives the mission clock in the chrome bar.
    start_time: Instant,
}

#[derive(Debug, Clone)]
pub enum Message {
    /// Fired on each display vsync via `iced::window::frames()`.
    /// Drives frame selection, GPU scheduler, and compositor bridge update.
    Frame,
    /// Fired every 500 ms.  Drives supervisor poll, ratchet, and persist flush.
    Poll,
    /// Window close button clicked or SIGTERM received — run graceful teardown.
    Exit,
    TogglePlay,
    /// Typed text in an offset box; commits on valid parse.
    OffsetEdit {
        index: usize,
        text: String,
    },
    /// Enter pressed in an offset box: sync the display buffer back to the
    /// clamped offset_ms so an out-of-range entry shows its actual value.
    OffsetNormalise {
        index: usize,
    },
    /// Stepper button: saturating add delta (ms), clamp to ±MAX_OFFSET_MS.
    OffsetStep {
        index: usize,
        delta: i32,
    },
    ToggleMute {
        index: usize,
    },
    /// Reboot button: graceful teardown → respawn for an external source.
    Reboot {
        index: usize,
    },
    /// Kill button: graceful teardown → no respawn; tile stays dead until Reboot.
    Kill {
        index: usize,
    },
    /// Reset the output framerate ratchet to the configured grid fps (ADR-0023).
    ResetRatchet,
    Resized {
        width: f32,
        height: f32,
    },
    /// Step 1 spike: window opened — request raw handles to create subsurface.
    #[cfg(target_os = "linux")]
    SpikeInit(iced::window::Id, f32, f32),
    /// Step 1 spike: raw handles delivered — create subsurface + spawn render thread.
    #[cfg(target_os = "linux")]
    SpikeHandles(Option<(usize, usize)>, f32, f32),
}

impl App {
    pub fn init(config_path: &std::path::Path) -> Self {
        let frame_store = bridge::new_store();
        match try_init(config_path, frame_store.clone()) {
            Ok(state) => state,
            Err(e) => {
                eprintln!("[app] try_init failed: {e}");
                Self {
                    transport: None,
                    metrics: None,
                    frame_store,
                    current_frame: None,
                    frame_gen: 0,
                    playing: false,
                    sources: Vec::new(),
                    source_metrics: Vec::new(),
                    grid_cols: 1,
                    grid_rows: 1,
                    grid_ar: 16.0 / 9.0,
                    win_w: 1280.0,
                    win_h: 720.0,
                    error: Some(e.to_string()),
                    supervisor: None,
                    net_clock: None,
                    config_persist: None,
                    last_offset_change: None,

                    external_source_ids: Vec::new(),
                    gpu_stores: HashMap::new(),
                    current_gpu_frames: HashMap::new(),
                    #[cfg(target_os = "linux")]
                    spike_thread: None,
                    #[cfg(target_os = "linux")]
                    render_shared: None,
                    #[cfg(target_os = "linux")]
                    render_running_time: None,
                    #[cfg(target_os = "linux")]
                    window_size: None,
                    start_time: Instant::now(),
                }
            }
        }
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Exit => {
                if let Some(mut sup) = self.supervisor.take() {
                    sup.shutdown_all();
                }
                return iced::exit();
            }

            // ── Vsync-driven render tick ───────────────────────────────────
            // Fired by iced::window::frames() in phase with the display
            // refresh.  Frame selection happens once per present — no beat
            // against vsync from a mismatched wall-clock timer.
            Message::Frame => {
                if let Some(frame) = bridge::latest_frame(&self.frame_store, &mut self.frame_gen) {
                    self.current_frame = Some(frame);
                }
                // GPU-path scheduler (ADR-0024): select the frame whose PTS
                // is closest to (running_time − offset_ns) at each vsync.
                // Also feeds running_time to the dedicated render thread (ADR-0026).
                if let Some(t) = &self.transport {
                    if let Some(running_ns) = t.pipeline_running_time_ns() {
                        // Feed the dedicated render thread.
                        #[cfg(target_os = "linux")]
                        if let Some(rt) = &self.render_running_time {
                            rt.store(running_ns, Ordering::Relaxed);
                        }
                        // GPU panel scheduler — skipped when the subsurface is active
                        // to avoid double-rendering the same stores every vsync.
                        #[cfg(target_os = "linux")]
                        let sub_active = self.spike_thread.is_some();
                        #[cfg(not(target_os = "linux"))]
                        let sub_active = false;
                        if !sub_active {
                            for (src_id, store) in &self.gpu_stores {
                                let offset_ns = self
                                    .sources
                                    .iter()
                                    .find(|s| &s.id == src_id)
                                    .map(|s| s.offset_ms as i64 * 1_000_000)
                                    .unwrap_or(0);
                                let target_ns = (running_ns as i64 - offset_ns).max(0) as u64;
                                if let Some(frame) = store.lock().unwrap().select(target_ns) {
                                    self.current_gpu_frames.insert(src_id.clone(), frame);
                                }
                            }
                        }
                    }
                }

                // Propagate any offset changes to the render thread's slot list.
                #[cfg(target_os = "linux")]
                if let Some(shared) = &self.render_shared {
                    if let Ok(mut guard) = shared.try_lock() {
                        for slot in guard.iter_mut() {
                            if let Some(src) = self.sources.iter().find(|s| {
                                self.gpu_stores
                                    .get(&s.id)
                                    .map(|st| Arc::ptr_eq(st, &slot.store))
                                    .unwrap_or(false)
                            }) {
                                slot.offset_ns = src.offset_ms as i64 * 1_000_000;
                            }
                        }
                    }
                }
                if let Some(metrics) = &self.metrics {
                    self.source_metrics = self
                        .sources
                        .iter()
                        .map(|s| metrics.snapshot(&s.id))
                        .collect();
                }
                // Update per-source display state for tile overlays.
                let status_snap = self
                    .supervisor
                    .as_ref()
                    .map(|sup| sup.status_handle().lock().unwrap().clone());
                for (i, src) in self.sources.iter_mut().enumerate() {
                    if src.is_external {
                        let adapter = status_snap.as_ref().and_then(|s| s.get(&src.id));
                        src.signal_lost = adapter
                            .map(|a| {
                                a.state != fm_core::supervisor::AdapterState::Running
                                    || a.is_reconnecting
                            })
                            .unwrap_or(false);
                        if src.transitioning {
                            src.transitioning = adapter
                                .map(|a| {
                                    a.state == fm_core::supervisor::AdapterState::Starting
                                        || a.state == fm_core::supervisor::AdapterState::Restarting
                                })
                                .unwrap_or(false);
                        }
                        if matches!(src.kill_cooldown_until, Some(t) if Instant::now() >= t) {
                            src.kill_cooldown_until = None;
                        }
                    } else {
                        let fps_in = self.source_metrics.get(i).map(|m| m.fps_in).unwrap_or(0.0);
                        if fps_in > 0.0 {
                            src.has_ever_had_frames = true;
                        }
                    }
                }
            }

            // ── 500 ms housekeeping tick ───────────────────────────────────
            // Supervisor poll, ratchet, and persist flush.  Decoupled from
            // the render path so the vsync loop carries zero extra work.
            Message::Poll => {
                #[cfg(unix)]
                if crate::SIGTERM_FLAG.load(std::sync::atomic::Ordering::Relaxed) {
                    if let Some(mut sup) = self.supervisor.take() {
                        sup.shutdown_all();
                    }
                    return iced::exit();
                }

                // Debounced persist: flush 500 ms after the last committed offset change.
                if self
                    .last_offset_change
                    .map_or(false, |t| t.elapsed() > Duration::from_millis(500))
                {
                    if let Some(p) = &mut self.config_persist {
                        let _ = p.flush();
                    }
                    self.last_offset_change = None;
                }

                if let Some(sup) = &mut self.supervisor {
                    sup.poll();
                    for id in sup.take_restarted() {
                        if let Some(t) = &self.transport {
                            t.restart_external_source(&id);
                        }
                    }
                    for (id, has_video, has_audio) in sup.take_streams_changed() {
                        if let Some(t) = &mut self.transport {
                            let fps = sup
                                .status_handle()
                                .lock()
                                .unwrap()
                                .get(&id)
                                .and_then(|s| s.latest_metrics.as_ref())
                                .map(|m| m.fps_in)
                                .unwrap_or(0.0);
                            t.apply_streams_changed(&id, has_video, has_audio, fps);
                            if let Some(m) = &self.metrics {
                                m.attach_source(&id, t.pipeline());
                            }
                            if let Some(store) = self.gpu_stores.get(&id) {
                                if let Some(pad) = t
                                    .pipeline()
                                    .source_pads()
                                    .get(&id)
                                    .and_then(|p| p.pre_scale_video_src.as_ref())
                                {
                                    gpu_path::install_probe(pad, store.clone());
                                    eprintln!(
                                        "[gpu-path] native-res probe reinstalled on vdeint_{id}"
                                    );
                                }
                            }
                        }
                    }
                    for id in &self.external_source_ids {
                        let has_chain = self
                            .transport
                            .as_ref()
                            .map_or(false, |t| t.pipeline().source_has_chain(id));
                        sup.update_chain_state(id, has_chain);
                    }
                }
                // Output framerate ratchet (ADR-0023).
                if let (Some(t), Some(m)) = (&mut self.transport, &self.metrics) {
                    let ids: Vec<String> = self.sources.iter().map(|s| s.id.clone()).collect();
                    t.check_and_ratchet(&ids, m);
                }
            }

            Message::TogglePlay => {
                if let Some(t) = &self.transport {
                    if self.playing {
                        let _ = t.pause();
                        if let Some(sup) = &mut self.supervisor {
                            sup.send_pause_all();
                        }
                        self.playing = false;
                    } else {
                        let _ = t.play();
                        if let Some(sup) = &mut self.supervisor {
                            sup.send_play_all();
                        }
                        self.playing = true;
                    }
                }
            }

            Message::OffsetEdit { index, text } => {
                let mut persist_change: Option<(String, i64)> = None;
                if let Some(src) = self.sources.get_mut(index) {
                    src.offset_buf = text.clone();
                    if let Ok(ms) = text.trim().parse::<i32>() {
                        src.offset_ms = ms.clamp(src.min_offset_ms, src.max_offset_ms);
                        if let Some(t) = &mut self.transport {
                            let _ = t.set_source_offset(&src.id, src.offset_ms as i64);
                        }
                        persist_change = Some((src.id.clone(), src.offset_ms as i64));
                    }
                }
                if let Some((id, ms)) = persist_change {
                    if let Some(p) = &mut self.config_persist {
                        p.set_source_offset(&id, ms);
                    }
                    self.last_offset_change = Some(Instant::now());
                }
            }

            Message::OffsetNormalise { index } => {
                if let Some(src) = self.sources.get_mut(index) {
                    src.offset_buf = src.offset_ms.to_string();
                }
            }

            Message::OffsetStep { index, delta } => {
                let mut persist_change: Option<(String, i64)> = None;
                if let Some(src) = self.sources.get_mut(index) {
                    src.offset_ms = src
                        .offset_ms
                        .saturating_add(delta)
                        .clamp(src.min_offset_ms, src.max_offset_ms);
                    src.offset_buf = src.offset_ms.to_string();
                    if let Some(t) = &mut self.transport {
                        let _ = t.set_source_offset(&src.id, src.offset_ms as i64);
                    }
                    persist_change = Some((src.id.clone(), src.offset_ms as i64));
                }
                if let Some((id, ms)) = persist_change {
                    if let Some(p) = &mut self.config_persist {
                        p.set_source_offset(&id, ms);
                    }
                    self.last_offset_change = Some(Instant::now());
                }
            }

            Message::ToggleMute { index } => {
                if let Some(src) = self.sources.get_mut(index) {
                    src.muted = !src.muted;
                    if let Some(t) = &mut self.transport {
                        let _ = t.set_source_mute(&src.id, src.muted);
                    }
                    if let Some(p) = &mut self.config_persist {
                        p.set_source_muted(&src.id, src.muted);
                    }
                }
            }

            Message::Reboot { index } => {
                if let Some(src) = self.sources.get_mut(index) {
                    src.transitioning = true;
                    if let Some(sup) = &mut self.supervisor {
                        sup.request_reboot(&src.id.clone());
                    }
                }
            }

            Message::Kill { index } => {
                if let Some(src) = self.sources.get_mut(index) {
                    src.transitioning = true;
                    src.kill_cooldown_until = Some(Instant::now() + Duration::from_secs(5));
                    if let Some(sup) = &mut self.supervisor {
                        sup.request_kill(&src.id.clone());
                    }
                }
            }

            Message::ResetRatchet => {
                if let Some(t) = &mut self.transport {
                    t.reset_ratchet();
                }
            }

            Message::Resized { width, height } => {
                self.win_w = width;
                self.win_h = height;
                #[cfg(target_os = "linux")]
                if let Some(ws) = &self.window_size {
                    ws.store(
                        crate::wayland_sub::pack_window_size(
                            width as u32,
                            (height - CHROME_H).max(1.0) as u32,
                        ),
                        Ordering::Relaxed,
                    );
                }
            }

            // ── Step 1 spike: subsurface creation ─────────────────────────
            // Fetch raw Wayland handles on the event-loop thread (which IS
            // the Wayland socket thread), create a wl_subsurface under iced's
            // window, then spawn a dedicated wgpu render thread.
            #[cfg(target_os = "linux")]
            Message::SpikeInit(win_id, w, h) => {
                self.win_w = w;
                self.win_h = h;
                let (ww, wh) = (w, h);
                return iced::window::run(win_id, move |window| {
                    let Ok(wh_) = window.window_handle() else {
                        return None;
                    };
                    let Ok(dh_) = window.display_handle() else {
                        return None;
                    };
                    match (wh_.as_raw(), dh_.as_raw()) {
                        (RawWindowHandle::Wayland(surf_h), RawDisplayHandle::Wayland(disp_h)) => {
                            Some((
                                surf_h.surface.as_ptr() as usize,
                                disp_h.display.as_ptr() as usize,
                            ))
                        }
                        _ => None,
                    }
                })
                .map(move |h| Message::SpikeHandles(h, ww, wh));
            }

            #[cfg(target_os = "linux")]
            Message::SpikeHandles(handles, w, h) => {
                self.win_w = w;
                self.win_h = h;
                if self.spike_thread.is_none() {
                    if let Some((surf_ptr, disp_ptr)) = handles {
                        let result = unsafe {
                            crate::wayland_sub::create_subsurface(
                                disp_ptr as *mut std::ffi::c_void,
                                surf_ptr as *mut std::ffi::c_void,
                                (0, 0),
                            )
                        };
                        if let Some(sub) = result {
                            // Build per-source render slots: store + offset + NDC rect.
                            let slots = build_render_slots(
                                &self.gpu_stores,
                                &self.sources,
                                self.grid_cols,
                                self.grid_rows,
                            );
                            let render_shared = std::sync::Arc::new(std::sync::Mutex::new(slots));
                            let running_time =
                                std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
                            // Subsurface height stops at the chrome bar so the
                            // iced controls at the bottom remain unobscured.
                            let sub_h = (h - CHROME_H).max(1.0) as u32;
                            let window_size =
                                std::sync::Arc::new(std::sync::atomic::AtomicU64::new(
                                    crate::wayland_sub::pack_window_size(w as u32, sub_h),
                                ));
                            self.spike_thread = Some(crate::wayland_sub::spawn_render_thread(
                                sub,
                                w as u32,
                                sub_h,
                                std::sync::Arc::clone(&render_shared),
                                std::sync::Arc::clone(&running_time),
                                std::sync::Arc::clone(&window_size),
                            ));
                            self.render_shared = Some(render_shared);
                            self.render_running_time = Some(running_time);
                            self.window_size = Some(window_size);
                        }
                    }
                }
            }
        }

        Task::none()
    }

    pub fn view(&self) -> Element<'_, Message> {
        if let Some(err) = &self.error {
            return container(text(format!("Fatal: {err}")))
                .width(Length::Fill)
                .height(Length::Fill)
                .center_x(Length::Fill)
                .center_y(Length::Fill)
                .into();
        }

        // Is the wgpu subsurface active on this platform?
        #[cfg(target_os = "linux")]
        let sub_active = self.spike_thread.is_some();
        #[cfg(not(target_os = "linux"))]
        let sub_active = false;

        // ── Compute video display dimensions locked to output aspect ratio ──
        // When the subsurface is active the GPU panel is suppressed so the full
        // width is available for the video area (and the subsurface fills it).
        let gpu_panel_w = if sub_active {
            0.0_f32
        } else {
            (self.win_w / 3.0).max(240.0_f32)
        };
        let avail_h = (self.win_h - CHROME_H).max(1.0);
        let avail_w = (self.win_w - gpu_panel_w).max(1.0);
        let video_w = if sub_active {
            avail_w // fill the width; subsurface handles AR internally
        } else {
            (avail_h * self.grid_ar).min(avail_w)
        };
        let video_h = if sub_active {
            avail_h
        } else {
            video_w / self.grid_ar
        };

        let black_bg = |_: &iced::Theme| container::Style {
            background: Some(Background::Color(Color::BLACK)),
            ..Default::default()
        };

        // ── Layer 0: compositor output (suppressed when subsurface is active) ──
        // When active the wgpu subsurface renders video below iced's layer;
        // iced renders a transparent background so the subsurface shows through.
        let video_layer: Element<Message> = if sub_active {
            container(text(""))
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
        } else if self.current_frame.is_some() {
            container(
                shader(VideoProg {
                    frame: self.current_frame.clone(),
                })
                .width(Length::Fill)
                .height(Length::Fill),
            )
            .style(black_bg)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
        } else {
            container(text("Waiting for first frame…"))
                .style(black_bg)
                .width(Length::Fill)
                .height(Length::Fill)
                .center_x(Length::Fill)
                .center_y(Length::Fill)
                .into()
        };

        // ── Layer 1: per-tile overlay grid ─────────────────────────────────
        let overlay_layer = self.tile_overlay_grid();

        let stack_w = Length::Fixed(video_w);
        let stack_h = Length::Fixed(video_h);
        let compositor_area = if sub_active {
            container(
                stack([video_layer, overlay_layer])
                    .width(stack_w)
                    .height(stack_h),
            )
            // Transparent: subsurface below shows through.
            .width(Length::Fill)
            .height(Length::Fill)
        } else {
            container(
                stack([video_layer, overlay_layer])
                    .width(stack_w)
                    .height(stack_h),
            )
            .style(black_bg)
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
        };

        // ── GPU side panel (ADR-0024 Block 2) ─────────────────────────────
        // All N GPU-path sources rendered as a mini-grid, mirroring the
        // compositor layout.  Each source gets its computed NDC rect so the
        // positions match the compositor tiles for the alignment check.
        let gpu_vid_w = (gpu_panel_w - 16.0).max(1.0); // 8 px padding each side
        let gpu_vid_h = (gpu_vid_w / self.grid_ar).min(avail_h);
        let dark_panel_bg = |_: &iced::Theme| container::Style {
            background: Some(Background::Color(Color::from_rgb(0.05, 0.05, 0.05))),
            ..Default::default()
        };
        // Build (frame, rect) pairs for all GPU-pathed sources.  A single
        // GpuRectProg widget carries all N pairs; the pipeline makes N draw
        // calls with per-slot wgpu resources so they don't overwrite each other.
        let cols_f = self.grid_cols as f32;
        let rows_f = self.grid_rows as f32;
        let gpu_sources: Vec<(Option<Arc<TimedFrame>>, [f32; 4])> = self
            .sources
            .iter()
            .enumerate()
            .map(|(idx, src)| {
                let col = (idx as u32 % self.grid_cols) as f32;
                let row = (idx as u32 / self.grid_cols) as f32;
                let rect = [
                    -1.0 + 2.0 * col / cols_f,
                    1.0 - 2.0 * (row + 1.0) / rows_f,
                    -1.0 + 2.0 * (col + 1.0) / cols_f,
                    1.0 - 2.0 * row / rows_f,
                ];
                (self.current_gpu_frames.get(&src.id).cloned(), rect)
            })
            .collect();
        let any_frame = gpu_sources.iter().any(|(f, _)| f.is_some());
        let gpu_grid: Element<Message> = if any_frame {
            shader(GpuRectProg {
                sources: gpu_sources,
            })
            .width(Length::Fixed(gpu_vid_w))
            .height(Length::Fixed(gpu_vid_h))
            .into()
        } else {
            container(text("waiting…").color(Color::WHITE))
                .width(Length::Fixed(gpu_vid_w))
                .height(Length::Fixed(gpu_vid_h))
                .center_x(Length::Fixed(gpu_vid_w))
                .center_y(Length::Fixed(gpu_vid_h))
                .into()
        };
        let n_probed = self.gpu_stores.len();
        let gpu_label = text(format!("GPU PATH — {n_probed} sources"))
            .size(11)
            .color(Color::WHITE);
        let gpu_side_panel = container(column![gpu_label, gpu_grid].spacing(4))
            .style(dark_panel_bg)
            .width(Length::Fixed(gpu_panel_w))
            .height(Length::Fill)
            .padding(8);

        let video_area: Element<Message> = if sub_active {
            compositor_area.into()
        } else {
            row![compositor_area, gpu_side_panel]
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
        };

        // ── Chrome: master Play/Pause only ─────────────────────────────────
        let play_label = if self.playing {
            "⏸  Pause"
        } else {
            "▶  Play"
        };
        let elapsed = self.start_time.elapsed().as_secs();
        let mission_clock = text(format!(
            "{:02}:{:02}:{:02}",
            elapsed / 3600,
            (elapsed % 3600) / 60,
            elapsed % 60,
        ))
        .size(14)
        .color(Color::WHITE);
        let chrome = container(
            row![
                button(play_label).on_press(Message::TogglePlay),
                button("↺  Reset Rate").on_press(Message::ResetRatchet),
                container(mission_clock)
                    .width(Length::Fill)
                    .align_x(iced::alignment::Horizontal::Right),
            ]
            .spacing(8)
            .align_y(iced::alignment::Vertical::Center),
        )
        .padding(8);

        column![video_area, chrome].into()
    }

    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            iced::window::frames().map(|_| Message::Frame),
            iced::time::every(Duration::from_millis(500)).map(|_| Message::Poll),
            iced::event::listen_with(on_window_event),
        ])
    }

    // ── Tile overlay helpers ───────────────────────────────────────────────

    fn tile_overlay_grid(&self) -> Element<'_, Message> {
        let mut col_children: Vec<Element<'_, Message>> = Vec::new();

        for row_idx in 0..self.grid_rows {
            let mut row_children: Vec<Element<'_, Message>> = Vec::new();

            for col_idx in 0..self.grid_cols {
                let src_idx = (row_idx * self.grid_cols + col_idx) as usize;
                let cell: Element<Message> = if src_idx < self.sources.len() {
                    self.tile_overlay(src_idx)
                } else {
                    // Empty transparent cell to fill the grid.
                    container(text(""))
                        .width(Length::FillPortion(1))
                        .height(Length::Fill)
                        .into()
                };
                row_children.push(cell);
            }

            col_children.push(
                row(row_children)
                    .width(Length::Fill)
                    .height(Length::FillPortion(1))
                    .into(),
            );
        }

        column(col_children)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    fn tile_overlay(&self, i: usize) -> Element<'_, Message> {
        let src = &self.sources[i];
        let (metrics_line, peak_db) = self
            .source_metrics
            .get(i)
            .map(|m| {
                (
                    format!(
                        "in {:.1} fps  out {:.1} fps  drop {}  bad {}",
                        m.fps_in, m.fps_out, m.dropped_frames, m.bad_frames
                    ),
                    m.audio_peak_db,
                )
            })
            .unwrap_or_default();

        // Offset controls: steppers disabled when at the source's effective limit.
        let at_min = src.offset_ms <= src.min_offset_ms;
        let at_max = src.offset_ms >= src.max_offset_ms;
        let btn_neg1s = {
            let b = button("−1s");
            if at_min {
                b
            } else {
                b.on_press(Message::OffsetStep {
                    index: i,
                    delta: -1000,
                })
            }
        };
        let btn_neg10 = {
            let b = button("−10");
            if at_min {
                b
            } else {
                b.on_press(Message::OffsetStep {
                    index: i,
                    delta: -10,
                })
            }
        };
        let btn_pos10 = {
            let b = button("+10");
            if at_max {
                b
            } else {
                b.on_press(Message::OffsetStep {
                    index: i,
                    delta: 10,
                })
            }
        };
        let btn_pos1s = {
            let b = button("+1s");
            if at_max {
                b
            } else {
                b.on_press(Message::OffsetStep {
                    index: i,
                    delta: 1000,
                })
            }
        };
        let offset_row = row![
            btn_neg1s,
            btn_neg10,
            text_input("0", &src.offset_buf)
                .on_input(move |s| Message::OffsetEdit { index: i, text: s })
                .on_submit(Message::OffsetNormalise { index: i })
                .width(Length::Fixed(60.0)),
            btn_pos10,
            btn_pos1s,
        ]
        .spacing(3)
        .align_y(iced::alignment::Vertical::Center);
        let range_label = text(format!("{}..{} ms", src.min_offset_ms, src.max_offset_ms)).size(9);

        // Level meter + mute toggle
        let mute_label = if src.muted { "[M]" } else { "M" };
        let meter_row = row![
            audio_meter(peak_db),
            button(mute_label).on_press(Message::ToggleMute { index: i }),
        ]
        .spacing(4)
        .align_y(iced::alignment::Vertical::Center);

        let reboot_row: Option<Element<Message>> = if src.is_external {
            let reboot_btn = button("⟳ Reboot");
            let kill_btn = button("✕ Kill");
            let kill_locked = src.transitioning || src.kill_cooldown_until.is_some();
            let (reboot_btn, kill_btn) = if src.transitioning {
                (reboot_btn, kill_btn)
            } else if kill_locked {
                (reboot_btn.on_press(Message::Reboot { index: i }), kill_btn)
            } else {
                (
                    reboot_btn.on_press(Message::Reboot { index: i }),
                    kill_btn.on_press(Message::Kill { index: i }),
                )
            };
            Some(row![reboot_btn, kill_btn].spacing(4).into())
        } else {
            None
        };

        let mut ctrl_col = column![
            text(&src.id).size(13),
            text(&src.display_name).size(10),
            offset_row,
            range_label,
            meter_row,
            text(metrics_line).size(10),
        ]
        .spacing(3);
        if let Some(rb) = reboot_row {
            ctrl_col = ctrl_col.push(rb);
        }
        let control_box = ctrl_col;

        let dark_bg = |_: &iced::Theme| container::Style {
            background: Some(Background::Color(Color::from_rgba(0.0, 0.0, 0.0, 0.7))),
            ..Default::default()
        };
        // Determine per-tile display state.
        let stream_drained = self
            .source_metrics
            .get(i)
            .map(|m| m.stream_drained)
            .unwrap_or(false);
        let file_terminated =
            !src.is_external && src.has_ever_had_frames && stream_drained && self.playing;
        let state_label: Option<&str> = if src.is_external && src.signal_lost {
            Some("SIGNAL LOST")
        } else if file_terminated {
            Some("FILE TERMINATED")
        } else {
            None
        };

        // State overlay: translucent 50% black, white text, centered.
        // White border only shown when the source is dead (signal lost / terminated).
        let state_layer: Element<Message> = if let Some(label) = state_label {
            let overlay_bg = |_: &iced::Theme| container::Style {
                background: Some(Background::Color(Color::from_rgba(0.0, 0.0, 0.0, 0.5))),
                ..Default::default()
            };
            container(
                container(text(label).size(20).color(Color::WHITE))
                    .style(overlay_bg)
                    .padding(12),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into()
        } else {
            container(text(""))
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
        };

        // Control box anchored bottom-left.
        let controls_layer = container(container(control_box).style(dark_bg).padding(6))
            .width(Length::Fill)
            .height(Length::Fill)
            .clip(true)
            .align_x(iced::alignment::Horizontal::Left)
            .align_y(iced::alignment::Vertical::Bottom);

        container(stack([controls_layer.into(), state_layer]))
            .width(Length::FillPortion(1))
            .height(Length::Fill)
            .into()
    }
}

// ── Standalone helpers ────────────────────────────────────────────────────────

/// Route window events to messages.
fn on_window_event(
    event: iced::Event,
    _status: iced::event::Status,
    id: iced::window::Id,
) -> Option<Message> {
    match event {
        iced::Event::Window(iced::window::Event::CloseRequested) => Some(Message::Exit),
        iced::Event::Window(iced::window::Event::Resized(s)) => Some(Message::Resized {
            width: s.width,
            height: s.height,
        }),
        iced::Event::Window(iced::window::Event::Opened { size: s, .. }) => {
            // On Linux/Wayland: SpikeInit fetches raw handles then kicks off
            // subsurface creation.  On other platforms fall back to Resized.
            #[cfg(target_os = "linux")]
            {
                return Some(Message::SpikeInit(id, s.width, s.height));
            }
            #[cfg(not(target_os = "linux"))]
            Some(Message::Resized {
                width: s.width,
                height: s.height,
            })
        }
        _ => None,
    }
}

/// LED-style segmented audio level meter spanning DB_FLOOR → 0 dBFS.
/// ~20 cells; green below -12 dB, yellow -12…-3 dB, red ≥ -3 dB.
fn audio_meter(peak_db: f64) -> Element<'static, Message> {
    const SEGMENTS: usize = 20;
    const DB_LOW: f64 = -12.0;
    const DB_CLIP: f64 = -3.0;
    use fm_adapter_sdk::metrics::DB_FLOOR;

    let cells: Vec<Element<'static, Message>> = (0..SEGMENTS)
        .map(|i| {
            let threshold = DB_FLOOR + (i + 1) as f64 * (-DB_FLOOR / SEGMENTS as f64);
            let zone = if threshold >= DB_CLIP {
                Color::from_rgb(0.8, 0.1, 0.1)
            } else if threshold >= DB_LOW {
                Color::from_rgb(0.8, 0.7, 0.0)
            } else {
                Color::from_rgb(0.1, 0.7, 0.1)
            };
            let color = if peak_db >= threshold {
                zone
            } else {
                Color::from_rgb(0.1, 0.1, 0.1)
            };
            container(text(""))
                .width(Length::Fixed(6.0))
                .height(Length::Fixed(14.0))
                .style(move |_: &iced::Theme| container::Style {
                    background: Some(Background::Color(color)),
                    ..Default::default()
                })
                .into()
        })
        .collect();

    row(cells).spacing(1).into()
}

/// Extract the display filename from a URI, truncated to preserve the extension.
fn uri_display_name(uri: &str) -> String {
    let raw = uri.split('/').last().unwrap_or(uri);
    truncate_preserve_ext(raw, 24)
}

fn truncate_preserve_ext(name: &str, budget: usize) -> String {
    if name.chars().count() <= budget {
        return name.to_string();
    }
    if let Some(dot) = name.rfind('.') {
        let ext = &name[dot..];
        let stem = &name[..dot];
        let ext_len = ext.chars().count();
        let stem_budget = budget.saturating_sub(ext_len + 1);
        if stem_budget == 0 {
            return format!("…{ext}");
        }
        let truncated: String = stem.chars().take(stem_budget).collect();
        format!("{truncated}…{ext}")
    } else {
        let truncated: String = name.chars().take(budget.saturating_sub(1)).collect();
        format!("{truncated}…")
    }
}

fn try_init(
    config_path: &std::path::Path,
    frame_store: bridge::FrameStore,
) -> Result<App, Box<dyn std::error::Error + Send + Sync>> {
    let scene = fm_core::config::load(config_path)?;

    let n = scene.source.len() as u32;
    let cols = scene.grid.columns.max(1).min(n.max(1));
    let rows = (n.max(1) + cols - 1) / cols;
    // scene.grid.width/height are per-tile; canvas is cols×tile × rows×tile.
    let grid_ar =
        (cols as f32 * scene.grid.width as f32) / (rows as f32 * scene.grid.height as f32);

    // Per-source bounds are finalised after the Ready wait loop; start with
    // file-source defaults and overwrite for external sources below.
    let file_min = MIN_OFFSET_MS;
    let file_max = MAX_OFFSET_MS;
    let mut sources: Vec<SourceRow> = scene
        .source
        .iter()
        .map(|s| SourceRow {
            id: s.id.clone(),
            offset_ms: s.offset_ms.clamp(file_min as i64, file_max as i64) as i32,
            offset_buf: s.offset_ms.to_string(),
            muted: s.muted,
            display_name: uri_display_name(s.uri.as_deref().unwrap_or("")),
            min_offset_ms: file_min,
            max_offset_ms: file_max,
            is_external: s.source_type == fm_core::config::SourceType::External,
            signal_lost: false,
            has_ever_had_frames: false,
            transitioning: false,
            kill_cooldown_until: None,
        })
        .collect();

    let config_persist = fm_core::persist::ConfigPersist::load(config_path).ok();

    // ── Adapter supervisor (Phase 2) ─────────────────────────────────────
    // Adapters must be spawned and their shmsink sockets must exist BEFORE
    // the core pipeline goes to PLAYING, because shmsrc tries to open those
    // sockets during its READY→PAUSED state transition.  Startup sequence:
    //   1. gstreamer::init (needed for SystemClock)
    //   2. Create NetClock
    //   3. Spawn adapters
    //   4. Wait for all adapters to send Ready (sockets now exist)
    //   5. Build pipeline + transport.play()
    gstreamer::init()?;

    let has_external = scene
        .source
        .iter()
        .any(|s| s.source_type == fm_core::config::SourceType::External);

    // Spawn adapters at the full grid resolution (ADR-0012 core-owned resize).
    // The adapter produces at prod_res; the core's vshmcaps → vscale → vcaps
    // chain downscales to tile dimensions inside the compositor pipeline.
    let (supervisor, external_ids, mut net_clock) = if has_external {
        let net = fm_core::net_clock::NetClock::new()?;
        let mut sup = fm_core::supervisor::Supervisor::new();
        sup.set_delivery_watchdog_ms(scene.grid.delivery_watchdog_ms);
        sup.set_adapter_dir(scene.grid.adapter_dir.clone());
        let mut ids: Vec<String> = Vec::new();
        for s in &scene.source {
            if s.source_type != fm_core::config::SourceType::External {
                continue;
            }
            let binary = s.adapter.as_deref().unwrap_or("fm-dummy-adapter");
            if let Err(e) = sup.spawn(
                binary,
                &s.id,
                &net,
                scene.grid.width,
                scene.grid.height,
                scene.grid.fps,
                s.uri.as_deref(),
                s.extra_args.clone(),
            ) {
                eprintln!("[app] failed to spawn adapter for '{}': {e}", s.id);
            } else {
                ids.push(s.id.clone());
            }
        }

        // Wait for all adapters to send Ready (sockets must exist before shmsrc
        // transitions to PAUSED).  Timeout is configurable in the scene [grid]
        // section; RTSP cold-start can comfortably exceed 10 s.
        let timeout = scene.grid.adapter_ready_timeout_secs;
        let status = sup.status_handle();
        let deadline = std::time::Instant::now() + Duration::from_secs(timeout);
        loop {
            let all_ready = {
                let s = status.lock().unwrap();
                ids.iter().all(|id| {
                    matches!(
                        s.get(id),
                        Some(fm_core::supervisor::AdapterStatus {
                            state: fm_core::supervisor::AdapterState::Running,
                            ..
                        })
                    )
                })
            };
            if all_ready {
                break;
            }
            if std::time::Instant::now() >= deadline {
                eprintln!(
                    "[app] WARNING: not all adapters ready within {timeout}s — proceeding anyway"
                );
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }

        (Some(sup), ids, Some(net))
    } else {
        (None, Vec::new(), None)
    };

    // Collect has_video/has_audio per external source from the Ready messages
    // received during the wait above.  The pipeline wires only present streams.
    // Also collect declared offset capability for per-source UI bounds (ADR-0017).
    let mut external_caps: std::collections::HashMap<String, (bool, bool)> =
        std::collections::HashMap::new();
    // effective bounds: (min_ms, max_ms)
    let mut source_effective_bounds: std::collections::HashMap<String, (i32, i32)> =
        std::collections::HashMap::new();
    if let Some(sup) = &supervisor {
        let s = sup.status_handle();
        let s = s.lock().unwrap();
        let ceiling = scene.grid.live_offset_ceiling_ms;
        for id in &external_ids {
            if let Some(a) = s.get(id) {
                // If the adapter never sent Ready (has_video is None — startup
                // timeout fired while it was still reconnecting), default to
                // false so we don't build a chain for a non-existent socket.
                // The chain arrives later via StreamsChanged when the source
                // comes online.
                external_caps.insert(
                    id.clone(),
                    (a.has_video.unwrap_or(false), a.has_audio.unwrap_or(false)),
                );
                // Reconcile declared max with core ceiling (ADR-0016/0017).
                let declared_max = a.max_offset_ms.unwrap_or(ceiling);
                let effective_max = declared_max.min(ceiling) as i32;
                // Live (PositiveOnly) sources: [0, effective_max]; Signed: unclamped at 0.
                use fm_adapter_sdk::contract::OffsetPolarity;
                let min = match &a.offset_polarity {
                    Some(OffsetPolarity::Signed) => -(effective_max),
                    _ => 0, // PositiveOnly or unset → no negative offsets
                };
                source_effective_bounds.insert(id.clone(), (min, effective_max));
            }
        }
    }
    // Apply effective bounds to sources (external sources only; file sources keep defaults).
    for src in &mut sources {
        if let Some(&(min, max)) = source_effective_bounds.get(&src.id) {
            src.min_offset_ms = min;
            src.max_offset_ms = max;
            src.offset_ms = src.offset_ms.clamp(min, max);
            src.offset_buf = src.offset_ms.to_string();
        }
    }

    let pipeline = fm_core::pipeline::Pipeline::build(&scene, &external_caps)?;
    let metrics = fm_core::metrics::MetricsCollector::attach(&pipeline);
    let bus_pipe = pipeline.inner().clone();

    bridge::install(pipeline.appsink(), frame_store.clone());

    // GPU presentation path (ADR-0024, Block 2/B2): probe every source with a
    // video pad at the pre-scale tap (vdeint:src) so the GPU path receives
    // frames at native input resolution rather than tile-res (ADR-0024 B2).
    // Ring sized by the scene's offset ceiling (time-based, not frame-count).
    let gpu_stores: HashMap<String, GpuFrameStore> = sources
        .iter()
        .filter_map(|s| {
            pipeline
                .source_pads()
                .get(&s.id)
                .and_then(|p| p.pre_scale_video_src.as_ref())
                .map(|pad| {
                    let store = gpu_path::new_store(scene.grid.live_offset_ceiling_ms as u64);
                    gpu_path::install_probe(pad, store.clone());
                    eprintln!("[gpu-path] native-res probe installed on vdeint_{}", s.id);
                    (s.id.clone(), store)
                })
        })
        .collect();

    let mut transport = fm_core::transport::Transport::new(pipeline);
    for (id, (min, max)) in &source_effective_bounds {
        transport.set_source_bounds(id, *min as i64, *max as i64);
    }
    transport.play()?;

    // Group 2 — cascade fix: wait for the pipeline to actually reach PLAYING
    // before telling adapters to push frames.  Live pipelines return Async from
    // set_state(Playing); if adapters push before aggregators are PLAYING, the
    // first buffer returns GST_FLOW_ERROR and the aggregator permanently latches
    // that error, rejecting all subsequent pushes with -5.
    if !transport.wait_for_playing(10) {
        eprintln!(
            "[app] WARNING: pipeline did not reach PLAYING within 10 s — \
             cascade possible"
        );
    }

    // Establish the audio hardware clock as pipeline master (ADR-0027).
    //
    // GStreamer's auto-clock selection ranks GstSystemClock (REALTIME) above
    // GstAudioClock (OTHER), so the system clock always wins.  pulsesink's
    // provided clock is only synced after PLAYING (the ring buffer isn't running
    // in PAUSED), which means we must force it explicitly here.
    //
    // Steps:
    //   1. Look up the audio sink by name and call provide_clock() to get its
    //      hardware clock (GstAudioClock backed by PulseAudio timing).
    //   2. Force it on the pipeline with use_clock() — this sets
    //      GST_PIPELINE_FLAG_FIXED_CLOCK so auto-selection can't override it.
    //      The switch is safe: on one machine audio_clock ≈ system_clock within
    //      microseconds, so running_time continuity is preserved within the
    //      audiomixer's 50 ms jitter budget.
    //   3. Switch the NetTimeProvider to the same clock so adapters' NetClientClock
    //      now tracks the sound card instead of the system clock.
    //
    // Fallback (no audio device / headless): provide_clock() returns None;
    // we keep the system-clock provider and live with slaving artifacts.
    if let Some(ref mut net) = net_clock {
        let gst_pipeline = transport.pipeline().inner();
        let audio_clock = gst_pipeline
            .by_name("autoaudiosink")
            .and_then(|sink| sink.provide_clock());

        match audio_clock {
            Some(clock) => {
                eprintln!(
                    "[net-clock] pulsesink clock: {} — forcing as pipeline master",
                    clock.type_().name()
                );
                gst_pipeline.use_clock(Some(&clock));
                if let Err(e) = net.switch_to_clock(&clock) {
                    eprintln!("[net-clock] WARNING: provider switch failed: {e}");
                }
            }
            None => {
                eprintln!(
                    "[net-clock] pulsesink returned no clock (headless?); \
                     keeping system clock"
                );
            }
        }
    }

    // Pipeline is confirmed PLAYING; safe to tell adapters to start streaming.
    let supervisor = if let Some(mut sup) = supervisor {
        sup.send_play_all();
        Some(sup)
    } else {
        None
    };
    let _ = external_ids; // used above

    let audio_store = metrics.audio_store();
    std::thread::spawn(move || fm_core::transport::run_bus_loop(bus_pipe, audio_store));

    Ok(App {
        transport: Some(transport),
        metrics: Some(metrics),
        supervisor,
        net_clock,
        frame_store,
        current_frame: None,
        frame_gen: 0,
        playing: true,
        sources,
        source_metrics: Vec::new(),
        grid_cols: cols,
        grid_rows: rows,
        grid_ar,
        win_w: 1280.0,
        win_h: 720.0,
        error: None,
        config_persist,
        last_offset_change: None,
        external_source_ids: external_ids,
        gpu_stores,
        current_gpu_frames: HashMap::new(),
        #[cfg(target_os = "linux")]
        spike_thread: None,
        #[cfg(target_os = "linux")]
        render_shared: None,
        #[cfg(target_os = "linux")]
        render_running_time: None,
        #[cfg(target_os = "linux")]
        window_size: None,
        start_time: Instant::now(),
    })
}

/// Build the initial render slot list from the current gpu_stores + source layout.
/// NDC rects computed as an equal-split grid matching the compositor layout.
#[cfg(target_os = "linux")]
fn build_render_slots(
    gpu_stores: &HashMap<String, GpuFrameStore>,
    sources: &[SourceRow],
    grid_cols: u32,
    grid_rows: u32,
) -> Vec<crate::wayland_sub::RenderSlot> {
    let cols_f = grid_cols as f32;
    let rows_f = grid_rows as f32;
    sources
        .iter()
        .enumerate()
        .filter_map(|(idx, src)| {
            let store = gpu_stores.get(&src.id)?.clone();
            let col = (idx as u32 % grid_cols) as f32;
            let row = (idx as u32 / grid_cols) as f32;
            let rect = [
                -1.0 + 2.0 * col / cols_f,
                1.0 - 2.0 * (row + 1.0) / rows_f,
                -1.0 + 2.0 * (col + 1.0) / cols_f,
                1.0 - 2.0 * row / rows_f,
            ];
            Some(crate::wayland_sub::RenderSlot {
                store,
                offset_ns: src.offset_ms as i64 * 1_000_000,
                rect,
            })
        })
        .collect()
}
