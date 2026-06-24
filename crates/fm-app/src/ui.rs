use crate::bridge::{self, FrameData};
use crate::video::VideoProg;
use fm_adapter_sdk::metrics::SourceMetrics;
use iced::widget::{button, column, container, row, shader, stack, text, text_input};
use iced::{Background, Color, Element, Length, Subscription, Task};
use std::sync::Arc;
use std::time::{Duration, Instant};

const MIN_OFFSET_MS: i32 = -60_000;
const MAX_OFFSET_MS: i32 = 60_000;
pub(crate) const CHROME_H: f32 = 50.0;

struct SourceRow {
    id: String,
    offset_ms: i32,
    /// Live editing buffer; only committed to offset_ms on valid parse.
    offset_buf: String,
    muted: bool,
    /// Truncated uri basename for display.
    display_name: String,
}

pub struct App {
    transport: Option<fm_core::transport::Transport>,
    metrics: Option<fm_core::metrics::MetricsCollector>,
    /// Out-of-process adapter supervisor (Phase 2). None if no external sources.
    supervisor: Option<fm_core::supervisor::Supervisor>,
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
    /// Tick counter for supervisor polling (poll every ~500 ms at 60 Hz ticks).
    tick_count: u64,
}

#[derive(Debug, Clone)]
pub enum Message {
    Tick,
    TogglePlay,
    /// Typed text in an offset box; commits on valid parse.
    OffsetEdit {
        index: usize,
        text: String,
    },
    /// Stepper button: saturating add delta (ms), clamp to ±MAX_OFFSET_MS.
    OffsetStep {
        index: usize,
        delta: i32,
    },
    ToggleMute {
        index: usize,
    },
    Resized {
        width: f32,
        height: f32,
    },
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
                    config_persist: None,
                    last_offset_change: None,
                    tick_count: 0,
                }
            }
        }
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Tick => {
                self.tick_count = self.tick_count.wrapping_add(1);

                if let Some(frame) = bridge::latest_frame(&self.frame_store, &mut self.frame_gen) {
                    self.current_frame = Some(frame);
                }
                if let Some(metrics) = &self.metrics {
                    self.source_metrics = self
                        .sources
                        .iter()
                        .map(|s| metrics.snapshot(&s.id))
                        .collect();
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
                // Poll adapter supervisor ~every 500 ms (every 30 ticks at 60 Hz).
                if self.tick_count % 30 == 0 {
                    if let Some(sup) = &mut self.supervisor {
                        sup.poll();
                        // Reset shmsrc elements for any adapters that restarted
                        // so they reconnect to the new adapter sockets.
                        for id in sup.take_restarted() {
                            if let Some(t) = &self.transport {
                                t.restart_external_source(&id);
                            }
                        }
                    }
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
                        src.offset_ms = ms.clamp(MIN_OFFSET_MS, MAX_OFFSET_MS);
                        if let Some(t) = &self.transport {
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

            Message::OffsetStep { index, delta } => {
                let mut persist_change: Option<(String, i64)> = None;
                if let Some(src) = self.sources.get_mut(index) {
                    src.offset_ms = src
                        .offset_ms
                        .saturating_add(delta)
                        .clamp(MIN_OFFSET_MS, MAX_OFFSET_MS);
                    src.offset_buf = src.offset_ms.to_string();
                    if let Some(t) = &self.transport {
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
                    if let Some(t) = &self.transport {
                        let _ = t.set_source_mute(&src.id, src.muted);
                    }
                }
            }

            Message::Resized { width, height } => {
                self.win_w = width;
                self.win_h = height;
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

        // ── Compute video display dimensions locked to output aspect ratio ──
        // The container is black (letterbox/pillarbox bars); the video Stack
        // inside is sized exactly to the grid AR so tile overlays align.
        let avail_h = (self.win_h - CHROME_H).max(1.0);
        let video_w = (avail_h * self.grid_ar).min(self.win_w);
        let video_h = video_w / self.grid_ar;

        // ── Layer 0: video shader ──────────────────────────────────────────
        let black_bg = |_: &iced::Theme| container::Style {
            background: Some(Background::Color(Color::BLACK)),
            ..Default::default()
        };
        let video_layer: Element<Message> = if self.current_frame.is_some() {
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

        // Stack + centre in black surround
        let video_area = container(
            stack([video_layer, overlay_layer])
                .width(Length::Fixed(video_w))
                .height(Length::Fixed(video_h)),
        )
        .style(black_bg)
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill);

        // ── Chrome: master Play/Pause only ─────────────────────────────────
        let play_label = if self.playing {
            "⏸  Pause"
        } else {
            "▶  Play"
        };
        let chrome =
            container(row![button(play_label).on_press(Message::TogglePlay)].spacing(8)).padding(8);

        column![video_area, chrome].into()
    }

    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            iced::time::every(Duration::from_millis(16)).map(|_| Message::Tick),
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
                        "in {:.1} fps  out {:.1} fps  dropped {}",
                        m.fps_in, m.fps_out, m.dropped_frames
                    ),
                    m.audio_peak_db,
                )
            })
            .unwrap_or_default();

        // Offset controls: [−1s] [−10ms] [ text box ] [+10ms] [+1s]
        let offset_row = row![
            button("−1s").on_press(Message::OffsetStep {
                index: i,
                delta: -1000
            }),
            button("−10").on_press(Message::OffsetStep {
                index: i,
                delta: -10
            }),
            text_input("0", &src.offset_buf)
                .on_input(move |s| Message::OffsetEdit { index: i, text: s })
                .width(Length::Fixed(60.0)),
            button("+10").on_press(Message::OffsetStep {
                index: i,
                delta: 10
            }),
            button("+1s").on_press(Message::OffsetStep {
                index: i,
                delta: 1000
            }),
        ]
        .spacing(3)
        .align_y(iced::alignment::Vertical::Center);

        // Level meter + mute toggle
        let mute_label = if src.muted { "[M]" } else { "M" };
        let meter_row = row![
            audio_meter(peak_db),
            button(mute_label).on_press(Message::ToggleMute { index: i }),
        ]
        .spacing(4)
        .align_y(iced::alignment::Vertical::Center);

        let control_box = column![
            text(&src.id).size(13),
            text(&src.display_name).size(10),
            offset_row,
            meter_row,
            text(metrics_line).size(10),
        ]
        .spacing(3);

        let dark_bg = |_: &iced::Theme| container::Style {
            background: Some(Background::Color(Color::from_rgba(0.0, 0.0, 0.0, 0.7))),
            ..Default::default()
        };

        // Transparent outer cell; dark control box anchored to bottom-left.
        container(container(control_box).style(dark_bg).padding(6))
            .width(Length::FillPortion(1))
            .height(Length::Fill)
            .align_x(iced::alignment::Horizontal::Left)
            .align_y(iced::alignment::Vertical::Bottom)
            .into()
    }
}

