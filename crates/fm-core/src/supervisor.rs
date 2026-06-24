//! Adapter process supervisor (ADR-0005 / ADR-0012).
//!
//! Spawns each out-of-process adapter, monitors it for death, and restarts it
//! with exponential backoff.  Source-specific recovery (e.g. RTSP reconnect)
//! is the adapter's own concern; the supervisor only restarts the *process*.
//!
//! Control channel: line-delimited JSON on the adapter's stdin/stdout.
//!   core → adapter stdin:  fm_adapter_sdk::contract::Command
//!   adapter stdout → core: fm_adapter_sdk::contract::AdapterMessage

use crate::net_clock::NetClock;
use fm_adapter_sdk::contract::{self, AdapterMessage, Command, PROTOCOL_VERSION};
use fm_adapter_sdk::metrics::SourceMetrics;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command as StdCommand, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const BACKOFF_SECS: &[u64] = &[1, 2, 4, 8, 16, 30];
/// Adapter must be alive this long before the backoff index resets.
const HEALTHY_RUN_SECS: u64 = 60;
/// Watchdog: restart if no frames arrive for this long while Running + playing.
/// Generous to accommodate RTSP cold-start (which can be several minutes).
const WATCHDOG_SECS: u64 = 120;

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
    /// `None` until the first frame arrives.
    pub last_frame_at: Option<Instant>,
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
    /// Optional source URI forwarded as `--uri` (e.g. `rtsp://...`).
    uri: Option<String>,
}

struct LiveHandle {
    child: Child,
    /// Shared with the reader thread so it can auto-send Play on Ready.
    stdin: Arc<Mutex<std::process::ChildStdin>>,
    started_at: Instant,
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
    /// Whether the supervisor is in the play state; new/restarted adapters
    /// that send Ready automatically receive Play when this is true.
    playing: Arc<Mutex<bool>>,
}

impl Supervisor {
    pub fn new() -> Self {
        Self {
            live: HashMap::new(),
            pending: HashMap::new(),
            specs: HashMap::new(),
            status: Arc::new(Mutex::new(HashMap::new())),
            restarted: Arc::new(Mutex::new(Vec::new())),
            playing: Arc::new(Mutex::new(false)),
        }
    }

    /// Clone of the shared status map; hand this to the UI.
    pub fn status_handle(&self) -> Arc<Mutex<HashMap<String, AdapterStatus>>> {
        Arc::clone(&self.status)
    }

    /// Drain and return source IDs that restarted since the last call.
    /// The caller should reset the corresponding shmsrc pipeline elements.
    pub fn take_restarted(&self) -> Vec<String> {
        std::mem::take(&mut *self.restarted.lock().unwrap())
    }

    /// Canonical shm socket paths for a source id.
    pub fn shm_paths(source_id: &str) -> (String, String) {
        (
            format!("/tmp/fm-video-{source_id}.sock"),
            format!("/tmp/fm-audio-{source_id}.sock"),
        )
    }

