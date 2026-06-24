//! fm-rtsp-adapter — RTSP source adapter for Final Multiplex (Phase 2 Step 5).
//!
//! Decodes an RTSP stream (H.264/H.265/MJPEG video + AAC/G.711 audio) into raw
//! RGBA video and S16LE PCM audio delivered to shmsink sockets consumed by the
//! core's shmsrc elements.  Slaved to the core's GstNetTimeProvider.
//!
//! Pipeline (built dynamically as RTSP PLAY causes decodebin3 to add pads):
//!   rtspsrc → decodebin3 → {
//!     video: videoconvert → deinterlace → videoscale → capsfilter(RGBA, prod_res) → shmsink
//!     audio: audioconvert → audioresample → capsfilter(S16LE 48k 2ch) → shmsink
//!   }
//!
//! Reconnect: on GstMessageError the pipeline is cycled NULL → PLAYING.  Existing
//! shmsink chains are reused so sockets stay open.  After MAX_RECONNECTS the adapter
//! emits Error and exits; the supervisor restarts the process with backoff.
//!
//! Launch args:
//!   --clock-addr   host:port   GstNetClientClock endpoint
//!   --video-shm    path        shmsink socket path for video
//!   --audio-shm    path        shmsink socket path for audio
//!   --source-id    id          identifier echoed in telemetry
//!   --video-width  px          production resolution width  (ADR-0012)
//!   --video-height px          production resolution height (ADR-0012)
//!   --framerate    fps         frames per second
//!   --base-time    ns          core pipeline base time in nanoseconds
//!   --uri          rtsp://...  RTSP stream URL (required)
//!
//! stdin:  line-delimited JSON fm_adapter_sdk::contract::Command
//! stdout: line-delimited JSON fm_adapter_sdk::contract::AdapterMessage

use fm_adapter_sdk::contract::{AdapterMessage, Command, PROTOCOL_VERSION};
use fm_adapter_sdk::metrics::{IngestState, SourceMetrics, DB_FLOOR};
use gstreamer::prelude::*;
use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// After the first decoded pad appears, wait this long for additional pads
// (video + audio usually arrive within ~500 ms of each other).
const PAD_STABILITY_SECS: u64 = 3;
// Maximum in-process reconnect attempts before emitting Error and exiting.
const MAX_RECONNECTS: u32 = 8;

