//! Adapter process supervisor (ADR-0005 / ADR-0012 / ADR-0013 / ADR-0014).
//!
//! Spawns each out-of-process adapter, monitors it for death, and restarts it
//! with exponential backoff.  Source-specific recovery (e.g. RTSP reconnect)
//! is the adapter's own concern; the supervisor only restarts the *process*.
//!
//! Control channel: line-delimited JSON on the adapter's stdin/stdout.
//!   core → adapter stdin:  fm_adapter_sdk::contract::Command
//!   adapter stdout → core: fm_adapter_sdk::contract::AdapterMessage
//!
//! Recovery protocol (ADR-0013): while an adapter is in the Reconnecting state
//! the frame-flow watchdog is suppressed; only total silence triggers a kill.
//!
//! Graceful stop (ADR-0013): all core-initiated kills send Shutdown first and
//! wait TEARDOWN_WINDOW_SECS for the adapter to release its source (e.g. RTSP
//! TEARDOWN) before force-killing.

use crate::adapter_resolver;
use crate::net_clock::NetClock;
use crate::runtime;
use fm_adapter_sdk::contract::{self, AdapterMessage, Command, OffsetPolarity, PROTOCOL_VERSION};
use fm_adapter_sdk::metrics::SourceMetrics;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command as StdCommand, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const BACKOFF_SECS: &[u64] = &[1, 2, 4, 8, 16, 30];
/// Adapter must be alive this long before the backoff index resets.
const HEALTHY_RUN_SECS: u64 = 60;
/// Frame-flow watchdog: restart if no frames for this long while Running and
/// not reconnecting.  Generous to cover RTSP cold-start (can be minutes).
const WATCHDOG_SECS: u64 = 120;
/// Silence watchdog: kill if the adapter emits no message of any kind for this
/// long.  Must exceed the ~1 Hz metrics cadence by a large margin.
/// 60 s: generous enough to survive GStreamer startup state-machine timing and
/// a single RTSP connect timeout (~30 s) without spurious fires.
const SILENCE_TIMEOUT_SECS: u64 = 60;
/// Wait this long for a graceful Shutdown response before force-killing.
const TEARDOWN_WINDOW_SECS: u64 = 3;
/// Grace period for StreamsChanged(false,false) events.  A full-drop event
/// is held for this long before the core chain is torn down.  This absorbs
/// EOS churn where the camera EOSes but reconnects within a second or two.
/// Must exceed the adapter's first reconnect delay (1 s) plus RTSP connect
/// time for fast cameras; 3 s matches the adapter's PAD_STABILITY_SECS.
const STREAMS_GRACE_MS: u64 = 3_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdapterState {
    Starting,
    Running,
    /// Waiting for the backoff delay to expire before the next spawn attempt.
    Restarting,
    Failed,
}

#[derive(Debug, Clone)]
pub struct AdapterStatus {
    pub state: AdapterState,
    pub latest_metrics: Option<SourceMetrics>,
    pub restart_count: u32,
    /// Stream presence reported by the adapter's `Ready` message.
    /// `None` until Ready is received.
    pub has_video: Option<bool>,
    pub has_audio: Option<bool>,
    /// Last time `fps_in > 0` was seen in a Metrics message.
    pub last_frame_at: Option<Instant>,
    /// Set when the adapter emits Reconnecting; cleared on StreamsChanged or
    /// when Metrics with fps_in > 0 arrives (recovery confirmed).
    pub is_reconnecting: bool,
    /// Updated on every message from the adapter (used by silence watchdog).
    pub last_any_msg_at: Option<Instant>,
    /// When this entry last transitioned into Running state.  Used by the frame
    /// watchdog to catch adapters that claim has_video=true but never produce a
    /// single frame (last_frame_at stays None).
    pub running_since: Option<Instant>,
    /// Offset capability declared in the adapter's Ready message (ADR-0017).
    /// None until Ready is received.
    pub offset_polarity: Option<OffsetPolarity>,
    pub max_offset_ms: Option<u32>,
    /// Whether the core pipeline currently has an active chain for this source.
    /// Updated from ui.rs after each supervisor poll.  Used by delivery watchdog.
    pub has_core_chain: bool,
    /// When adapter-producing-but-no-core-chain divergence was first observed.
    /// None means no active divergence; set by the delivery watchdog in poll().
    pub delivery_diverge_since: Option<Instant>,
}

