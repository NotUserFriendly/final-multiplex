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
    /// Composited output width in pixels.
    pub width: u32,
    /// Composited output height in pixels.
    pub height: u32,
    /// Composited output frame rate.
    pub fps: u32,
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
