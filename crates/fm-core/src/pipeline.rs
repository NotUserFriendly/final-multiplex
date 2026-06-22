use std::collections::HashMap;
use gstreamer::prelude::*;
use crate::config::SceneConfig;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Source-side pad references for a single source.
///
/// `gst_pad_set_offset` only works reliably on **source** pads, so we store
/// the `capsfilter:src` pads that feed the compositor and audiomixer rather
/// than the mixer sink pads themselves (ADR-0004).
pub struct SourcePads {
    /// `vcaps_{id}:src` — the source pad feeding `compositor:sink_N`.
    pub video_src: gstreamer::Pad,
    /// `acaps_{id}:src` — the source pad feeding `audiomixer:sink_N`.
    pub audio_src: gstreamer::Pad,
}

/// The in-core GStreamer pipeline (Phase 1).
///
/// Topology per source:
///   uridecodebin ─► videoconvert ─► videoscale ─► capsfilter(tile RGBA) ─► compositor
///                ─► audioconvert ─► audioresample ─► capsfilter(48k/2ch) ─► audiomixer
///
/// compositor ─► capsfilter(output RGBA) ─► appsink  (→ UI bridge, ADR-0006)
/// audiomixer ─► audioconvert ─► audioresample ─► autoaudiosink
pub struct Pipeline {
    inner: gstreamer::Pipeline,
    appsink: gstreamer_app::AppSink,
    source_pads: HashMap<String, SourcePads>,
}

impl Pipeline {
    pub fn build(scene: &SceneConfig) -> Result<Self> {
        gstreamer::init()?;

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
            .build();

        let comp_capsfilter: gstreamer::Element =
            gstreamer::ElementFactory::make("capsfilter")
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
        let audiomixer: gstreamer::Element =
            gstreamer::ElementFactory::make("audiomixer")
                .name("audiomixer")
                .build()?;
        let aconv_out: gstreamer::Element =
            gstreamer::ElementFactory::make("audioconvert")
                .name("aconv_out")
                .build()?;
        let aresamp_out: gstreamer::Element =
            gstreamer::ElementFactory::make("audioresample")
                .name("aresamp_out")
                .build()?;
        let autoaudiosink: gstreamer::Element =
            gstreamer::ElementFactory::make("autoaudiosink")
                .name("autoaudiosink")
                .build()?;

        pipeline.add(&compositor)?;
        pipeline.add(&comp_capsfilter)?;
        pipeline.add(&appsink)?;
        pipeline.add(&audiomixer)?;
        pipeline.add(&aconv_out)?;
        pipeline.add(&aresamp_out)?;
        pipeline.add(&autoaudiosink)?;

        compositor.link(&comp_capsfilter)?;
        comp_capsfilter.link(&appsink)?;
        gstreamer::Element::link_many([&audiomixer, &aconv_out, &aresamp_out, &autoaudiosink])?;

        // ── Per-source elements ────────────────────────────────────────────
        let n = scene.source.len() as u32;
        if n == 0 {
            return Err("scene.toml has no [[source]] entries".into());
        }
        let cols = scene.grid.columns.max(1).min(n);
        let rows = (n + cols - 1) / cols;
        let tile_w = (scene.grid.width / cols) as i32;
        let tile_h = (scene.grid.height / rows) as i32;

        let mut source_pads: HashMap<String, SourcePads> = HashMap::new();

        for (idx, source) in scene.source.iter().enumerate() {
            let xpos = ((idx as u32 % cols) * tile_w as u32) as i32;
            let ypos = ((idx as u32 / cols) * tile_h as u32) as i32;

            // ── Video conversion chain ─────────────────────────────────────
            let vconv: gstreamer::Element = gstreamer::ElementFactory::make("videoconvert")
                .name(format!("vconv_{}", source.id))
                .build()?;
            // deinterlace passes progressive content through unchanged and
            // converts interlaced fields to progressive frames, preventing
            // the comb-tooth stripe artefact on interlaced sources.
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
                    .build(),
            );

            // ── Audio conversion chain ─────────────────────────────────────
            let aconv: gstreamer::Element = gstreamer::ElementFactory::make("audioconvert")
                .name(format!("aconv_{}", source.id))
                .build()?;
            let aresamp: gstreamer::Element = gstreamer::ElementFactory::make("audioresample")
                .name(format!("aresamp_{}", source.id))
                .build()?;
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

            pipeline.add_many([&vconv, &vdeint, &vscale, &vcaps, &aconv, &aresamp, &acaps])?;
            gstreamer::Element::link_many([&vconv, &vdeint, &vscale, &vcaps])?;
            gstreamer::Element::link_many([&aconv, &aresamp, &acaps])?;

            // ── Compositor sink pad (tile position + looping) ──────────────
            let comp_sink = compositor
                .request_pad_simple("sink_%u")
                .ok_or("could not request compositor sink pad")?;
            comp_sink.set_property("xpos", xpos);
            comp_sink.set_property("ypos", ypos);
            comp_sink.set_property("width", tile_w);
            comp_sink.set_property("height", tile_h);
            // Repeat the last frame when a source loops (keeps the tile alive
            // during the seek-back gap produced by the EOS→seek loop strategy).
            comp_sink.set_property("repeat-after-eos", true);

            let vcaps_src = vcaps.static_pad("src").ok_or("vcaps: no src pad")?;
            vcaps_src.link(&comp_sink)?;

            // ── Audiomixer sink pad ────────────────────────────────────────
            let mix_sink = audiomixer
                .request_pad_simple("sink_%u")
                .ok_or("could not request audiomixer sink pad")?;

            let acaps_src = acaps.static_pad("src").ok_or("acaps: no src pad")?;
            acaps_src.link(&mix_sink)?;

            // Apply initial pad offset on the source pads (ms → ns, signed).
            // gst_pad_set_offset only works reliably on source pads; the
            // compositor/audiomixer sink pads are the wrong side of the link.
            let offset_ns = source.offset_ms * 1_000_000;
            vcaps_src.set_offset(offset_ns);
            acaps_src.set_offset(offset_ns);

            source_pads.insert(
                source.id.clone(),
                SourcePads {
                    video_src: vcaps_src,
                    audio_src: acaps_src,
                },
            );

            // ── uridecodebin with dynamic pad wiring ───────────────────────
            let uri: gstreamer::Element = gstreamer::ElementFactory::make("uridecodebin")
                .name(format!("uri_{}", source.id))
                .build()?;
            uri.set_property("uri", &source.uri);
            pipeline.add(&uri)?;

            // Capture static sink pads for the callback (Clone = GObject refcount bump).
            let vconv_sink = vconv.static_pad("sink").ok_or("vconv: no sink pad")?;
            let aconv_sink = aconv.static_pad("sink").ok_or("aconv: no sink pad")?;
            let id_for_cb = source.id.clone();

            uri.connect_pad_added(move |_src, pad| {
                let caps = match pad.current_caps() {
                    Some(c) => c,
                    None => pad.query_caps(None),
                };
                let Some(s) = caps.structure(0) else { return };
                let media = s.name();

                if media.starts_with("video/") && !vconv_sink.is_linked() {
                    if let Err(e) = pad.link(&vconv_sink) {
                        eprintln!("[fm-core] {id_for_cb}: video pad link failed: {e}");
                    }
                } else if media.starts_with("audio/") && !aconv_sink.is_linked() {
                    if let Err(e) = pad.link(&aconv_sink) {
                        eprintln!("[fm-core] {id_for_cb}: audio pad link failed: {e}");
                    }
                }
            });
        }

        Ok(Self { inner: pipeline, appsink, source_pads })
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
}
