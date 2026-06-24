//! Adapter process contract for Final Multiplex (ADR-0012).
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
//!
//! **stdout-JSON fragility note:** any stray output on the adapter's stdout
//! (e.g. from a GStreamer debug print) corrupts the line protocol.  Adapters
//! must ensure all debug output goes to stderr.  A dedicated control fd is the
//! correct long-term fix; deferred until it actually bites (see BUGS.md).

use crate::metrics::SourceMetrics;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Protocol version
// ---------------------------------------------------------------------------

/// Bump this when the wire format changes in a backward-incompatible way.
/// The core rejects adapters that send a different version in [`AdapterMessage::Ready`].
pub const PROTOCOL_VERSION: u32 = 1;

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
    /// Production resolution width in pixels.  The adapter produces at this
    /// resolution; the core scales to tile size (ADR-0012).
    pub const VIDEO_WIDTH: &str = "--video-width";
    /// Production resolution height in pixels.
    pub const VIDEO_HEIGHT: &str = "--video-height";
    /// Output framerate in frames per second (integer).
    pub const FRAMERATE: &str = "--framerate";
    /// Core pipeline base time in nanoseconds; lets the adapter align its
    /// GStreamer base time without a clock query round-trip.
    pub const BASE_TIME: &str = "--base-time";
}

// ---------------------------------------------------------------------------
// Video / audio caps that cross the boundary (ADR-0011 / ADR-0012)
// ---------------------------------------------------------------------------

/// GStreamer caps template for the video shmsink the adapter must produce.
/// Substitute `{width}`, `{height}`, `{fps}` before passing to GStreamer.
/// `pixel-aspect-ratio=1/1` is required; the core's post-shmsrc capsfilter
/// pins this field so negotiation is deterministic.
pub const VIDEO_CAPS_TEMPLATE: &str =
    "video/x-raw,format=RGBA,width={width},height={height},framerate={fps}/1,pixel-aspect-ratio=1/1";

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
    ///
    /// `protocol_version` must equal [`PROTOCOL_VERSION`]; the core logs an
    /// error and will not send Play if the version mismatches.
    ///
    /// `has_video` / `has_audio` tell the core which shm sockets are active.
    /// The core wires only the pads for present streams (same logic as the
    /// Phase-1 discoverer probe).  A video-only adapter sets `has_audio: false`
    /// and must not create the audio shmsink socket.
    Ready {
        has_video: bool,
        has_audio: bool,
        protocol_version: u32,
    },
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
