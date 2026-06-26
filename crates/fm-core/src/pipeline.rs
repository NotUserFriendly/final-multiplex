use crate::config::{SceneConfig, SourceType};
use crate::runtime;
use gstreamer::prelude::*;
use std::collections::HashMap;

type ExternalCaps = HashMap<String, (bool, bool)>;

/// Attach a permanent offset-accuracy canary to `pad` (a `voff_q:src` pad).
///
/// **What it measures:** `running_time − pts` for buffers exiting the offset
/// buffer queue.  In steady state this equals the active pad offset because the
/// compositor (GstAggregator) creates backpressure: it accepts one buffer per
/// sink pad at a time and pops only when `running_time ≈ pts + pad_offset`,
/// causing the queue to block until the compositor is ready.
///
/// **Windowing:** sampling starts after `ceiling_ms + 500 ms` of chain running
/// time have elapsed.  `ceiling_ms` is the worst-case fill-phase duration (the
/// voff_q holds at most `ceiling_ms` of frames); the 500 ms margin provides
/// headroom regardless of source framerate.  This is framerate-independent —
/// no hardcoded frame count, no assumed frame period.
///
/// **Canary behaviour:** silent when `|running−pts − expected_offset| ≤ 150 ms`.
/// Emits one `[offset-canary] WARN` line per diverging sample over a 20-buffer
/// window, then goes silent for the rest of the chain's lifetime.
///
/// Note: actual source framerate is a shared question with the `fps_in` metric
/// and is deferred to the pre-Phase-3 metrics pass; the time-window avoids
/// depending on it here.
fn add_offset_canary(
    pad: &gstreamer::Pad,
    source_id: &str,
    expected_offset_ns: i64,
    ceiling_ms: u64,
    pipeline_weak: gstreamer::glib::WeakRef<gstreamer::Pipeline>,
    source_fps: f64,
) {
    let sid = source_id.to_string();
    let expected_ms = expected_offset_ns / 1_000_000;
    // Sampling window opens after the fill phase: ceiling_ms (worst-case) + 500 ms margin.
    let window_start_ns = ceiling_ms as i64 * 1_000_000 + 500_000_000i64;
    // Tolerance = frame_period + chain_latency (~100 ms empirical), floor 150 ms.
    // The probe fires when the compositor accepts the *next* buffer after
    // popping the current one; the measured running-pts is therefore
    // approximately (offset − frame_period − pipeline_latency).  Empirically
    // this reads ~80 ms below the configured offset at 30 fps with ~100 ms
    // chain latency (e.g. 419 ms measured for 500 ms offset).
    // At off-rates (e.g. 15 fps) the frame period doubles; without the real fps
    // the bias can exceed the old hardcoded 150 ms and emit false WARNs.
    let fps = if source_fps > 0.0 { source_fps } else { 30.0 };
    let frame_period_ms = (1000.0 / fps) as i64;
    let tolerance_ms: i64 = (frame_period_ms + 100).max(150);
    const SAMPLE_COUNT: u32 = 20;

    // u64::MAX = "not yet recorded"; running time is always > 0 in practice.
    let chain_start_ns = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(u64::MAX));
    let samples_taken = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));

    pad.add_probe(gstreamer::PadProbeType::BUFFER, move |_, info| {
        if samples_taken.load(std::sync::atomic::Ordering::Relaxed) >= SAMPLE_COUNT {
            return gstreamer::PadProbeReturn::Ok;
        }
        let Some(gstreamer::PadProbeData::Buffer(buf)) = &info.data else {
            return gstreamer::PadProbeReturn::Ok;
        };
        let Some(p) = pipeline_weak.upgrade() else {
            return gstreamer::PadProbeReturn::Ok;
        };
        let Some(running) = p.current_running_time() else {
            return gstreamer::PadProbeReturn::Ok;
        };
        let Some(pts) = buf.pts() else {
            return gstreamer::PadProbeReturn::Ok;
        };
        let running_ns = running.nseconds() as i64;

        // Latch chain start on the first buffer.
        let start = chain_start_ns.load(std::sync::atomic::Ordering::Relaxed);
        if start == u64::MAX {
            chain_start_ns.store(running_ns as u64, std::sync::atomic::Ordering::Relaxed);
            return gstreamer::PadProbeReturn::Ok;
        }

        // Stay silent until past the fill phase.
        if running_ns - (start as i64) < window_start_ns {
            return gstreamer::PadProbeReturn::Ok;
        }

        let diff_ms = (running_ns - pts.nseconds() as i64) / 1_000_000;
        let deviation = (diff_ms - expected_ms).abs();
        if deviation > tolerance_ms {
            eprintln!(
                "[offset-canary] WARN '{}' expected {}ms got {}ms (deviation {}ms tolerance {}ms)",
                sid, expected_ms, diff_ms, deviation, tolerance_ms
            );
        }
        samples_taken.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        gstreamer::PadProbeReturn::Ok
    });
}

/// Discover which stream types a URI contains before building the pipeline.
/// Returns (has_video, has_audio). Runs with a 2-second timeout per source.
/// Called in parallel threads, one per source, in `Pipeline::build`.
fn probe_streams(uri: &str) -> (bool, bool) {
    let timeout = gstreamer::ClockTime::from_seconds(2);
    let discoverer = match gstreamer_pbutils::Discoverer::new(timeout) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("[fm-core] probe: discoverer init failed: {e}");
            return (true, true); // conservative: assume both present
        }
    };
    match discoverer.discover_uri(uri) {
        Ok(info) => {
            let v = !info.video_streams().is_empty();
            let a = !info.audio_streams().is_empty();
            eprintln!("[fm-core] probe {uri}: video={v} audio={a}");
            (v, a)
        }
        Err(e) => {
            eprintln!("[fm-core] probe failed for {uri}: {e}");
            (false, false)
        }
    }
}

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

fn make(factory: &str, name: &str) -> Result<gstreamer::Element> {
    gstreamer::ElementFactory::make(factory)
        .name(name)
        .build()
        .map_err(|e| format!("missing GStreamer element '{factory}': {e}").into())
}

/// Platform-selected transport source (ADR-0019 receive seam).
/// On Linux: `unixfdsrc` with `do-timestamp=false` so the adapter's PTS
/// passes through unmodified rather than being overwritten with arrival time.
/// Adding a new platform means adding a new `cfg` branch here and in
/// `fm-adapter-sdk/src/transport.rs`.
#[cfg(target_os = "linux")]
fn make_transport_src(name: &str, socket_path: &str) -> Result<gstreamer::Element> {
    let src = make("unixfdsrc", name)?;
    src.set_property_from_str("socket-path", socket_path);
    src.set_property("do-timestamp", false);
    Ok(src)
}

