// appsink → frame store bridge  (ADR-0006)
//
// The AppSink callback runs on a GStreamer streaming thread. It writes the
// most recent decoded frame into a shared FrameStore as an Arc<FrameData>,
// so the UI never copies raw pixel data — it just bumps a reference count.
//
// This is the deliberate, replaceable seam described in ADR-0006.

use std::sync::{Arc, Mutex};

pub type FrameStore = Arc<Mutex<Option<Arc<FrameData>>>>;

#[derive(Debug)]
pub struct FrameData {
    pub width: u32,
    pub height: u32,
    /// Raw RGBA8 pixels, row-major, width × height × 4 bytes, no row padding.
    pub rgba: Vec<u8>,
    /// Monotonically increasing; incremented by the GStreamer callback on every
    /// new frame so the GPU state can skip write_texture when nothing changed.
    pub generation: u64,
}

pub fn new_store() -> FrameStore {
    Arc::new(Mutex::new(None))
}

/// Install a `new_sample` callback on `appsink` that writes each decoded RGBA
/// frame into `store` as an `Arc<FrameData>`, overwriting any unread frame.
pub fn install(appsink: &gstreamer_app::AppSink, store: FrameStore) {
    appsink.set_callbacks(
        gstreamer_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink
                    .pull_sample()
                    .map_err(|_| gstreamer::FlowError::Error)?;

                let caps = sample.caps().ok_or(gstreamer::FlowError::Error)?;
                let info = gstreamer_video::VideoInfo::from_caps(caps)
                    .map_err(|_| gstreamer::FlowError::Error)?;
                let width = info.width();
                let height = info.height();
                let stride = info.stride()[0] as usize;
                let row_bytes = width as usize * 4; // RGBA = 4 bytes/pixel

                let buffer =
                    sample.buffer().ok_or(gstreamer::FlowError::Error)?;
                let map = buffer
                    .map_readable()
                    .map_err(|_| gstreamer::FlowError::Error)?;
                let src = map.as_slice();
                let h = height as usize;
                // Guard against short/truncated buffers.
                let expected = stride * (h.saturating_sub(1)) + row_bytes;
                if src.len() < expected {
                    return Err(gstreamer::FlowError::Error);
                }
                // Strip any row padding so the GPU upload always sees a tightly
                // packed RGBA buffer.
                let rgba = if stride == row_bytes {
                    src[..row_bytes * h].to_vec()
                } else {
                    let mut packed = Vec::with_capacity(row_bytes * h);
                    for row in 0..h {
                        let start = row * stride;
                        packed.extend_from_slice(&src[start..start + row_bytes]);
                    }
                    packed
                };
                drop(map);

                let mut guard = store.lock().unwrap();
                let generation =
                    guard.as_ref().map(|f| f.generation + 1).unwrap_or(1);
                *guard = Some(Arc::new(FrameData {
                    width: width as u32,
                    height: height as u32,
                    rgba,
                    generation,
                }));

                Ok(gstreamer::FlowSuccess::Ok)
            })
            .build(),
    );
}

/// Return the latest frame if its generation is newer than `last_gen`.
/// Returns `None` when nothing has changed, keeping the caller's existing
/// reference alive without any pixel data copy.
/// `last_gen` is updated on every `Some` return.
pub fn latest_frame(
    store: &FrameStore,
    last_gen: &mut u64,
) -> Option<Arc<FrameData>> {
    let guard = store.lock().ok()?;
    let frame = guard.as_ref()?;
    if frame.generation == *last_gen {
        return None;
    }
    *last_gen = frame.generation;
    Some(Arc::clone(frame))
}
