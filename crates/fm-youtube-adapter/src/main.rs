//! fm-youtube-adapter — YouTube source adapter for Final Multiplex (Phase 4, ADR-0022).
//!
//! Resolves a YouTube watch URL to a direct media URL via yt-dlp, decodes it
//! through GStreamer (uridecodebin3), and emits frames over the unixfd transport.
//!
//! Block 2: on stream failure (Error/EOS/SIGUSR1) re-runs yt-dlp for a fresh
//! URL, restarts uridecodebin3 in-place (chains stay connected), and resumes.
//! Backoff and MAX_RECONNECTS cap the retry rate (ADR-0013/ADR-0020).
//! SIGUSR1 (Linux) forces an immediate re-resolve for deterministic testing.
//!
//! Launch args: same contract as fm-rtsp-adapter (ADR-0014, ADR-0022).
//!   --clock-addr   host:port
//!   --video-shm    path
//!   --audio-shm    path
//!   --source-id    id
//!   --video-width  px   (accepted; GPU path taps native res)
//!   --video-height px   (accepted)
//!   --framerate    fps  (accepted; native rate flows through)
//!   --base-time    ns
//!
//! stdin:  line-delimited JSON fm_adapter_sdk::contract::Command
//! stdout: line-delimited JSON fm_adapter_sdk::contract::AdapterMessage

use fm_adapter_sdk::contract::{AdapterMessage, Command, OffsetPolarity, PROTOCOL_VERSION};
use fm_adapter_sdk::metrics::{IngestState, SourceMetrics, DB_FLOOR};
use gstreamer::prelude::*;
use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const PAD_STABILITY_SECS: u64 = 3;
const MAX_RECONNECTS: u32 = 8;

// Set by SIGUSR1 handler; checked each main-loop iteration to trigger
// a forced re-resolve without waiting for a real stream failure.
#[cfg(unix)]
static FORCE_RECONNECT: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn sigusr1_handler(_: libc::c_int) {
    FORCE_RECONNECT.store(true, Ordering::Relaxed);
}

