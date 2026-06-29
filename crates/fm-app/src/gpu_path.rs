// Per-source frame ring-buffer and scheduler for the GPU presentation path
// (ADR-0024, Phase 3).
//
// Frames arrive from a pad probe on vdeint_{id}:src (native-res RGBA, with
// the buffer's raw PTS).  The tap is before vscale so the GPU path receives
// frames at the source's actual input resolution — the camera's native res
// for RTSP sources, the decoded file res for file sources (ADR-0024 B2).
// The compositor path's vscale→vcaps(tile) is unaffected.
//
// The scheduler selects the frame whose PTS best matches
// (pipeline_running_time − source_offset_ns) at each display refresh.  This
// re-implements, in the renderer, the alignment the GStreamer compositor
// provides "for free" on the CPU path — proving that a per-source renderer
// can maintain frame accuracy against the shared clock (ADR-0005).
//
// Res-appropriate-to-rect (ADR-0024 B2): when tiles have different sizes
// (focus mode, Phase 5+), the GPU path should import native-res only for
// large tiles and tile-res for thumbnails, reducing bandwidth for small rects.
// Not yet implemented — all tiles are equal-size today, making the
// distinction moot.  Wire it here when focus mode is built.

use gstreamer::prelude::*;
use std::collections::VecDeque;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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

struct RingEntry {
    frame: Arc<TimedFrame>,
    captured_at: Instant,
}

pub struct FrameRing {
    entries: VecDeque<RingEntry>,
    window: Duration,
}

impl FrameRing {
    // Size by time, not frame count — same lesson as the Phase-2.3 voff_q
    // fix: a 16-slot count-ring holds ~133 ms at 120 fps, far short of the
    // 2000 ms offset ceiling.  Wall-clock eviction is framerate-independent.
    fn new(ceiling_ms: u64) -> Self {
        Self {
            entries: VecDeque::new(),
            window: Duration::from_millis(ceiling_ms + 500),
        }
    }

    pub fn push(&mut self, frame: Arc<TimedFrame>) {
        let now = Instant::now();
        self.entries.push_back(RingEntry {
            frame,
            captured_at: now,
        });
        while let Some(front) = self.entries.front() {
            if now.duration_since(front.captured_at) > self.window {
                self.entries.pop_front();
            } else {
                break;
            }
        }
    }

    /// Return the frame whose pts_ns is closest to `target_ns`.
    /// Returns `None` when the ring is empty.
    pub fn select(&self, target_ns: u64) -> Option<Arc<TimedFrame>> {
        self.entries
            .iter()
            .map(|e| &e.frame)
            .min_by_key(|f| {
                let diff = f.pts_ns as i64 - target_ns as i64;
                diff.unsigned_abs()
            })
            .cloned()
    }
}

pub type GpuFrameStore = Arc<Mutex<FrameRing>>;

pub fn new_store(ceiling_ms: u64) -> GpuFrameStore {
    Arc::new(Mutex::new(FrameRing::new(ceiling_ms)))
}

struct CaptureMsg {
    buf: gstreamer::Buffer,
    pts_ns: u64,
    width: u32,
    height: u32,
    stride: usize,
}

/// Install a pad probe on `pad` (a tile-res RGBA video src pad such as
/// `vcaps_{id}:src`) that enqueues each buffer into `store` with its PTS.
///
/// The probe does an Arc bump (no pixel copy) on the streaming thread;
/// a dedicated capture thread does the pixel copy into the ring.
/// Channel bound = 4: if the capture thread falls behind, frames are dropped
/// rather than blocking the streaming thread or growing memory unboundedly.
pub fn install_probe(pad: &gstreamer::Pad, store: GpuFrameStore) {
    let (tx, rx) = mpsc::sync_channel::<CaptureMsg>(4);

    std::thread::spawn(move || {
        while let Ok(msg) = rx.recv() {
            let row_bytes = msg.width as usize * 4;
            let Ok(map) = msg.buf.map_readable() else {
                continue;
            };
            let src = map.as_slice();
            let expected = msg.stride * (msg.height as usize).saturating_sub(1) + row_bytes;
            if src.len() < expected {
                continue;
            }
            let rgba = if msg.stride == row_bytes {
                src[..row_bytes * msg.height as usize].to_vec()
            } else {
                let mut packed = Vec::with_capacity(row_bytes * msg.height as usize);
                for row in 0..msg.height as usize {
                    let start = row * msg.stride;
                    packed.extend_from_slice(&src[start..start + row_bytes]);
                }
                packed
            };
            drop(map);
            let frame = Arc::new(TimedFrame {
                width: msg.width,
                height: msg.height,
                rgba,
                pts_ns: msg.pts_ns,
            });
            store.lock().unwrap().push(frame);
        }
    });

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
        // Arc bump only — no pixel copy on the streaming thread.
        let _ = tx.try_send(CaptureMsg {
            buf: buf.clone(),
            pts_ns,
            width: vinfo.width(),
            height: vinfo.height(),
            stride: vinfo.stride()[0] as usize,
        });
        gstreamer::PadProbeReturn::Ok
    });
}
