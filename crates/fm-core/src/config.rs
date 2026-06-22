use serde::Deserialize;

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
    /// GStreamer-compatible URI (file://, rtsp://, etc.).
    pub uri: String,
    /// Initial per-source pad offset in milliseconds (ADR-0004).
    #[serde(default)]
    pub offset_ms: i64,
}

pub fn load(path: &std::path::Path) -> Result<SceneConfig, Box<dyn std::error::Error + Send + Sync>> {
    let text = std::fs::read_to_string(path)?;
    Ok(toml::from_str(&text)?)
}