fn main() {
    let args = parse_args();

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
        libc::signal(
            libc::SIGUSR1,
            sigusr1_handler as *const () as libc::sighandler_t,
        );
    }

    gstreamer::init().expect("GStreamer init failed");

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
                Err(e) => eprintln!("[yt-adapter] bad command: {e} ({line:?})"),
            }
        }
    });

    let youtube_url = loop {
        match cmd_rx.recv() {
            Ok(Command::Configure { uri }) => break uri,
            Ok(_) => {}
            Err(_) => {
                emit(AdapterMessage::Error {
                    description: "stdin closed before Configure".to_string(),
                });
                return;
            }
        }
    };

    let source_id = args
        .get("source-id")
        .cloned()
        .unwrap_or_else(|| "youtube".to_string());
    eprintln!("[yt-adapter] source={source_id} url={youtube_url}");

    let stream_url = match resolve_ytdlp(&youtube_url) {
        Ok(u) => {
            eprintln!(
                "[yt-adapter] resolved (truncated): {}…",
                &u[..u.len().min(80)]
            );
            u
        }
        Err(e) => {
            let desc = format!("yt-dlp resolution failed: {e}");
            eprintln!("[yt-adapter] {desc}");
            emit(AdapterMessage::Error { description: desc });
            return;
        }
    };

    let clock_addr = args
        .get("clock-addr")
        .map(String::as_str)
        .unwrap_or("127.0.0.1:5637");
    let (clock_host, clock_port) = split_addr(clock_addr);
    let initial_time = gstreamer::SystemClock::obtain().time();
    let net_clock = gstreamer_net::NetClientClock::new(None, &clock_host, clock_port, initial_time);
    eprintln!("[yt-adapter] syncing to clock {clock_host}:{clock_port}");
    let sync_start = Instant::now();
    match net_clock.wait_for_sync(gstreamer::ClockTime::from_seconds(5)) {
        Ok(()) => eprintln!(
            "[yt-adapter] clock synced in {}ms",
            sync_start.elapsed().as_millis()
        ),
        Err(_) => eprintln!(
            "[yt-adapter] clock sync: net calibration incomplete ({}ms); seeded clock proceeds",
            sync_start.elapsed().as_millis()
        ),
    }

    let video_shm = args
        .get("video-shm")
        .cloned()
        .expect("--video-shm required");
    let audio_shm = args
        .get("audio-shm")
        .cloned()
        .expect("--audio-shm required");
    let base_time_ns: u64 = args
        .get("base-time")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let configured_fps: u64 = args
        .get("framerate")
        .and_then(|v| v.parse().ok())
        .unwrap_or(30)
        .max(1);

    let pipeline = gstreamer::Pipeline::new();
    let uridecodebin = make("uridecodebin3", "uridecodebin");
    uridecodebin.set_property("uri", &stream_url);

    pipeline.add(&uridecodebin).unwrap();
    pipeline.use_clock(Some(&net_clock));
    pipeline.set_start_time(gstreamer::ClockTime::NONE);
    if base_time_ns > 0 {
        pipeline.set_base_time(gstreamer::ClockTime::from_nseconds(base_time_ns));
    }

    let shared: Arc<Mutex<Shared>> = Arc::new(Mutex::new(Shared {
        video_chain: None,
        audio_chain: None,
        first_pad_at: None,
        ready_sent: false,
        post_reconnect_check_at: None,
        last_reported_caps: None,
    }));

    let reconnect_count: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));
    let reconnecting: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
    let source_frames: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));
    let output_frames: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));

    // ── pad-added: first connect and reconnect re-link ────────────────────
    {
        let pipeline_c = pipeline.clone();
        let shared_c = Arc::clone(&shared);
        let video_shm_c = video_shm.clone();
        let audio_shm_c = audio_shm.clone();
        let source_frames_c = Arc::clone(&source_frames);
        let output_frames_c = Arc::clone(&output_frames);
        let configured_fps_c = configured_fps;

        uridecodebin.connect("pad-added", false, move |args| {
            let pad = args[1].get::<gstreamer::Pad>().unwrap();
            let caps = pad.current_caps().unwrap_or_else(|| pad.query_caps(None));
            let media = caps
                .structure(0)
                .map(|s| s.name().to_string())
                .unwrap_or_default();

            let mut s = shared_c.lock().unwrap();

            // Reset stability timer on each new pad during a post-reconnect window.
            if s.post_reconnect_check_at.is_some() {
                s.post_reconnect_check_at = Some(Instant::now());
            }

            if media.starts_with("video/") {
                if let Some(ref chain) = s.video_chain {
                    if !chain.sink.is_linked() {
                        if let Err(e) = pad.link(&chain.sink) {
                            eprintln!("[yt-adapter] video re-link: {e}");
                        } else {
                            eprintln!("[yt-adapter] video reconnected");
                            for elem in &chain.elements {
                                let _ = elem.sync_state_with_parent();
                            }
                        }
                    }
                } else {
                    match build_video_chain(
                        &pipeline_c,
                        &video_shm_c,
                        Arc::clone(&source_frames_c),
                        Arc::clone(&output_frames_c),
                        configured_fps_c,
                    ) {
                        Ok(chain) => {
                            if let Err(e) = pad.link(&chain.sink) {
                                eprintln!("[yt-adapter] video link: {e}");
                            } else {
                                eprintln!("[yt-adapter] video chain linked ({media})");
                                s.video_chain = Some(chain);
                                if s.first_pad_at.is_none() {
                                    s.first_pad_at = Some(Instant::now());
                                }
                            }
                        }
                        Err(e) => eprintln!("[yt-adapter] build_video_chain: {e}"),
                    }
                }
            } else if media.starts_with("audio/") {
                if let Some(ref chain) = s.audio_chain {
                    if !chain.sink.is_linked() {
                        if let Err(e) = pad.link(&chain.sink) {
                            eprintln!("[yt-adapter] audio re-link: {e}");
                        } else {
                            eprintln!("[yt-adapter] audio reconnected");
                            for elem in &chain.elements {
                                let _ = elem.sync_state_with_parent();
                            }
                        }
                    }
                } else {
                    match build_audio_chain(&pipeline_c, &audio_shm_c) {
                        Ok(chain) => {
                            if let Err(e) = pad.link(&chain.sink) {
                                eprintln!("[yt-adapter] audio link: {e}");
                            } else {
                                eprintln!("[yt-adapter] audio chain linked ({media})");
                                s.audio_chain = Some(chain);
                                if s.first_pad_at.is_none() {
                                    s.first_pad_at = Some(Instant::now());
                                }
                            }
                        }
                        Err(e) => eprintln!("[yt-adapter] build_audio_chain: {e}"),
                    }
                }
            }
            None
        });
    }

    if let Err(e) = pipeline.set_state(gstreamer::State::Playing) {
        let desc = format!("pipeline PLAYING failed: {e}");
        eprintln!("[yt-adapter] {desc}");
        emit(AdapterMessage::Error { description: desc });
        return;
    }
    eprintln!("[yt-adapter] pipeline PLAYING — waiting for pads");

    let bus = pipeline.bus().unwrap();
    let mut ingest_state = IngestState::Running;
    let mut last_metrics = Instant::now();
    let mut prev_source_frames: u64 = 0;
    let mut prev_output_frames: u64 = 0;
    let hard_deadline = Instant::now() + Duration::from_secs(60);

    loop {
        // ── Commands ──────────────────────────────────────────────────────
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                Command::Configure { .. } => {}
                Command::Play => {
                    eprintln!("[yt-adapter] Play");
                    let _ = pipeline.set_state(gstreamer::State::Playing);
                    ingest_state = IngestState::Running;
                }
                Command::Pause => {
                    eprintln!("[yt-adapter] Pause");
                    let _ = pipeline.set_state(gstreamer::State::Paused);
                    ingest_state = IngestState::Idle;
                }
                Command::Shutdown => {
                    eprintln!("[yt-adapter] Shutdown");
                    let _ = pipeline.set_state(gstreamer::State::Null);
                    return;
                }
            }
        }

        // ── SIGUSR1 forced re-resolve (test trigger) ──────────────────────
        #[cfg(unix)]
        if FORCE_RECONNECT
            .compare_exchange(true, false, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            eprintln!("[yt-adapter] SIGUSR1 — forcing re-resolve");
            trigger_reconnect(
                &youtube_url,
                &uridecodebin,
                &shared,
                &reconnect_count,
                &reconnecting,
                true,
            );
        }

        // ── Bus messages ──────────────────────────────────────────────────
        while let Some(msg) = bus.pop() {
            use gstreamer::MessageView;
            match msg.view() {
                MessageView::Error(e) => {
                    let desc = format!("{} ({})", e.error(), e.debug().unwrap_or_default());
                    eprintln!("[yt-adapter] GStreamer error: {desc}");

                    if reconnecting
                        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                        .is_err()
                    {
                        continue;
                    }

                    let count = reconnect_count.fetch_add(1, Ordering::Relaxed) + 1;
                    if count > MAX_RECONNECTS as u64 {
                        reconnecting.store(false, Ordering::Release);
                        emit(AdapterMessage::Error { description: desc });
                        let _ = pipeline.set_state(gstreamer::State::Null);
                        return;
                    }

                    emit(AdapterMessage::Reconnecting {
                        attempt: count as u32,
                    });
                    let delay = reconnect_delay_secs(count as u32);
                    eprintln!("[yt-adapter] re-resolve #{count}/{MAX_RECONNECTS} in {delay}s");

                    spawn_reconnect_thread(
                        youtube_url.clone(),
                        uridecodebin.clone(),
                        Arc::clone(&shared),
                        Arc::clone(&reconnecting),
                        delay,
                    );
                }
                MessageView::Eos(_) => {
                    eprintln!("[yt-adapter] EOS — re-resolving for fresh URL");

                    if reconnecting
                        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                        .is_err()
                    {
                        continue;
                    }

                    let count = reconnect_count.fetch_add(1, Ordering::Relaxed) + 1;
                    if count > MAX_RECONNECTS as u64 {
                        reconnecting.store(false, Ordering::Release);
                        let desc = "max reconnects reached (EOS)".to_string();
                        eprintln!("[yt-adapter] {desc}");
                        emit(AdapterMessage::Error { description: desc });
                        let _ = pipeline.set_state(gstreamer::State::Null);
                        return;
                    }
                    emit(AdapterMessage::Reconnecting {
                        attempt: count as u32,
                    });
                    let delay = reconnect_delay_secs(count as u32);
                    eprintln!("[yt-adapter] EOS re-resolve #{count}/{MAX_RECONNECTS} in {delay}s");

                    spawn_reconnect_thread(
                        youtube_url.clone(),
                        uridecodebin.clone(),
                        Arc::clone(&shared),
                        Arc::clone(&reconnecting),
                        delay,
                    );
                }
                MessageView::Warning(w) => {
                    eprintln!("[yt-adapter] WARNING: {}", w.error());
                }
                MessageView::StateChanged(sc) => {
                    if msg
                        .src()
                        .map_or(false, |s| s == pipeline.upcast_ref::<gstreamer::Object>())
                    {
                        eprintln!("[yt-adapter] state: {:?} → {:?}", sc.old(), sc.current());
                    }
                }
                _ => {}
            }
        }

        // ── Ready (once, at startup) ───────────────────────────────────────
        let ready_to_emit: Option<(bool, bool)> = {
            let mut s = shared.lock().unwrap();
            if s.ready_sent {
                None
            } else {
                let has_video = s.video_chain.is_some();
                let has_audio = s.audio_chain.is_some();
                let stability_ok = s.first_pad_at.map_or(false, |t| {
                    t.elapsed() >= Duration::from_secs(PAD_STABILITY_SECS)
                });
                let deadline_passed = Instant::now() >= hard_deadline;
                if (stability_ok && has_video) || deadline_passed {
                    s.ready_sent = true;
                    s.last_reported_caps = Some((has_video, has_audio));
                    Some((has_video, has_audio))
                } else {
                    None
                }
            }
        };
        if let Some((has_video, has_audio)) = ready_to_emit {
            eprintln!("[yt-adapter] Ready (video={has_video} audio={has_audio})");
            emit(AdapterMessage::Ready {
                has_video,
                has_audio,
                protocol_version: PROTOCOL_VERSION,
                offset_polarity: OffsetPolarity::PositiveOnly,
                max_offset_ms: 30_000,
            });
        }

        // ── Post-reconnect StreamsChanged ─────────────────────────────────
        {
            let mut s = shared.lock().unwrap();
            if s.ready_sent {
                if let Some(reconnect_time) = s.post_reconnect_check_at {
                    if reconnect_time.elapsed() >= Duration::from_secs(PAD_STABILITY_SECS) {
                        let has_video =
                            s.video_chain.as_ref().map_or(false, |c| c.sink.is_linked());
                        let has_audio =
                            s.audio_chain.as_ref().map_or(false, |c| c.sink.is_linked());
                        let current = (has_video, has_audio);
                        if Some(current) != s.last_reported_caps {
                            eprintln!(
                                "[yt-adapter] StreamsChanged (video={has_video} audio={has_audio})"
                            );
                            emit(AdapterMessage::StreamsChanged {
                                has_video,
                                has_audio,
                            });
                            s.last_reported_caps = Some(current);
                        }
                        s.post_reconnect_check_at = None;
                    }
                }
            }
        }

        // ── Metrics ~1 Hz ─────────────────────────────────────────────────
        if last_metrics.elapsed() >= Duration::from_secs(1) {
            let elapsed = last_metrics.elapsed().as_secs_f64();
            last_metrics = Instant::now();

            let cur_src = source_frames.load(Ordering::Relaxed);
            let cur_out = output_frames.load(Ordering::Relaxed);
            let src_delta = cur_src.saturating_sub(prev_source_frames);
            let out_delta = cur_out.saturating_sub(prev_output_frames);
            prev_source_frames = cur_src;
            prev_output_frames = cur_out;

            let fps_in = src_delta as f64 / elapsed;
            let dropped = src_delta.saturating_sub(out_delta);
            let rc = reconnect_count.load(Ordering::Relaxed) as u32;

            emit(AdapterMessage::Metrics(SourceMetrics {
                source_id: source_id.clone(),
                fps_in,
                fps_out: 0.0,
                dropped_frames: dropped,
                bad_frames: 0,
                offset_vs_master_ms: 0,
                state: ingest_state.clone(),
                reconnect_count: rc,
                audio_rms_db: DB_FLOOR,
                audio_peak_db: DB_FLOOR,
                stream_drained: false,
            }));
        }

        std::thread::sleep(Duration::from_millis(50));
    }
}