/// Source-side pad references for a single source.
///
/// `gst_pad_set_offset` only works reliably on **source** pads, so we store
/// the `capsfilter:src` pads that feed the compositor and audiomixer rather
/// than the mixer sink pads themselves (ADR-0004).
pub struct SourcePads {
    /// `vcaps_{id}:src` — the source pad feeding `compositor:sink_N`.
    /// None when the source has no video stream.
    pub video_src: Option<gstreamer::Pad>,
    /// `acaps_{id}:src` — the source pad feeding `audiomixer:sink_N`.
    /// None when the source has no audio stream.
    pub audio_src: Option<gstreamer::Pad>,
}

/// Tile position and offset for a single external source.
/// Stored at build time so `build_shmsrc_chain` can add chains dynamically
/// without access to the original SceneConfig.
struct SourceLayout {
    xpos: i32,
    ypos: i32,
    tile_w: i32,
    tile_h: i32,
    offset_ns: i64,
    volume: f64,
    /// Survives chain rebuild — source_layouts is what add_audio_chain re-applies.
    muted: bool,
}

/// The in-core GStreamer pipeline (Phase 1 + Phase 2).
///
/// File source topology (Phase 1):
///   uridecodebin ─► videoconvert ─► deinterlace ─► videoscale ─► capsfilter(tile RGBA) ─► compositor
///                ─► audioconvert ─► audioresample ─► capsfilter(48k/2ch) ─► audiomixer
///
/// External source topology (Phase 2, ADR-0005/0011/0012/0019/0016):
///   unixfdsrc(video, do-timestamp=false) ─► vshmcaps(prod_res) ─► queue ─►
///     vconv ─► vdeint ─► vscale ─► vcaps(tile) ─► voff_q ─► compositor
///   unixfdsrc(audio, do-timestamp=false) ─► ashmcaps(S16LE) ─► queue ─►
///     aconv ─► aresamp ─► acaps(48k/2ch) ─► audiomixer
///
///   unixfdsrc transfers full GstBuffers across the process boundary (PTS/caps/events
///   intact, zero-copy via fd passing).  do-timestamp=false keeps unixfdsrc from
///   overwriting the adapter's preserved PTS with arrival wall-clock time (ADR-0019).
///   voff_q = leaky(upstream) offset-buffer queue sized to live_offset_ceiling_ms (ADR-0016).
///
///   prod_res = full grid output resolution (ADR-0012 core-owned resize).
///   The adapter produces at prod_res; the core scales to tile size here.
///
/// compositor ─► capsfilter(output RGBA) ─► appsink  (→ UI bridge, ADR-0006)
/// audiomixer ─► audioconvert ─► audioresample ─► autoaudiosink
pub struct Pipeline {
    inner: gstreamer::Pipeline,
    appsink: gstreamer_app::AppSink,
    source_pads: HashMap<String, SourcePads>,
    /// `audiomixer` sink pad per source, keyed by source id.
    /// Separate from the offset-carrying `audio_src` pads (ADR-0004).
    mixer_sink_pads: HashMap<String, gstreamer::Pad>,
    /// `compositor` sink pad per source — stored so we can release it on teardown.
    comp_sink_pads: HashMap<String, gstreamer::Pad>,
    /// Grid output resolution and framerate — needed when building chains dynamically.
    grid_w: i32,
    grid_h: i32,
    grid_fps: i32,
    /// Tile layout per external source — needed when building chains dynamically.
    source_layouts: HashMap<String, SourceLayout>,
    /// Configured live-source offset ceiling in milliseconds (ADR-0016).
    /// Used to size the per-source offset buffer queues.
    ceiling_ms: u32,
    /// Compositor latency in milliseconds — equals ceiling_ms when external
    /// sources are present, 0 otherwise.  Used by MetricsCollector to adjust
    /// the fps stale threshold so FILE TERMINATED fires after buffered frames
    /// have actually been displayed, not while they are still in the compositor.
    compositor_latency_ms: u32,
}

