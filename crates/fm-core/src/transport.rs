use crate::metrics::{AudioLevel, AudioStore};
use crate::pipeline::Pipeline;
use fm_adapter_sdk::metrics::DB_FLOOR;
use gstreamer::prelude::*;
use std::collections::HashMap;
use std::time::Instant;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Per-source effective offset bounds (in milliseconds, inclusive) derived from
/// the adapter's declared capability and the core's ceiling (ADR-0016/0017).
pub struct SourceBounds {
    pub min_ms: i64,
    pub max_ms: i64,
}

/// Master transport: play / pause / seek-all / per-source pad offset.
///
/// All timing decisions live here, never in source adapters (ADR-0004).
pub struct Transport {
    pipeline: Pipeline,
    /// Effective per-source offset bounds, populated at startup from supervisor
    /// Ready data.  Sources not in this map use file-source defaults (±60 s).
    source_bounds: HashMap<String, SourceBounds>,
}

impl Transport {
    pub fn new(pipeline: Pipeline) -> Self {
        Self {
            pipeline,
            source_bounds: HashMap::new(),
        }
    }

    /// Register effective offset bounds for a source (call after Ready is received).
    pub fn set_source_bounds(&mut self, source_id: &str, min_ms: i64, max_ms: i64) {
        self.source_bounds
            .insert(source_id.to_string(), SourceBounds { min_ms, max_ms });
    }

    /// Effective bounds for a source, or file-source defaults if not registered.
    pub fn source_bounds(&self, source_id: &str) -> (i64, i64) {
        if let Some(b) = self.source_bounds.get(source_id) {
            (b.min_ms, b.max_ms)
        } else {
            (-60_000, 60_000)
        }
    }

    pub fn play(&self) -> Result<()> {
        self.pipeline.inner().set_state(gstreamer::State::Playing)?;
        Ok(())
    }

    /// Block until the pipeline reaches PLAYING state (or the timeout expires).
    /// Live pipelines return `Async` from `set_state(Playing)`; call this after
    /// `play()` at startup to ensure all aggregators and sinks are PLAYING before
    /// adapters push their first frame (Group 2 cascade fix).
    /// Returns `true` if PLAYING was confirmed, `false` on timeout or failure.
    pub fn wait_for_playing(&self, timeout_secs: u64) -> bool {
        let (result, _, _) = self
            .pipeline
            .inner()
            .state(Some(gstreamer::ClockTime::from_seconds(timeout_secs)));
        matches!(
            result,
            Ok(gstreamer::StateChangeSuccess::Success | gstreamer::StateChangeSuccess::NoPreroll)
        )
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
    /// The offset is a compositor-timeline shift: a positive value delays when
    /// the source's content appears relative to t=0; negative makes it lead.
    /// No seek is issued, so the offset is source-agnostic and unaffected by
    /// file duration or seekability — it survives the Phase-2 RTSP boundary.
    pub fn set_source_offset(&self, source_id: &str, offset_ms: i64) -> Result<()> {
        let pads = self
            .pipeline
            .source_pads()
            .get(source_id)
            .ok_or_else(|| format!("unknown source id: {source_id}"))?;
        let (min_ms, max_ms) = self.source_bounds(source_id);
        let clamped_ms = offset_ms.clamp(min_ms, max_ms);
        let offset_ns = clamped_ms * 1_000_000;
        if let Some(ref p) = pads.video_src {
            p.set_offset(offset_ns);
        }
        if let Some(ref p) = pads.audio_src {
            p.set_offset(offset_ns);
        }
        Ok(())
    }

    /// Mute or unmute a source's audiomixer sink pad.
    /// Independent of the configured volume; muting/unmuting does not alter it.
    pub fn set_source_mute(&self, source_id: &str, muted: bool) -> Result<()> {
        if let Some(pad) = self.pipeline.mixer_sink_pads().get(source_id) {
            pad.set_property("mute", muted);
        }
        Ok(())
    }

    pub fn pipeline(&self) -> &Pipeline {
        &self.pipeline
    }

    /// Reset shmsrc elements for a restarted external source so they
    /// reconnect to the adapter's new shmsink sockets.
    pub fn restart_external_source(&self, source_id: &str) {
        self.pipeline.restart_shmsrc(source_id);
    }

    /// Apply a topology change from the adapter's StreamsChanged message (ADR-0013).
    /// Adds or removes shmsrc chains on the live pipeline to match `has_video`/`has_audio`.
    pub fn apply_streams_changed(&mut self, source_id: &str, has_video: bool, has_audio: bool) {
        self.pipeline
            .build_shmsrc_chain(source_id, has_video, has_audio);
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
                // Snapshot state before seeking: a FLUSH seek internally
                // cycles through PLAYING to pre-roll the new position, so a
                // pipeline that was PAUSED ends up PLAYING without this guard.
                let was_paused = pipeline.current_state() == gstreamer::State::Paused;

                if let Err(e) = pipeline.seek_simple(
                    gstreamer::SeekFlags::FLUSH | gstreamer::SeekFlags::KEY_UNIT,
                    gstreamer::ClockTime::ZERO,
                ) {
                    eprintln!("[fm-core] loop seek failed: {e}");
                }

                if was_paused {
                    let _ = pipeline.set_state(gstreamer::State::Paused);
                }

                // Clear stale levels so meters return to floor while sources
                // restart; new level messages refill the store after seek.
                audio_levels.lock().unwrap().clear();
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
                                audio_levels.lock().unwrap().insert(
                                    id.to_string(),
                                    AudioLevel {
                                        rms_db,
                                        peak_db,
                                        updated_at: Instant::now(),
                                    },
                                );
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// Extract the max value across all channels from a level message structure field.
/// The level plugin posts GValueArray (G_TYPE_VALUE_ARRAY), not GST_TYPE_ARRAY.
/// Floors at DB_FLOOR to handle -inf (complete silence).
fn parse_level_array(s: &gstreamer::StructureRef, field: &str) -> f64 {
    // ValueArray derefs to [Value]; iterate directly.
    #[allow(deprecated)]
    let raw = s
        .get::<gstreamer::glib::ValueArray>(field)
        .ok()
        .and_then(|arr| {
            arr.iter()
                .filter_map(|v| v.get::<f64>().ok())
                .reduce(f64::max)
        })
        .unwrap_or(DB_FLOOR);
    raw.max(DB_FLOOR)
}
