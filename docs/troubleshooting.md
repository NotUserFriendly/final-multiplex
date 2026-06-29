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

**Status:** Open — expected overhead for tile-res copy; acceptable for the proof stage.
Off-thread copy is the next incremental fix; full zero-copy is Block 3.

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

**Status:** Open. Not yet confirmed whether the probe is the cause or whether it is
coincidental measurement noise. If stutter is observed, check fps_out — a compositor
running at 37 fps against a 23.976 fps file source produces the same 37/24 judder
pattern seen in the pre-RATCHET_MIN_DELTA era. Potential fixes:
- Off-thread copy: have the probe enqueue a buffer reference and do the pixel copy on
  a dedicated thread, removing the inline CPU cost from the streaming thread.
- Increase RATCHET_MIN_DELTA (e.g. to 8) to absorb probe-induced jitter at 30 fps
  while still passing a genuine 48/50/60 fps source (≥18 fps above baseline).

---