fn main() {
    let args = parse_args();

    gstreamer::init().expect("GStreamer init failed");

    // ── Net clock ─────────────────────────────────────────────────────────
    let clock_addr = args
        .get("clock-addr")
        .map(String::as_str)
        .unwrap_or("127.0.0.1:5637");
    let (clock_host, clock_port) = split_addr(clock_addr);
    let net_clock = gstreamer_net::NetClientClock::new(
        None,
        &clock_host,
        clock_port,
        gstreamer::ClockTime::ZERO,
    );
    eprintln!("[rtsp-adapter] syncing to clock {clock_host}:{clock_port}");
    if net_clock
        .wait_for_sync(gstreamer::ClockTime::from_seconds(5))
        .is_err()
    {
        eprintln!("[rtsp-adapter] WARNING: clock sync timed out — proceeding");
    }

    // ── Config ────────────────────────────────────────────────────────────
    let source_id = args
        .get("source-id")
        .cloned()
        .unwrap_or_else(|| "rtsp".to_string());
    let uri = match args.get("uri").filter(|u| !u.is_empty()).cloned() {
        Some(u) => u,
        None => {
            let desc = "--uri not specified".to_string();
            eprintln!("[rtsp-adapter] FATAL: {desc}");
            emit(AdapterMessage::Error { description: desc });
            std::process::exit(1);
        }
    };
    let video_shm = args
        .get("video-shm")
        .cloned()
        .unwrap_or_else(|| format!("/tmp/fm-video-{source_id}.sock"));
    let audio_shm = args
        .get("audio-shm")
        .cloned()
        .unwrap_or_else(|| format!("/tmp/fm-audio-{source_id}.sock"));
    let prod_w: i32 = args
        .get("video-width")
        .and_then(|v| v.parse().ok())
        .unwrap_or(1920);
    let prod_h: i32 = args
        .get("video-height")
        .and_then(|v| v.parse().ok())
        .unwrap_or(1080);
    let fps: i32 = args
        .get("framerate")
        .and_then(|v| v.parse().ok())
        .unwrap_or(30);
    let base_time_ns: u64 = args
        .get("base-time")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    eprintln!("[rtsp-adapter] source={source_id} uri={uri} prod={prod_w}×{prod_h}@{fps}");

    // ── Pipeline ─────────────────────────────────────────────────────────
    let pipeline = gstreamer::Pipeline::new();

    let rtspsrc = make("rtspsrc", "rtspsrc");
    rtspsrc.set_property("location", &uri);
    rtspsrc.set_property("latency", 200u32);
    // Try TCP first; many cameras sit behind NAT that blocks UDP.
    rtspsrc.set_property_from_str("protocols", "tcp+udp-mcast+udp");

    let decodebin = make("decodebin3", "decodebin3");

    pipeline.add_many([&rtspsrc, &decodebin]).unwrap();

    // Slave to core clock and align base time (ADR-0005).
    pipeline.use_clock(Some(&net_clock));
    pipeline.set_start_time(gstreamer::ClockTime::NONE);
    if base_time_ns > 0 {
        pipeline.set_base_time(gstreamer::ClockTime::from_nseconds(base_time_ns));
    }

    // ── Shared adapter state ──────────────────────────────────────────────
    // Accessed from the GStreamer streaming thread (pad-added callbacks) and
    // the main loop.
    let shared: Arc<Mutex<Shared>> = Arc::new(Mutex::new(Shared {
        video_chain: None,
        audio_chain: None,
        first_pad_at: None,
        reconnect_count: 0,
        ready_sent: false,
    }));

    // ── rtspsrc::pad-added → link to decodebin3 ───────────────────────────
    {
        let decodebin_c = decodebin.clone();
        rtspsrc.connect("pad-added", false, move |args| {
            let pad = args[1].get::<gstreamer::Pad>().unwrap();
            match decodebin_c.request_pad_simple("sink_%u") {
                Some(dec_sink) => {
                    if let Err(e) = pad.link(&dec_sink) {
                        eprintln!("[rtsp-adapter] rtspsrc→decodebin3 link error: {e}");
                    }
                }
                None => eprintln!("[rtsp-adapter] decodebin3: no sink_%u pad"),
            }
            None
        });
    }

    // ── decodebin3::pad-added → build / reuse chain, link, sync ───────────
    {
        let pipeline_c = pipeline.clone();
        let shared_c = Arc::clone(&shared);
        let video_shm_c = video_shm.clone();
        let audio_shm_c = audio_shm.clone();

        decodebin.connect("pad-added", false, move |args| {
            let pad = args[1].get::<gstreamer::Pad>().unwrap();
            let caps = pad.current_caps().unwrap_or_else(|| pad.query_caps(None));
            let media = caps
                .structure(0)
                .map(|s| s.name().to_string())
                .unwrap_or_default();

            let mut s = shared_c.lock().unwrap();

            if media.starts_with("video/") {
                if let Some(ref chain) = s.video_chain {
                    // Reconnect: re-link existing chain.
                    if !chain.sink.is_linked() {
                        if let Err(e) = pad.link(&chain.sink) {
                            eprintln!("[rtsp-adapter] video re-link: {e}");
                        } else {
                            eprintln!("[rtsp-adapter] video reconnected");
                            for elem in &chain.elements {
                                let _ = elem.sync_state_with_parent();
                            }
                        }
                    }
                } else {
                    // First time: build chain.
                    match build_video_chain(&pipeline_c, &video_shm_c, prod_w, prod_h, fps) {
                        Ok(chain) => {
                            if let Err(e) = pad.link(&chain.sink) {
                                eprintln!("[rtsp-adapter] video link: {e}");
                            } else {
                                eprintln!("[rtsp-adapter] video chain linked ({media})");
                                s.video_chain = Some(chain);
                                if s.first_pad_at.is_none() {
                                    s.first_pad_at = Some(Instant::now());
                                }
                            }
                        }
                        Err(e) => eprintln!("[rtsp-adapter] build_video_chain: {e}"),
                    }
                }
            } else if media.starts_with("audio/") {
                if let Some(ref chain) = s.audio_chain {
                    if !chain.sink.is_linked() {
                        if let Err(e) = pad.link(&chain.sink) {
                            eprintln!("[rtsp-adapter] audio re-link: {e}");
                        } else {
                            eprintln!("[rtsp-adapter] audio reconnected");
                            for elem in &chain.elements {
                                let _ = elem.sync_state_with_parent();
                            }
                        }
                    }
                } else {
                    match build_audio_chain(&pipeline_c, &audio_shm_c) {
                        Ok(chain) => {
                            if let Err(e) = pad.link(&chain.sink) {
                                eprintln!("[rtsp-adapter] audio link: {e}");
                            } else {
                                eprintln!("[rtsp-adapter] audio chain linked ({media})");
                                s.audio_chain = Some(chain);
                                if s.first_pad_at.is_none() {
                                    s.first_pad_at = Some(Instant::now());
                                }
                            }
                        }
                        Err(e) => eprintln!("[rtsp-adapter] build_audio_chain: {e}"),
                    }
                }
            }
            None
        });
    }

    // ── Stdin command reader ──────────────────────────────────────────────
    let (cmd_tx, cmd_rx) = mpsc::channel::<Command>();
    std::thread::spawn(move || {
        for line in io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            let line = line.trim().to_string();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<Command>(&line) {
                Ok(cmd) => {
                    if cmd_tx.send(cmd).is_err() {
                        break;
                    }
                }
                Err(e) => eprintln!("[rtsp-adapter] bad command: {e} ({line:?})"),
            }
        }
    });

    // ── Start pipeline ────────────────────────────────────────────────────
    // Go straight to PLAYING — rtspsrc sends RTSP PLAY and data flows.
    // Decoded pads appear shortly after via the pad-added callback above.
    if let Err(e) = pipeline.set_state(gstreamer::State::Playing) {
        let desc = format!("pipeline PLAYING failed: {e}");
        eprintln!("[rtsp-adapter] {desc}");
        emit(AdapterMessage::Error { description: desc });
        return;
    }
    eprintln!("[rtsp-adapter] pipeline PLAYING — waiting for pads");

    let bus = pipeline.bus().unwrap();
    let mut ingest_state = IngestState::Running;
    let mut last_metrics = Instant::now();

    // Max time to wait for ANY pad from RTSP before giving up and emitting Ready.
    let hard_deadline = Instant::now() + Duration::from_secs(30);

    // ── Main loop ─────────────────────────────────────────────────────────
    loop {
        // ── Stdin commands ────────────────────────────────────────────────
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                Command::Play => {
                    eprintln!("[rtsp-adapter] Play");
                    let _ = pipeline.set_state(gstreamer::State::Playing);
                    ingest_state = IngestState::Running;
                }
                Command::Pause => {
                    eprintln!("[rtsp-adapter] Pause");
                    let _ = pipeline.set_state(gstreamer::State::Paused);
                    ingest_state = IngestState::Idle;
                }
                Command::Shutdown => {
                    eprintln!("[rtsp-adapter] Shutdown — exiting");
                    let _ = pipeline.set_state(gstreamer::State::Null);
                    return;
                }
            }
        }

        // ── Bus messages ──────────────────────────────────────────────────
        while let Some(msg) = bus.pop() {
            use gstreamer::MessageView;
            match msg.view() {
                MessageView::Error(e) => {
                    let desc = format!("{} ({})", e.error(), e.debug().unwrap_or_default());
                    eprintln!("[rtsp-adapter] GStreamer error: {desc}");

                    let (count, needs_exit) = {
                        let mut s = shared.lock().unwrap();
                        s.reconnect_count += 1;
                        let count = s.reconnect_count;
                        (count, count > MAX_RECONNECTS)
                    };

                    if needs_exit {
                        emit(AdapterMessage::Error { description: desc });
                        let _ = pipeline.set_state(gstreamer::State::Null);
                        return;
                    }

                    let delay = reconnect_delay_secs(count);
                    eprintln!("[rtsp-adapter] reconnect #{count}/{MAX_RECONNECTS} in {delay}s");
                    std::thread::sleep(Duration::from_secs(delay));

                    let _ = pipeline.set_state(gstreamer::State::Null);
                    // Restore PLAYING; pad-added callbacks will re-link.
                    let _ = pipeline.set_state(gstreamer::State::Playing);
                }
                MessageView::Eos(_) => {
                    eprintln!("[rtsp-adapter] EOS — restarting");
                    let _ = pipeline.set_state(gstreamer::State::Null);
                    let _ = pipeline.set_state(gstreamer::State::Playing);
                }
                MessageView::Warning(w) => {
                    eprintln!("[rtsp-adapter] WARNING: {}", w.error());
                }
                MessageView::StateChanged(sc) => {
                    if msg
                        .src()
                        .map_or(false, |s| s == pipeline.upcast_ref::<gstreamer::Object>())
                    {
                        eprintln!("[rtsp-adapter] state: {:?} → {:?}", sc.old(), sc.current());
                    }
                }
                _ => {}
            }
        }

        // ── Ready emission ────────────────────────────────────────────────
        {
            let mut s = shared.lock().unwrap();
            if !s.ready_sent {
                let has_video = s.video_chain.is_some();
                let has_audio = s.audio_chain.is_some();
                let stability_ok = s.first_pad_at.map_or(false, |t| {
                    t.elapsed() >= Duration::from_secs(PAD_STABILITY_SECS)
                });
                let hard_deadline_passed = Instant::now() >= hard_deadline;

                if stability_ok || hard_deadline_passed {
                    eprintln!(
                        "[rtsp-adapter] Ready (video={has_video} audio={has_audio}{})",
                        if hard_deadline_passed && !stability_ok {
                            " — hard deadline"
                        } else {
                            ""
                        }
                    );
                    emit(AdapterMessage::Ready {
                        has_video,
                        has_audio,
                        protocol_version: PROTOCOL_VERSION,
                    });
                    s.ready_sent = true;
                }
            }
        }

        // ── Metrics ~1 Hz ─────────────────────────────────────────────────
        if last_metrics.elapsed() >= Duration::from_secs(1) {
            last_metrics = Instant::now();
            let rc = shared.lock().unwrap().reconnect_count;
            emit(AdapterMessage::Metrics(SourceMetrics {
                source_id: source_id.clone(),
                fps_in: 0.0,
                fps_out: 0.0,
                dropped_frames: 0,
                offset_vs_master_ms: 0,
                state: ingest_state.clone(),
                reconnect_count: rc,
                audio_rms_db: DB_FLOOR,
                audio_peak_db: DB_FLOOR,
            }));
        }

        std::thread::sleep(Duration::from_millis(50));
    }
}