impl AdapterStatus {
    fn new() -> Self {
        Self {
            state: AdapterState::Starting,
            latest_metrics: None,
            restart_count: 0,
            has_video: None,
            has_audio: None,
            last_frame_at: None,
            is_reconnecting: false,
            last_any_msg_at: None,
            running_since: None,
            offset_polarity: None,
            max_offset_ms: None,
            has_core_chain: false,
            delivery_diverge_since: None,
        }
    }
}

/// Arguments needed to (re-)launch one adapter process.
#[derive(Clone)]
struct LaunchSpec {
    binary: String,
    source_id: String,
    clock_addr: String,
    video_shm: String,
    audio_shm: String,
    video_width: u32,
    video_height: u32,
    framerate: u32,
    base_time_ns: u64,
    /// Source URI delivered to the adapter via Configure on stdin (ADR-0014).
    /// Never passed as argv so credentials do not appear in process listings.
    uri: Option<String>,
}

struct LiveHandle {
    child: Child,
    /// Shared with the reader thread so it can auto-send Play on Ready.
    stdin: Arc<Mutex<std::process::ChildStdin>>,
    started_at: Instant,
    /// Set when Shutdown has been sent; the process is in teardown.
    /// None means the process is running normally.
    shutdown_deadline: Option<Instant>,
}

struct PendingRestart {
    spec: LaunchSpec,
    retry_at: Instant,
    attempt: usize,
}

pub struct Supervisor {
    live: HashMap<String, LiveHandle>,
    pending: HashMap<String, PendingRestart>,
    specs: HashMap<String, LaunchSpec>,
    status: Arc<Mutex<HashMap<String, AdapterStatus>>>,
    /// Source IDs whose adapter just restarted and whose shmsrc elements in
    /// the core pipeline need to be reset.  Drained by `take_restarted()`.
    restarted: Arc<Mutex<Vec<String>>>,
    /// Topology changes signalled by StreamsChanged messages.
    /// Drained by `take_streams_changed()`.
    streams_changed: Arc<Mutex<Vec<(String, bool, bool)>>>,
    /// StreamsChanged(false,false) events held for STREAMS_GRACE_MS to absorb
    /// EOS churn.  A recovery event cancels the pending drop.
    /// Shared with reader threads via Arc so handle_msg can write to it.
    pending_streams: Arc<Mutex<HashMap<String, (bool, bool, Instant)>>>,
    /// Whether the supervisor is in the play state; new/restarted adapters
    /// that send Ready automatically receive Play when this is true.
    playing: Arc<Mutex<bool>>,
    /// Delivery watchdog timeout (ADR-0020).  0 = disabled.
    delivery_watchdog_ms: u64,
    /// Scene-level adapter dir override (ADR-0022 tier 1).  None = use the
    /// normal search path (FM_ADAPTER_DIR → XDG user dir → bundled).
    adapter_dir: Option<String>,
}

impl Supervisor {
    pub fn new() -> Self {
        runtime::reap_orphans();
        if let Err(e) = runtime::ensure_dirs() {
            eprintln!("[supervisor] WARNING: could not create runtime dirs: {e}");
        }
        adapter_resolver::ensure_user_dir();
        Self {
            live: HashMap::new(),
            pending: HashMap::new(),
            specs: HashMap::new(),
            status: Arc::new(Mutex::new(HashMap::new())),
            restarted: Arc::new(Mutex::new(Vec::new())),
            streams_changed: Arc::new(Mutex::new(Vec::new())),
            pending_streams: Arc::new(Mutex::new(HashMap::new())),
            playing: Arc::new(Mutex::new(false)),
            delivery_watchdog_ms: 30_000,
            adapter_dir: None,
        }
    }

