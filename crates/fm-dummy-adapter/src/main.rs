//! fm-dummy-adapter — Phase 2 test adapter (ADR-0005 / ADR-0011).
//!
//! Produces a moving videotestsrc ball + audiotestsrc sine wave, slaved to
//! the core's GstNetTimeProvider, and writes raw frames to shmsink sockets.
//! Used to validate the process boundary and crash-isolation before RTSP.
//!
//! Launch args (defined in fm_adapter_sdk::contract::args):
//!   --clock-addr  host:port   GstNetClientClock endpoint
//!   --video-shm   path        shmsink socket path for video
//!   --audio-shm   path        shmsink socket path for audio
//!   --source-id   id          identifier echoed in telemetry
//!   --video-width  px         tile width
//!   --video-height px         tile height
//!   --framerate    fps        frames per second
//!   --base-time    ns         core pipeline base time in nanoseconds
//!
//! stdin:  line-delimited JSON fm_adapter_sdk::contract::Command
//! stdout: line-delimited JSON fm_adapter_sdk::contract::AdapterMessage

use fm_adapter_sdk::contract::{AdapterMessage, Command};
use fm_adapter_sdk::metrics::{IngestState, SourceMetrics, DB_FLOOR};
use gstreamer::prelude::*;
use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant};

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
    eprintln!("[dummy-adapter] syncing to clock {clock_host}:{clock_port}");
    if net_clock
        .wait_for_sync(gstreamer::ClockTime::from_seconds(5))
        .is_err()
    {
        eprintln!("[dummy-adapter] WARNING: clock sync timed out");
    }

    // ── Config ────────────────────────────────────────────────────────────
    let source_id = args
        .get("source-id")
        .map(String::as_str)
        .unwrap_or("dummy")
        .to_string();
    let video_shm = args
        .get("video-shm")
        .cloned()
        .unwrap_or_else(|| format!("/tmp/fm-video-{source_id}.sock"));
    let audio_shm = args
        .get("audio-shm")
        .cloned()
        .unwrap_or_else(|| format!("/tmp/fm-audio-{source_id}.sock"));
    let width: i32 = args
        .get("video-width")
        .and_then(|v| v.parse().ok())
        .unwrap_or(960);
    let height: i32 = args
        .get("video-height")
        .and_then(|v| v.parse().ok())
        .unwrap_or(540);
    let fps: i32 = args
        .get("framerate")
        .and_then(|v| v.parse().ok())
        .unwrap_or(30);
    let base_time_ns: u64 = args
        .get("base-time")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    // ── Pipeline ─────────────────────────────────────────────────────────
    let pipeline = gstreamer::Pipeline::new();

    let vsrc = make("videotestsrc", "vsrc");
    let vconv = make("videoconvert", "vconv");
    let vscale = make("videoscale", "vscale");
    let vcaps = make("capsfilter", "vcaps");
    let vshmsink = make("shmsink", "vshmsink");

    vsrc.set_property_from_str("pattern", "ball");
    vsrc.set_property("is-live", true);
    vcaps.set_property(
        "caps",
        &gstreamer::Caps::builder("video/x-raw")
            .field("format", "RGBA")
            .field("width", width)
            .field("height", height)
            .field("framerate", gstreamer::Fraction::new(fps, 1))
            .build(),
    );
    vshmsink.set_property_from_str("socket-path", &video_shm);
    vshmsink.set_property("sync", true);
    // Don't block waiting for shmsrc to connect; drop frames until it does.
    vshmsink.set_property("wait-for-connection", false);

    pipeline
        .add_many([&vsrc, &vconv, &vscale, &vcaps, &vshmsink])
        .unwrap();
    gstreamer::Element::link_many([&vsrc, &vconv, &vscale, &vcaps, &vshmsink]).unwrap();

    let asrc = make("audiotestsrc", "asrc");
    let aconv = make("audioconvert", "aconv");
    let aresamp = make("audioresample", "aresamp");
    let acaps = make("capsfilter", "acaps");
    let ashmsink = make("shmsink", "ashmsink");

    asrc.set_property("is-live", true);
    asrc.set_property_from_str("wave", "sine");
    acaps.set_property(
        "caps",
        &gstreamer::Caps::builder("audio/x-raw")
            .field("format", "S16LE")
            .field("rate", 48_000i32)
            .field("channels", 2i32)
            .field("layout", "interleaved")
            .build(),
    );
    ashmsink.set_property_from_str("socket-path", &audio_shm);
    ashmsink.set_property("sync", true);
    ashmsink.set_property("wait-for-connection", false);

    pipeline
        .add_many([&asrc, &aconv, &aresamp, &acaps, &ashmsink])
        .unwrap();
    gstreamer::Element::link_many([&asrc, &aconv, &aresamp, &acaps, &ashmsink]).unwrap();

    // Slave to the core's shared clock and align base time (ADR-0005).
    pipeline.use_clock(Some(&net_clock));
    pipeline.set_start_time(gstreamer::ClockTime::NONE);
    if base_time_ns > 0 {
        pipeline.set_base_time(gstreamer::ClockTime::from_nseconds(base_time_ns));
    }

    // Start paused; produce frames only after Play is received.
    pipeline.set_state(gstreamer::State::Paused).unwrap();

    // ── Announce ready ────────────────────────────────────────────────────
    emit(AdapterMessage::Ready);

    // ── Stdin command reader (separate thread) ────────────────────────────
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
                Err(e) => eprintln!("[dummy-adapter] bad command: {e} ({line:?})"),
            }
        }
    });

    // ── Main loop ─────────────────────────────────────────────────────────
    let bus = pipeline.bus().unwrap();
    let mut last_metrics = Instant::now();
    let mut ingest_state = IngestState::Idle;

    loop {
        // Process pending commands.
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                Command::Play => {
                    pipeline.set_state(gstreamer::State::Playing).unwrap();
                    ingest_state = IngestState::Running;
                    eprintln!("[dummy-adapter] Play");
                }
                Command::Pause => {
                    pipeline.set_state(gstreamer::State::Paused).unwrap();
                    ingest_state = IngestState::Idle;
                    eprintln!("[dummy-adapter] Pause");
                }
                Command::Shutdown => {
                    eprintln!("[dummy-adapter] Shutdown — exiting");
                    pipeline.set_state(gstreamer::State::Null).unwrap();
                    return;
                }
            }
        }

        // Drain bus messages.
        while let Some(msg) = bus.pop() {
            use gstreamer::MessageView;
            if let MessageView::Error(e) = msg.view() {
                let desc = e.error().to_string();
                eprintln!("[dummy-adapter] GStreamer error: {desc}");
                emit(AdapterMessage::Error { description: desc });
                pipeline.set_state(gstreamer::State::Null).unwrap();
                return;
            }
        }

        // Emit metrics ~1 Hz.
        if last_metrics.elapsed() >= Duration::from_secs(1) {
            last_metrics = Instant::now();
            emit(AdapterMessage::Metrics(SourceMetrics {
                source_id: source_id.clone(),
                fps_in: 0.0,
                fps_out: 0.0,
                dropped_frames: 0,
                offset_vs_master_ms: 0,
                state: ingest_state.clone(),
                reconnect_count: 0,
                audio_rms_db: DB_FLOOR,
                audio_peak_db: DB_FLOOR,
            }));
        }

        std::thread::sleep(Duration::from_millis(50));
    }
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