// ── Shared state ──────────────────────────────────────────────────────────────

struct Shared {
    video_chain: Option<Chain>,
    audio_chain: Option<Chain>,
    /// When the first decoded pad appeared; starts the stability timer.
    first_pad_at: Option<Instant>,
    reconnect_count: u32,
    ready_sent: bool,
}

struct Chain {
    /// Sink pad of the first element — receives decoded frames from decodebin3.
    sink: gstreamer::Pad,
    /// All elements in the chain, source-to-sink order — for sync_state on reconnect.
    elements: Vec<gstreamer::Element>,
}

// ── Chain builders ────────────────────────────────────────────────────────────

fn build_video_chain(
    pipeline: &gstreamer::Pipeline,
    shm_path: &str,
    prod_w: i32,
    prod_h: i32,
    fps: i32,
) -> Result<Chain, Box<dyn std::error::Error + Send + Sync>> {
    let vconv = make("videoconvert", "vconv");
    let vdeint = make("deinterlace", "vdeint");
    let vscale = make("videoscale", "vscale");
    let vcaps = make("capsfilter", "vcaps");
    let vshmsink = make("shmsink", "vshmsink");

    vcaps.set_property(
        "caps",
        &gstreamer::Caps::builder("video/x-raw")
            .field("format", "RGBA")
            .field("width", prod_w)
            .field("height", prod_h)
            .field("framerate", gstreamer::Fraction::new(fps, 1))
            .field("pixel-aspect-ratio", gstreamer::Fraction::new(1, 1))
            .build(),
    );
    vshmsink.set_property_from_str("socket-path", shm_path);
    vshmsink.set_property("sync", true);
    vshmsink.set_property("wait-for-connection", false);

    pipeline.add_many([&vconv, &vdeint, &vscale, &vcaps, &vshmsink])?;
    gstreamer::Element::link_many([&vconv, &vdeint, &vscale, &vcaps, &vshmsink])?;

    // Sync each element to the pipeline state (which is PLAYING — live source,
    // so state changes return NO_PREROLL and complete immediately).
    for elem in [&vconv, &vdeint, &vscale, &vcaps, &vshmsink] {
        let _ = elem.sync_state_with_parent();
    }

    let sink = vconv.static_pad("sink").ok_or("vconv: no sink pad")?;
    eprintln!("[rtsp-adapter] video chain ready → {shm_path}");
    Ok(Chain {
        sink,
        elements: vec![vconv, vdeint, vscale, vcaps, vshmsink],
    })
}

