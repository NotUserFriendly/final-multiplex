//! fm-dummy-adapter — Phase 2 test adapter (ADR-0005 / ADR-0011 / ADR-0012 / ADR-0014).
//!
//! Normal mode: produces a moving videotestsrc ball + audiotestsrc sine wave,
//! slaved to the core's GstNetTimeProvider, and writes raw frames to shmsink
//! sockets.  Used to validate the process boundary and crash-isolation before RTSP.
//!
//! **`--no-frames` mode (C9 silent-adapter test):** opens shmsink sockets and
//! emits Ready but keeps the pipeline in PAUSED — no frames ever enter the shm
//! ring buffer.  Used to confirm the core compositor does not stall when a live
//! shmsrc receives no data.
//!
//! Startup order (ADR-0014): wait for Configure on stdin → slave clock →
//! open sockets → emit Ready.  The URI in Configure is accepted but ignored.
//!
//! Launch args (defined in fm_adapter_sdk::contract::args):
//!   --clock-addr   host:port   GstNetClientClock endpoint
//!   --video-shm    path        shmsink socket path for video
//!   --audio-shm    path        shmsink socket path for audio
//!   --source-id    id          identifier echoed in telemetry
//!   --video-width  px          production resolution width  (ADR-0012)
//!   --video-height px          production resolution height (ADR-0012)
//!   --framerate    fps         frames per second
//!   --base-time    ns          core pipeline base time in nanoseconds
//!   --no-frames    (flag)      open sockets + emit Ready but never produce frames
//!
//! stdin:  line-delimited JSON fm_adapter_sdk::contract::Command
//! stdout: line-delimited JSON fm_adapter_sdk::contract::AdapterMessage

use fm_adapter_sdk::contract::{AdapterMessage, Command, OffsetPolarity, PROTOCOL_VERSION};
use fm_adapter_sdk::metrics::{IngestState, SourceMetrics, DB_FLOOR};
use gstreamer::prelude::*;
use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

