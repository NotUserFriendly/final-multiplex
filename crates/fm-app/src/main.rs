mod bridge;
mod gpu_path;
mod ui;
mod video;
#[cfg(target_os = "linux")]
mod wayland_sub;

// Layer 2: set by the SIGTERM signal handler so the iced Tick loop can
// call shutdown_all() on the main thread where it is safe to do so.
#[cfg(unix)]
pub(crate) static SIGTERM_FLAG: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn sigterm_handler(_: libc::c_int) {
    SIGTERM_FLAG.store(true, std::sync::atomic::Ordering::SeqCst);
}

fn boot() -> ui::App {
    let config_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "scene.toml".to_string());
    ui::App::init(std::path::Path::new(&config_path))
}

fn main() -> iced::Result {
    // Install SIGTERM handler before starting the iced event loop so that
    // killing the app process triggers graceful adapter teardown via Tick.
    #[cfg(unix)]
    unsafe {
        libc::signal(
            libc::SIGTERM,
            sigterm_handler as *const () as libc::sighandler_t,
        );
    }

    // Group 1 — test-run isolation (ADR-0014):
    // Reap dead orphan dirs, then refuse to launch if a live instance is
    // already running (it holds camera sessions and pollutes logs).
    fm_core::runtime::reap_orphans();
    if let Some(pid) = fm_core::runtime::another_instance_running() {
        eprintln!(
            "final-multiplex already running as PID {pid} — \
             stop it first; it holds camera sessions and pollutes logs"
        );
        std::process::exit(1);
    }
    // Create run dir; init session log (redirects stderr fd 2 to the file).
    // Print the path first so the user can find it from the terminal.
    if fm_core::runtime::ensure_dirs().is_ok() {
        eprintln!(
            "[fm-core] logging to {}",
            fm_core::runtime::session_log_path().display()
        );
        let _ = fm_core::runtime::init_session_log();
        eprintln!("[fm-core] session log open for PID {}", std::process::id());
    }

    let config_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "scene.toml".to_string());

    // Pre-read config just to size the window; the full build happens in boot().
    //
    // On Linux the dedicated Wayland subsurface (ADR-0026) activates within the
    // first couple of frames and takes over the *entire* window width for video
    // (no GPU side panel reserved — see `sub_active` in ui.rs).  Sizing the
    // initial window as if the GPU panel were permanent (compositor_w +
    // GPU_PANEL_W total width, but height computed only from compositor_w)
    // leaves the window's actual aspect ratio wider than the grid's once the
    // subsurface takes over, so every tile's per-source letterbox shows a
    // visible black bar until the user manually resizes.  Size purely off the
    // full window width on Linux so the window opens already at the right AR.
    #[cfg(target_os = "linux")]
    let initial_size = fm_core::config::load(std::path::Path::new(&config_path))
        .map(|scene| {
            let n = scene.source.len() as u32;
            let cols = scene.grid.columns.max(1).min(n.max(1));
            let rows = (n.max(1) + cols - 1) / cols;
            let ar =
                (cols as f32 * scene.grid.width as f32) / (rows as f32 * scene.grid.height as f32);
            let w = 1280.0f32 + ui::GPU_PANEL_W;
            let h = (w / ar).round() + ui::CHROME_H;
            iced::Size::new(w, h)
        })
        .unwrap_or(iced::Size::new(
            1280.0 + ui::GPU_PANEL_W,
            720.0 + ui::CHROME_H,
        ));
    #[cfg(not(target_os = "linux"))]
    let initial_size = fm_core::config::load(std::path::Path::new(&config_path))
        .map(|scene| {
            let n = scene.source.len() as u32;
            let cols = scene.grid.columns.max(1).min(n.max(1));
            let rows = (n.max(1) + cols - 1) / cols;
            let ar =
                (cols as f32 * scene.grid.width as f32) / (rows as f32 * scene.grid.height as f32);
            // Compositor portion sized at 1280 wide; GPU side panel adds
            // GPU_PANEL_W to the right for the Block 1 proof display.
            let compositor_w = 1280.0f32;
            let h = (compositor_w / ar).round() + ui::CHROME_H;
            iced::Size::new(compositor_w + ui::GPU_PANEL_W, h)
        })
        .unwrap_or(iced::Size::new(
            1280.0 + ui::GPU_PANEL_W,
            720.0 + ui::CHROME_H,
        ));

    iced::application(boot, ui::App::update, ui::App::view)
        .title("Final Multiplex")
        .subscription(ui::App::subscription)
        .window_size(initial_size)
        .exit_on_close_request(false)
        .transparent(true)
        .style(|_app, theme: &iced::Theme| iced::theme::Style {
            background_color: iced::Color::TRANSPARENT,
            text_color: theme.palette().text,
        })
        .run()
}
