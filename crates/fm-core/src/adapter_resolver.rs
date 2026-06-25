//! Adapter binary discovery (ADR-0022).
//!
//! Search order, first match wins:
//!   1. Scene config `adapter_dir` key — per-scene override.
//!   2. `FM_ADAPTER_DIR` env var — dev/power-user override.
//!   3. XDG data user dir — `$XDG_DATA_HOME/final-multiplex/adapters`
//!      (default `~/.local/share/final-multiplex/adapters`).
//!   4. Bundled dir — `adapters/` subdirectory next to the running executable.
//!
//! If `name` is already an absolute path it is used as-is (no search).

use std::path::{Path, PathBuf};

/// Resolve `name` to a full executable path.
///
/// `config_dir` is the scene-level `adapter_dir` override (tier 1); pass `None`
/// when the scene does not specify one.
pub fn resolve(name: &str, config_dir: Option<&str>) -> Result<PathBuf, String> {
    let p = Path::new(name);
    if p.is_absolute() {
        return if p.is_file() {
            Ok(p.to_path_buf())
        } else {
            Err(format!("adapter path '{name}' does not exist"))
        };
    }

    let dirs = search_dirs(config_dir);
    for dir in &dirs {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    Err(format!(
        "adapter '{}' not found; searched: {}",
        name,
        dirs.iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// Create the XDG user adapter directory if it does not already exist.
/// Called once at supervisor startup; logs but does not fail.
pub fn ensure_user_dir() {
    if let Some(dir) = user_adapter_dir() {
        if !dir.exists() {
            match std::fs::create_dir_all(&dir) {
                Ok(()) => eprintln!(
                    "[adapter_resolver] created user adapter dir: {}",
                    dir.display()
                ),
                Err(e) => eprintln!(
                    "[adapter_resolver] could not create user adapter dir {}: {e}",
                    dir.display()
                ),
            }
        }
    }
}

fn search_dirs(config_dir: Option<&str>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    // Tier 1: scene config key
    if let Some(d) = config_dir {
        dirs.push(PathBuf::from(d));
    }

    // Tier 2: FM_ADAPTER_DIR env var
    if let Ok(val) = std::env::var("FM_ADAPTER_DIR") {
        dirs.push(PathBuf::from(val));
    }

    // Tier 3: XDG user data dir
    if let Some(d) = user_adapter_dir() {
        dirs.push(d);
    }

    // Tier 4: bundled adapters/ next to the executable
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            dirs.push(exe_dir.join("adapters"));
        }
    }

    dirs
}

fn user_adapter_dir() -> Option<PathBuf> {
    let base = if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        PathBuf::from(xdg)
    } else {
        let home = std::env::var("HOME").ok()?;
        PathBuf::from(home).join(".local").join("share")
    };
    Some(base.join("final-multiplex").join("adapters"))
}
