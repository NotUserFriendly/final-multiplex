mod bridge;
mod ui;
mod video;

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

    let config_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "scene.toml".to_string());

    // Pre-read config just to size the window; the full build happens in boot().
    let initial_size = fm_core::config::load(std::path::Path::new(&config_path))
        .map(|scene| {
            let ar = scene.grid.width as f32 / scene.grid.height as f32;
            let w = 1280.0f32;
            iced::Size::new(w, (w / ar).round() + ui::CHROME_H)
        })
        .unwrap_or(iced::Size::new(1280.0, 720.0 + ui::CHROME_H));

    iced::application(boot, ui::App::update, ui::App::view)
        .title("Final Multiplex")
        .subscription(ui::App::subscription)
        .window_size(initial_size)
        .exit_on_close_request(false)
        .run()
}
