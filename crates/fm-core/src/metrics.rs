use fm_adapter_sdk::metrics::SourceMetrics;

/// Harvests always-on per-source counters from the GStreamer pipeline bus
/// (QoS messages, pad probes) and exposes them as `SourceMetrics` (ADR-0008).
pub struct MetricsCollector;

impl MetricsCollector {
    /// Attach to `pipeline`'s bus and begin listening for QoS / state messages.
    pub fn attach(_pipeline: &gstreamer::Pipeline) -> Self {
        todo!("Phase 1: subscribe to GST bus, install pad probes for fps counting")
    }

    /// Snapshot always-on counters for one source. Intended to be polled ~1 Hz.
    pub fn snapshot(&self, _source_id: &str) -> SourceMetrics {
        todo!("return fps_in/fps_out/dropped_frames/offset_vs_master from accumulated state")
    }
}