impl Pipeline {
    /// Build the pipeline.
    ///
    /// `external_caps`: per external-source `(has_video, has_audio)` reported by
    /// the adapter's `Ready` message.  The core wires only the pads for streams
    /// that are present — same pattern as the Phase-1 discoverer probe.
    /// For sources not in the map, defaults to `(true, true)` (conservative).
    pub fn build(scene: &SceneConfig, external_caps: &ExternalCaps) -> Result<Self> {
        gstreamer::init()?;

        // Probe each source in parallel before building to learn which stream
        // types exist.  We only add chains and request aggregator sink pads for
        // streams confirmed to be present.
        //
        // A corrupt source (false, false) is skipped entirely — adding its
        // uridecodebin would post a bus error during the async PAUSED→PLAYING
        // state change, which prevents the pipeline from ever reaching PLAYING.
        //
        // A video-only source gets only the video chain; no idle audio chain is
        // left in the pipeline with an unlinked sink.
        // External sources use the caps reported in their Ready message.
        let stream_caps: Vec<(bool, bool)> = {
            let handles: Vec<_> = scene
                .source
                .iter()
                .map(|s| {
                    if s.source_type == SourceType::External {
                        let caps = external_caps.get(&s.id).copied().unwrap_or((true, true));
                        return std::thread::spawn(move || caps);
                    }
                    let uri = s.uri.clone().unwrap_or_default();
                    std::thread::spawn(move || probe_streams(&uri))
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap_or((false, false)))
                .collect()
        };

        // Compute grid geometry up front — needed for both the compositor output
        // caps (canvas = cols×tile_w × rows×tile_h) and per-source layout below.
        // scene.grid.width/height are per-TILE dimensions; the canvas expands to
        // fit cols×rows tiles, so a 2×1 grid of 1920×1080 tiles gives a 3840×1080
        // canvas (32:9), not a squashed 1920×1080 (the "2×1 AR bug").
        let n_sources = scene.source.len() as u32;
        let cols = scene.grid.columns.max(1).min(n_sources.max(1));
        let rows = (n_sources.max(1) + cols - 1) / cols;
        let tile_w = scene.grid.width as i32;
        let tile_h = scene.grid.height as i32;
        let canvas_w = cols as i32 * tile_w;
        let canvas_h = rows as i32 * tile_h;

        let pipeline = gstreamer::Pipeline::new();

        // ── Output video path ──────────────────────────────────────────────
        let compositor: gstreamer::Element = gstreamer::ElementFactory::make("compositor")
            .name("compositor")
            .build()?;
        compositor.set_property_from_str("background", "black");
        // Set compositor latency to the live-source offset ceiling so
        // GstAggregator waits for the most-delayed source (ADR-0016).
        // Safe because Group 2 (wait_for_playing) ensures aggregators are in
        // PLAYING state before any adapter pushes a frame.  Without this, the
        // aggregator consumes from whichever source has the earliest buffer and
        // positive-offset sources lag permanently.
        let has_external = scene
            .source
            .iter()
            .any(|s| s.source_type == SourceType::External);
        let compositor_latency_ms = if has_external {
            scene.grid.live_offset_ceiling_ms
        } else {
            0
        };
        if has_external {
            let ceiling_ns: u64 = scene.grid.live_offset_ceiling_ms as u64 * 1_000_000;
            compositor.set_property("latency", ceiling_ns);
        }

        let output_caps = gstreamer::Caps::builder("video/x-raw")
            .field("format", "RGBA")
            .field("width", canvas_w)
            .field("height", canvas_h)
            .field(
                "framerate",
                gstreamer::Fraction::new(scene.grid.fps as i32, 1),
            )
            .field("pixel-aspect-ratio", gstreamer::Fraction::new(1, 1))
            .build();

        let comp_capsfilter: gstreamer::Element = gstreamer::ElementFactory::make("capsfilter")
            .name("comp_capsfilter")
            .build()?;
        comp_capsfilter.set_property("caps", &output_caps);

        // sync=true so the compositor respects the pipeline clock (playback at
        // the configured fps, not decode speed). drop=true + max_buffers=2 means
        // a frame that arrives while the UI is still holding the mutex is dropped
        // rather than stalling the pipeline — the bridge never becomes a bottleneck.
        let appsink = gstreamer_app::AppSink::builder()
            .name("appsink")
            .sync(true)
            .max_buffers(2)
            .drop(true)
            .build();

        // ── Output audio path ──────────────────────────────────────────────
        let audiomixer: gstreamer::Element = gstreamer::ElementFactory::make("audiomixer")
            .name("audiomixer")
            .build()?;
        let aconv_out: gstreamer::Element = gstreamer::ElementFactory::make("audioconvert")
            .name("aconv_out")
            .build()?;
        let aresamp_out: gstreamer::Element = gstreamer::ElementFactory::make("audioresample")
            .name("aresamp_out")
            .build()?;
        let autoaudiosink: gstreamer::Element = gstreamer::ElementFactory::make("autoaudiosink")
            .name("autoaudiosink")
            .build()?;

        // ── Synthetic floor inputs (ADR-0018) ─────────────────────────────
        // A permanent silent audiotestsrc gives the audiomixer a heartbeat so
        // it reaches PLAYING regardless of how many real audio sources are
        // present (the common case is a video-only camera: zero real audio
        // inputs).  Volume=0 on the mixer pad keeps it inaudible; wave=silence
        // makes it the additive identity even if volume somehow slips.
        let silence_src: gstreamer::Element = gstreamer::ElementFactory::make("audiotestsrc")
            .name("silence_src")
            .build()?;
        silence_src.set_property_from_str("wave", "silence");
        silence_src.set_property("is-live", true);
        let silence_caps: gstreamer::Element = gstreamer::ElementFactory::make("capsfilter")
            .name("silence_caps")
            .build()?;
        silence_caps.set_property(
            "caps",
            gstreamer::Caps::builder("audio/x-raw")
                .field("format", "S16LE")
                .field("rate", 48_000i32)
                .field("channels", 2i32)
                .field("layout", "interleaved")
                .build(),
        );

        // A permanent black videotestsrc gives the compositor a heartbeat so
        // it reaches PLAYING when all real video sources are absent at cold
        // start.  It occupies zorder=0 (first pad requested); every real source
        // pad gets a higher zorder automatically and renders on top.
        let black_src: gstreamer::Element = gstreamer::ElementFactory::make("videotestsrc")
            .name("black_src")
            .build()?;
        // White floor (zorder=0) covers the full canvas.  Per-cell gray insets
        // at zorder=1 sit inside it; the white shows as a border only on dead
        // tiles (video at zorder=2 covers both when the source is live).
        black_src.set_property_from_str("pattern", "solid-color");
        black_src.set_property("foreground-color", 0xFFFFFFFFu32);
        black_src.set_property("is-live", true);
        let black_caps: gstreamer::Element = gstreamer::ElementFactory::make("capsfilter")
            .name("black_caps")
            .build()?;
        black_caps.set_property(
            "caps",
            gstreamer::Caps::builder("video/x-raw")
                .field("format", "RGBA")
                .field("width", canvas_w)
                .field("height", canvas_h)
                .field(
                    "framerate",
                    gstreamer::Fraction::new(scene.grid.fps as i32, 1),
                )
                .field("pixel-aspect-ratio", gstreamer::Fraction::new(1, 1))
                .build(),
        );

        pipeline.add(&compositor)?;
        pipeline.add(&comp_capsfilter)?;
        pipeline.add(&appsink)?;
        pipeline.add(&audiomixer)?;
        pipeline.add(&aconv_out)?;
        pipeline.add(&aresamp_out)?;
        pipeline.add(&autoaudiosink)?;
        pipeline.add(&silence_src)?;
        pipeline.add(&silence_caps)?;
        pipeline.add(&black_src)?;
        pipeline.add(&black_caps)?;

        compositor.link(&comp_capsfilter)?;
        comp_capsfilter.link(&appsink)?;
        gstreamer::Element::link_many([&audiomixer, &aconv_out, &aresamp_out, &autoaudiosink])?;

        // Wire audio floor: silence_src → silence_caps → audiomixer(volume=0)
        gstreamer::Element::link_many([&silence_src, &silence_caps])?;
        let mix_silence_pad = audiomixer
            .request_pad_simple("sink_%u")
            .ok_or("audiomixer: could not request silence sink pad")?;
        mix_silence_pad.set_property("volume", 0.0f64);
        silence_caps
            .static_pad("src")
            .ok_or("silence_caps: no src pad")?
            .link(&mix_silence_pad)?;

        // Wire video floor: black_src → black_caps → compositor(zorder=0)
        gstreamer::Element::link_many([&black_src, &black_caps])?;
        let comp_floor_pad = compositor
            .request_pad_simple("sink_%u")
            .ok_or("compositor: could not request floor sink pad")?;
        comp_floor_pad.set_property("zorder", 0u32);
        comp_floor_pad.set_property("width", canvas_w);
        comp_floor_pad.set_property("height", canvas_h);
        black_caps
            .static_pad("src")
            .ok_or("black_caps: no src pad")?
            .link(&comp_floor_pad)?;

        // ── Per-source elements ────────────────────────────────────────────
        if n_sources == 0 {
            return Err("scene.toml has no [[source]] entries".into());
        }
        // tile_w/tile_h = adapter production size (per-tile, not canvas).
        // grid_w/grid_h stored in Pipeline struct so add_video_chain can set
        // vshmcaps to the adapter production resolution on reconnect.
        let grid_w = tile_w;
        let grid_h = tile_h;
        let grid_fps = scene.grid.fps as i32;

        let mut source_pads: HashMap<String, SourcePads> = HashMap::new();
        let mut mixer_sink_pads: HashMap<String, gstreamer::Pad> = HashMap::new();
        let mut comp_sink_pads: HashMap<String, gstreamer::Pad> = HashMap::new();
        let mut source_layouts: HashMap<String, SourceLayout> = HashMap::new();

        for (idx, source) in scene.source.iter().enumerate() {
            let (has_video, has_audio) = stream_caps[idx];

            let xpos = ((idx as u32 % cols) * tile_w as u32) as i32;
            let ypos = ((idx as u32 / cols) * tile_h as u32) as i32;
            let offset_ns = source.offset_ms * 1_000_000;

            // Store layout for ALL sources — including offline ones — so that
            // add_video_chain / add_audio_chain can populate the tile later when
            // a source comes online via StreamsChanged after initial build().
            source_layouts.insert(
                source.id.clone(),
                SourceLayout {
                    xpos,
                    ypos,
                    tile_w,
                    tile_h,
                    offset_ns,
                    volume: source.volume,
                    muted: source.muted,
                },
            );

            // zorder=1 gray inset — ~25% gray inset inside the white floor.
            // Visible only when the source is dead (video at zorder=2 covers it).
            // border_w controls the white frame thickness on a dead tile.
            const BORDER_W: i32 = 4;
            {
                let gi = gstreamer::ElementFactory::make("videotestsrc")
                    .name(format!("gray_{}", source.id))
                    .build()?;
                gi.set_property_from_str("pattern", "solid-color");
                gi.set_property("foreground-color", 0xFF404040u32);
                gi.set_property("is-live", true);
                let gi_caps = gstreamer::ElementFactory::make("capsfilter")
                    .name(format!("gray_caps_{}", source.id))
                    .build()?;
                gi_caps.set_property(
                    "caps",
                    &gstreamer::Caps::builder("video/x-raw")
                        .field("format", "RGBA")
                        .field("width", tile_w - 2 * BORDER_W)
                        .field("height", tile_h - 2 * BORDER_W)
                        .field("framerate", gstreamer::Fraction::new(grid_fps, 1))
                        .field("pixel-aspect-ratio", gstreamer::Fraction::new(1, 1))
                        .build(),
                );
                pipeline.add_many([&gi, &gi_caps])?;
                gstreamer::Element::link_many([&gi, &gi_caps])?;
                let gi_sink = compositor
                    .request_pad_simple("sink_%u")
                    .ok_or("compositor: could not request gray inset sink pad")?;
                gi_sink.set_property("zorder", 1u32);
                gi_sink.set_property("xpos", xpos + BORDER_W);
                gi_sink.set_property("ypos", ypos + BORDER_W);
                gi_sink.set_property("width", tile_w - 2 * BORDER_W);
                gi_sink.set_property("height", tile_h - 2 * BORDER_W);
                gi_caps
                    .static_pad("src")
                    .ok_or("gray_caps: no src pad")?
                    .link(&gi_sink)?;
            }

            if !has_video && !has_audio {
                eprintln!(
                    "[fm-core] skipping source '{}' — no usable streams (corrupt or empty)",
                    source.id
                );
                continue;
            }

            // ── Video chain (only when probe confirmed video) ──────────────
            let mut vcaps_src: Option<gstreamer::Pad> = None;
            let mut vconv_sink_for_cb: Option<gstreamer::Pad> = None;

            if has_video {
                let vconv: gstreamer::Element = gstreamer::ElementFactory::make("videoconvert")
                    .name(format!("vconv_{}", source.id))
                    .build()?;
                // deinterlace passes progressive content through unchanged and
                // converts interlaced fields to progressive frames.
                let vdeint: gstreamer::Element = gstreamer::ElementFactory::make("deinterlace")
                    .name(format!("vdeint_{}", source.id))
                    .build()?;
                let vscale: gstreamer::Element = gstreamer::ElementFactory::make("videoscale")
                    .name(format!("vscale_{}", source.id))
                    .build()?;
                let vcaps: gstreamer::Element = gstreamer::ElementFactory::make("capsfilter")
                    .name(format!("vcaps_{}", source.id))
                    .build()?;
                vcaps.set_property(
                    "caps",
                    &gstreamer::Caps::builder("video/x-raw")
                        .field("format", "RGBA")
                        .field("width", tile_w)
                        .field("height", tile_h)
                        .field("pixel-aspect-ratio", gstreamer::Fraction::new(1, 1))
                        .build(),
                );

                pipeline.add_many([&vconv, &vdeint, &vscale, &vcaps])?;
                gstreamer::Element::link_many([&vconv, &vdeint, &vscale, &vcaps])?;

                let comp_sink = compositor
                    .request_pad_simple("sink_%u")
                    .ok_or("could not request compositor sink pad")?;
                comp_sink.set_property("zorder", 2u32);
                comp_sink.set_property("xpos", xpos);
                comp_sink.set_property("ypos", ypos);
                comp_sink.set_property("width", tile_w);
                comp_sink.set_property("height", tile_h);
                // Repeat the last frame when a source loops (keeps the tile
                // alive during the seek-back gap from the EOS→seek strategy).
                comp_sink.set_property("repeat-after-eos", true);

                let vs = vcaps.static_pad("src").ok_or("vcaps: no src pad")?;
                vs.set_offset(offset_ns);

                // For external (live) sources, insert the offset buffer queue
                // between vcaps and the compositor (ADR-0016).  The queue holds
                // up to ceiling_ms of tile-resolution frames so a positive pad
                // offset can delay presentation without losing frames.
                //
                // leaky=upstream: when the queue is full (offset at or beyond
                // the ceiling), incoming frames are dropped rather than the
                // oldest.  Dropping oldest frames (leaky=downstream) would
                // discard the very frames the compositor is waiting to consume,
                // causing n×frame_duration PTS divergence (T3).
                // ADR-0016 text says "leaky=downstream" — that text is incorrect
                // for a delay buffer; leaky=upstream is the correct behavior.
                // Flag to review chat for ADR correction/supersession.
                if source.source_type == SourceType::External {
                    let ceiling_ms = scene.grid.live_offset_ceiling_ms as u64;
                    let ceiling_ns = ceiling_ms * 1_000_000;
                    let ceiling_buffers = (ceiling_ms * scene.grid.fps as u64 / 1000 + 4) as u32;
                    let voff_q: gstreamer::Element = gstreamer::ElementFactory::make("queue")
                        .name(format!("voff_q_{}", source.id))
                        .build()?;
                    voff_q.set_property("max-size-buffers", ceiling_buffers);
                    voff_q.set_property("max-size-bytes", 0u32);
                    voff_q.set_property("max-size-time", ceiling_ns);
                    voff_q.set_property_from_str("leaky", "upstream");
                    pipeline.add(&voff_q)?;
                    vs.link(&voff_q.static_pad("sink").ok_or("voff_q: no sink")?)?;
                    voff_q
                        .static_pad("src")
                        .ok_or("voff_q: no src")?
                        .link(&comp_sink)?;
                } else {
                    vs.link(&comp_sink)?;
                }

                comp_sink_pads.insert(source.id.clone(), comp_sink);
                vconv_sink_for_cb = Some(vconv.static_pad("sink").ok_or("vconv: no sink pad")?);
                vcaps_src = Some(vs);
            }

            // ── Audio chain (only when probe confirmed audio) ──────────────
            let mut acaps_src: Option<gstreamer::Pad> = None;
            let mut aconv_sink_for_cb: Option<gstreamer::Pad> = None;

            if has_audio {
                let aconv: gstreamer::Element = gstreamer::ElementFactory::make("audioconvert")
                    .name(format!("aconv_{}", source.id))
                    .build()?;
                let aresamp: gstreamer::Element = gstreamer::ElementFactory::make("audioresample")
                    .name(format!("aresamp_{}", source.id))
                    .build()?;
                let alevel: gstreamer::Element = gstreamer::ElementFactory::make("level")
                    .name(format!("alevel_{}", source.id))
                    .build()?;
                alevel.set_property("post-messages", true);
                let acaps: gstreamer::Element = gstreamer::ElementFactory::make("capsfilter")
                    .name(format!("acaps_{}", source.id))
                    .build()?;
                acaps.set_property(
                    "caps",
                    &gstreamer::Caps::builder("audio/x-raw")
                        .field("rate", 48_000i32)
                        .field("channels", 2i32)
                        .build(),
                );

                pipeline.add_many([&aconv, &aresamp, &alevel, &acaps])?;
                gstreamer::Element::link_many([&aconv, &aresamp, &alevel, &acaps])?;

                let mix_sink = audiomixer
                    .request_pad_simple("sink_%u")
                    .ok_or("could not request audiomixer sink pad")?;

                let as_ = acaps.static_pad("src").ok_or("acaps: no src pad")?;
                as_.set_offset(offset_ns);
                as_.link(&mix_sink)?;
                mix_sink.set_property("volume", source.volume);
                mix_sink.set_property("mute", source.muted);
                mixer_sink_pads.insert(source.id.clone(), mix_sink);

                aconv_sink_for_cb = Some(aconv.static_pad("sink").ok_or("aconv: no sink pad")?);
                acaps_src = Some(as_);
            }

            source_pads.insert(
                source.id.clone(),
                SourcePads {
                    video_src: vcaps_src,
                    audio_src: acaps_src,
                },
            );

            // ── Source elements (file vs external) ─────────────────────────
            match source.source_type {
                SourceType::File => {
                    let uri_elem: gstreamer::Element =
                        gstreamer::ElementFactory::make("uridecodebin")
                            .name(format!("uri_{}", source.id))
                            .build()?;
                    uri_elem.set_property("uri", source.uri.as_deref().unwrap_or(""));
                    pipeline.add(&uri_elem)?;

                    let id_for_cb = source.id.clone();
                    uri_elem.connect_pad_added(move |_src, pad| {
                        let caps = match pad.current_caps() {
                            Some(c) => c,
                            None => pad.query_caps(None),
                        };
                        let Some(s) = caps.structure(0) else { return };
                        let media = s.name();

                        if media.starts_with("video/") {
                            if let Some(ref vconv_sink) = vconv_sink_for_cb {
                                if !vconv_sink.is_linked() {
                                    if let Err(e) = pad.link(vconv_sink) {
                                        eprintln!("[fm-core] {id_for_cb}: video link failed: {e}");
                                    }
                                }
                            }
                        } else if media.starts_with("audio/") {
                            if let Some(ref aconv_sink) = aconv_sink_for_cb {
                                if !aconv_sink.is_linked() {
                                    if let Err(e) = pad.link(aconv_sink) {
                                        eprintln!("[fm-core] {id_for_cb}: audio link failed: {e}");
                                    }
                                }
                            }
                        }
                    });
                }
                SourceType::External => {
                    // unixfd transport (ADR-0019): unixfdsrc carries GstBuffers
                    // across the process boundary with PTS/caps/events intact —
                    // no framing layer needed.  do-timestamp=false preserves the
                    // adapter's PTS rather than overwriting with arrival time.
                    let (video_sock, audio_sock) = runtime::shm_paths(&source.id);

                    if has_video {
                        let vunixfdsrc =
                            make_transport_src(&format!("vunixfdsrc_{}", source.id), &video_sock)?;

                        let vshmcaps: gstreamer::Element =
                            gstreamer::ElementFactory::make("capsfilter")
                                .name(format!("vshmcaps_{}", source.id))
                                .build()?;
                        vshmcaps.set_property(
                            "caps",
                            gstreamer::Caps::builder("video/x-raw")
                                .field("format", "RGBA")
                                .field("width", grid_w)
                                .field("height", grid_h)
                                .field("framerate", gstreamer::Fraction::new(grid_fps, 1))
                                .field("pixel-aspect-ratio", gstreamer::Fraction::new(1, 1))
                                .build(),
                        );

                        let vqueue: gstreamer::Element = gstreamer::ElementFactory::make("queue")
                            .name(format!("vshm_q_{}", source.id))
                            .build()?;
                        vqueue.set_property("max-size-buffers", 2u32);
                        vqueue.set_property("max-size-bytes", 0u32);
                        vqueue.set_property("max-size-time", 0u64);
                        vqueue.set_property_from_str("leaky", "downstream");

                        pipeline.add_many([&vunixfdsrc, &vshmcaps, &vqueue])?;
                        gstreamer::Element::link_many([&vunixfdsrc, &vshmcaps, &vqueue])?;
                        if let Some(ref vconv_sink) = vconv_sink_for_cb {
                            vqueue
                                .static_pad("src")
                                .ok_or("vshm_q: no src pad")?
                                .link(vconv_sink)?;
                        }
                    }

                    if has_audio {
                        let aunixfdsrc =
                            make_transport_src(&format!("aunixfdsrc_{}", source.id), &audio_sock)?;

                        let ashmcaps: gstreamer::Element =
                            gstreamer::ElementFactory::make("capsfilter")
                                .name(format!("ashmcaps_{}", source.id))
                                .build()?;
                        ashmcaps.set_property(
                            "caps",
                            gstreamer::Caps::builder("audio/x-raw")
                                .field("format", "S16LE")
                                .field("rate", 48_000i32)
                                .field("channels", 2i32)
                                .field("layout", "interleaved")
                                .build(),
                        );

                        let aqueue: gstreamer::Element = gstreamer::ElementFactory::make("queue")
                            .name(format!("ashm_q_{}", source.id))
                            .build()?;
                        aqueue.set_property("max-size-buffers", 4u32);
                        aqueue.set_property("max-size-bytes", 0u32);
                        aqueue.set_property("max-size-time", 0u64);
                        aqueue.set_property_from_str("leaky", "downstream");

                        pipeline.add_many([&aunixfdsrc, &ashmcaps, &aqueue])?;
                        gstreamer::Element::link_many([&aunixfdsrc, &ashmcaps, &aqueue])?;
                        if let Some(ref aconv_sink) = aconv_sink_for_cb {
                            aqueue
                                .static_pad("src")
                                .ok_or("ashm_q: no src pad")?
                                .link(aconv_sink)?;
                        }
                    }
                }
            }
        }

        Ok(Self {
            inner: pipeline,
            appsink,
            source_pads,
            mixer_sink_pads,
            comp_sink_pads,
            grid_w,
            grid_h,
            grid_fps,
            source_layouts,
            ceiling_ms: scene.grid.live_offset_ceiling_ms,
            compositor_latency_ms,
        })
    }

    pub fn inner(&self) -> &gstreamer::Pipeline {
        &self.inner
    }

    pub fn compositor_latency_ms(&self) -> u32 {
        self.compositor_latency_ms
    }

    pub fn appsink(&self) -> &gstreamer_app::AppSink {
        &self.appsink
    }

    pub fn source_pads(&self) -> &HashMap<String, SourcePads> {
        &self.source_pads
    }

    /// True if the core pipeline has an active video chain for `source_id`.
    /// Used by the delivery watchdog (ADR-0020) to detect producing-but-no-chain.
    pub fn source_has_chain(&self, source_id: &str) -> bool {
        self.inner
            .by_name(&format!("vunixfdsrc_{source_id}"))
            .is_some()
    }

    pub fn mixer_sink_pads(&self) -> &HashMap<String, gstreamer::Pad> {
        &self.mixer_sink_pads
    }

    /// Keep source_layouts in sync so add_video/audio_chain re-applies the
    /// correct offset on the next chain rebuild (reconnect path).
    pub fn update_source_layout_offset(&mut self, source_id: &str, offset_ns: i64) {
        if let Some(layout) = self.source_layouts.get_mut(source_id) {
            layout.offset_ns = offset_ns;
        }
    }

    /// Keep source_layouts in sync so add_audio_chain re-applies the correct
    /// mute state on the next chain rebuild (reconnect path).
    pub fn update_source_layout_mute(&mut self, source_id: &str, muted: bool) {
        if let Some(layout) = self.source_layouts.get_mut(source_id) {
            layout.muted = muted;
        }
    }

    /// Reset the shmsrc elements for an external source after its adapter
    /// process has restarted and created fresh unixfdsink sockets.
    /// Sets each element to NULL then syncs it back to the pipeline state so
    /// it reconnects to the new socket.
    pub fn restart_shmsrc(&self, source_id: &str) {
        for prefix in &["vunixfdsrc_", "aunixfdsrc_"] {
            let name = format!("{prefix}{source_id}");
            if let Some(elem) = self.inner.by_name(&name) {
                eprintln!("[pipeline] resetting {name} for reconnect");
                let _ = elem.set_state(gstreamer::State::Null);
                let _ = elem.sync_state_with_parent();
            }
        }
    }

    /// Apply a topology change from a StreamsChanged message (ADR-0013).
    ///
    /// Adds missing unixfdsrc chains and tears down chains that are no longer
    /// present, live on a PLAYING pipeline.  This is the implementation risk
    /// called out in ADR-0013 — if it stalls the compositor, stop and treat it
    /// as a core problem to solve.
    pub fn build_shmsrc_chain(
        &mut self,
        source_id: &str,
        has_video: bool,
        has_audio: bool,
        source_fps: f64,
    ) {
        let current_has_video = self
            .inner
            .by_name(&format!("vunixfdsrc_{source_id}"))
            .is_some();
        let current_has_audio = self
            .inner
            .by_name(&format!("aunixfdsrc_{source_id}"))
            .is_some();

        if has_video {
            if current_has_video {
                // Rebuild, not reuse: the compositor's sink pad carries a PTS
                // timeline from the previous stream epoch.  After an adapter
                // restart the new RTSP stream begins at PTS≈0; the aggregator
                // stalls until PTS+offset catches up to the current session
                // running time (~20 s freeze / single-frame pulses).
                // Re-creating the pad (remove then add) gives the aggregator a
                // blank slate — identical to what cold-start does via
                // request_pad_simple.  This converges reconnect onto the same
                // code path so the asymmetry cannot reappear.
                self.remove_video_chain(source_id);
            }
            if let Err(e) = self.add_video_chain(source_id, source_fps) {
                eprintln!("[pipeline] add_video_chain '{source_id}': {e}");
            }
        } else if current_has_video {
            self.remove_video_chain(source_id);
        }

        if has_audio {
            if current_has_audio {
                self.remove_audio_chain(source_id);
            }
            if let Err(e) = self.add_audio_chain(source_id) {
                eprintln!("[pipeline] add_audio_chain '{source_id}': {e}");
            }
        } else if current_has_audio {
            self.remove_audio_chain(source_id);
        }
    }

    /// Remove all unixfdsrc chains for a source (full teardown for permanent removal).
    pub fn teardown_shmsrc_chain(&mut self, source_id: &str) {
        self.remove_video_chain(source_id);
        self.remove_audio_chain(source_id);
    }

    // ── Dynamic chain builders / tearers ─────────────────────────────────────

    fn add_video_chain(&mut self, source_id: &str, source_fps: f64) -> Result<()> {
        let layout = self
            .source_layouts
            .get(source_id)
            .ok_or_else(|| format!("no layout for '{source_id}'"))?;

        let (video_sock, _) = runtime::shm_paths(source_id);

        let vunixfdsrc = make_transport_src(&format!("vunixfdsrc_{source_id}"), &video_sock)?;

        let vshmcaps = make("capsfilter", &format!("vshmcaps_{source_id}"))?;
        vshmcaps.set_property(
            "caps",
            gstreamer::Caps::builder("video/x-raw")
                .field("format", "RGBA")
                .field("width", self.grid_w)
                .field("height", self.grid_h)
                .field("framerate", gstreamer::Fraction::new(self.grid_fps, 1))
                .field("pixel-aspect-ratio", gstreamer::Fraction::new(1, 1))
                .build(),
        );

        let vqueue = make("queue", &format!("vshm_q_{source_id}"))?;
        vqueue.set_property("max-size-buffers", 2u32);
        vqueue.set_property("max-size-bytes", 0u32);
        vqueue.set_property("max-size-time", 0u64);
        vqueue.set_property_from_str("leaky", "downstream");

        let vconv = make("videoconvert", &format!("vconv_{source_id}"))?;
        let vdeint = make("deinterlace", &format!("vdeint_{source_id}"))?;
        let vscale = make("videoscale", &format!("vscale_{source_id}"))?;
        let vcaps = make("capsfilter", &format!("vcaps_{source_id}"))?;
        vcaps.set_property(
            "caps",
            &gstreamer::Caps::builder("video/x-raw")
                .field("format", "RGBA")
                .field("width", layout.tile_w)
                .field("height", layout.tile_h)
                .field("pixel-aspect-ratio", gstreamer::Fraction::new(1, 1))
                .build(),
        );

        self.inner.add_many([
            &vunixfdsrc,
            &vshmcaps,
            &vqueue,
            &vconv,
            &vdeint,
            &vscale,
            &vcaps,
        ])?;
        gstreamer::Element::link_many([&vunixfdsrc, &vshmcaps, &vqueue])?;
        gstreamer::Element::link_many([&vconv, &vdeint, &vscale, &vcaps])?;
        vqueue
            .static_pad("src")
            .ok_or("vshm_q: no src")?
            .link(&vconv.static_pad("sink").ok_or("vconv: no sink")?)?;

        let compositor = self
            .inner
            .by_name("compositor")
            .ok_or("compositor not found")?;
        let comp_sink = compositor
            .request_pad_simple("sink_%u")
            .ok_or("compositor: no sink pad")?;
        comp_sink.set_property("zorder", 2u32);
        comp_sink.set_property("xpos", layout.xpos);
        comp_sink.set_property("ypos", layout.ypos);
        comp_sink.set_property("width", layout.tile_w);
        comp_sink.set_property("height", layout.tile_h);
        comp_sink.set_property("repeat-after-eos", true);

        let vcaps_src = vcaps.static_pad("src").ok_or("vcaps: no src")?;
        vcaps_src.set_offset(layout.offset_ns);

        let ceiling_ms = self.ceiling_ms as u64;
        let ceiling_ns = ceiling_ms * 1_000_000;
        let ceiling_buffers = (ceiling_ms * self.grid_fps as u64 / 1000 + 4) as u32;
        let voff_q = make("queue", &format!("voff_q_{source_id}"))?;
        voff_q.set_property("max-size-buffers", ceiling_buffers);
        voff_q.set_property("max-size-bytes", 0u32);
        voff_q.set_property("max-size-time", ceiling_ns);
        // leaky=upstream: drop incoming frames when full to preserve the oldest
        // frames the compositor needs to consume (ADR-0016 delay buffer).
        voff_q.set_property_from_str("leaky", "upstream");
        self.inner.add(&voff_q)?;
        vcaps_src.link(&voff_q.static_pad("sink").ok_or("voff_q: no sink")?)?;
        let voff_src = voff_q.static_pad("src").ok_or("voff_q: no src")?;
        voff_src.link(&comp_sink)?;

        // One-shot probe: log first PTS vs pipeline running time so PTS gaps at
        // reconnect are visible in the session log.
        {
            let pipeline_weak = self.inner.downgrade();
            let sid = source_id.to_string();
            let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            voff_src.add_probe(gstreamer::PadProbeType::BUFFER, move |_, info| {
                if !done.swap(true, std::sync::atomic::Ordering::Relaxed) {
                    if let Some(gstreamer::PadProbeData::Buffer(buf)) = &info.data {
                        if let Some(p) = pipeline_weak.upgrade() {
                            let running = p.current_running_time();
                            eprintln!(
                                "[reconnect-pts] '{}' first_pts={:?} pipeline_running={:?}",
                                sid,
                                buf.pts(),
                                running
                            );
                        }
                    }
                }
                gstreamer::PadProbeReturn::Ok
            });
        }
        add_offset_canary(
            &voff_src,
            source_id,
            layout.offset_ns,
            self.ceiling_ms as u64,
            self.inner.downgrade(),
            source_fps,
        );

        for elem in [
            &vunixfdsrc,
            &vshmcaps,
            &vqueue,
            &vconv,
            &vdeint,
            &vscale,
            &vcaps,
            &voff_q,
        ] {
            let _ = elem.sync_state_with_parent();
        }

        eprintln!("[pipeline] added video chain for '{source_id}'");
        self.comp_sink_pads.insert(source_id.to_string(), comp_sink);
        self.source_pads
            .entry(source_id.to_string())
            .or_insert(SourcePads {
                video_src: None,
                audio_src: None,
            })
            .video_src = Some(vcaps_src);
        Ok(())
    }

    fn remove_video_chain(&mut self, source_id: &str) {
        // Set shmsrc to NULL first to stop the data flow.
        if let Some(elem) = self.inner.by_name(&format!("vunixfdsrc_{source_id}")) {
            let _ = elem.set_state(gstreamer::State::Null);
        }

        // Unlink and release compositor pad.  With the offset buffer queue,
        // voff_q:src is what links to comp_sink (not vcaps_src directly).
        if let Some(comp_sink) = self.comp_sink_pads.remove(source_id) {
            if let Some(voff_q) = self.inner.by_name(&format!("voff_q_{source_id}")) {
                if let Some(voff_src) = voff_q.static_pad("src") {
                    let _ = voff_src.unlink(&comp_sink);
                }
            }
            if let Some(compositor) = self.inner.by_name("compositor") {
                compositor.release_request_pad(&comp_sink);
            }
        }
        if let Some(sp) = self.source_pads.get_mut(source_id) {
            sp.video_src = None;
        }

        for name in [
            format!("vunixfdsrc_{source_id}"),
            format!("vshmcaps_{source_id}"),
            format!("vshm_q_{source_id}"),
            format!("vconv_{source_id}"),
            format!("vdeint_{source_id}"),
            format!("vscale_{source_id}"),
            format!("vcaps_{source_id}"),
            format!("voff_q_{source_id}"),
        ] {
            if let Some(elem) = self.inner.by_name(&name) {
                let _ = elem.set_state(gstreamer::State::Null);
                let _ = self.inner.remove(&elem);
            }
        }
        eprintln!("[pipeline] removed video chain for '{source_id}'");
    }

    fn add_audio_chain(&mut self, source_id: &str) -> Result<()> {
        let layout = self
            .source_layouts
            .get(source_id)
            .ok_or_else(|| format!("no layout for '{source_id}'"))?;

        let (_, audio_sock) = runtime::shm_paths(source_id);

        let aunixfdsrc = make_transport_src(&format!("aunixfdsrc_{source_id}"), &audio_sock)?;

        let ashmcaps = make("capsfilter", &format!("ashmcaps_{source_id}"))?;
        ashmcaps.set_property(
            "caps",
            gstreamer::Caps::builder("audio/x-raw")
                .field("format", "S16LE")
                .field("rate", 48_000i32)
                .field("channels", 2i32)
                .field("layout", "interleaved")
                .build(),
        );

        let aqueue = make("queue", &format!("ashm_q_{source_id}"))?;
        aqueue.set_property("max-size-buffers", 4u32);
        aqueue.set_property("max-size-bytes", 0u32);
        aqueue.set_property("max-size-time", 0u64);
        aqueue.set_property_from_str("leaky", "downstream");

        let aconv = make("audioconvert", &format!("aconv_{source_id}"))?;
        let aresamp = make("audioresample", &format!("aresamp_{source_id}"))?;
        let alevel = make("level", &format!("alevel_{source_id}"))?;
        alevel.set_property("post-messages", true);
        let acaps = make("capsfilter", &format!("acaps_{source_id}"))?;
        acaps.set_property(
            "caps",
            &gstreamer::Caps::builder("audio/x-raw")
                .field("rate", 48_000i32)
                .field("channels", 2i32)
                .build(),
        );

        self.inner.add_many([
            &aunixfdsrc,
            &ashmcaps,
            &aqueue,
            &aconv,
            &aresamp,
            &alevel,
            &acaps,
        ])?;
        gstreamer::Element::link_many([&aunixfdsrc, &ashmcaps, &aqueue])?;
        gstreamer::Element::link_many([&aconv, &aresamp, &alevel, &acaps])?;
        aqueue
            .static_pad("src")
            .ok_or("ashm_q: no src")?
            .link(&aconv.static_pad("sink").ok_or("aconv: no sink")?)?;

        let audiomixer = self
            .inner
            .by_name("audiomixer")
            .ok_or("audiomixer not found")?;
        let mix_sink = audiomixer
            .request_pad_simple("sink_%u")
            .ok_or("audiomixer: no sink pad")?;

        let acaps_src = acaps.static_pad("src").ok_or("acaps: no src")?;
        acaps_src.set_offset(layout.offset_ns);
        acaps_src.link(&mix_sink)?;
        mix_sink.set_property("volume", layout.volume);
        mix_sink.set_property("mute", layout.muted);

        for elem in [
            &aunixfdsrc,
            &ashmcaps,
            &aqueue,
            &aconv,
            &aresamp,
            &alevel,
            &acaps,
        ] {
            let _ = elem.sync_state_with_parent();
        }

        eprintln!("[pipeline] added audio chain for '{source_id}'");
        self.mixer_sink_pads.insert(source_id.to_string(), mix_sink);
        self.source_pads
            .entry(source_id.to_string())
            .or_insert(SourcePads {
                video_src: None,
                audio_src: None,
            })
            .audio_src = Some(acaps_src);
        Ok(())
    }

    fn remove_audio_chain(&mut self, source_id: &str) {
        if let Some(elem) = self.inner.by_name(&format!("aunixfdsrc_{source_id}")) {
            let _ = elem.set_state(gstreamer::State::Null);
        }

        if let Some(mix_sink) = self.mixer_sink_pads.remove(source_id) {
            if let Some(sp) = self.source_pads.get(source_id) {
                if let Some(ref acaps_src) = sp.audio_src {
                    let _ = acaps_src.unlink(&mix_sink);
                }
            }
            if let Some(audiomixer) = self.inner.by_name("audiomixer") {
                audiomixer.release_request_pad(&mix_sink);
            }
        }
        if let Some(sp) = self.source_pads.get_mut(source_id) {
            sp.audio_src = None;
        }

        for name in [
            format!("aunixfdsrc_{source_id}"),
            format!("ashmcaps_{source_id}"),
            format!("ashm_q_{source_id}"),
            format!("aconv_{source_id}"),
            format!("aresamp_{source_id}"),
            format!("alevel_{source_id}"),
            format!("acaps_{source_id}"),
        ] {
            if let Some(elem) = self.inner.by_name(&name) {
                let _ = elem.set_state(gstreamer::State::Null);
                let _ = self.inner.remove(&elem);
            }
        }
        eprintln!("[pipeline] removed audio chain for '{source_id}'");
    }
}