    /// Override the delivery watchdog timeout (call after new(), before spawn()).
    pub fn set_delivery_watchdog_ms(&mut self, ms: u64) {
        self.delivery_watchdog_ms = ms;
    }

    /// Set the scene-level adapter dir override (ADR-0022 tier 1).
    /// Call after new(), before spawn().
    pub fn set_adapter_dir(&mut self, dir: Option<String>) {
        self.adapter_dir = dir;
    }

    /// Update whether the core pipeline has an active chain for `source_id`.
    /// Called from ui.rs after each poll cycle.  Clears delivery divergence
    /// tracking when a chain exists.
    pub fn update_chain_state(&self, source_id: &str, has_chain: bool) {
        let mut s = self.status.lock().unwrap();
        if let Some(entry) = s.get_mut(source_id) {
            entry.has_core_chain = has_chain;
            if has_chain {
                entry.delivery_diverge_since = None;
            }
        }
    }

    /// Clone of the shared status map; hand this to the UI.
    pub fn status_handle(&self) -> Arc<Mutex<HashMap<String, AdapterStatus>>> {
        Arc::clone(&self.status)
    }

    /// Drain source IDs that restarted since the last call.
    /// The caller should reset the corresponding shmsrc pipeline elements.
    pub fn take_restarted(&self) -> Vec<String> {
        std::mem::take(&mut *self.restarted.lock().unwrap())
    }

    /// Drain topology changes from StreamsChanged messages.
    /// Each entry is `(source_id, has_video, has_audio)`.
    /// The caller should call `Pipeline::build_shmsrc_chain` for each.
    pub fn take_streams_changed(&self) -> Vec<(String, bool, bool)> {
        std::mem::take(&mut *self.streams_changed.lock().unwrap())
    }

    /// Canonical shm socket paths for a source id.
    pub fn shm_paths(source_id: &str) -> (String, String) {
        runtime::shm_paths(source_id)
    }

