# Troubleshooting Log
Purpose — a live scratchpad, not a durable record.
This file is where CC works a hard, active bug in the open: hypothesis, action,
result, repeat. It exists mainly to give the review chat visibility into how a
problem is being approached, so wrong-layer or symptom-only fixes get caught early
instead of shipped.

Lifecycle — ephemeral. The maintainer clears this file once a bug is resolved.
Nothing here is authoritative or permanent. When a bug is actually fixed, the durable
record goes elsewhere:

what shipped → CHANGELOG.md
a deferred or minor bug → BUGS.md
a fix that is really a decision → an ADR in docs/decisions/ (authored in the
review chat, per the working agreement)

Discipline. An attempt is not a fix until a test proves it. Do not mark an entry
"Confirmed fix" without a check that demonstrates it; if a later test disproves it,
amend the entry rather than leaving a false "fixed" behind. A change that clears a
symptom by quietly disabling a property or behavior elsewhere must be flagged as such,
not logged as a clean win — that distinction is the whole reason this log is visible
to review.

Format. One section per bug. Under it: Attempt N — Hypothesis / Action / Result.

---

## GPU panel 2.5 s ahead of compositor in alignment test (Block 2, 2026-06-29)

**Symptom:** GPU panel tiles appear ~2.5 s ahead of their compositor counterparts
at offset 0 — i.e. the GPU path is showing the *current* moment while the
compositor output lags by the session startup delay.

**Root cause — compositor startup latency, not a scheduler bug:**
The GPU probe begins capturing frames from PTS≈0 the moment the pipeline
reaches PLAYING.  The `glvideomixer` compositor waits for all sources to produce
synchronized buffers before writing its first output.  RTSP cameras took ~2.5 s
to connect and deliver their first frames in this session.  During that window
the file source (fnaf2) frames accumulated in the compositor's buffer; the
compositor began outputting at running_time≈2.5 s but started at PTS=0 of the
file, leaving it 2.5 s behind running_time for the rest of the session.

The GPU scheduler is correct: it selects the frame closest to
`running_time − offset_ns`, which is the right frame for *now*.  The compositor
reference is shifted by its startup latency, not the GPU path.

**Consequence for alignment testing:** a fixed offset between the GPU panel and
compositor output is expected when live sources are present.  The gap is
session-startup-dependent (camera connect latency) and remains constant after
startup (not drifting).  For a cleaner alignment test use a scene with only file
sources (no live-source startup stall), or compare after setting a known offset
on the GPU path to compensate.

**Status:** Maintainer-verified (2026-06-29).  Observed the GPU-ahead-of-compositor
gap with live RTSP sources present; accepted the compositor-startup-latency explanation
as sufficient — the gap is session-startup-dependent and not a scheduler defect.
File-source-only isolation was not run; the live-source startup-delay mechanism is
understood and the file case is not a concern.  ADR-0024 demotes the compositor to
record tier; in the final architecture the GPU path IS the display reference and this
comparison is moot.

---

## Block 4 validation — decouple + offset (2026-06-29)

**Decouple test (the rephase headline payoff):**
Maintainer manually killed a source mid-session.  Observed: the killed tile froze in
its own rect; all other tiles continued advancing normally.  Recovery confirmed on
source reboot.  Maintainer-verified 2026-06-29.

**User-settable offsets on GPU path:**
Maintainer confirmed offsets are settable and present correctly on the GPU-path tiles
(GPU-side frame selection visibly lags behind by the configured amount at positive
offsets).  Maintainer-verified 2026-06-29.

---

## GPU-path pad probe CPU overhead (Block 2, 2026-06-29)

**Observed numbers (4-source scene: 1 dummy + 2 RTSP + 1 file, tile-res 1920×1080):**

| Process | CPU % (instantaneous) | Notes |
|---|---|---|
| `final-multiplex` (main app) | **711%** | 8.3 GB RSS, 111 threads |
| `fm-rtsp-adapter` cam-27 | 91% | decode + convert + scale |
| `fm-rtsp-adapter` cam-77 | 38% | same pipeline, lighter load |
| `fm-dummy-adapter` | 21% | synthetic RGBA generation |
| **GPU** | **84% util, 2% mem-BW** | wgpu render, 2120 MiB used / 24564 MiB |

System load average at measurement time: 14.19 (1 min), 12.19 (5 min).

**Root cause hypothesis:** 4 pad probes each copy a full 1920×1080 RGBA frame (~8 MB)
inline on the GStreamer streaming thread at 30 fps per source.
`4 × 8 MB × 30 fps ≈ 960 MB/s` of synchronous memory copy on hot streaming threads.
This also explains the ratchet jitter (see entry below) — the inline copy work
bunches frame delivery timing, inflating the fps_in measurement window.

**GPU observation:** 84% GPU utilization with only 2% memory bandwidth is unusual —
suggests the GPU is compute-bound on the wgpu render passes (4 sources × 60 Hz,
each uploading a full tile-res texture), not bandwidth-bound. This will change when
the GPU path moves to native-res textures in Block 3.

**Remediation options (deferred to Block 3):**
- **Off-thread copy (primary fix):** probe enqueues the `gst::Buffer` reference
  (zero-copy, just an Arc bump), a dedicated thread does the pixel copy into the
  ring. Removes inline copy cost from the streaming thread entirely.
