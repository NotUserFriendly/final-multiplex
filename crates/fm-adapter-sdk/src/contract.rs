//! Adapter process contract for Final Multiplex (ADR-0005 / ADR-0011).
//!
//! An adapter is a subprocess that:
//!   1. Slaves to the core's GstNetTimeProvider via GstNetClientClock.
//!   2. Produces raw decoded frames to two shmsink sockets (video + audio).
//!   3. Exchanges line-delimited JSON with the core over stdin/stdout.
//!
//! Wire format: one JSON object per line, flushed immediately.
//! Core writes [`Command`] lines to the adapter's stdin.
//! Adapter writes [`AdapterMessage`] lines to its stdout.
//! Adapter's stderr is left for logs and is not parsed.

use crate::metrics::SourceMetrics;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Launch argument names
// ---------------------------------------------------------------------------

/// Argv constants shared by the core supervisor and every adapter binary so
/// the spelling is never out of sync.
pub mod args {
    /// GstNetClientClock endpoint: `"host:port"` (e.g. `"127.0.0.1:5637"`).
    pub const CLOCK_ADDR: &str = "--clock-addr";
    /// shmsink socket path for the video stream (e.g. `/tmp/fm-video-cam0`).
    pub const VIDEO_SHM: &str = "--video-shm";
    /// shmsink socket path for the audio stream (e.g. `/tmp/fm-audio-cam0`).
    pub const AUDIO_SHM: &str = "--audio-shm";
    /// Source identifier string; echoed back in [`SourceMetrics::source_id`].
    pub const SOURCE_ID: &str = "--source-id";
    /// Output video width in pixels (must match the core compositor tile width).
    pub const VIDEO_WIDTH: &str = "--video-width";
    /// Output video height in pixels (must match the core compositor tile height).
    pub const VIDEO_HEIGHT: &str = "--video-height";
    /// Output framerate in frames per second (integer).
    pub const FRAMERATE: &str = "--framerate";
}

// ---------------------------------------------------------------------------
// Video / audio caps that cross the boundary (ADR-0011)
// ---------------------------------------------------------------------------

/// GStreamer caps template for the video shmsink the adapter must produce.
/// Substitute `{width}`, `{height}`, `{fps}` before passing to GStreamer.
pub const VIDEO_CAPS_TEMPLATE: &str =
    "video/x-raw,format=RGBA,width={width},height={height},framerate={fps}/1";

/// GStreamer caps for the audio shmsink the adapter must produce.
pub const AUDIO_CAPS: &str = "audio/x-raw,format=S16LE,rate=48000,channels=2,layout=interleaved";

// ---------------------------------------------------------------------------
// Control-channel message types
// ---------------------------------------------------------------------------

/// Commands sent **core → adapter** on the adapter's stdin, one JSON line each.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    /// Begin (or resume) producing frames.
    Play,
    /// Pause frame production; shm sockets remain open.
    Pause,
    /// Flush and exit cleanly.
    Shutdown,
}

/// Messages sent **adapter → core** on the adapter's stdout, one JSON line each.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "msg", rename_all = "snake_case")]
pub enum AdapterMessage {
    /// Adapter has slaved the net clock and opened shm sockets; ready for [`Command::Play`].
    Ready,
    /// Per-source telemetry, ~1 Hz cadence (ADR-0008).
    Metrics(SourceMetrics),
    /// Adapter hit an unrecoverable error and will exit.
    Error { description: String },
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Serialise a [`Command`] to a newline-terminated JSON string for writing to
/// the adapter's stdin.
pub fn encode_command(cmd: &Command) -> String {
    let mut s = serde_json::to_string(cmd).expect("Command is always serialisable");
    s.push('\n');
    s
}

/// Deserialise one line of stdout into an [`AdapterMessage`].
pub fn decode_message(line: &str) -> Result<AdapterMessage, serde_json::Error> {
    serde_json::from_str(line.trim())
}