    /// Initial spawn for `source_id`. Call once at startup per external source.
    pub fn spawn(
        &mut self,
        binary: &str,
        source_id: &str,
        net: &NetClock,
        prod_w: u32,
        prod_h: u32,
        fps: u32,
        uri: Option<&str>,
    ) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let resolved = adapter_resolver::resolve(binary, self.adapter_dir.as_deref())
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::NotFound, e))?;
        let binary_path = resolved.to_string_lossy().into_owned();
        eprintln!("[supervisor] adapter '{binary}' → {binary_path}");
        let (video_shm, audio_shm) = runtime::shm_paths(source_id);
        let spec = LaunchSpec {
            binary: binary_path,
            source_id: source_id.to_string(),
            clock_addr: format!("127.0.0.1:{}", net.port.max(0) as u32),
            video_shm,
            audio_shm,
            video_width: prod_w,
            video_height: prod_h,
            framerate: fps,
            base_time_ns: net.base_time_ns,
            uri: uri.map(|s| s.to_string()),
        };
        self.specs.insert(source_id.to_string(), spec.clone());
        self.do_spawn(spec, 0)
    }

    /// Poll all live processes; restart any that have died or stalled.
    /// Call periodically (e.g. from the iced Tick handler, ~every 500 ms).
    pub fn poll(&mut self) {
        let now = Instant::now();

        // ── Phase 1: collect processes to reap ───────────────────────────────
        // Includes both graceful teardowns-in-progress and unexpected deaths.
        let mut to_reap: Vec<(String, Duration, bool)> = Vec::new(); // (id, ran_for, was_teardown)

        for (id, handle) in self.live.iter_mut() {
            if let Some(deadline) = handle.shutdown_deadline {
                // Graceful teardown in progress.
                match handle.child.try_wait() {
                    Ok(Some(_)) => {
                        to_reap.push((id.clone(), handle.started_at.elapsed(), true));
                    }
                    Ok(None) if now >= deadline => {
                        eprintln!("[supervisor] '{id}': force-kill after teardown timeout");
                        let _ = handle.child.kill();
                        to_reap.push((id.clone(), handle.started_at.elapsed(), true));
                    }
                    _ => {}
                }
            } else {
                // Normal running process — check for unexpected death.
                match handle.child.try_wait() {
                    Ok(Some(status)) => {
                        eprintln!("[supervisor] '{id}' exited ({status})");
                        to_reap.push((id.clone(), handle.started_at.elapsed(), false));
                    }
                    Ok(None) => {}
                    Err(e) => eprintln!("[supervisor] '{id}' wait error: {e}"),
                }
            }
        }

        // ── Phase 2: process reaps → schedule restarts ────────────────────────
        for (id, ran_for, _was_teardown) in to_reap {
            self.live.remove(&id);

            if ran_for >= Duration::from_secs(HEALTHY_RUN_SECS) {
                eprintln!(
                    "[supervisor] '{id}' ran for {}s — resetting backoff",
                    ran_for.as_secs()
                );
                let mut s = self.status.lock().unwrap();
                if let Some(a) = s.get_mut(&id) {
                    a.restart_count = 0;
                }
            }

            let attempt = self
                .status
                .lock()
                .unwrap()
                .get(&id)
                .map(|a| a.restart_count as usize)
                .unwrap_or(0);
            let delay = BACKOFF_SECS[attempt.min(BACKOFF_SECS.len() - 1)];
            {
                let mut s = self.status.lock().unwrap();
                if let Some(a) = s.get_mut(&id) {
                    a.state = AdapterState::Restarting;
                    a.is_reconnecting = false;
                }
            }
            if let Some(spec) = self.specs.get(&id).cloned() {
                eprintln!("[supervisor] '{id}' will restart in {delay}s");
                self.pending.insert(
                    id,
                    PendingRestart {
                        spec,
                        retry_at: now + Duration::from_secs(delay),
                        attempt,
                    },
                );
            }
        }

        // ── Phase 3: watchdogs (only on normally-running processes) ───────────
        if *self.playing.lock().unwrap() {
            let mut to_shutdown: Vec<String> = Vec::new();
            {
                let s = self.status.lock().unwrap();
                for (id, handle) in &self.live {
                    if handle.shutdown_deadline.is_some() {
                        continue; // already in teardown
                    }
                    let Some(status) = s.get(id) else { continue };
                    if status.state != AdapterState::Running {
                        continue;
                    }

                    // Silence watchdog: kill if adapter emitted nothing at all.
                    if let Some(last_msg) = status.last_any_msg_at {
                        if last_msg.elapsed() > Duration::from_secs(SILENCE_TIMEOUT_SECS) {
                            eprintln!(
                                "[supervisor] '{id}' silence watchdog: no message for \
                                 {SILENCE_TIMEOUT_SECS}s — initiating graceful shutdown"
                            );
                            to_shutdown.push(id.clone());
                            continue;
                        }
                    }

                    // Frame-flow watchdog: skip adapters that are recovering.
                    if status.is_reconnecting {
                        continue;
                    }
                    if status.has_video == Some(true) {
                        let stale = match status.last_frame_at {
                            Some(t) => t.elapsed() > Duration::from_secs(WATCHDOG_SECS),
                            // Never produced a frame: fire once Running for WATCHDOG_SECS.
                            None => status.running_since.map_or(false, |t| {
                                t.elapsed() > Duration::from_secs(WATCHDOG_SECS)
                            }),
                        };
                        if stale {
                            eprintln!(
                                "[supervisor] '{id}' frame watchdog: no frames for \
                                 {WATCHDOG_SECS}s — initiating graceful shutdown"
                            );
                            to_shutdown.push(id.clone());
                        }
                    }
                }
            }

            // Delivery watchdog (ADR-0020): adapter producing but core has no
            // chain.  Needs a mutable borrow, so separate from the read pass.
            if self.delivery_watchdog_ms > 0 {
                let wdog_ms = self.delivery_watchdog_ms;
                let mut s = self.status.lock().unwrap();
                for (id, handle) in &self.live {
                    if handle.shutdown_deadline.is_some() {
                        continue;
                    }
                    let Some(status) = s.get_mut(id) else {
                        continue;
                    };
                    if status.state != AdapterState::Running || status.is_reconnecting {
                        status.delivery_diverge_since = None;
                        continue;
                    }
                    let producing = status
                        .latest_metrics
                        .as_ref()
                        .map_or(false, |m| m.fps_in > 0.0);
                    if producing && !status.has_core_chain {
                        let since = status.delivery_diverge_since.get_or_insert(now);
                        if since.elapsed() >= Duration::from_millis(wdog_ms) {
                            eprintln!(
                                "[supervisor] '{id}' delivery watchdog: producing but no \
                                 core chain for {wdog_ms}ms — force-respawn"
                            );
                            to_shutdown.push(id.clone());
                            status.delivery_diverge_since = None;
                        }
                    } else {
                        status.delivery_diverge_since = None;
                    }
                }
            }

            to_shutdown.dedup();
            for id in to_shutdown {
                self.graceful_shutdown_live(&id);
            }
        }

        // ── Phase 4: promote debounced StreamsChanged(false) events ──────────
        {
            let mut pending = self.pending_streams.lock().unwrap();
            let mut sc = self.streams_changed.lock().unwrap();
            pending.retain(|id, (_hv, _ha, received_at)| {
                if received_at.elapsed() >= Duration::from_millis(STREAMS_GRACE_MS) {
                    eprintln!(
                        "[supervisor] '{id}' StreamsChanged grace expired — tearing down chain"
                    );
                    sc.push((id.clone(), false, false));
                    false
                } else {
                    true
                }
            });
        }

        // ── Phase 5: fire expired pending restarts ───────────────────────────
        let ready: Vec<String> = self
            .pending
            .iter()
            .filter(|(_, p)| now >= p.retry_at)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ready {
            let p = self.pending.remove(&id).unwrap();
            if let Err(e) = self.do_spawn(p.spec, p.attempt + 1) {
                eprintln!("[supervisor] '{id}' respawn failed: {e}");
            }
        }
    }

    /// Send Play to all live adapters and flip the internal play flag so
    /// newly-restarted adapters auto-receive Play on their Ready message.
    pub fn send_play_all(&mut self) {
        *self.playing.lock().unwrap() = true;
        self.broadcast(Command::Play);
    }

    /// Send Pause to all live adapters.
    pub fn send_pause_all(&mut self) {
        *self.playing.lock().unwrap() = false;
        self.broadcast(Command::Pause);
    }

    /// Send Shutdown to every adapter; wait up to TEARDOWN_WINDOW_SECS per process.
    pub fn shutdown_all(&mut self) {
        self.broadcast(Command::Shutdown);
        let deadline = Instant::now() + Duration::from_secs(TEARDOWN_WINDOW_SECS);
        for (id, handle) in self.live.iter_mut() {
            loop {
                match handle.child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    _ => {
                        eprintln!("[supervisor] '{id}': force-kill after shutdown timeout");
                        let _ = handle.child.kill();
                        break;
                    }
                }
            }
        }
        self.live.clear();
        self.pending.clear();
        runtime::cleanup();
    }

    // ── Internal ─────────────────────────────────────────────────────────────

    /// Send Shutdown to a live adapter and mark it for graceful teardown.
    fn graceful_shutdown_live(&mut self, id: &str) {
        if let Some(handle) = self.live.get_mut(id) {
            let line = contract::encode_command(&Command::Shutdown);
            let _ = handle.stdin.lock().unwrap().write_all(line.as_bytes());
            handle.shutdown_deadline =
                Some(Instant::now() + Duration::from_secs(TEARDOWN_WINDOW_SECS));
        }
    }

    fn do_spawn(
        &mut self,
        spec: LaunchSpec,
        attempt: usize,
    ) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // Remove stale sockets so the adapter creates fresh ones.
        let _ = std::fs::remove_file(&spec.video_shm);
        let _ = std::fs::remove_file(&spec.audio_shm);

        use contract::args::*;
        let video_w = spec.video_width.to_string();
        let video_h = spec.video_height.to_string();
        let framerate = spec.framerate.to_string();
        let base_time = spec.base_time_ns.to_string();
        let argv: Vec<&str> = vec![
            CLOCK_ADDR,
            &spec.clock_addr,
            VIDEO_SHM,
            &spec.video_shm,
            AUDIO_SHM,
            &spec.audio_shm,
            SOURCE_ID,
            &spec.source_id,
            VIDEO_WIDTH,
            &video_w,
            VIDEO_HEIGHT,
            &video_h,
            FRAMERATE,
            &framerate,
            BASE_TIME,
            &base_time,
        ];
        let mut child = StdCommand::new(&spec.binary)
            .args(&argv)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;

        let stdin = Arc::new(Mutex::new(child.stdin.take().expect("stdin was piped")));

        // Send Configure immediately so the adapter has the URI before it tries
        // to connect to its source.  URI is never in argv (ADR-0014).
        {
            let uri = spec.uri.as_deref().unwrap_or("").to_string();
            let line = contract::encode_command(&Command::Configure { uri });
            if let Err(e) = stdin.lock().unwrap().write_all(line.as_bytes()) {
                eprintln!(
                    "[supervisor] '{}': Configure write failed: {e}",
                    spec.source_id
                );
            }
        }

        let stdout = child.stdout.take().expect("stdout was piped");

        let source_id = spec.source_id.clone();
        let status = Arc::clone(&self.status);
        let streams_changed = Arc::clone(&self.streams_changed);
        let pending_streams = Arc::clone(&self.pending_streams);
        let playing = Arc::clone(&self.playing);
        let stdin_for_reader = Arc::clone(&stdin);
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                let Ok(line) = line else { break };
                if line.trim().is_empty() {
                    continue;
                }
                match contract::decode_message(&line) {
                    Ok(msg) => handle_msg(
                        &source_id,
                        msg,
                        &status,
                        &streams_changed,
                        &pending_streams,
                        &playing,
                        &stdin_for_reader,
                    ),
                    Err(e) => {
                        eprintln!("[supervisor] '{source_id}': parse error: {e} ({line:?})")
                    }
                }
            }
            eprintln!("[supervisor] '{source_id}': stdout EOF");
        });

        // Cancel any pending debounced drop event — the process is being
        // respawned, so the Ready message will establish fresh state.
        self.pending_streams.lock().unwrap().remove(&spec.source_id);
        {
            let mut s = self.status.lock().unwrap();
            let entry = s
                .entry(spec.source_id.clone())
                .or_insert_with(AdapterStatus::new);
            entry.state = AdapterState::Starting;
            entry.has_video = None;
            entry.has_audio = None;
            entry.is_reconnecting = false;
            entry.last_frame_at = None;
            entry.last_any_msg_at = None;
            entry.running_since = None;
            entry.offset_polarity = None;
            entry.max_offset_ms = None;
            if attempt > 0 {
                entry.restart_count += 1;
            }
        }

        eprintln!(
            "[supervisor] spawned '{}' attempt={} pid={}",
            spec.source_id,
            attempt,
            child.id()
        );
        self.live.insert(
            spec.source_id.clone(),
            LiveHandle {
                child,
                stdin,
                started_at: Instant::now(),
                shutdown_deadline: None,
            },
        );
        Ok(())
    }

    fn broadcast(&mut self, cmd: Command) {
        let line = contract::encode_command(&cmd);
        for (id, handle) in self.live.iter_mut() {
            if let Err(e) = handle.stdin.lock().unwrap().write_all(line.as_bytes()) {
                eprintln!("[supervisor] '{id}': write error: {e}");
            }
        }
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        if !self.live.is_empty() {
            self.shutdown_all();
        }
    }
}

