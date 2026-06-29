// Per-source frame ring-buffer and scheduler for the GPU presentation path
// (ADR-0024, Phase 3 Block 1).
//
// Frames arrive from a pad probe on vcaps_{id}:src (tile-res RGBA, with the
// buffer's raw PTS).  The scheduler selects the frame whose PTS best matches
// (pipeline_running_time − source_offset_ns) at each display refresh.  This
// re-implements, in the renderer, the alignment the GStreamer compositor
// provides "for free" on the CPU path — proving that a per-source renderer
// can maintain frame accuracy against the shared clock (ADR-0005).
//
// Resolution note (Block 1): probing vcaps:src gives tile-res RGBA (the same
// resolution the compositor receives after scaling).  Native-res tap is the
// next block once the scheduler is proven.

use gstreamer::prelude::*;
use std::sync::{Arc, Mutex};

const RING_SIZE: usize = 16;

#[derive(Debug)]
pub struct TimedFrame {
    pub width: u32,
    pub height: u32,
    /// Tightly packed RGBA8, row-major, width × height × 4 bytes.
    pub rgba: Vec<u8>,
    /// Raw buffer PTS in nanoseconds.  For file sources this is the file's
    /// presentation time; compare against (running_time − offset_ns).
    pub pts_ns: u64,
}

pub struct FrameRing {
    slots: Vec<Option<Arc<TimedFrame>>>,
    next: usize,
}

impl FrameRing {
    fn new() -> Self {
        Self {
            slots: (0..RING_SIZE).map(|_| None).collect(),
            next: 0,
        }
    }

    pub fn push(&mut self, frame: Arc<TimedFrame>) {
        self.slots[self.next] = Some(frame);
        self.next = (self.next + 1) % RING_SIZE;
    }

    /// Return the frame whose pts_ns is closest to `target_ns`.
    /// Returns `None` when the ring is empty.
    pub fn select(&self, target_ns: u64) -> Option<Arc<TimedFrame>> {
        self.slots
            .iter()
            .filter_map(|s| s.as_ref())
            .min_by_key(|f| {
                let diff = f.pts_ns as i64 - target_ns as i64;
                diff.unsigned_abs()
            })
            .cloned()
    }
}

pub type GpuFrameStore = Arc<Mutex<FrameRing>>;

pub fn new_store() -> GpuFrameStore {
    Arc::new(Mutex::new(FrameRing::new()))
}

/// Install a pad probe on `pad` (a tile-res RGBA video src pad such as
/// `vcaps_{id}:src`) that copies each buffer into `store` with its PTS.
///
/// The probe is non-blocking — it copies pixel data then returns `Ok` so the
/// buffer continues downstream to the compositor unchanged.
pub fn install_probe(pad: &gstreamer::Pad, store: GpuFrameStore) {
    pad.add_probe(gstreamer::PadProbeType::BUFFER, move |pad, info| {
        let Some(gstreamer::PadProbeData::Buffer(buf)) = &info.data else {
            return gstreamer::PadProbeReturn::Ok;
        };
        let Some(caps) = pad.current_caps() else {
            return gstreamer::PadProbeReturn::Ok;
        };
        let Ok(vinfo) = gstreamer_video::VideoInfo::from_caps(&caps) else {
            return gstreamer::PadProbeReturn::Ok;
        };

        let pts_ns = buf.pts().map(|t| t.nseconds()).unwrap_or(0);
        let w = vinfo.width();
        let h = vinfo.height();
        let stride = vinfo.stride()[0] as usize;
        let row_bytes = w as usize * 4; // RGBA

        let Ok(map) = buf.map_readable() else {
            return gstreamer::PadProbeReturn::Ok;
        };
        let src = map.as_slice();
        let expected = stride * (h as usize).saturating_sub(1) + row_bytes;
        if src.len() < expected {
            return gstreamer::PadProbeReturn::Ok;
        }
        let rgba = if stride == row_bytes {
            src[..row_bytes * h as usize].to_vec()
        } else {
            let mut packed = Vec::with_capacity(row_bytes * h as usize);
            for row in 0..h as usize {
                let start = row * stride;
                packed.extend_from_slice(&src[start..start + row_bytes]);
            }
            packed
        };
        drop(map);

        let frame = Arc::new(TimedFrame {
            width: w,
            height: h,
            rgba,
            pts_ns,
        });
        store.lock().unwrap().push(frame);
        gstreamer::PadProbeReturn::Ok
    });
}
