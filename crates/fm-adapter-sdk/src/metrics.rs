use serde::{Deserialize, Serialize};

/// Always-on per-source counters (~1 Hz cadence) — ADR-0008.
///
/// Defined here so the core, UI, and out-of-process adapters all share the
/// same schema without taking a transitive GStreamer dependency.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SourceMetrics {
    pub source_id: String,
    /// Frames received from the ingest path per second.
    pub fps_in: f64,
    /// Frames delivered to the compositor per second.
    pub fps_out: f64,
    /// Cumulative dropped frames since pipeline start.
    pub dropped_frames: u64,
    /// This source's pad offset relative to the master clock (ms).
    pub offset_vs_master_ms: i64,
    pub state: IngestState,
    /// Cumulative supervisor restarts (out-of-process adapters, Phase 2+).
    pub reconnect_count: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IngestState {
    #[default]
    Idle,
    Running,
    Buffering,
    Error,
}
