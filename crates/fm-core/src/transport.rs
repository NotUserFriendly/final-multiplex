use crate::metrics::{AudioLevel, AudioStore};
use crate::pipeline::Pipeline;
use fm_adapter_sdk::metrics::DB_FLOOR;
use gstreamer::prelude::*;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Master transport: play / pause / seek-all / per-source pad offset.
///
/// All timing decisions live here, never in source adapters (ADR-0004).
pub struct Transport {
    pipeline: Pipeline,
}

impl Transport {
    pub fn new(pipeline: Pipeline) -> Self {
        Self { pipeline }
    }

    pub fn play(&self) -> Result<()> {
        self.pipeline.inner().set_state(gstreamer::State::Playing)?;
        Ok(())
    }

    pub fn pause(&self) -> Result<()> {
        self.pipeline.inner().set_state(gstreamer::State::Paused)?;
        Ok(())
    }

    /// Seek every source to `position_ms` on the master clock simultaneously.
    pub fn seek_all(&self, position_ms: i64) -> Result<()> {
        let pos = gstreamer::ClockTime::from_mseconds(position_ms.max(0) as u64);
        self.pipeline.inner().seek_simple(
            gstreamer::SeekFlags::FLUSH | gstreamer::SeekFlags::KEY_UNIT,
            pos,
        )?;
        Ok(())
    }

    /// Shift one source's audio and video together by adjusting the
    /// `gst_pad_set_offset` on its capsfilter source pads (ADR-0004).
    /// No seek required; the change takes effect on the next buffer.
    pub fn set_source_offset(&self, source_id: &str, offset_ms: i64) -> Result<()> {
        let pads = self
            .pipeline
            .source_pads()
            .get(source_id)
            .ok_or_else(|| format!("unknown source id: {source_id}"))?;
        let offset_ns = offset_ms * 1_000_000;
        if let Some(ref p) = pads.video_src {
            p.set_offset(offset_ns);
        }
        if let Some(ref p) = pads.audio_src {
            p.set_offset(offset_ns);
        }
        Ok(())
    }

    pub fn pipeline(&self) -> &Pipeline {
        &self.pipeline
    }
}

/// Run the GStreamer bus message loop in a dedicated thread.
///
/// EOS causes a seek back to t=0 so local-file sources loop continuously.
/// Parses `level` element messages and writes audio RMS/peak into `audio_levels`
/// so `MetricsCollector::snapshot` can read them.
/// The loop exits on a terminal error; otherwise it blocks until the pipeline
/// transitions to NULL state.
pub fn run_bus_loop(pipeline: gstreamer::Pipeline, audio_levels: AudioStore) {
    let bus = match pipeline.bus() {
        Some(b) => b,
        None => return,
    };

    for msg in bus.iter_timed(gstreamer::ClockTime::NONE) {
        use gstreamer::MessageView;
        match msg.view() {
            MessageView::Eos(..) => {
                if let Err(e) = pipeline.seek_simple(
                    gstreamer::SeekFlags::FLUSH | gstreamer::SeekFlags::KEY_UNIT,
                    gstreamer::ClockTime::ZERO,
                ) {
                    eprintln!("[fm-core] loop seek failed: {e}");
                }
            }
            MessageView::Error(err) => {
                eprintln!(
                    "[fm-core] error from {:?}: {}",
                    err.src().map(|s| s.name()),
                    err.error()
                );
                if let Some(dbg) = err.debug() {
                    eprintln!("[fm-core] debug: {dbg}");
                }
            }
            MessageView::Warning(warn) => {
                eprintln!("[fm-core] warning: {}", warn.error());
            }
            MessageView::Element(elem) => {
                if let Some(s) = elem.structure() {
                    if s.name() == "level" {
                        if let Some(src) = msg.src() {
                            let name = src.name();
                            if let Some(id) = name.strip_prefix("alevel_") {
                                let rms_db = parse_level_array(s, "rms");
                                let peak_db = parse_level_array(s, "peak");
                                audio_levels
                                    .lock()
                                    .unwrap()
                                    .insert(id.to_string(), AudioLevel { rms_db, peak_db });
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// Extract the max value across all channels from a GStreamer Array field in
/// a level message structure. Floors at DB_FLOOR to handle -inf (silence).
fn parse_level_array(s: &gstreamer::StructureRef, field: &str) -> f64 {
    let raw = s
        .get::<gstreamer::Array>(field)
        .ok()
        .and_then(|arr| {
            arr.as_slice()
                .iter()
                .filter_map(|v| v.get::<f64>().ok())
                .reduce(f64::max)
        })
        .unwrap_or(DB_FLOOR);
    // -inf means silence; clamp to floor for display.
    raw.max(DB_FLOOR)
}
