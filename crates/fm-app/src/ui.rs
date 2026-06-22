use std::time::Duration;
use iced::widget::{button, column, container, image, row, slider, text};
use iced::{Element, Length, Subscription, Task};
use fm_adapter_sdk::metrics::SourceMetrics;
use crate::bridge;

pub struct App {
    transport: Option<fm_core::transport::Transport>,
    metrics: Option<fm_core::metrics::MetricsCollector>,
    frame_store: bridge::FrameStore,
    current_frame: Option<image::Handle>,
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
    /// Boot the app: load config, build pipeline, start playing.
    /// Any error is stored in `self.error` so the UI can display it.
    pub fn init(config_path: &std::path::Path) -> Self {
        let frame_store = bridge::new_store();

        let result = try_init(config_path, frame_store.clone());

        match result {
            Ok(state) => state,
            Err(e) => Self {
                transport: None,
                metrics: None,
                frame_store,
                current_frame: None,
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
                if let Some(handle) = bridge::latest_handle(&self.frame_store) {
                    self.current_frame = Some(handle);
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

        // ── Video display ──────────────────────────────────────────────────
        let video: Element<Message> = if let Some(handle) = &self.current_frame {
            image::Image::new(handle.clone())
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
        } else {
            container(text("Waiting for first frame…"))
                .width(Length::Fill)
                .height(Length::Fill)
                .center_x(Length::Fill)
                .center_y(Length::Fill)
                .into()
        };

        // ── Transport controls ─────────────────────────────────────────────
        let play_label = if self.playing { "⏸  Pause" } else { "▶  Play" };
        let transport_row = row![button(play_label).on_press(Message::TogglePlay)].spacing(8);

        // ── Per-source offset + metrics ───────────────────────────────────
        let mut sources_col = column![].spacing(6);
        for (i, (id, offset)) in self.offsets_ms.iter().enumerate() {
            let m = self.source_metrics.get(i);
            let metrics_line = m.map(|m| {
                format!(
                    "in {:.1} fps  out {:.1} fps  dropped {}",
                    m.fps_in, m.fps_out, m.dropped_frames
                )
            }).unwrap_or_default();

            let row_widget = row![
                text(format!("{id}")).width(Length::Fixed(100.0)),
                slider(-5000..=5000, *offset, move |v| Message::SetOffset { index: i, ms: v })
                    .width(Length::Fixed(280.0)),
                text(format!("{:+} ms", offset)).width(Length::Fixed(80.0)),
                text(metrics_line),
            ]
            .spacing(8)
            .align_y(iced::alignment::Vertical::Center);

            sources_col = sources_col.push(row_widget);
        }

        column![
            video,
            container(
                column![
                    transport_row,
                    sources_col,
                ]
                .spacing(8)
            )
            .padding(10),
        ]
        .into()
    }

    pub fn subscription(&self) -> Subscription<Message> {
        iced::time::every(Duration::from_millis(16)).map(|_| Message::Tick)
    }
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
        .map(|s| (s.id.clone(), s.offset_ms.clamp(i32::MIN as i64, i32::MAX as i64) as i32))
        .collect();

    let pipeline = fm_core::pipeline::Pipeline::build(&scene)?;
    let metrics = fm_core::metrics::MetricsCollector::attach(&pipeline);

    // Clone inner pipeline before Transport takes ownership, for the bus thread.
    let bus_pipeline = pipeline.inner().clone();

    bridge::install(pipeline.appsink(), frame_store.clone());

    let transport = fm_core::transport::Transport::new(pipeline);
    transport.play()?;

    std::thread::spawn(move || fm_core::transport::run_bus_loop(bus_pipeline));

    Ok(App {
        transport: Some(transport),
        metrics: Some(metrics),
        frame_store,
        current_frame: None,
        playing: true,
        source_ids,
        offsets_ms,
        source_metrics: Vec::new(),
        error: None,
    })
}
