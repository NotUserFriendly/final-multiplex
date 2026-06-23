//! Adapter process supervisor (ADR-0005).
//!
//! Spawns each out-of-process adapter, monitors it for death, and restarts it
//! with exponential backoff.  Source-specific recovery (e.g. RTSP reconnect)
//! is the adapter's own concern; the supervisor only restarts the *process*.
//!
//! Control channel: line-delimited JSON on the adapter's stdin/stdout.
//!   core → adapter stdin:  fm_adapter_sdk::contract::Command
//!   adapter stdout → core: fm_adapter_sdk::contract::AdapterMessage

use crate::net_clock::NetClock;
use fm_adapter_sdk::contract::{self, AdapterMessage, Command};
use fm_adapter_sdk::metrics::SourceMetrics;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command as StdCommand, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const BACKOFF_SECS: &[u64] = &[1, 2, 4, 8, 16, 30];

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
}

struct LiveHandle {
    child: Child,
    stdin: std::process::ChildStdin,
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
}

impl Supervisor {
    pub fn new() -> Self {
        Self {
            live: HashMap::new(),
            pending: HashMap::new(),
            specs: HashMap::new(),
            status: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Clone of the shared status map; hand this to the UI.
    pub fn status_handle(&self) -> Arc<Mutex<HashMap<String, AdapterStatus>>> {
        Arc::clone(&self.status)
    }

    /// Canonical shm socket paths for a source id.
    pub fn shm_paths(source_id: &str) -> (String, String) {
        (
            format!("/tmp/fm-video-{source_id}.sock"),
            format!("/tmp/fm-audio-{source_id}.sock"),
        )
    }

    /// Initial spawn for `source_id`. Call once at startup per external source.
    pub fn spawn(
        &mut self,
        binary: &str,
        source_id: &str,
        net: &NetClock,
        tile_w: u32,
        tile_h: u32,
        fps: u32,
    ) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let (video_shm, audio_shm) = Self::shm_paths(source_id);
        let spec = LaunchSpec {
            binary: binary.to_string(),
            source_id: source_id.to_string(),
            clock_addr: format!("127.0.0.1:{}", net.port.max(0) as u32),
            video_shm,
            audio_shm,
            video_width: tile_w,
            video_height: tile_h,
            framerate: fps,
            base_time_ns: net.base_time_ns,
        };
        self.specs.insert(source_id.to_string(), spec.clone());
        self.do_spawn(spec, 0)
    }

    /// Poll all live processes; restart any that have died.
    /// Call periodically (e.g. from the iced Tick handler, ~every 500 ms).
    pub fn poll(&mut self) {
        // 1. Check live processes for death.
        let mut died: Vec<String> = Vec::new();
        for (id, handle) in self.live.iter_mut() {
            match handle.child.try_wait() {
                Ok(Some(status)) => {
                    eprintln!("[supervisor] '{id}' exited ({status})");
                    died.push(id.clone());
                }
                Ok(None) => {}
                Err(e) => eprintln!("[supervisor] '{id}' wait error: {e}"),
            }
        }
        for id in died {
            self.live.remove(&id);
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

        // 2. Fire any expired pending restarts.
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

    /// Send Play to all live adapters.
    pub fn send_play_all(&mut self) {
        self.broadcast(Command::Play);
    }

    /// Send Pause to all live adapters.
    pub fn send_pause_all(&mut self) {
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
        use contract::args::*;
        let mut child = StdCommand::new(&spec.binary)
            .args([
                CLOCK_ADDR,
                &spec.clock_addr,
                VIDEO_SHM,
                &spec.video_shm,
                AUDIO_SHM,
                &spec.audio_shm,
                SOURCE_ID,
                &spec.source_id,
                VIDEO_WIDTH,
                &spec.video_width.to_string(),
                VIDEO_HEIGHT,
                &spec.video_height.to_string(),
                FRAMERATE,
                &spec.framerate.to_string(),
                "--base-time",
                &spec.base_time_ns.to_string(),
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;

        let stdin = child.stdin.take().expect("stdin was piped");
        let stdout = child.stdout.take().expect("stdout was piped");

        let source_id = spec.source_id.clone();
        let status = Arc::clone(&self.status);
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                let Ok(line) = line else { break };
                if line.trim().is_empty() {
                    continue;
                }
                match contract::decode_message(&line) {
                    Ok(msg) => handle_msg(&source_id, msg, &status),
                    Err(e) => {
                        eprintln!("[supervisor] '{source_id}': parse error: {e} ({line:?})")
                    }
                }
            }
            eprintln!("[supervisor] '{source_id}': stdout EOF");
        });

        {
            let mut s = self.status.lock().unwrap();
            let entry = s.entry(spec.source_id.clone()).or_insert(AdapterStatus {
                state: AdapterState::Starting,
                latest_metrics: None,
                restart_count: 0,
            });
            entry.state = AdapterState::Starting;
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
        self.live
            .insert(spec.source_id.clone(), LiveHandle { child, stdin });
        Ok(())
    }

    fn broadcast(&mut self, cmd: Command) {
        let line = contract::encode_command(&cmd);
        for (id, handle) in self.live.iter_mut() {
            if let Err(e) = handle.stdin.write_all(line.as_bytes()) {
                eprintln!("[supervisor] '{id}': write error: {e}");
            }
        }
    }
}

fn handle_msg(
    source_id: &str,
    msg: AdapterMessage,
    status: &Arc<Mutex<HashMap<String, AdapterStatus>>>,
) {
    let mut s = status.lock().unwrap();
    let entry = s.entry(source_id.to_string()).or_insert(AdapterStatus {
        state: AdapterState::Starting,
        latest_metrics: None,
        restart_count: 0,
    });
    match msg {
        AdapterMessage::Ready => {
            eprintln!("[supervisor] '{source_id}': Ready");
            entry.state = AdapterState::Running;
        }
        AdapterMessage::Metrics(m) => {
            entry.latest_metrics = Some(m);
        }
        AdapterMessage::Error { description } => {
            eprintln!("[supervisor] '{source_id}': Error: {description}");
        }
    }
}