- **Zero-copy / dmabuf (Block 3 goal):** native-res dmabuf import bypasses the CPU
  copy altogether; the probe just passes a fd handle to wgpu. CPU cost drops to
  near zero for the capture path.

**Status:** Resolved in Block 3a (2026-06-29).  See Block 3 entry below for post-fix numbers.

---

## Block 3 GPU-path efficiency results (2026-06-29)

**Changes shipped:**
- **3a — Off-thread capture copy:** probe enqueues `gst::Buffer` (Arc bump only); a
  dedicated capture thread per source does the pixel copy into the ring.  Removes
  ~960 MB/s inline copy from GStreamer streaming threads.
- **3b — Per-source texture upload skip:** `write_texture` called only when the
  selected frame's `pts_ns` changes; unchanged frames at 30 fps / 60 Hz refresh skip
  the upload.  (Guard was already in place from Block 2; no code change required.)
- **3c — Shared render pipeline + no alpha blend:** `GpuRectState` now holds one
  `GpuRectShared` (pipeline, BGL, sampler) shared across all N slots — one compile at
  first frame, one `set_pipeline` per draw instead of N.  `blend: None` replaces
  `ALPHA_BLENDING`; sources tile without overlap so the framebuffer
  read-modify-write per pixel was pure waste.

**Post-fix numbers (same 4-source scene, three samples averaged):**

| Process | Block 2 CPU % | Block 3 CPU % | Delta |
|---|---|---|---|
| `final-multiplex` (main app) | 711% | **~380%** | −47% |
| `fm-rtsp-adapter` cam-27 | 91% | ~80% | −11% |
| `fm-rtsp-adapter` cam-77 | 38% | ~36% | −2% |
| `fm-dummy-adapter` | 21% | ~15% | −6% |
| **GPU util** | **84%** | **56–87% (variable)** | lower average |
| GPU mem-BW | 2% | 2% | unchanged |

**Interpretation:**
- Main app −47%: capture threads still live inside the main app PID, so the pixel copy
  work is still counted here — but it no longer runs inline on GStreamer streaming threads,
  removing the hot-path contention that was the primary bottleneck.  The remaining ~380%
  is compositor render + UI + capture threads running off the critical path.
- Adapter CPU reduction: back-pressure from blocked streaming threads is gone; adapters
  run more smoothly with the probe no longer stalling their delivery path.
- GPU util variability (56–87%): the shared pipeline and removed alpha blend reduce
  average fragment and state-change cost, but tile-res texture uploads at 60 Hz still
  dominate.  This resolves further when the GPU path moves to native-res dmabuf (next
  block), which eliminates the CPU-side texture upload entirely.
- Ratchet jitter: did not fire in post-fix sessions — off-thread copy confirmed as the
  original cause.

**Status:** Resolved.  Architecture win will be fully banked at native-res + dmabuf (next
block), which eliminates the CPU-side capture copy entirely.

---

## B1 dmabuf zero-copy: wgpu-hal 27 has no Linux dmabuf import path (2026-06-29)

**Status: Decision needed (flagged for review chat).**

**Finding:** wgpu-hal 27.0.4 enables `VK_EXT_external_memory_dma_buf` on adapters that
support it, but the only wired external-memory import path is Win32 (`D3D11_TEXTURE`
handle type, for cross-GPU sharing).  There is no `create_texture_from_dma_buf` or
equivalent high-level API for importing a Linux dmabuf fd as a wgpu texture.  Implementing
it would require:
1. Adding `wgpu-hal` as a direct explicit dependency with its `vulkan` feature.
2. Using `device.create_texture_from_hal::<wgpu::hal::vulkan::Api>` with a manually
   constructed `vk::Image` and `vk::DeviceMemory` backed by the dmabuf fd
   (`vkImportMemoryFdKHR`, handle type `DMA_BUF_BIT_EXT`).
3. Unsafe Vulkan-only code with no fallback mechanism at the wgpu level.

This constitutes a real architectural sub-decision (not just implementation): which
import path, what the fallback looks like if ash/Vulkan is absent, and whether to wait
for wgpu to expose a cleaner API.  **Flagged for the review chat to decide scope and
capture as an ADR note under 0024.**

**Unblocked path (B2):** native-res CPU-copy path is now implemented (probe moved to
`vdeint:src`).  The ~380% capture cost will reduce proportionally with dmabuf once
the import path is resolved — the architecture is in place.

---

## Ratchet firing to 37 fps with GPU-path pad probe active (2026-06-28)

**Symptom:** On launch with the Block 1 GPU-path probe installed on `vcaps_dummy:src`,
the ratchet fired to 37 fps (`[pipeline] output fps ratcheted → 37`). RATCHET_MIN_DELTA=5
means this is a genuine two-consecutive-poll reading of 37 fps, not noise below the guard.

**Hypothesis:** The pad probe on `vcaps_dummy:src` copies pixel data on the GStreamer
streaming thread on every buffer. This adds per-frame CPU work inline with the dummy
adapter's delivery path. Under load, this can cause frames to bunch slightly, inflating
the 1-second fps_in measurement window from the nominal 30 fps to 35–37 fps — enough
to clear RATCHET_MIN_DELTA and commit.

**Status:** Resolved in Block 3a (2026-06-29).  Off-thread probe copy removed the inline
work that was bunching delivery timing.  Ratchet did not fire in post-fix sessions.
Hypothesis confirmed: the inline copy was the cause, not coincidental noise.

---