// ── Re-resolution helpers ─────────────────────────────────────────────────────

/// Immediately gate and fire a reconnect (used by the SIGUSR1 path).
fn trigger_reconnect(
    youtube_url: &str,
    uridecodebin: &gstreamer::Element,
    shared: &Arc<Mutex<Shared>>,
    reconnect_count: &Arc<AtomicU64>,
    reconnecting: &Arc<AtomicBool>,
    forced: bool,
) {
    if reconnecting
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        eprintln!("[yt-adapter] re-resolve already in progress — skipping");
        return;
    }
    let count = reconnect_count.fetch_add(1, Ordering::Relaxed) + 1;
    if count > MAX_RECONNECTS as u64 {
        reconnecting.store(false, Ordering::Release);
        eprintln!("[yt-adapter] MAX_RECONNECTS reached — giving up");
        emit(AdapterMessage::Error {
            description: "max reconnects reached".to_string(),
        });
        return;
    }
    let label = if forced { "forced" } else { "auto" };
    eprintln!("[yt-adapter] {label} re-resolve #{count}/{MAX_RECONNECTS} — immediate");
    emit(AdapterMessage::Reconnecting {
        attempt: count as u32,
    });
    spawn_reconnect_thread(
        youtube_url.to_string(),
        uridecodebin.clone(),
        Arc::clone(shared),
        Arc::clone(reconnecting),
        0,
    );
}

