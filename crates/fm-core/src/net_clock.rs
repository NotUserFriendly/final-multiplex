//! GstNetTimeProvider wrapper — makes the core's clock available on the
//! network so out-of-process adapters can slave to it (ADR-0005 / ADR-0027).

use gstreamer::prelude::*;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Serves a clock on localhost UDP.  Keep this alive as long as any adapter
/// is running; dropping it closes the server.
///
/// At creation time wraps `GstSystemClock` so adapters can begin syncing
/// before the pipeline exists.  After the pipeline reaches PLAYING call
/// [`switch_to_clock`] to re-bind the provider to the audio hardware clock
/// (ADR-0027).
pub struct NetClock {
    _provider: Option<gstreamer_net::NetTimeProvider>,
    /// The UDP port the provider is listening on.
    pub port: i32,
    /// Snapshot of the clock at the time the provider was last (re-)created (ns).
    pub base_time_ns: u64,
}

impl NetClock {
    /// Create a new provider backed by the system clock.
    /// Safe to call at any time after `gstreamer::init()`.
    pub fn new() -> Result<Self> {
        let clock = gstreamer::SystemClock::obtain();

        // Port 0 → OS picks a free ephemeral port.
        let provider = gstreamer_net::NetTimeProvider::new(&clock, None, 0)?;
        let port = provider.port();
        let base_time_ns = clock.time().nseconds();

        eprintln!("[net-clock] serving on UDP :{port} (system clock)");
        Ok(Self {
            _provider: Some(provider),
            port,
            base_time_ns,
        })
    }

    /// Switch the provider to `clock` (the audio hardware clock after pipeline
    /// PLAYING).  Drops the old provider first to free the UDP port, then
    /// re-binds on the same port.  The brief gap is benign — adapters'
    /// `GstNetClientClock` extrapolates during handoff and re-syncs on the
    /// next poll.
    ///
    /// Falls back gracefully: if binding fails the caller should log and
    /// continue — the system-clock provider was already dropped, so the
    /// adapters will still run on their last-known extrapolated clock.
    pub fn switch_to_clock(&mut self, clock: &gstreamer::Clock) -> Result<()> {
        let port = self.port;
        self._provider = None; // free the UDP port
        let new_provider = gstreamer_net::NetTimeProvider::new(clock, None, port)?;
        self._provider = Some(new_provider);
        self.base_time_ns = clock.time().nseconds();
        eprintln!("[net-clock] switched to audio hardware clock on UDP :{port}");
        Ok(())
    }
}
