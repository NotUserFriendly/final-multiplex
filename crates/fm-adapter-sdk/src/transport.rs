//! Platform-selected transport elements (ADR-0019).
//!
//! One call site per adapter; one `cfg` guard here.  Adding a new platform
//! means adding a new `cfg` branch in this file and in `fm-core/src/pipeline.rs`.

use gstreamer::prelude::*;

/// Create and configure the platform output sink for one stream.
///
/// On Linux: `unixfdsink` with `sync=false`.
///
/// Panics if the GStreamer plugin is absent — fatal misconfiguration at startup.
#[cfg(target_os = "linux")]
pub fn make_output_sink(name: &str, socket_path: &str) -> gstreamer::Element {
    let sink = gstreamer::ElementFactory::make("unixfdsink")
        .name(name)
        .build()
        .unwrap_or_else(|_| panic!("missing GStreamer element 'unixfdsink' (gst-plugins-bad)"));
    sink.set_property_from_str("socket-path", socket_path);
    sink.set_property("sync", false);
    sink
}
