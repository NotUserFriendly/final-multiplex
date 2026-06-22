use crate::config::SceneConfig;

/// The in-core GStreamer pipeline for Phase 1.
///
/// Topology: N × (uridecodebin → [videoconvert → compositor sink pad]
///                              + [audioconvert → audiomixer sink pad])
///           compositor → appsink (video to UI bridge, ADR-0006)
///           audiomixer  → autoaudiosink
///
/// The master clock lives here; per-source pad offsets are set via
/// `Transport::set_source_offset`, not here (ADR-0004).
pub struct Pipeline {
    inner: gstreamer::Pipeline,
    /// Exposes the composited video frames to the UI bridge (ADR-0006).
    appsink: gstreamer_app::AppSink,
}

impl Pipeline {
    /// Build the pipeline from `scene`. GStreamer must be initialised before calling.
    pub fn build(_scene: &SceneConfig) -> Result<Self, Box<dyn std::error::Error>> {
        todo!("Phase 1: construct compositor + audiomixer pipeline from SceneConfig")
    }

    pub fn inner(&self) -> &gstreamer::Pipeline {
        &self.inner
    }

    pub fn appsink(&self) -> &gstreamer_app::AppSink {
        &self.appsink
    }
}
