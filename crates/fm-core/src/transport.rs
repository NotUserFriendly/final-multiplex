use crate::metrics::{AudioLevel, AudioStore, MetricsCollector};
use crate::pipeline::Pipeline;
use fm_adapter_sdk::metrics::DB_FLOOR;
use gstreamer::prelude::*;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How long after startup or a manual reset before any source may contribute to
/// the ratchet.  Holds at grid_fps during the window; sources re-discover when it
/// clears.  Also long enough to let burst readings (jitter-buffer drain, decode
/// flush) subside before they can lock in a false high.
const SETTLE_WINDOW: Duration = Duration::from_secs(3);

/// Minimum fps above the current high-water mark required to accept a ratchet
/// candidate.  The 1-second measurement window can round up a nominally-30 fps
/// source to 31–34 fps under normal jitter; a genuine upgrade (48/50/60 fps) is
/// always ≥18 fps above a 30 fps baseline.  5 fps blocks noise without masking
/// real rate changes.
const RATCHET_MIN_DELTA: i32 = 5;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Session-scoped output framerate high-water mark (ADR-0023).
///
/// Ratchets up to the maximum input rate observed across active real sources;
/// never falls back within a session.  Resets to the configured grid fps on
/// scene reload.
///
/// Hysteresis: a candidate must appear in two consecutive polls before the
/// ratchet commits, guarding against single-sample bursts from decode/jitter.
struct FramerateRatchet {
    /// Current session high-water mark.  Initialized to grid_fps at startup.
    high_water_fps: i32,
    /// Candidate new high from the previous poll; committed on the second
    /// consecutive appearance of the same value.
    pending_candidate: Option<i32>,
}

impl FramerateRatchet {
    fn new(initial_fps: i32) -> Self {
        Self {
            high_water_fps: initial_fps,
            pending_candidate: None,
        }
    }

    /// Check `candidate_fps` against the high-water mark.
    ///
    /// Commits when the same candidate appears in two consecutive polls AND is
    /// at least `RATCHET_MIN_DELTA` above the current mark (guards against
    /// measurement jitter on sources near the current rate).
    /// Returns `Some(new_fps)` if the mark should be ratcheted up.
    fn check(&mut self, candidate_fps: i32) -> Option<i32> {
        if candidate_fps <= self.high_water_fps
            || candidate_fps < self.high_water_fps + RATCHET_MIN_DELTA
        {
            self.pending_candidate = None;
            return None;
        }
        if self.pending_candidate == Some(candidate_fps) {
            self.pending_candidate = None;
            self.high_water_fps = candidate_fps;
            Some(candidate_fps)
        } else {
            self.pending_candidate = Some(candidate_fps);
            None
        }
    }
}

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
    /// Output framerate ratchet (ADR-0023).
    ratchet: FramerateRatchet,
    /// Per-source settle timers for the measured-fallback ratchet path.
    ///
    /// Entry is inserted when a source's fps_in first becomes non-zero; removed
    /// when fps_in drops back to zero (source disconnected / not yet delivering).
    /// A source whose entry age is below SETTLE_WINDOW is excluded from the
    /// measured-fallback ratchet contribution for that poll cycle.
    settle_timers: HashMap<String, Instant>,
    /// Global ratchet suppression until this instant.  While active, no source
    /// (caps-declared or measured) may contribute to the ratchet — the output
    /// holds at grid_fps.  Set at construction (startup) and by reset_ratchet()
    /// (manual reset) so the button produces a visible rate drop before
    /// re-discovery.
    suppress_until: Instant,
}