fn build_audio_chain(
    pipeline: &gstreamer::Pipeline,
    shm_path: &str,
) -> Result<Chain, Box<dyn std::error::Error + Send + Sync>> {
    let aconv = make("audioconvert", "aconv");
    let aresamp = make("audioresample", "aresamp");
    let acaps = make("capsfilter", "acaps");
    let ashmsink = make("shmsink", "ashmsink");

    acaps.set_property(
        "caps",
        &gstreamer::Caps::builder("audio/x-raw")
            .field("format", "S16LE")
            .field("rate", 48_000i32)
            .field("channels", 2i32)
            .field("layout", "interleaved")
            .build(),
    );
    ashmsink.set_property_from_str("socket-path", shm_path);
    ashmsink.set_property("sync", true);
    ashmsink.set_property("wait-for-connection", false);

    pipeline.add_many([&aconv, &aresamp, &acaps, &ashmsink])?;
    gstreamer::Element::link_many([&aconv, &aresamp, &acaps, &ashmsink])?;

    for elem in [&aconv, &aresamp, &acaps, &ashmsink] {
        let _ = elem.sync_state_with_parent();
    }

    let sink = aconv.static_pad("sink").ok_or("aconv: no sink pad")?;
    eprintln!("[rtsp-adapter] audio chain ready → {shm_path}");
    Ok(Chain {
        sink,
        elements: vec![aconv, aresamp, acaps, ashmsink],
    })
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn reconnect_delay_secs(attempt: u32) -> u64 {
    const DELAYS: &[u64] = &[1, 2, 4, 8, 16, 30];
    DELAYS[(attempt as usize).saturating_sub(1).min(DELAYS.len() - 1)]
}

fn emit(msg: AdapterMessage) {
    let mut line = serde_json::to_string(&msg).expect("serialisable");
    line.push('\n');
    let out = io::stdout();
    let mut lock = out.lock();
    let _ = lock.write_all(line.as_bytes());
    let _ = lock.flush();
}

fn make(factory: &str, name: &str) -> gstreamer::Element {
    gstreamer::ElementFactory::make(factory)
        .name(name)
        .build()
        .unwrap_or_else(|_| panic!("missing GStreamer element '{factory}'"))
}

fn split_addr(addr: &str) -> (String, i32) {
    let mut parts = addr.rsplitn(2, ':');
    let port: i32 = parts.next().unwrap_or("5637").parse().unwrap_or(5637);
    let host = parts.next().unwrap_or("127.0.0.1");
    (host.to_string(), port)
}

fn parse_args() -> HashMap<String, String> {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let mut map = HashMap::new();
    let mut i = 0;
    while i < raw.len() {
        let key = raw[i].trim_start_matches('-').to_string();
        if i + 1 < raw.len() && !raw[i + 1].starts_with("--") {
            map.insert(key, raw[i + 1].clone());
            i += 2;
        } else {
            map.insert(key, String::new());
            i += 1;
        }
    }
    map
}
