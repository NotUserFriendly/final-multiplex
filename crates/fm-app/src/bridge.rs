// appsink → iced image::Handle bridge  (ADR-0006)
//
// The AppSink callback runs on a GStreamer streaming thread. It writes the
// most recent RGBA frame into a shared `FrameStore` (Arc<Mutex<Option<…>>>).
// The iced update loop reads it via `latest_handle` on each ~16 ms tick,
// creating an image::Handle from raw RGBA bytes.
//
// This is the deliberate, replaceable seam described in ADR-0006. If the
// per-frame copy cost proves too high (measured via MetricsCollector), this
// file is the only thing that changes.

use std::sync::{Arc, Mutex};

pub type FrameStore = Arc<Mutex<Option<FrameData>>>;

pub struct FrameData {
    pub width: u32,
    pub height: u32,
    /// Raw RGBA8 pixels, row-major, width × height × 4 bytes.
    pub rgba: Vec<u8>,
}

pub fn new_store() -> FrameStore {
    Arc::new(Mutex::new(None))
}

/// Install a `new_sample` callback on `appsink` that writes each decoded
/// RGBA frame into `store`, overwriting any unread frame.
pub fn install(appsink: &gstreamer_app::AppSink, store: FrameStore) {
    appsink.set_callbacks(
        gstreamer_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink
                    .pull_sample()
                    .map_err(|_| gstreamer::FlowError::Error)?;

                let caps = sample.caps().ok_or(gstreamer::FlowError::Error)?;
                let s = caps.structure(0).ok_or(gstreamer::FlowError::Error)?;
                let width: i32 =
                    s.get("width").map_err(|_| gstreamer::FlowError::Error)?;
                let height: i32 =
                    s.get("height").map_err(|_| gstreamer::FlowError::Error)?;

                let buffer =
                    sample.buffer().ok_or(gstreamer::FlowError::Error)?;
                let map = buffer
                    .map_readable()
                    .map_err(|_| gstreamer::FlowError::Error)?;
                let rgba = map.as_slice().to_vec();
                drop(map);

                *store.lock().unwrap() = Some(FrameData {
                    width: width as u32,
                    height: height as u32,
                    rgba,
                });

                Ok(gstreamer::FlowSuccess::Ok)
            })
            .build(),
    );
}

/// Return an iced image handle for the most recently decoded frame, or `None`
/// if no frame has arrived yet.
pub fn latest_handle(store: &FrameStore) -> Option<iced::widget::image::Handle> {
    let guard = store.lock().ok()?;
    let frame = guard.as_ref()?;
    Some(iced::widget::image::Handle::from_rgba(
        frame.width,
        frame.height,
        frame.rgba.clone(),
    ))
}
