use crate::pipeline::Pipeline;
use fm_adapter_sdk::metrics::{IngestState, SourceMetrics, DB_FLOOR};
use gstreamer::prelude::*;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Per-source audio level snapshot written by the bus-loop `level` handler.
pub struct AudioLevel {
    pub rms_db: f64,
    pub peak_db: f64,
    /// When this entry was last written; used to detect stale data.
    pub updated_at: Instant,
}

/// Shared store for audio levels; cloned into the bus loop thread.
pub type AudioStore = Arc<Mutex<HashMap<String, AudioLevel>>>;

struct SourceCounter {
    frames_since_reset: u64,
    fps: f64,
    dropped: u64,
    last_reset: Instant,
    /// Updated on every incoming buffer; used to detect EOS / stalled source.
    last_frame_at: Instant,
}

impl SourceCounter {
    fn new() -> Self {
        // Initialise last_frame_at far in the past so a source that never
        // delivers any frames reads as stale immediately.
        let long_ago = Instant::now()
            .checked_sub(std::time::Duration::from_secs(3600))
            .unwrap_or_else(Instant::now);
        Self {
            frames_since_reset: 0,
            fps: 0.0,
            dropped: 0,
            last_reset: Instant::now(),
            last_frame_at: long_ago,
        }
    }

    fn on_buffer(&mut self) {
        self.last_frame_at = Instant::now();
        self.frames_since_reset += 1;
        let elapsed = self.last_reset.elapsed().as_secs_f64();
        if elapsed >= 1.0 {
            self.fps = self.frames_since_reset as f64 / elapsed;
            self.frames_since_reset = 0;
            self.last_reset = Instant::now();
        }
    }
}

struct OutputCounter {
    frames_since_reset: u64,
    fps: f64,
    last_reset: Instant,
}

impl OutputCounter {
    fn new() -> Self {
        Self {
            frames_since_reset: 0,
            fps: 0.0,
            last_reset: Instant::now(),
        }
    }

    fn on_buffer(&mut self) {
        self.frames_since_reset += 1;
        let elapsed = self.last_reset.elapsed().as_secs_f64();
        if elapsed >= 1.0 {
            self.fps = self.frames_since_reset as f64 / elapsed;
            self.frames_since_reset = 0;
            self.last_reset = Instant::now();
        }
    }
}

/// Collects always-on per-source counters via GStreamer pad probes (ADR-0008).
///
/// - `fps_in`: BUFFER probes on each capsfilter source pad (post-scale, entering
///   the compositor) — not true ingest rate; see `SourceMetrics::fps_in`.
/// - `fps_out`: BUFFER probe on the appsink's sink pad (compositor output rate).
/// - `dropped_frames`: incremented when a QoS event signals a drop on a
///   capsfilter source pad.
/// - `audio_levels`: populated by the bus-loop thread from GStreamer `level` messages.
pub struct MetricsCollector {
    per_source: Arc<Mutex<HashMap<String, SourceCounter>>>,
    output: Arc<Mutex<OutputCounter>>,
    audio_levels: AudioStore,
    /// Stale threshold for fps_in: 1500 ms + compositor latency.  Ensures
    /// FILE TERMINATED fires only after buffered frames have cleared the
    /// compositor, not while the last frames are still in the latency window.
    fps_stale_ms: u64,
}

