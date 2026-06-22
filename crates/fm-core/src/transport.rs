use crate::pipeline::Pipeline;

/// Master transport: play/pause/seek-all and per-source pad offset (ADR-0004).
///
/// All timing decisions live here, never in adapters.
pub struct Transport {
    pipeline: Pipeline,
}

impl Transport {
    pub fn new(pipeline: Pipeline) -> Self {
        Self { pipeline }
    }

    pub fn play(&self) -> Result<(), Box<dyn std::error::Error>> {
        todo!("set pipeline to Playing, starting the master clock")
    }

    pub fn pause(&self) -> Result<(), Box<dyn std::error::Error>> {
        todo!("set pipeline to Paused")
    }

    /// Seek every source to `position_ms` on the master clock simultaneously.
    pub fn seek_all(&self, _position_ms: i64) -> Result<(), Box<dyn std::error::Error>> {
        todo!("flush seek on master clock, broadcast to all pads")
    }

    /// Shift one source's audio and video together by changing its sink pad offset
    /// (`gst_pad_set_offset`) — does not require a seek (ADR-0004).
    pub fn set_source_offset(
        &self,
        _source_id: &str,
        _offset_ms: i64,
    ) -> Result<(), Box<dyn std::error::Error>> {
        todo!("call gst_pad_set_offset on compositor + audiomixer sink pads for source_id")
    }

    pub fn pipeline(&self) -> &Pipeline {
        &self.pipeline
    }
}