fn handle_msg(
    source_id: &str,
    msg: AdapterMessage,
    status: &Arc<Mutex<HashMap<String, AdapterStatus>>>,
    streams_changed: &Arc<Mutex<Vec<(String, bool, bool)>>>,
    pending_streams: &Arc<Mutex<HashMap<String, (bool, bool, Instant)>>>,
    playing: &Arc<Mutex<bool>>,
    stdin: &Arc<Mutex<std::process::ChildStdin>>,
) {
    let mut s = status.lock().unwrap();
    let entry = s
        .entry(source_id.to_string())
        .or_insert_with(AdapterStatus::new);

    entry.last_any_msg_at = Some(Instant::now());

    match msg {
        AdapterMessage::Ready {
            has_video,
            has_audio,
            protocol_version,
            offset_polarity,
            max_offset_ms,
        } => {
            if protocol_version != PROTOCOL_VERSION {
                eprintln!(
                    "[supervisor] '{source_id}': protocol mismatch \
                     (expected {PROTOCOL_VERSION}, got {protocol_version}) — not sending Play"
                );
                entry.state = AdapterState::Failed;
                return;
            }
            eprintln!(
                "[supervisor] '{source_id}': Ready \
                 (video={has_video} audio={has_audio} \
                 offset_polarity={offset_polarity:?} max_offset_ms={max_offset_ms})"
            );
            let is_restart = entry.restart_count > 0;
            entry.state = AdapterState::Running;
            entry.has_video = Some(has_video);
            entry.has_audio = Some(has_audio);
            entry.is_reconnecting = false;
            entry.running_since = Some(Instant::now());
            entry.offset_polarity = Some(offset_polarity);
            entry.max_offset_ms = Some(max_offset_ms);

            if *playing.lock().unwrap() {
                let line = contract::encode_command(&Command::Play);
                if let Ok(mut w) = stdin.lock() {
                    let _ = w.write_all(line.as_bytes());
                }
            }
            if is_restart {
                // Always push to streams_changed on restart — not only when caps
                // changed.  build_shmsrc_chain will tear down and rebuild the chain
                // even when has_video/audio are unchanged, which releases the
                // compositor sink pad and resets its PTS timeline (fixes the
                // ~20 s reconnect freeze; see pipeline.rs build_shmsrc_chain).
                // Also cancel any pending debounced drop event — the process
                // restarted cleanly; Ready supersedes it.
                pending_streams.lock().unwrap().remove(source_id);
                streams_changed
                    .lock()
                    .unwrap()
                    .push((source_id.to_string(), has_video, has_audio));
            }
        }

        AdapterMessage::Reconnecting { attempt } => {
            eprintln!("[supervisor] '{source_id}': Reconnecting (attempt {attempt})");
            entry.is_reconnecting = true;
            // Clear last_frame_at so the frame watchdog does not immediately
            // fire if recovery takes longer than WATCHDOG_SECS.
            entry.last_frame_at = None;
        }

        AdapterMessage::StreamsChanged {
            has_video,
            has_audio,
        } => {
            eprintln!(
                "[supervisor] '{source_id}': StreamsChanged \
                 (video={has_video} audio={has_audio})"
            );
            entry.is_reconnecting = false;
            entry.has_video = Some(has_video);
            entry.has_audio = Some(has_audio);
            if !has_video && !has_audio {
                // Hold full-drop events for STREAMS_GRACE_MS to absorb EOS churn
                // where the camera reconnects before the core needs to tear down.
                pending_streams.lock().unwrap().insert(
                    source_id.to_string(),
                    (has_video, has_audio, Instant::now()),
                );
            } else {
                // Recovery event: cancel any pending drop and apply immediately.
                pending_streams.lock().unwrap().remove(source_id);
                streams_changed
                    .lock()
                    .unwrap()
                    .push((source_id.to_string(), has_video, has_audio));
            }
        }

        AdapterMessage::Metrics(m) => {
            if m.fps_in > 0.0 {
                entry.last_frame_at = Some(Instant::now());
                // Frames flowing again means in-process recovery completed.
                entry.is_reconnecting = false;
            }
            entry.latest_metrics = Some(m);
        }

        AdapterMessage::Error { description } => {
            eprintln!("[supervisor] '{source_id}': Error: {description}");
        }
    }
}
