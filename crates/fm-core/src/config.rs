use serde::Deserialize;

/// How a source's media is delivered to the core.
#[derive(Debug, Deserialize, Default, PartialEq, Eq, Clone)]
#[serde(rename_all = "snake_case")]
pub enum SourceType {
    /// In-process uridecodebin; Phase-1 path. Default.
    #[default]
    File,
    /// Out-of-process adapter; Phase-2+ path. The core spawns `adapter` as a
    /// subprocess, serves it the net clock, and reads frames from shmsrc.
    External,
}

/// Top-level scene declaration, loaded from a TOML file (ADR-0007).
///
/// Minimal example:
/// ```toml
/// [grid]
/// columns = 2
/// width   = 1920
/// height  = 1080
/// fps     = 30
///
/// [[source]]
/// id        = "clip-a"
/// uri       = "file:///path/to/clip.mp4"
/// offset_ms = 0
/// volume    = 1.0
/// ```
#[derive(Debug, Deserialize)]
pub struct SceneConfig {
    pub grid: GridConfig,
    #[serde(default)]
    pub source: Vec<SourceConfig>,
}

#[derive(Debug, Deserialize)]
pub struct GridConfig {
    /// Number of columns in the equal-split tile grid.
    pub columns: u32,
    /// Per-tile width in pixels.  The composited canvas is `columns × width`
    /// wide, so a 2-column grid of 1920-wide tiles produces a 3840-pixel canvas.
    pub width: u32,
    /// Per-tile height in pixels.  The composited canvas is `rows × height`
    /// tall (rows derived from source count and columns).
    pub height: u32,
    /// Composited output frame rate.
    pub fps: u32,
    /// How long to wait for each external adapter to send `Ready` before
    /// proceeding to PLAYING anyway.  RTSP cold-start can exceed 10 s, so
    /// the default is generous.  Units: seconds.
    #[serde(default = "default_adapter_ready_timeout")]
    pub adapter_ready_timeout_secs: u64,
    /// Maximum live-source offset in milliseconds (ADR-0016).
    ///
    /// An offset buffer of this depth is inserted after `videoscale` for each
    /// external source.  A larger value allows aligning more out-of-sync
    /// cameras but costs proportionally more memory (~tile_frame_size × ceiling
    /// × fps per source at tile resolution).  Must be ≥ the largest per-source
    /// `offset_ms` in the scene, or that offset will be clamped.
    ///
    /// Default: 2000 ms (enough to align camera latency differences).
    #[serde(default = "default_live_offset_ceiling_ms")]
    pub live_offset_ceiling_ms: u32,
    /// Delivery watchdog timeout in milliseconds (ADR-0020).
    ///
    /// When an adapter reports `fps_in > 0` but the core has no active chain
    /// for that source, and this divergence persists beyond this timeout, the
    /// supervisor force-respawns the adapter.  Lower values give faster
    /// recovery but risk false respawns if normal reconnect takes longer than
    /// expected.  Must exceed the normal recovery + RTSP connect window.
    ///
    /// Default: 30 000 ms (30 s).
    #[serde(default = "default_delivery_watchdog_ms")]
    pub delivery_watchdog_ms: u64,
    /// Override directory for adapter binaries (ADR-0022, tier 1).
    ///
    /// When set, the resolver checks this directory before the XDG user dir
    /// and the bundled `adapters/` dir.  Useful for pointing at a custom
    /// adapter build (e.g. `adapter_dir = "target/debug"` in a dev scene).
    /// `FM_ADAPTER_DIR` env var has the same effect without touching the scene
    /// file.  If absent the normal search path applies.
    #[serde(default)]
    pub adapter_dir: Option<String>,
}

fn default_adapter_ready_timeout() -> u64 {
    30
}

fn default_live_offset_ceiling_ms() -> u32 {
    2000
}

fn default_delivery_watchdog_ms() -> u64 {
    30_000
}

#[derive(Debug, Deserialize)]
pub struct SourceConfig {
    /// Stable identifier used to address this source in transport/offset calls.
    pub id: String,
    /// How this source's media reaches the core.
    #[serde(default)]
    pub source_type: SourceType,
    /// GStreamer-compatible URI (file://, rtsp://, etc.). Required for `file` sources.
    pub uri: Option<String>,
    /// Path or name of the adapter binary. Required for `external` sources.
    /// If just a name (e.g. `"fm-dummy-adapter"`), it must be on `$PATH` or
    /// alongside the main binary.
    pub adapter: Option<String>,
    /// Initial per-source pad offset in milliseconds (ADR-0004).
    #[serde(default)]
    pub offset_ms: i64,
    /// Linear volume applied to this source's audiomixer sink pad.
    /// 0.0 = silent, 1.0 = unity gain, >1.0 amplifies. Defaults to 1.0.
    /// Static: read once at pipeline build; not adjustable at runtime.
    #[serde(default = "default_volume")]
    pub volume: f64,
    /// Source starts muted. Persisted back to the TOML on every toggle so
    /// mute state survives app restarts.
    #[serde(default)]
    pub muted: bool,
    /// Extra command-line arguments appended verbatim to the adapter's argv.
    /// Useful for test flags (e.g. `["--bump-fps-after", "20", "--bump-fps-to", "60"]`).
    /// Ignored for file sources.
    #[serde(default)]
    pub extra_args: Vec<String>,
}

fn default_volume() -> f64 {
    1.0
}

pub fn load(
    path: &std::path::Path,
) -> Result<SceneConfig, Box<dyn std::error::Error + Send + Sync>> {
    let text = std::fs::read_to_string(path)?;
    Ok(toml::from_str(&text)?)
}