/// Spawn the background re-resolve thread.  Waits `delay_secs`, tears down
/// uridecodebin3, re-runs yt-dlp for a fresh URL, then restarts the element
/// in-place.  The video/audio chains remain in the pipeline; their sink pads
/// are re-linked by the `pad-added` callback.
fn spawn_reconnect_thread(
    youtube_url: String,
    uridecodebin: gstreamer::Element,
    shared: Arc<Mutex<Shared>>,
    reconnecting: Arc<AtomicBool>,
    delay_secs: u64,
) {
    std::thread::spawn(move || {
        if delay_secs > 0 {
            std::thread::sleep(Duration::from_secs(delay_secs));
        }

        // Tear down the source element — this unlinks its dynamic pads so
        // chain.sink.is_linked() becomes false, readying the re-link path.
        let _ = uridecodebin.set_state(gstreamer::State::Null);

        // Notify the core that streams are gone while we re-resolve.
        let should_notify = {
            let mut s = shared.lock().unwrap();
            if s.ready_sent && s.last_reported_caps != Some((false, false)) {
                s.last_reported_caps = Some((false, false));
                true
            } else {
                false
            }
        };
        if should_notify {
            emit(AdapterMessage::StreamsChanged {
                has_video: false,
                has_audio: false,
            });
        }

        // Re-resolve: get a fresh signed URL from yt-dlp.
        match resolve_ytdlp(&youtube_url) {
            Ok(new_url) => {
                eprintln!(
                    "[yt-adapter] fresh URL (truncated): {}…",
                    &new_url[..new_url.len().min(80)]
                );
                uridecodebin.set_property("uri", &new_url);
            }
            Err(e) => {
                eprintln!("[yt-adapter] re-resolve failed: {e} — retrying with same URL");
                // Leave the existing URI; the element will re-attempt on restart.
            }
        }

        // Restart the source; pad-added re-links the chains.
        let _ = uridecodebin.sync_state_with_parent();
        shared.lock().unwrap().post_reconnect_check_at = Some(Instant::now());
        reconnecting.store(false, Ordering::Release);
    });
}

