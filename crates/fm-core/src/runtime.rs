//! Runtime file locations and lifecycle (ADR-0014).
//!
//! All ephemeral files live under a single user-private root:
//!   $XDG_RUNTIME_DIR/final-multiplex/  (if XDG_RUNTIME_DIR is set)
//!   /tmp/final-multiplex/              (fallback)
//!
//! Each core instance uses a subdirectory keyed on its PID so concurrent
//! instances coexist and orphaned directories from crashes are identifiable.
//!
//! Sockets are created with mode 0600 by GStreamer's shmsink; directories
//! are created with mode 0700 so other users cannot enumerate them.

use std::path::PathBuf;

/// Root directory for all Final Multiplex runtime files.
pub fn runtime_root() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    base.join("final-multiplex")
}

/// Per-run subdirectory for the calling process.
pub fn run_dir() -> PathBuf {
    runtime_root().join(std::process::id().to_string())
}

/// Create the runtime root and this process's run directory (mode 0700).
/// Idempotent; safe to call multiple times.
pub fn ensure_dirs() -> std::io::Result<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    let root = runtime_root();
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&root)?;
    let dir = run_dir();
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)?;
    Ok(dir)
}

/// Canonical shm socket paths for a source within this process's run dir.
pub fn shm_paths(source_id: &str) -> (String, String) {
    let dir = run_dir();
    let vid = dir
        .join(format!("{source_id}.vid.sock"))
        .to_string_lossy()
        .into_owned();
    let aud = dir
        .join(format!("{source_id}.aud.sock"))
        .to_string_lossy()
        .into_owned();
    (vid, aud)
}

/// Remove run directories left behind by crashed prior instances.
/// Only removes directories whose owning PID is confirmed dead.
/// Never touches the current process's directory or any live PID's directory.
pub fn reap_orphans() {
    let root = runtime_root();
    let Ok(entries) = std::fs::read_dir(&root) else {
        return;
    };
    let my_pid = std::process::id();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Ok(pid) = name.parse::<u32>() else {
            continue;
        };
        if pid == my_pid {
            continue;
        }
        if !pid_is_alive(pid) {
            eprintln!("[runtime] reaping orphan run dir for dead pid {pid}");
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

/// Remove this process's run directory on clean exit.
pub fn cleanup() {
    let dir = run_dir();
    if dir.exists() {
        let _ = std::fs::remove_dir_all(&dir);
    }
}

fn pid_is_alive(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}
