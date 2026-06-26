use serde::{Deserialize, Serialize};

/// Silence floor for audio level fields (dBFS). 0.0 dB is full-scale clipping,
/// so never default these fields to 0.0.  When no data is available, floor to
/// this value so the meter reads "silent" rather than "clipping".
pub const DB_FLOOR: f64 = -60.0;

/// Always-on per-source counters (~1 Hz cadence) — ADR-0008.
///
/// Defined here so the core, UI, and out-of-process adapters all share the
/// same schema without taking a transitive GStreamer dependency.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceMetrics {
    pub source_id: String,
    /// Frames entering the compositor from this source's processing chain
    /// (post-decode, post-scale) per second. In Phase 1 this is a BUFFER probe
    /// on the capsfilter source pad feeding the compositor. Out-of-process
    /// adapters (Phase 2+) will report true ingest rate here instead.
    pub fps_in: f64,
    /// Frames emitted by the compositor into the appsink per second.
    pub fps_out: f64,
    /// Dropped frames over the last 60 s (live sources) or cumulative (finite sources).
    /// Live: videorate drops when source fps exceeds the configured output fps.
    pub dropped_frames: u64,
    /// Frames arriving from the decoder with the CORRUPTED flag set (RTP packet loss).
    /// Windowed over the last 60 s for live sources; cumulative for finite sources.
    pub bad_frames: u64,
    /// This source's pad offset relative to the master clock (ms).
    pub offset_vs_master_ms: i64,
    pub state: IngestState,
    /// Cumulative supervisor restarts (out-of-process adapters, Phase 2+).
    pub reconnect_count: u32,
    /// Current RMS level in dBFS (loudest channel). DB_FLOOR when silent or no audio.
    pub audio_rms_db: f64,
    /// Current peak level in dBFS (loudest channel). DB_FLOOR when silent or no audio.
    pub audio_peak_db: f64,
    /// True once `last_frame_at` has been silent for compositor_latency_ms + 300 ms.
    /// "The source stopped *and* the downstream buffer has drained."
    /// Used by the UI to gate FILE TERMINATED so the overlay fires only after the
    /// last buffered frame is actually displayed.  Always false for external sources
    /// (adapters report their own stream state via `IngestState`).
    /// Default false so older adapter builds that predate this field still parse.
    #[serde(default)]
    pub stream_drained: bool,
}

impl Default for SourceMetrics {
    fn default() -> Self {
        Self {
            source_id: String::new(),
            fps_in: 0.0,
            fps_out: 0.0,
            dropped_frames: 0,
            bad_frames: 0,
            offset_vs_master_ms: 0,
            state: IngestState::default(),
            reconnect_count: 0,
            audio_rms_db: DB_FLOOR,
            audio_peak_db: DB_FLOOR,
            stream_drained: false,
        }
    }
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