    /// Initial spawn for `source_id`. Call once at startup per external source.
    /// `prod_w` / `prod_h` are the production resolution the adapter should
    /// produce at (typically the full grid output resolution — ADR-0012).
    /// `uri` is forwarded as `--uri` to adapters that use it (e.g. RTSP).
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
        let (video_shm, audio_shm) = Self::shm_paths(source_id);
        let spec = LaunchSpec {
            binary: binary.to_string(),
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
        // 1. Check live processes for death.
        let mut died: Vec<(String, Duration)> = Vec::new();
        for (id, handle) in self.live.iter_mut() {
            match handle.child.try_wait() {
                Ok(Some(status)) => {
                    let ran_for = handle.started_at.elapsed();
                    eprintln!("[supervisor] '{id}' exited ({status})");
                    died.push((id.clone(), ran_for));
                }
                Ok(None) => {}
                Err(e) => eprintln!("[supervisor] '{id}' wait error: {e}"),
            }
        }
        for (id, ran_for) in died {
            self.live.remove(&id);

            // Reset backoff if the adapter ran healthily — don't penalise a
            // source that had one bad moment after a long successful run.
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
                }
            }
            if let Some(spec) = self.specs.get(&id).cloned() {
                eprintln!("[supervisor] '{id}' will restart in {delay}s");
                self.pending.insert(
                    id,
                    PendingRestart {
                        spec,
                        retry_at: Instant::now() + Duration::from_secs(delay),
                        attempt,
                    },
                );
            }
        }

        // 2. Frame-flow watchdog: restart adapters that are alive but stalled.
        if *self.playing.lock().unwrap() {
            let mut stalled: Vec<String> = Vec::new();
            {
                let s = self.status.lock().unwrap();
                for (id, handle) in &mut self.live {
                    let _ = handle; // keep borrow checker happy
                    if let Some(status) = s.get(id) {
                        if status.state == AdapterState::Running {
                            // Only watchdog if we've seen at least one frame
                            // (cold-start before first frame is not a stall).
                            if let Some(last_frame) = status.last_frame_at {
                                if last_frame.elapsed() > Duration::from_secs(WATCHDOG_SECS) {
                                    stalled.push(id.clone());
                                }
                            }
                        }
                    }
                }
            }
            for id in stalled {
                eprintln!("[supervisor] '{id}' watchdog: no frames for {WATCHDOG_SECS}s — killing");
                if let Some(handle) = self.live.get_mut(&id) {
                    let _ = handle.child.kill();
                }
            }
        }

        // 3. Fire any expired pending restarts.
        let now = Instant::now();
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

    /// Send Shutdown to every adapter and wait up to 2 s per process.
    pub fn shutdown_all(&mut self) {
        self.broadcast(Command::Shutdown);
        let deadline = Instant::now() + Duration::from_secs(2);
        for (id, handle) in self.live.iter_mut() {
            loop {
                match handle.child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    _ => {
                        eprintln!("[supervisor] '{id}': kill after shutdown timeout");
                        let _ = handle.child.kill();
                        break;
                    }
                }
            }
        }
        self.live.clear();
        self.pending.clear();
    }

    // ── Internal ─────────────────────────────────────────────────────────────

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
        let mut argv: Vec<&str> = vec![
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
        if let Some(ref u) = spec.uri {
            argv.push(URI);
            argv.push(u.as_str());
        }
        let mut child = StdCommand::new(&spec.binary)
            .args(&argv)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;

        let stdin = Arc::new(Mutex::new(child.stdin.take().expect("stdin was piped")));
        let stdout = child.stdout.take().expect("stdout was piped");

        let source_id = spec.source_id.clone();
        let status = Arc::clone(&self.status);
        let restarted = Arc::clone(&self.restarted);
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
                        &restarted,
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

        {
            let mut s = self.status.lock().unwrap();
            let entry = s
                .entry(spec.source_id.clone())
                .or_insert_with(AdapterStatus::new);
            entry.state = AdapterState::Starting;
            entry.has_video = None;
            entry.has_audio = None;
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

fn handle_msg(
    source_id: &str,
    msg: AdapterMessage,
    status: &Arc<Mutex<HashMap<String, AdapterStatus>>>,
    restarted: &Arc<Mutex<Vec<String>>>,
    playing: &Arc<Mutex<bool>>,
    stdin: &Arc<Mutex<std::process::ChildStdin>>,
) {
    let mut s = status.lock().unwrap();
    let entry = s
        .entry(source_id.to_string())
        .or_insert_with(AdapterStatus::new);
    match msg {
        AdapterMessage::Ready {
            has_video,
            has_audio,
            protocol_version,
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
                 (video={has_video} audio={has_audio})"
            );
            let is_restart = entry.restart_count > 0;
            entry.state = AdapterState::Running;
            entry.has_video = Some(has_video);
            entry.has_audio = Some(has_audio);

            // Auto-send Play if the pipeline is playing.
            if *playing.lock().unwrap() {
                let line = contract::encode_command(&Command::Play);
                if let Ok(mut w) = stdin.lock() {
                    let _ = w.write_all(line.as_bytes());
                }
            }
            // Signal shmsrc reset only on actual restarts, not initial startup.
            if is_restart {
                restarted.lock().unwrap().push(source_id.to_string());
            }
        }
        AdapterMessage::Metrics(m) => {
            if m.fps_in > 0.0 {
                entry.last_frame_at = Some(Instant::now());
            }
            entry.latest_metrics = Some(m);
        }
        AdapterMessage::Error { description } => {
            eprintln!("[supervisor] '{source_id}': Error: {description}");
        }
    }
}