impl MetricsCollector {
    /// Install pad probes and return a collector ready to be polled.
    /// Must be called before `Transport::new` consumes the `Pipeline`.
    pub fn attach(pipeline: &Pipeline) -> Self {
        let per_source: Arc<Mutex<HashMap<String, SourceCounter>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let output: Arc<Mutex<OutputCounter>> = Arc::new(Mutex::new(OutputCounter::new()));

        // ── fps_in: capsfilter src pad BUFFER probes (post-scale, pre-compositor) ──
        for (id, pads) in pipeline.source_pads() {
            let counters = per_source.clone();
            let id_clone = id.clone();

            if let Some(ref video_src) = pads.video_src {
                video_src.add_probe(gstreamer::PadProbeType::BUFFER, move |_pad, _info| {
                    counters
                        .lock()
                        .unwrap()
                        .entry(id_clone.clone())
                        .or_insert_with(SourceCounter::new)
                        .on_buffer();
                    gstreamer::PadProbeReturn::Ok
                });

                // ── dropped_frames: QoS events travelling upstream ─────────────
                let counters_qos = per_source.clone();
                let id_qos = id.clone();

                video_src.add_probe(
                    gstreamer::PadProbeType::EVENT_UPSTREAM,
                    move |_pad, info| {
                        if let Some(gstreamer::PadProbeData::Event(ev)) = &info.data {
                            if ev.type_() == gstreamer::EventType::Qos {
                                counters_qos
                                    .lock()
                                    .unwrap()
                                    .entry(id_qos.clone())
                                    .or_insert_with(SourceCounter::new)
                                    .dropped += 1;
                            }
                        }
                        gstreamer::PadProbeReturn::Ok
                    },
                );
            } // end if let Some(video_src)
        }

        // ── fps_out: appsink sink pad BUFFER probe ─────────────────────────
        if let Some(sink_pad) = pipeline.appsink().static_pad("sink") {
            let output_clone = output.clone();
            sink_pad.add_probe(gstreamer::PadProbeType::BUFFER, move |_pad, _info| {
                output_clone.lock().unwrap().on_buffer();
                gstreamer::PadProbeReturn::Ok
            });
        }

        let audio_levels: AudioStore = Arc::new(Mutex::new(HashMap::new()));
        let fps_stale_ms = pipeline.compositor_latency_ms() as u64 + 300;

        Self {
            per_source,
            output,
            audio_levels,
            fps_stale_ms,
        }
    }

    /// Clone of the audio level store for the bus-loop thread.
    pub fn audio_store(&self) -> AudioStore {
        self.audio_levels.clone()
    }

    /// Snapshot always-on counters for one source (~1 Hz poll cadence).
    pub fn snapshot(&self, source_id: &str) -> SourceMetrics {
        let per = self.per_source.lock().unwrap();
        let out = self.output.lock().unwrap();
        let audio = self.audio_levels.lock().unwrap();

        // Report fps_in as 0 once no buffer has arrived for fps_stale_ms.
        // fps_stale_ms = compositor_latency_ms + 1500 so the stale window
        // covers the compositor's latency buffer; without this, FILE TERMINATED
        // fires while the last frames are still being displayed from the buffer.
        let stale_fps = std::time::Duration::from_millis(self.fps_stale_ms);
        let (fps_in, dropped) = per
            .get(source_id)
            .map(|c| {
                let fps = if c.last_frame_at.elapsed() > stale_fps {
                    0.0
                } else {
                    c.fps
                };
                (fps, c.dropped)
            })
            .unwrap_or((0.0, 0));

        // Floor the meter if no level message has arrived in the last 300 ms
        // (3× the 100 ms default level interval).  This handles individual
        // source EOS, pause, or error without depending on pipeline-level EOS.
        let stale = std::time::Duration::from_millis(300);
        let (audio_rms_db, audio_peak_db) = audio
            .get(source_id)
            .filter(|l| l.updated_at.elapsed() < stale)
            .map(|l| (l.rms_db, l.peak_db))
            .unwrap_or((DB_FLOOR, DB_FLOOR));

        SourceMetrics {
            source_id: source_id.to_string(),
            fps_in,
            fps_out: out.fps,
            dropped_frames: dropped,
            bad_frames: 0,
            // Phase 2 will read actual drift from the net clock (ADR-0005).
            offset_vs_master_ms: 0,
            state: IngestState::Running,
            reconnect_count: 0,
            audio_rms_db,
            audio_peak_db,
        }
    }
}