// ── yt-dlp resolution ─────────────────────────────────────────────────────────

/// Resolve a YouTube watch URL to a direct stream URL via yt-dlp.
///
/// Prefers YouTube format 18 (360p mp4, muxed) or 22 (720p mp4, muxed), then
/// any muxed format, then absolute best.  Takes only the first output line so
/// GStreamer receives a single URI regardless of format split.
fn resolve_ytdlp(youtube_url: &str) -> Result<String, String> {
    let output = std::process::Command::new("yt-dlp")
        .args([
            "--no-playlist",
            "-f",
            "18/22/best[vcodec!=none][acodec!=none]/best",
            "-g",
            "--no-warnings",
            youtube_url,
        ])
        .output()
        .map_err(|e| format!("yt-dlp not found or could not launch: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("exit {}: {}", output.status, stderr.trim()));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let url = stdout
        .lines()
        .next()
        .ok_or("yt-dlp produced no output")?
        .trim()
        .to_string();

    if url.is_empty() {
        return Err("yt-dlp produced an empty URL".to_string());
    }
    Ok(url)
}

// ── Shared state ──────────────────────────────────────────────────────────────

struct Shared {
    video_chain: Option<Chain>,
    audio_chain: Option<Chain>,
    first_pad_at: Option<Instant>,
    ready_sent: bool,
    /// Set when a reconnect completes; starts the post-reconnect stability
    /// window for StreamsChanged.  Reset by pad-added (extends window on pads).
    post_reconnect_check_at: Option<Instant>,
    last_reported_caps: Option<(bool, bool)>,
}

struct Chain {
    sink: gstreamer::Pad,
    elements: Vec<gstreamer::Element>,
}

// ── Chain builders ────────────────────────────────────────────────────────────

fn build_video_chain(
    pipeline: &gstreamer::Pipeline,
    shm_path: &str,
    source_counter: Arc<AtomicU64>,
    output_counter: Arc<AtomicU64>,
    configured_fps: u64,
) -> Result<Chain, Box<dyn std::error::Error + Send + Sync>> {
    let vconv = make("videoconvert", "vconv");
    let vdeint = make("deinterlace", "vdeint");
    let vscale = make("videoscale", "vscale");
    let vcaps = make("capsfilter", "vcaps");
    let vunixfdsink = fm_adapter_sdk::transport::make_output_sink("vunixfdsink", shm_path);

    vcaps.set_property(
        "caps",
        &gstreamer::Caps::builder("video/x-raw")
            .field("format", "RGBA")
            .field("pixel-aspect-ratio", gstreamer::Fraction::new(1, 1))
            .build(),
    );

    pipeline.add_many([&vconv, &vdeint, &vscale, &vcaps, &vunixfdsink])?;
    gstreamer::Element::link_many([&vconv, &vdeint, &vscale, &vcaps, &vunixfdsink])?;

    for elem in [&vconv, &vdeint, &vscale, &vcaps, &vunixfdsink] {
        let _ = elem.sync_state_with_parent();
    }

    let sink_pad = vconv.static_pad("sink").ok_or("vconv: no sink pad")?;

    // Throttle HTTP burst by sleeping in the GStreamer streaming-thread probe.
    // uridecodebin3 decodes progressive MP4 at 10-21× real-time; sleeping here
    // blocks the streaming thread, creating backpressure that propagates upstream
    // through the decoder to the HTTP download and limits throughput to ~fps.
    // GStreamer's identity(sync=true) cannot do this for HTTP sources because
    // non-live pipelines never anchor segment.base to the pipeline running_time,
    // so all frame running_times are "in the past" and identity releases them
    // instantly regardless of the clock.
    let frame_interval = Duration::from_millis(1000 / configured_fps);
    let last_frame = std::sync::Arc::new(std::sync::Mutex::new(
        Instant::now()
            .checked_sub(frame_interval)
            .unwrap_or_else(Instant::now),
    ));
    let last_frame_c = std::sync::Arc::clone(&last_frame);
    sink_pad.add_probe(gstreamer::PadProbeType::BUFFER, move |_, _| {
        source_counter.fetch_add(1, Ordering::Relaxed);
        let remaining = {
            let last = last_frame_c.lock().unwrap();
            frame_interval.saturating_sub(last.elapsed())
        };
        if !remaining.is_zero() {
            std::thread::sleep(remaining);
        }
        *last_frame_c.lock().unwrap() = Instant::now();
        gstreamer::PadProbeReturn::Ok
    });

    if let Some(vcaps_src) = vcaps.static_pad("src") {
        vcaps_src.add_probe(gstreamer::PadProbeType::BUFFER, move |_, _| {
            output_counter.fetch_add(1, Ordering::Relaxed);
            gstreamer::PadProbeReturn::Ok
        });
    }

    eprintln!("[yt-adapter] video chain ready → {shm_path}");
    Ok(Chain {
        sink: sink_pad,
        elements: vec![vconv, vdeint, vscale, vcaps, vunixfdsink],
    })
}

fn build_audio_chain(
    pipeline: &gstreamer::Pipeline,
    shm_path: &str,
) -> Result<Chain, Box<dyn std::error::Error + Send + Sync>> {
    let aconv = make("audioconvert", "aconv");
    let aresamp = make("audioresample", "aresamp");
    let acaps = make("capsfilter", "acaps");
    let aunixfdsink = fm_adapter_sdk::transport::make_output_sink("aunixfdsink", shm_path);

    acaps.set_property(
        "caps",
        &gstreamer::Caps::builder("audio/x-raw")
            .field("format", "S16LE")
            .field("rate", 48_000i32)
            .field("channels", 2i32)
            .field("layout", "interleaved")
            .build(),
    );

    pipeline.add_many([&aconv, &aresamp, &acaps, &aunixfdsink])?;
    gstreamer::Element::link_many([&aconv, &aresamp, &acaps, &aunixfdsink])?;

    for elem in [&aconv, &aresamp, &acaps, &aunixfdsink] {
        let _ = elem.sync_state_with_parent();
    }

    let sink = aconv.static_pad("sink").ok_or("aconv: no sink pad")?;

    // Throttle audio burst: sleep in the streaming-thread probe on aunixfdsink.sink.
    // The probe is placed AFTER acaps (which forces S16LE 48 kHz 2ch), so the format
    // is known and duration can be derived exactly from buffer size without calling
    // b.duration() — avdec_aac leaves that field unset (GST_CLOCK_TIME_NONE), which
    // would silently skip the sleep and let the entire clip burst in seconds.
    // S16LE 48 kHz 2ch: 2 bytes × 2 channels × 48 000 Hz = 192 000 bytes/s.
    let last_audio = Arc::new(Mutex::new(Instant::now()));
    let last_audio_c = Arc::clone(&last_audio);
    let aunixfdsink_sink = aunixfdsink
        .static_pad("sink")
        .ok_or("aunixfdsink: no sink pad")?;
    aunixfdsink_sink.add_probe(gstreamer::PadProbeType::BUFFER, move |_, info| {
        if let Some(buf) = info.buffer() {
            let dur = Duration::from_nanos(buf.size() as u64 * 1_000_000_000 / 192_000);
            let remaining = {
                let last = last_audio_c.lock().unwrap();
                dur.saturating_sub(last.elapsed())
            };
            if !remaining.is_zero() {
                std::thread::sleep(remaining);
            }
            *last_audio_c.lock().unwrap() = Instant::now();
        }
        gstreamer::PadProbeReturn::Ok
    });

    eprintln!("[yt-adapter] audio chain ready → {shm_path}");
    Ok(Chain {
        sink,
        elements: vec![aconv, aresamp, acaps, aunixfdsink],
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
