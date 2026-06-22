use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use gstreamer::prelude::*;
use fm_adapter_sdk::metrics::{IngestState, SourceMetrics};
use crate::pipeline::Pipeline;

struct SourceCounter {
    frames_since_reset: u64,
    fps: f64,
    dropped: u64,
    last_reset: Instant,
}

impl SourceCounter {
    fn new() -> Self {
        Self {
            frames_since_reset: 0,
            fps: 0.0,
            dropped: 0,
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

struct OutputCounter {
    frames_since_reset: u64,
    fps: f64,
    last_reset: Instant,
}

impl OutputCounter {
    fn new() -> Self {
        Self { frames_since_reset: 0, fps: 0.0, last_reset: Instant::now() }
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
/// - `fps_in`: BUFFER probes on each compositor sink pad.
/// - `fps_out`: BUFFER probe on the appsink's sink pad (compositor output rate).
/// - `dropped_frames`: incremented when a QoS event signals a drop on a
///   compositor sink pad.
pub struct MetricsCollector {
    per_source: Arc<Mutex<HashMap<String, SourceCounter>>>,
    output: Arc<Mutex<OutputCounter>>,
}

impl MetricsCollector {
    /// Install pad probes and return a collector ready to be polled.
    /// Must be called before `Transport::new` consumes the `Pipeline`.
    pub fn attach(pipeline: &Pipeline) -> Self {
        let per_source: Arc<Mutex<HashMap<String, SourceCounter>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let output: Arc<Mutex<OutputCounter>> =
            Arc::new(Mutex::new(OutputCounter::new()));

        // ── fps_in: compositor sink pad BUFFER probes ──────────────────────
        for (id, pads) in pipeline.source_pads() {
            let counters = per_source.clone();
            let id_clone = id.clone();

            pads.compositor_sink.add_probe(
                gstreamer::PadProbeType::BUFFER,
                move |_pad, _info| {
                    counters
                        .lock()
                        .unwrap()
                        .entry(id_clone.clone())
                        .or_insert_with(SourceCounter::new)
                        .on_buffer();
                    gstreamer::PadProbeReturn::Ok
                },
            );

            // ── dropped_frames: QoS events on compositor sink pads ─────────
            let counters_qos = per_source.clone();
            let id_qos = id.clone();

            pads.compositor_sink.add_probe(
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
        }

        // ── fps_out: appsink sink pad BUFFER probe ─────────────────────────
        if let Some(sink_pad) = pipeline.appsink().static_pad("sink") {
            let output_clone = output.clone();
            sink_pad.add_probe(
                gstreamer::PadProbeType::BUFFER,
                move |_pad, _info| {
                    output_clone.lock().unwrap().on_buffer();
                    gstreamer::PadProbeReturn::Ok
                },
            );
        }

        Self { per_source, output }
    }

    /// Snapshot always-on counters for one source (~1 Hz poll cadence).
    pub fn snapshot(&self, source_id: &str) -> SourceMetrics {
        let per = self.per_source.lock().unwrap();
        let out = self.output.lock().unwrap();

        let (fps_in, dropped) = per
            .get(source_id)
            .map(|c| (c.fps, c.dropped))
            .unwrap_or((0.0, 0));

        SourceMetrics {
            source_id: source_id.to_string(),
            fps_in,
            fps_out: out.fps,
            dropped_frames: dropped,
            // Phase 2 will read actual drift from the net clock (ADR-0005).
            offset_vs_master_ms: 0,
            state: IngestState::Running,
            reconnect_count: 0,
        }
    }
}