impl Transport {
    pub fn new(pipeline: Pipeline) -> Self {
        let initial_fps = pipeline.grid_fps();
        Self {
            pipeline,
            source_bounds: HashMap::new(),
            ratchet: FramerateRatchet::new(initial_fps),
            settle_timers: HashMap::new(),
            suppress_until: Instant::now() + SETTLE_WINDOW,
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
    ///
    /// source_layouts is also updated so the offset survives a chain rebuild
    /// (reconnect); without this write-back, add_video/audio_chain would
    /// re-apply the stale TOML value and silently reset the offset to 0.
    pub fn set_source_offset(&mut self, source_id: &str, offset_ms: i64) -> Result<()> {
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
        self.pipeline
            .update_source_layout_offset(source_id, offset_ns);
        Ok(())
    }

    /// Mute or unmute a source's audiomixer sink pad.
    /// Independent of the configured volume; muting/unmuting does not alter it.
    ///
    /// source_layouts.muted is also updated so the state survives a chain
    /// rebuild (reconnect); without this write-back, add_audio_chain would
    /// always start the new pad unmuted.
    pub fn set_source_mute(&mut self, source_id: &str, muted: bool) -> Result<()> {
        if let Some(pad) = self.pipeline.mixer_sink_pads().get(source_id) {
            pad.set_property("mute", muted);
        }
        self.pipeline.update_source_layout_mute(source_id, muted);
        Ok(())
    }

    pub fn pipeline(&self) -> &Pipeline {
        &self.pipeline
    }

    /// Poll each source's input fps and ratchet the output rate up to the
    /// observed maximum (ADR-0023).  Call ~once per second from the Tick loop.
    ///
    /// `source_ids`: all real source IDs (excludes synthetic floors, which are
    /// not in the pipeline's source_pads and not tracked by MetricsCollector).
    pub fn check_and_ratchet(&mut self, source_ids: &[String], metrics: &MetricsCollector) {
        // Global suppression window: hold at grid_fps until it clears.
        if Instant::now() < self.suppress_until {
            return;
        }

        // Use measured fps_in exclusively.  RTSP SDP-declared rates are
        // unreliable — cameras routinely declare their maximum capability (e.g.
        // 50 fps) while delivering a fraction of that (e.g. 12 fps).  Measured
        // rates require the SETTLE_WINDOW + 2-poll hysteresis to commit, which is
        // sufficient protection against startup bursts.
        let mut max_fps: i32 = 0;

        for id in source_ids {
            let measured = metrics.snapshot(id).fps_in;
            if measured > 0.0 {
                let started = self
                    .settle_timers
                    .entry(id.clone())
                    .or_insert_with(Instant::now);
                if started.elapsed() < SETTLE_WINDOW {
                    continue;
                }
                let candidate = measured.round() as i32;
                if candidate > max_fps {
                    max_fps = candidate;
                }
            } else {
                self.settle_timers.remove(id);
            }
        }

        if max_fps > 0 {
            if let Some(new_fps) = self.ratchet.check(max_fps) {
                self.pipeline.set_output_fps(new_fps);
            }
        }
    }

    /// Reset the ratchet to the configured grid fps (call on scene reload or manual reset).
    ///
    /// Clears settle timers so re-discovery runs through the settle window —
    /// a reset immediately after a burst won't re-lock the false high.
    pub fn reset_ratchet(&mut self) {
        self.ratchet = FramerateRatchet::new(self.pipeline.grid_fps());
        self.settle_timers.clear();
        self.suppress_until = Instant::now() + SETTLE_WINDOW;
    }

    /// Reset shmsrc elements for a restarted external source so they
    /// reconnect to the adapter's new shmsink sockets.
    pub fn restart_external_source(&self, source_id: &str) {
        self.pipeline.restart_shmsrc(source_id);
    }

    /// Apply a topology change from the adapter's StreamsChanged message (ADR-0013).
    /// Adds or removes shmsrc chains on the live pipeline to match `has_video`/`has_audio`.
    pub fn apply_streams_changed(
        &mut self,
        source_id: &str,
        has_video: bool,
        has_audio: bool,
        source_fps: f64,
    ) {
        self.pipeline
            .build_shmsrc_chain(source_id, has_video, has_audio, source_fps);
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
