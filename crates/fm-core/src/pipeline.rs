use crate::config::SceneConfig;
use gstreamer::prelude::*;
use std::collections::HashMap;

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

/// The in-core GStreamer pipeline (Phase 1).
///
/// Topology per source (only chains for streams confirmed by probe):
///   uridecodebin ─► videoconvert ─► deinterlace ─► videoscale ─► capsfilter(tile RGBA) ─► compositor
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
        let stream_caps: Vec<(bool, bool)> = {
            let handles: Vec<_> = scene
                .source
                .iter()
                .map(|s| {
                    let uri = s.uri.clone();
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

                pipeline.add_many([&aconv, &aresamp, &acaps])?;
                gstreamer::Element::link_many([&aconv, &aresamp, &acaps])?;

                let mix_sink = audiomixer
                    .request_pad_simple("sink_%u")
                    .ok_or("could not request audiomixer sink pad")?;

                let as_ = acaps.static_pad("src").ok_or("acaps: no src pad")?;
                as_.set_offset(offset_ns);
                as_.link(&mix_sink)?;
                mix_sink.set_property("volume", source.volume);

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

            // ── uridecodebin with dynamic pad wiring ───────────────────────
            let uri: gstreamer::Element = gstreamer::ElementFactory::make("uridecodebin")
                .name(format!("uri_{}", source.id))
                .build()?;
            uri.set_property("uri", &source.uri);
            pipeline.add(&uri)?;

            let id_for_cb = source.id.clone();

            uri.connect_pad_added(move |_src, pad| {
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
                                eprintln!("[fm-core] {id_for_cb}: video pad link failed: {e}");
                            }
                        }
                    }
                } else if media.starts_with("audio/") {
                    if let Some(ref aconv_sink) = aconv_sink_for_cb {
                        if !aconv_sink.is_linked() {
                            if let Err(e) = pad.link(aconv_sink) {
                                eprintln!("[fm-core] {id_for_cb}: audio pad link failed: {e}");
                            }
                        }
                    }
                }
            });
        }

        Ok(Self {
            inner: pipeline,
            appsink,
            source_pads,
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
}
