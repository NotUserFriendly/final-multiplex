//! GstNetTimeProvider wrapper — makes the core's pipeline clock available on
//! the network so out-of-process adapters can slave to it (ADR-0005).

use gstreamer::prelude::*;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Serves the pipeline clock on localhost UDP.  Keep this alive as long as any
/// adapter is running; dropping it closes the server.
pub struct NetClock {
    _provider: gstreamer_net::NetTimeProvider,
    /// The UDP port the provider is listening on (chosen by the OS when
    /// `NetTimeProvider::new` is called with port 0).
    pub port: i32,
    /// The base time the core's pipeline was started with, in nanoseconds.
    /// Passed to each adapter so they can set the same base time and produce
    /// buffers with timestamps that are directly comparable to the core's
    /// running time.
    pub base_time_ns: u64,
}

impl NetClock {
    /// Create a new provider bound to the pipeline's current clock.
    ///
    /// Must be called after the pipeline has been set to at least PAUSED (so
    /// the pipeline clock is available) but before going to PLAYING.
    pub fn new(pipeline: &gstreamer::Pipeline) -> Result<Self> {
        let clock = pipeline
            .clock()
            .ok_or("pipeline has no clock — call after PAUSED")?;

        // Port 0 → OS picks a free ephemeral port.
        let provider = gstreamer_net::NetTimeProvider::new(&clock, None, 0)?;
        let port = provider.port();

        // Record the base time so adapters can align their timeline to ours.
        let base_time_ns = pipeline.base_time().map(|t| t.nseconds()).unwrap_or(0);

        eprintln!("[net-clock] serving on UDP :{port}, base_time={base_time_ns}ns");
        Ok(Self {
            _provider: provider,
            port,
            base_time_ns,
        })
    }
}