fn main() {
    let args = parse_args();

    // Layer 1: signal SIGTERM to this process when the parent (supervisor) dies,
    // covering the SIGKILL-of-app case where no app-side handler can run.
    // Race: if parent died between fork and prctl, getppid() will return 1 (init
    // adopted us); exit immediately before doing anything expensive.
    #[cfg(target_os = "linux")]
    unsafe {
        libc::prctl(
            libc::PR_SET_PDEATHSIG,
            libc::SIGTERM as libc::c_ulong,
            0,
            0,
            0,
        );
        if libc::getppid() == 1 {
            std::process::exit(1);
        }
    }
    // TODO(windows): assign to a Job Object with JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE.

    gstreamer::init().expect("GStreamer init failed");

    // ── Stdin command reader — started before Configure so the channel
    // is ready when the core writes the first message.
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

    // ── Wait for Configure (ADR-0014): block until the core sends it.
    // The dummy adapter does not use the URI but must wait before proceeding.
    loop {
        match cmd_rx.recv() {
            Ok(Command::Configure { .. }) => break,
            Ok(_) => {} // ignore Play/Pause/Shutdown before Configure
            Err(_) => {
                emit(AdapterMessage::Error {
                    description: "stdin closed before Configure".to_string(),
                });
                return;
            }
        }
    }

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
        .expect("--video-shm required");
    let audio_shm = args
        .get("audio-shm")
        .cloned()
        .expect("--audio-shm required");
    let width: i32 = args
        .get("video-width")
        .and_then(|v| v.parse().ok())
        .unwrap_or(1920);
    let height: i32 = args
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
    let no_frames = args.contains_key("no-frames");

    if no_frames {
        eprintln!("[dummy-adapter] --no-frames mode: sockets open but no frames produced");
    }

    // ── Pipeline ─────────────────────────────────────────────────────────
    let pipeline = gstreamer::Pipeline::new();

    let vsrc = make("videotestsrc", "vsrc");
    let vconv = make("videoconvert", "vconv");
    let vscale = make("videoscale", "vscale");
    let vcaps = make("capsfilter", "vcaps");
    let vgdppay = make("gdppay", "vgdppay");
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
            .field("pixel-aspect-ratio", gstreamer::Fraction::new(1, 1))
            .build(),
    );
    vshmsink.set_property_from_str("socket-path", &video_shm);
    // Push ASAP — the core side presents per-PTS from the GDP headers.
    vshmsink.set_property("sync", false);
    // Don't block waiting for shmsrc to connect; drop frames until it does.
    vshmsink.set_property("wait-for-connection", false);

    pipeline
        .add_many([&vsrc, &vconv, &vscale, &vcaps, &vgdppay, &vshmsink])
        .unwrap();
    gstreamer::Element::link_many([&vsrc, &vconv, &vscale, &vcaps, &vgdppay, &vshmsink]).unwrap();

    // BUFFER probe on vcaps:src counts frames written toward shmsink.
    let frame_counter = Arc::new(AtomicU64::new(0));
    if let Some(vcaps_src) = vcaps.static_pad("src") {
        let fc = Arc::clone(&frame_counter);
        vcaps_src.add_probe(gstreamer::PadProbeType::BUFFER, move |_, _info| {
            fc.fetch_add(1, Ordering::Relaxed);
            gstreamer::PadProbeReturn::Ok
        });
    }

    let asrc = make("audiotestsrc", "asrc");
    let aconv = make("audioconvert", "aconv");
    let aresamp = make("audioresample", "aresamp");
    let acaps = make("capsfilter", "acaps");
    let agdppay = make("gdppay", "agdppay");
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
    ashmsink.set_property("sync", false);
    ashmsink.set_property("wait-for-connection", false);

    pipeline
        .add_many([&asrc, &aconv, &aresamp, &acaps, &agdppay, &ashmsink])
        .unwrap();
    gstreamer::Element::link_many([&asrc, &aconv, &aresamp, &acaps, &agdppay, &ashmsink]).unwrap();

    // Slave to the core's shared clock and align base time (ADR-0005).
    pipeline.use_clock(Some(&net_clock));
    pipeline.set_start_time(gstreamer::ClockTime::NONE);
    if base_time_ns > 0 {
        pipeline.set_base_time(gstreamer::ClockTime::from_nseconds(base_time_ns));
    }

    // PAUSED creates the shmsink sockets (READY→PAUSED opens the socket file).
    pipeline.set_state(gstreamer::State::Paused).unwrap();

    // ── Announce ready ────────────────────────────────────────────────────
    emit(AdapterMessage::Ready {
        has_video: true,
        has_audio: true,
        protocol_version: PROTOCOL_VERSION,
        offset_polarity: OffsetPolarity::PositiveOnly,
        max_offset_ms: 2000,
    });

    // ── Main loop ─────────────────────────────────────────────────────────
    let bus = pipeline.bus().unwrap();
    let mut last_metrics = Instant::now();
    let mut ingest_state = IngestState::Idle;
    let mut prev_frames: u64 = 0;

    loop {
        // Process pending commands.
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                Command::Configure { .. } => {} // ignore; already handled
                Command::Play => {
                    if no_frames {
                        eprintln!("[dummy-adapter] Play (ignored — --no-frames mode)");
                    } else {
                        pipeline.set_state(gstreamer::State::Playing).unwrap();
                        ingest_state = IngestState::Running;
                        eprintln!("[dummy-adapter] Play");
                    }
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
            let elapsed = last_metrics.elapsed().as_secs_f64();
            last_metrics = Instant::now();

            let current = frame_counter.load(Ordering::Relaxed);
            let fps_in = current.saturating_sub(prev_frames) as f64 / elapsed;
            prev_frames = current;

            emit(AdapterMessage::Metrics(SourceMetrics {
                source_id: source_id.clone(),
                fps_in,
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
