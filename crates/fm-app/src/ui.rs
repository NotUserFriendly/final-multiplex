use crate::bridge::{self, FrameData};
use crate::video::VideoProg;
use fm_adapter_sdk::metrics::SourceMetrics;
use iced::widget::{button, column, container, row, shader, slider, text};
use iced::{Background, Color, Element, Length, Subscription, Task};
use std::sync::Arc;
use std::time::Duration;

pub struct App {
    transport: Option<fm_core::transport::Transport>,
    metrics: Option<fm_core::metrics::MetricsCollector>,
    frame_store: bridge::FrameStore,
    current_frame: Option<Arc<FrameData>>,
    frame_gen: u64,
    playing: bool,
    source_ids: Vec<String>,
    /// (source_id, offset_ms) — matches scene order.
    offsets_ms: Vec<(String, i32)>,
    source_metrics: Vec<SourceMetrics>,
    error: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Message {
    Tick,
    TogglePlay,
    SetOffset { index: usize, ms: i32 },
}

impl App {
    pub fn init(config_path: &std::path::Path) -> Self {
        let frame_store = bridge::new_store();
        match try_init(config_path, frame_store.clone()) {
            Ok(state) => state,
            Err(e) => Self {
                transport: None,
                metrics: None,
                frame_store,
                current_frame: None,
                frame_gen: 0,
                playing: false,
                source_ids: Vec::new(),
                offsets_ms: Vec::new(),
                source_metrics: Vec::new(),
                error: Some(e.to_string()),
            },
        }
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Tick => {
                if let Some(frame) = bridge::latest_frame(&self.frame_store, &mut self.frame_gen) {
                    self.current_frame = Some(frame);
                }
                if let Some(metrics) = &self.metrics {
                    self.source_metrics = self
                        .source_ids
                        .iter()
                        .map(|id| metrics.snapshot(id))
                        .collect();
                }
            }

            Message::TogglePlay => {
                if let Some(t) = &self.transport {
                    if self.playing {
                        let _ = t.pause();
                        self.playing = false;
                    } else {
                        let _ = t.play();
                        self.playing = true;
                    }
                }
            }

            Message::SetOffset { index, ms } => {
                if let Some((id, current)) = self.offsets_ms.get_mut(index) {
                    *current = ms;
                    if let Some(t) = &self.transport {
                        let _ = t.set_source_offset(id, ms as i64);
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

        // ── Video display — persistent wgpu texture, no Handle churn ──────
        // The black container provides the letterbox/pillarbox bar colour;
        // the shader scales the quad to maintain aspect ratio within its bounds.
        let black_bg = |_: &iced::Theme| container::Style {
            background: Some(Background::Color(Color::BLACK)),
            ..Default::default()
        };
        let video: Element<Message> = if self.current_frame.is_some() {
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

        // ── Transport controls ─────────────────────────────────────────────
        let play_label = if self.playing {
            "⏸  Pause"
        } else {
            "▶  Play"
        };
        let transport_row = row![button(play_label).on_press(Message::TogglePlay)].spacing(8);

        // ── Per-source offset + metrics ───────────────────────────────────
        let mut sources_col = column![].spacing(6);
        for (i, (id, offset)) in self.offsets_ms.iter().enumerate() {
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

            let row_widget = row![
                text(format!("{id}")).width(Length::Fixed(100.0)),
                slider(-5000..=5000, *offset, move |v| Message::SetOffset {
                    index: i,
                    ms: v
                })
                .width(Length::Fixed(280.0)),
                text(format!("{:+} ms", offset)).width(Length::Fixed(80.0)),
                audio_meter(peak_db),
                text(metrics_line),
            ]
            .spacing(8)
            .align_y(iced::alignment::Vertical::Center);

            sources_col = sources_col.push(row_widget);
        }

        column![
            video,
            container(column![transport_row, sources_col].spacing(8)).padding(10),
        ]
        .into()
    }

    pub fn subscription(&self) -> Subscription<Message> {
        iced::time::every(Duration::from_millis(16)).map(|_| Message::Tick)
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

fn try_init(
    config_path: &std::path::Path,
    frame_store: bridge::FrameStore,
) -> Result<App, Box<dyn std::error::Error + Send + Sync>> {
    let scene = fm_core::config::load(config_path)?;

    let source_ids: Vec<String> = scene.source.iter().map(|s| s.id.clone()).collect();
    let offsets_ms: Vec<(String, i32)> = scene
        .source
        .iter()
        .map(|s| {
            (
                s.id.clone(),
                s.offset_ms.clamp(i32::MIN as i64, i32::MAX as i64) as i32,
            )
        })
        .collect();

    let pipeline = fm_core::pipeline::Pipeline::build(&scene)?;
    let metrics = fm_core::metrics::MetricsCollector::attach(&pipeline);
    let bus_pipe = pipeline.inner().clone();

    bridge::install(pipeline.appsink(), frame_store.clone());

    let transport = fm_core::transport::Transport::new(pipeline);
    transport.play()?;
    let audio_store = metrics.audio_store();
    std::thread::spawn(move || fm_core::transport::run_bus_loop(bus_pipe, audio_store));

    Ok(App {
        transport: Some(transport),
        metrics: Some(metrics),
        frame_store,
        current_frame: None,
        frame_gen: 0,
        playing: true,
        source_ids,
        offsets_ms,
        source_metrics: Vec::new(),
        error: None,
    })
}
