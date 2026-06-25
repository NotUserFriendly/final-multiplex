use crate::config::{SceneConfig, SourceType};
use crate::runtime;
use gstreamer::prelude::*;
use std::collections::HashMap;

type ExternalCaps = HashMap<String, (bool, bool)>;

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
}

/// The in-core GStreamer pipeline (Phase 1 + Phase 2).
///
/// File source topology (Phase 1):
///   uridecodebin ─► videoconvert ─► deinterlace ─► videoscale ─► capsfilter(tile RGBA) ─► compositor
///                ─► audioconvert ─► audioresample ─► capsfilter(48k/2ch) ─► audiomixer
///
/// External source topology (Phase 2, ADR-0005 / ADR-0011 / ADR-0012):
///   shmsrc(video) ─► vshmcaps(prod_res) ─► queue ─► vconv ─► vdeint ─► vscale ─► vcaps(tile) ─► compositor
///   shmsrc(audio) ─► ashmcaps(S16LE)    ─► queue ─► aconv ─► aresamp ─► acaps(48k/2ch) ─► audiomixer
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

        let pipeline = gstreamer::Pipeline::new();

        // ── Output video path ──────────────────────────────────────────────
        let compositor: gstreamer::Element = gstreamer::ElementFactory::make("compositor")
            .name("compositor")
            .build()?;
        compositor.set_property_from_str("background", "black");

        let output_caps = gstreamer::Caps::builder("video/x-raw")
            .field("format", "RGBA")
            .field("width", scene.grid.width as i32)
            .field("height", scene.grid.height as i32)
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

        // audiotestsrc(silence) ensures audiomixer always has at least one
        // input so it can negotiate caps even when every source is video-only
        // or offline at startup. Adding live=true keeps it consistent with
        // the rest of the live pipeline.
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
                .field("rate", 48_000i32)
                .field("channels", 2i32)
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

        compositor.link(&comp_capsfilter)?;
        comp_capsfilter.link(&appsink)?;
        gstreamer::Element::link_many([&audiomixer, &aconv_out, &aresamp_out, &autoaudiosink])?;
        gstreamer::Element::link_many([&silence_src, &silence_caps])?;
        let mix_silence_pad = audiomixer
            .request_pad_simple("sink_%u")
            .ok_or("audiomixer: could not request silence sink pad")?;
        silence_caps
            .static_pad("src")
            .ok_or("silence_caps: no src pad")?
            .link(&mix_silence_pad)?;

        // ── Per-source elements ────────────────────────────────────────────
        let n = scene.source.len() as u32;
        if n == 0 {
            return Err("scene.toml has no [[source]] entries".into());
        }
        let cols = scene.grid.columns.max(1).min(n);
        let rows = (n + cols - 1) / cols;
        let tile_w = (scene.grid.width / cols) as i32;
        let tile_h = (scene.grid.height / rows) as i32;
        let grid_w = scene.grid.width as i32;
        let grid_h = scene.grid.height as i32;
        let grid_fps = scene.grid.fps as i32;

        let mut source_pads: HashMap<String, SourcePads> = HashMap::new();
        let mut mixer_sink_pads: HashMap<String, gstreamer::Pad> = HashMap::new();
        let mut comp_sink_pads: HashMap<String, gstreamer::Pad> = HashMap::new();
        let mut source_layouts: HashMap<String, SourceLayout> = HashMap::new();

        for (idx, source) in scene.source.iter().enumerate() {
            let (has_video, has_audio) = stream_caps[idx];

            if !has_video && !has_audio {
                eprintln!(
                    "[fm-core] skipping source '{}' — no usable streams (corrupt or empty)",
                    source.id
                );
                continue;
            }

            let xpos = ((idx as u32 % cols) * tile_w as u32) as i32;
            let ypos = ((idx as u32 / cols) * tile_h as u32) as i32;
            let offset_ns = source.offset_ms * 1_000_000;

            // Store layout for later dynamic chain builds.
            source_layouts.insert(
                source.id.clone(),
                SourceLayout {
                    xpos,
                    ypos,
                    tile_w,
                    tile_h,
                    offset_ns,
                    volume: source.volume,
                },
            );

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
                comp_sink.set_property("xpos", xpos);
                comp_sink.set_property("ypos", ypos);
                comp_sink.set_property("width", tile_w);
                comp_sink.set_property("height", tile_h);
                // Repeat the last frame when a source loops (keeps the tile
                // alive during the seek-back gap from the EOS→seek strategy).
                comp_sink.set_property("repeat-after-eos", true);

                let vs = vcaps.static_pad("src").ok_or("vcaps: no src pad")?;
                vs.set_offset(offset_ns);
                vs.link(&comp_sink)?;

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
                    // shmsrc elements — connect to the adapter's shmsink sockets.
                    // Capsfilters after each shmsrc pin the expected caps so GStreamer
                    // doesn't have to guess during negotiation (the adapter contract
                    // guarantees these formats — ADR-0011).
                    let (video_sock, audio_sock) = runtime::shm_paths(&source.id);

                    if has_video {
                        let vshmsrc: gstreamer::Element = gstreamer::ElementFactory::make("shmsrc")
                            .name(format!("vshmsrc_{}", source.id))
                            .build()?;
                        vshmsrc.set_property_from_str("socket-path", &video_sock);
                        vshmsrc.set_property("is-live", true);
                        // GDP headers carry PTS from the adapter; don't overwrite.
                        vshmsrc.set_property("do-timestamp", false);

                        // gdpdepay restores the buffer PTS and caps serialized by
                        // the adapter's gdppay — the adapter's clock-coherent PTS
                        // is what we want to use for timing.
                        let vgdpdepay: gstreamer::Element =
                            gstreamer::ElementFactory::make("gdpdepay")
                                .name(format!("vgdpdepay_{}", source.id))
                                .build()?;

                        // vshmcaps pins the production resolution that the adapter
                        // was launched with (full grid output resolution by default —
                        // ADR-0012 core-owned resize).  The vscale + vcaps chain
                        // downstream scales to tile dimensions.
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

                        // queue decouples the shmsrc (live) thread from the
                        // compositor thread so they don't block each other.
                        let vqueue: gstreamer::Element = gstreamer::ElementFactory::make("queue")
                            .name(format!("vshm_q_{}", source.id))
                            .build()?;
                        vqueue.set_property("max-size-buffers", 2u32);
                        vqueue.set_property("max-size-bytes", 0u32);
                        vqueue.set_property("max-size-time", 0u64);
                        vqueue.set_property_from_str("leaky", "downstream");

                        pipeline.add_many([&vshmsrc, &vgdpdepay, &vshmcaps, &vqueue])?;
                        gstreamer::Element::link_many([&vshmsrc, &vgdpdepay, &vshmcaps, &vqueue])?;
                        if let Some(ref vconv_sink) = vconv_sink_for_cb {
                            vqueue
                                .static_pad("src")
                                .ok_or("vshm_q: no src pad")?
                                .link(vconv_sink)?;
                        }
                    }

                    if has_audio {
                        let ashmsrc: gstreamer::Element = gstreamer::ElementFactory::make("shmsrc")
                            .name(format!("ashmsrc_{}", source.id))
                            .build()?;
                        ashmsrc.set_property_from_str("socket-path", &audio_sock);
                        ashmsrc.set_property("is-live", true);
                        ashmsrc.set_property("do-timestamp", false);

                        let agdpdepay: gstreamer::Element =
                            gstreamer::ElementFactory::make("gdpdepay")
                                .name(format!("agdpdepay_{}", source.id))
                                .build()?;

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

                        pipeline.add_many([&ashmsrc, &agdpdepay, &ashmcaps, &aqueue])?;
                        gstreamer::Element::link_many([&ashmsrc, &agdpdepay, &ashmcaps, &aqueue])?;
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
        })
    }

    pub fn inner(&self) -> &gstreamer::Pipeline {
        &self.inner
    }

    pub fn appsink(&self) -> &gstreamer_app::AppSink {
        &self.appsink
    }

    pub fn source_pads(&self) -> &HashMap<String, SourcePads> {
        &self.source_pads
    }

    pub fn mixer_sink_pads(&self) -> &HashMap<String, gstreamer::Pad> {
        &self.mixer_sink_pads
    }

    /// Reset the shmsrc elements for an external source after its adapter
    /// process has restarted and created fresh shmsink sockets.
    /// Sets each element to NULL then syncs it back to the pipeline state so
    /// it reconnects to the new socket.
    pub fn restart_shmsrc(&self, source_id: &str) {
        for prefix in &["vshmsrc_", "ashmsrc_"] {
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
    /// Adds missing shmsrc chains and tears down chains that are no longer
    /// present, live on a PLAYING pipeline.  This is the implementation risk
    /// called out in ADR-0013 — if it stalls the compositor, stop and treat it
    /// as a core problem to solve.
    pub fn build_shmsrc_chain(&mut self, source_id: &str, has_video: bool, has_audio: bool) {
        let current_has_video = self
            .inner
            .by_name(&format!("vshmsrc_{source_id}"))
            .is_some();
        let current_has_audio = self
            .inner
            .by_name(&format!("ashmsrc_{source_id}"))
            .is_some();

        if has_video && !current_has_video {
            if let Err(e) = self.add_video_chain(source_id) {
                eprintln!("[pipeline] add_video_chain '{source_id}': {e}");
            }
        } else if !has_video && current_has_video {
            self.remove_video_chain(source_id);
        }

        if has_audio && !current_has_audio {
            if let Err(e) = self.add_audio_chain(source_id) {
                eprintln!("[pipeline] add_audio_chain '{source_id}': {e}");
            }
        } else if !has_audio && current_has_audio {
            self.remove_audio_chain(source_id);
        }
    }

    /// Remove all shmsrc chains for a source (full teardown for permanent removal).
    pub fn teardown_shmsrc_chain(&mut self, source_id: &str) {
        self.remove_video_chain(source_id);
        self.remove_audio_chain(source_id);
    }

    // ── Dynamic chain builders / tearers ─────────────────────────────────────

    fn add_video_chain(&mut self, source_id: &str) -> Result<()> {
        let layout = self
            .source_layouts
            .get(source_id)
            .ok_or_else(|| format!("no layout for '{source_id}'"))?;

        let (video_sock, _) = runtime::shm_paths(source_id);

        let vshmsrc = make("shmsrc", &format!("vshmsrc_{source_id}"))?;
        vshmsrc.set_property_from_str("socket-path", &video_sock);
        vshmsrc.set_property("is-live", true);
        vshmsrc.set_property("do-timestamp", false);

        let vgdpdepay = make("gdpdepay", &format!("vgdpdepay_{source_id}"))?;

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
            &vshmsrc, &vgdpdepay, &vshmcaps, &vqueue, &vconv, &vdeint, &vscale, &vcaps,
        ])?;
        gstreamer::Element::link_many([&vshmsrc, &vgdpdepay, &vshmcaps, &vqueue])?;
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
        comp_sink.set_property("xpos", layout.xpos);
        comp_sink.set_property("ypos", layout.ypos);
        comp_sink.set_property("width", layout.tile_w);
        comp_sink.set_property("height", layout.tile_h);
        comp_sink.set_property("repeat-after-eos", true);

        let vcaps_src = vcaps.static_pad("src").ok_or("vcaps: no src")?;
        vcaps_src.set_offset(layout.offset_ns);
        vcaps_src.link(&comp_sink)?;

        for elem in [
            &vshmsrc, &vgdpdepay, &vshmcaps, &vqueue, &vconv, &vdeint, &vscale, &vcaps,
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
        if let Some(elem) = self.inner.by_name(&format!("vshmsrc_{source_id}")) {
            let _ = elem.set_state(gstreamer::State::Null);
        }

        // Unlink and release compositor pad.
        if let Some(comp_sink) = self.comp_sink_pads.remove(source_id) {
            if let Some(sp) = self.source_pads.get(source_id) {
                if let Some(ref vcaps_src) = sp.video_src {
                    let _ = vcaps_src.unlink(&comp_sink);
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
            format!("vshmsrc_{source_id}"),
            format!("vgdpdepay_{source_id}"),
            format!("vshmcaps_{source_id}"),
            format!("vshm_q_{source_id}"),
            format!("vconv_{source_id}"),
            format!("vdeint_{source_id}"),
            format!("vscale_{source_id}"),
            format!("vcaps_{source_id}"),
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

        let ashmsrc = make("shmsrc", &format!("ashmsrc_{source_id}"))?;
        ashmsrc.set_property_from_str("socket-path", &audio_sock);
        ashmsrc.set_property("is-live", true);
        ashmsrc.set_property("do-timestamp", false);

        let agdpdepay = make("gdpdepay", &format!("agdpdepay_{source_id}"))?;

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
            &ashmsrc, &agdpdepay, &ashmcaps, &aqueue, &aconv, &aresamp, &alevel, &acaps,
        ])?;
        gstreamer::Element::link_many([&ashmsrc, &agdpdepay, &ashmcaps, &aqueue])?;
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

        for elem in [
            &ashmsrc, &agdpdepay, &ashmcaps, &aqueue, &aconv, &aresamp, &alevel, &acaps,
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
        if let Some(elem) = self.inner.by_name(&format!("ashmsrc_{source_id}")) {
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
            format!("ashmsrc_{source_id}"),
            format!("agdpdepay_{source_id}"),
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
