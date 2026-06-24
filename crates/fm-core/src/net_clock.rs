//! GstNetTimeProvider wrapper — makes the core's clock available on the
//! network so out-of-process adapters can slave to it (ADR-0005).

use gstreamer::prelude::*;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Serves the system clock on localhost UDP.  Keep this alive as long as any
/// adapter is running; dropping it closes the server.
pub struct NetClock {
    _provider: gstreamer_net::NetTimeProvider,
    /// The UDP port the provider is listening on.
    pub port: i32,
    /// Snapshot of the system clock at creation time (ns).
    /// Passed to adapters as a shared timeline reference so buffer timestamps
    /// are comparable to the core's running time.
    pub base_time_ns: u64,
}

impl NetClock {
    /// Create a new provider.  Safe to call at any time after `gstreamer::init()`.
    ///
    /// Uses `GstSystemClock` directly rather than the pipeline clock so the
    /// provider can be created before or after the pipeline transitions to
    /// PLAYING — the pipeline's default clock IS the system clock.
    pub fn new() -> Result<Self> {
        let clock = gstreamer::SystemClock::obtain();

        // Port 0 → OS picks a free ephemeral port.
        let provider = gstreamer_net::NetTimeProvider::new(&clock, None, 0)?;
        let port = provider.port();
        let base_time_ns = clock.time().nseconds();

        eprintln!("[net-clock] serving on UDP :{port}");
        Ok(Self {
            _provider: provider,
            port,
            base_time_ns,
        })
    }
}