// ── Standalone helpers ────────────────────────────────────────────────────────

/// Route window open/resize events to Message::Resized.
fn on_window_event(
    event: iced::Event,
    _status: iced::event::Status,
    _id: iced::window::Id,
) -> Option<Message> {
    let size = match event {
        iced::Event::Window(iced::window::Event::Resized(s)) => s,
        iced::Event::Window(iced::window::Event::Opened { size: s, .. }) => s,
        _ => return None,
    };
    Some(Message::Resized {
        width: size.width,
        height: size.height,
    })
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
    let grid_ar = scene.grid.width as f32 / scene.grid.height as f32;

    let sources: Vec<SourceRow> = scene
        .source
        .iter()
        .map(|s| SourceRow {
            id: s.id.clone(),
            offset_ms: s
                .offset_ms
                .clamp(MIN_OFFSET_MS as i64, MAX_OFFSET_MS as i64) as i32,
            offset_buf: s.offset_ms.to_string(),
            muted: false,
            display_name: uri_display_name(s.uri.as_deref().unwrap_or("")),
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
    let (supervisor, external_ids) = if has_external {
        let net = fm_core::net_clock::NetClock::new()?;
        let mut sup = fm_core::supervisor::Supervisor::new();
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

        (Some(sup), ids)
    } else {
        (None, Vec::new())
    };

    // Collect has_video/has_audio per external source from the Ready messages
    // received during the wait above.  The pipeline wires only present streams.
    let mut external_caps: std::collections::HashMap<String, (bool, bool)> =
        std::collections::HashMap::new();
    if let Some(sup) = &supervisor {
        let s = sup.status_handle();
        let s = s.lock().unwrap();
        for id in &external_ids {
            if let Some(a) = s.get(id) {
                external_caps.insert(
                    id.clone(),
                    (a.has_video.unwrap_or(true), a.has_audio.unwrap_or(true)),
                );
            }
        }
    }

    let pipeline = fm_core::pipeline::Pipeline::build(&scene, &external_caps)?;
    let metrics = fm_core::metrics::MetricsCollector::attach(&pipeline);
    let bus_pipe = pipeline.inner().clone();

    bridge::install(pipeline.appsink(), frame_store.clone());

    let transport = fm_core::transport::Transport::new(pipeline);
    transport.play()?;

    // Pipeline is playing; tell adapters to start streaming.
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
        tick_count: 0,
    })
}
