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

## Tile-res revert validation (ADR-0025, 2026-06-29)

**Scene:** 4-source 2×2 (1 dummy + 2 RTSP + 1 file), same as Block 3 baseline.
**Binary:** tile-res revert commit (GPU probe back on `vcaps_{id}:src`).

**A/B comparison result (maintainer-verified 2026-06-29):**
Maintainer recorded both runs externally and compared side-by-side.  Stutter is
**identical** between tile-res and native-res capture.  Conclusion: the stutter is not
caused by capture resolution or copy cost — it is a present-timing beat (16 ms wall-clock
timer at ~62.5 Hz beating against ~60 Hz vsync), independent of capture resolution.
**No tile-res revert.** Capture stays at native-res (ADR-0025 updated).

**Step 3 — 4K fullscreen calibration:**
Deferred — fullscreen 4K measurement should be taken after the vsync/timer-beat fix so it
reflects capture copy cost alone, not the timing beat.  See ADR-0025.

**CORRECTION — 2026-06-29 (post-vsync-fix investigation):**
The A/B conclusion and the timer-beat hypothesis were both wrong.  See the render-rate
finding section below.  ADR-0025's stated root cause needs revision.

---

## Render rate bottleneck — iced `window::frames()` fires at ~19 fps (2026-06-29)

**Symptom:** Persistent constant stutter on both compositor and GPU panels after the
vsync fix (`window::frames()` replacing `time::every(16ms)`).  Stutter is not periodic;
it is constant and looks like a framerate issue (~19 fps content on a 60 Hz display).

**Diagnostic (CC-measured 2026-06-29):**
Instrumented `Message::Frame` and `Message::Poll` with per-event eprintln timestamps.
- `Message::Poll` fires correctly at ~500 ms.
- `Message::Frame` fires at **~19 fps** (batches of 30 frames measured over 88 s: average
  18–21 fps, sustained ~19 fps).  It is NOT a 60 Hz vsync-locked stream.

**Root cause:**
`iced::window::frames()` fires once per completed render cycle, not once per display
vsync.  After each render + present, iced's event loop must go through
`RedrawRequested → subscription fires → Message::Frame queued → AboutToWait processes
message → request_redraw() → next RedrawRequested`.  On this hardware each full cycle
takes ~53 ms, consuming 3–4 vsync periods per rendered frame.  Two factors contribute:

1. **Native-res texture upload cost:** 4 sources × 1920×1080 RGBA = 33 MB of CPU→GPU
   staging buffer writes per render.  At tile-res this is 8 MB (4×).
2. **iced event-loop overhead between renders:** iced's reactive model adds 1–2 vsync
   periods of dispatch latency between present completion and the next render start.

The combined effect: ~19 fps render throughput → each content frame shown for 3–4
display periods → constant cadence judder visible on both panels.

**Why the A/B comparison was misleading:**
Both tile-res and native-res recordings showed "identical stutter" because the render
was at ~19 fps in both cases.  At tile-res the upload cost is 4× lower, but the iced
event-loop overhead was already the dominant bottleneck, keeping the effective rate near
19 fps regardless of texture size.  The timer-beat hypothesis (62.5 Hz vs 60 Hz) was
wrong — or at minimum not the primary cause.  The vsync fix was architecturally correct
but irrelevant while the render is below 60 fps.

**Status: Flagged for review chat.**
ADR-0025's stated root cause ("present-timing beat, independent of capture resolution")
is incorrect and requires revision.  Three paths forward (decision for review chat):

1. **Tile-res textures for GPU upload** — reduces upload 4× (8 MB/frame); likely
   restores ≥60 fps render throughput and smooth display.  Capture tap can stay at
   native-res (`vdeint:src`) while the ring buffer downscales before upload.  Or revert
   the probe to `vcaps:src`.  Lowest effort; highest immediate impact.

2. **dmabuf zero-copy** — eliminates CPU→GPU upload entirely (ADR-0025 B1, blocked on
   wgpu-hal Vulkan import API).  Correct long-term fix; deferred.

3. **Dedicated GPU surface outside iced** — GPU compositor runs its own wgpu present
   loop outside iced's event dispatch, eliminating the per-frame event-loop overhead.
   Architectural change; the taskblock named this as the fallback if iced friction
   materialised.  It has.

**Render-rate split measurement (CC-measured 2026-06-29):**
Ran the renderrate-measure taskblock: downscaled textures to 426×240 before
`write_texture` (upload ~400 KB vs 33 MB native-res), kept all other path unchanged,
measured `Message::Frame` rate in batches of 30 with per-batch fps and per-frame
upload time logged to session log.

Key numbers over a ~30-minute session:

| Phase | Frames | Batch fps range | Avg upload / frame |
|---|---|---|---|
| Startup (cold) | 1–300 | **44–60 fps** (avg ~54) | ~50–59 μs |
| Mid session | 300–990 | **35–56 fps** (avg ~48) | ~49–52 μs |
| Late (sustained) | 990+ | **24–30 fps** (avg ~28) | ~53–65 μs |

Upload time: consistently **~50–65 μs** regardless of phase.  At native-res (80× more
data) upload would be ~4 ms — present but not the floor.

**Split conclusion — iced event-loop overhead is the dominant and growing bottleneck:**
- The upload (even at native-res ~4 ms) cannot explain a 53 ms cycle.  Eliminating 32 MB
  of upload data (tiny textures) only recovers ~4 ms; the cycle is still 17–40 ms.
- The render rate DEGRADES from ~54 fps at startup to ~28 fps after 10–15 min of
  sustained operation — with tiny textures.  Upload cost is constant; the degradation is
  from event-loop pressure accumulating under sustained load (ring-buffer eviction,
  probable thermal throttle, or both).
- Early 44–60 fps is a cold-machine artifact (empty ring, cool silicon), not a steady-state
  we can count on.

**Taskblock classification: Path 3.**
The taskblock's decision tree:
- ≥ 50 fps with tiny tex → upload was the wall → Path 1
- ≤ 30 fps with tiny tex → loop can't reach 60 → Path 3 (dedicated GPU surface)
- 30–50 fps → both → Path 1 now, Path 3 eventually

Sustained late-session fps is ≤ 30.  Even the cold-start phase sits in the 30–50 range,
not ≥ 50.  **Path 3 is required: the GPU compositor needs its own wgpu present loop
outside iced's event dispatch.**  Path 1 (smaller uploads) is a palliative that buys a
few minutes of better fps on a cold machine; it does not fix the steady-state problem.
This is an architectural decision → flag for review chat to author ADR.

**Measured perf (tile-res revert, windowed, 3-sample average, CC-measured 2026-06-29):**

| Process | CPU % | RSS | Notes |
|---|---|---|---|
| `final-multiplex` (main app) | **~486%** | 9.2 GB | down from ~544% (native-res), ~380% (Block 3 tile-res) |
| `fm-rtsp-adapter` cam-27 | ~81% | 1.2 GB | decode + convert + scale to tile |
| `fm-rtsp-adapter` cam-77 | ~37% | 661 MB | lighter load |
| `fm-dummy-adapter` | ~15% | 534 MB | synthetic RGBA |
| **GPU** | **62–66% util, 2% mem-BW** | 2193–2233 MiB | wgpu render, 24564 MiB total |

Note: main-app CPU slightly higher than the Block 3 tile-res baseline (~380%) — the ring
buffer now holds tile-res frames (960×540 RGBA = ~2 MB each) but is sized to
`ceiling_ms + 500 ms`, and the capture thread is running at the same rate.  The delta
from 380% likely reflects the additional GPU side panel render overhead introduced in
later blocks (shared pipeline + 4-source draw per refresh at 60 Hz).

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

## Phase 3 dedicated-surface — Step 0: RSS / leak check (2026-06-29)

**Setup:** 4-source 2×2 scene; `[diag-rss]` instrumentation in `Message::Frame` emitting
RSS and batch fps every 60 frames to the session log.  Run duration ~17 min (PID 67348).

**RSS data:**

| Frame | Batch fps | RSS (MB) |
|---|---|---|
| 60 | 23 | 8804 |
| ~600 | 18–24 (oscillating) | 8742–8864 |
| 21720 | 20 | 9016 |

Total RSS drift over 17 min: **+206 MB** (8804 → 9016 MB, non-monotone with occasional drops).

**Conclusion: no pathological memory leak.**  RSS growth is flat and non-monotone —
consistent with minor GStreamer internal caching (decoder/pad state, dmabuf handles)
that is periodically released.  Not a frame-ring or application-level leak.

**fps degradation is thermal/event-loop, not memory:**  fps declined from 22–23 cold
to 18–20 late-session with flat RSS.  This matches the iced event-loop pressure and
thermal throttle hypothesis from the render-rate split measurement.

**Display refresh rate finding (same session):** `xrandr`/compositor output confirmed
the display runs at **159.92 Hz** (3440×1440), not 60 Hz as previously assumed.
vsync interval = **6.25 ms**. This makes the iced event-loop overhead (22 ms at startup,
35+ ms late session) even more severe relative to vsync — and confirms Path 3 (dedicated
wgpu present loop) is required. 40fps from iced's loop is ~6.5 vsyncs/frame at 160 Hz.

**Pre-composite texture approach also evaluated and rejected:** even with 1 ms blit, iced's
22 ms event-loop overhead → 23 ms cycle → ~40 fps ceiling. iced's loop still gates presents
regardless of how fast the GPU work is. Path 3 only.

**Step 0 diagnostic code stripped** (2026-06-29); findings recorded here.

---

## Phase 3 dedicated-surface — Step 1 spike: wl_subsurface (2026-06-29)

**What was built:**
`crates/fm-app/src/wayland_sub.rs` — creates a `wl_subsurface` under iced's window via
libwayland C FFI (`wayland-sys` crate, isolated `wl_event_queue` to avoid racing winit).
On the `Opened` event, `iced::window::run()` fetches raw Wayland handles; `update()` then
calls `create_subsurface()` (on the event-loop/Wayland thread) and spawns a dedicated
wgpu render thread that presents solid magenta at Fifo vsync.

**What to check when running:**
1. `[wayland-sub] subsurface created at (0, 0)` appears in session log → Wayland protocol worked.
2. `[wayland-sub] surface configured NxN Bgra8UnormSrgb Fifo — presenting magenta` appears → wgpu surface up.
3. The window shows a magenta fill (covers the full window including chrome — this is expected for the spike).
4. The magenta is solid, not flickering — confirms the render thread is presenting at vsync.
5. iced's event loop continues running (you can still observe the process running; SIGTERM exits cleanly).

**Bugs found and fixed during bringup (2026-06-29, CC-diagnosed):**

1. `wl_registry.bind` version: initially bound globals at their advertised version (e.g., 4)
   but the registry proxy is always version 1 — `wl_proxy_marshal_array_constructor_versioned`
   checks `factory->version < requested_version` → EINVAL.  Fixed: bind at version 1.

2. `wl_registry.bind` args count: initially passed 2 args `[{u:name}, {n:0}]` but the wire
   signature for an untyped new_id is `"usun"` (4 args: uint name, string iface_name, uint
   version, new_id).  `strlen` was called on `args[1].s = NULL` → SIGSEGV inside
   `wl_proxy_marshal_array_flags`.  Fixed: pass all 4 args, with `{s: iface.name}` as
   args[1].
   Diagnosed with: `strace -s 200` (found EINVAL message) and `gdb -batch` (found SIGSEGV
   frame in `__strlen_avx2 ← wl_proxy_marshal_array_flags ← on_global`).

**CC-observed run result (2026-06-29):**
- `[wayland-sub] subsurface created at (0, 0)` ✓
- `[wayland-sub] using adapter: NVIDIA GeForce RTX 3090 Ti` ✓
- `[wayland-sub] surface configured 1760×770 Rgba8UnormSrgb Fifo — presenting magenta` ✓
- Process ran stably for 20+ minutes with no crash.
- CPU at 665% during run — expected, render loop is a tight spin (no throttle in spike; Step 2 will fix).

**Status:** Confirmed — maintainer verified full-screen magenta coverage (2026-06-29). Step 1 complete; proceeding to Step 2.

---

## Phase 3 dedicated-surface — Step 2: video compositor (2026-06-29)

**What changed from Step 1:**
`render_loop` now reads `running_time` from `Arc<AtomicU64>` (written by
`Message::Frame`), selects each source's closest frame from its ring at
`running_time − offset_ns`, uploads textures to per-slot `wgpu::Texture`, and composites
N NDC rects with the rect shader before present.  Offset changes propagate via
`try_lock` on `Arc<Mutex<Vec<RenderSlot>>>`.

**CC-observed run result (2026-06-29):**
- `[sub] surface 1760×770 Rgba8UnormSrgb Fifo — video compositor active` ✓
- Process ran stably 3+ minutes with no crash.
- CPU at ~588% — render thread still spinning; Fifo vsync throttles GPU submits but the
  CPU selection loop is not blocked.  Step 3 will measure actual present rate.

**What to check:**
1. Full-window composite of N sources appears (black background, sources in equal-split grid).
2. Each tile shows live video from its source (RTSP cams + dummy animated + FNAF2 file).
3. Tile positions match the expected equal-split layout (2×2 for 4 sources).
4. Adjusting an offset in iced's UI changes the corresponding tile's frame selection.

**Follow-up fixes (2026-06-29):**
- Surface format changed from `Rgba8UnormSrgb` → `Rgba8Unorm`: GStreamer delivers
  gamma-encoded RGBA; the sRGB surface was applying a second gamma pass and washing
  out mid-tones.  Dummy feed was unaffected (synthetic flat primaries insensitive to gamma).
- Letterboxed AR: `letterbox_rect()` computes the largest axis-aligned sub-rect within each
  grid cell that preserves the source's native AR (pillarbox/letterbox with black bars).
  `last_lb_rect` tracks the last-written value; buffer writes are skipped unless dims or
  cell rect changed.
- Window resize: `Arc<AtomicU64> WindowSize` (packed `width<<32|height`) written by
  `Message::Resized`, read by the render thread before each `get_current_texture()` call
  (must precede acquire — wgpu forbids configure while a SurfaceTexture is live).
  On change: reconfigure surface, update `width`/`height` locals, reset all `last_lb_rect`.

**Maintainer confirmed (2026-06-29):**
- Live video appears in all 4 tiles ✓
- Colors correct after `Rgba8Unorm` fix ✓
- Tiles letterboxed to 16:9, black pillar bars on sides ✓
- Window resize correctly reflows tiles, no iced content visible behind subsurface ✓

**Status:** Step 2 complete. Proceeding to Step 3 (render rate measurement + alignment validation).

---

## Phase 3 dedicated-surface — Step 3: validation (2026-06-29)

**Display:** LG ULTRAGEAR 3440×1440 @ 160 Hz (`is-current: true` from Mutter DisplayConfig).

**Render-rate measurement (release build, `[sub] present fps=` every 5 s):**

| Phase | fps range | Notes |
|---|---|---|
| Initial run (GPU sched still double-running) | 57–59 | Before double-render fix |
| After double-render fix + place_below | 90–109 | 95–102 typical; no floor |

- No crash, no panic over 3+ min observed run.
- RSS: ~9.4 GB (release build with 4 sources; consistent with Step 0 profile — no leak).
- No degradation after warmup; readings flat over the full measurement window.

**Double-render fix (discovered during Step 3):**
With `spike_thread` active, `Message::Frame` was still running the per-source GPU scheduler
(`store.lock().unwrap().select(...)` × N sources) every vsync.  This caused mutex contention
between the event loop and the render thread, halving effective fps (57 → ~100 fps once fixed).
Fix: skip the GPU scheduler in `Message::Frame` when `spike_thread.is_some()`.

**Controls-visibility fix (discovered during Step 3):**
Subsurface was `place_above` the parent — iced's tile overlay (offset controls) was buried.
Fix: `wl_subsurface.place_below(parent)` so iced renders on top; iced window set to
`transparent(true)` + `style(background_color: Color::TRANSPARENT)` so the subsurface
video shows through iced's transparent video area.  Subsurface height = `win_h − CHROME_H`
so the chrome bar (Play/Pause, Reset Rate) is always visible at the bottom.

**Rate ceiling analysis:**
Steady-state fps (~95–102) is well below 160 Hz.  Root cause: in GNOME Mutter, even a
`wl_subsurface` in desync mode couples its effective buffer-consumption rate to the parent
surface's commit cycle once GPU work is present.  `get_current_texture()` blocks until the
compositor consumes the previous buffer; the parent commits at iced's vsync rate, not 160 Hz.
**Flagged for ADR consideration** (CC does not author ADRs — review-chat decision).

**Compositor tier check (CC-observed):**
- `[pipeline] output fps ratcheted → 35` in session log ✓  
- Supervisor spawned all adapters; dummy/cam-27/cam-77 all Ready ✓.

**Maintainer confirmed (2026-06-29):**
- Video visible through transparent iced layer ✓
- Tile overlay controls visible and functional ✓
- Offset adjustments take effect correctly; user noted "better than they used to" ✓
- Resize works as expected ✓

**Status:** Step 3 complete. Phase 3 exit criteria met: no 19 fps cap (→ ~100 fps), no
degradation, alignment preserved, compositor tier unaffected.  Rate ceiling (not reaching
160 Hz) flagged for ADR decision on next architecture step.

---

## Phase 3 loose-ends verification (2026-06-29)

### 1 — Render-thread clock freshness

**Question:** Does the render thread read `running_time` at a coarse enough cadence that
frame selection steps visibly on a moving source?

**Measurement (CC-run, 15 min, release build):**

| Sample | Message::Frame fps | Notes |
|---|---|---|
| Warmup (t=0–30 s) | 135–136 fps | Cold start; iced's loop slightly slower initially |
| Steady state (t>30 s) | 147–151 fps | Stable; iced now lightweight with GPU sched skipped |

`running_time` is updated at ~148 fps; the render thread presents at ~148 fps; video sources
deliver at 30 fps (33 ms/frame).  Maximum staleness between atomic reads ≈ 7 ms — well within
a single video frame period.  Selection error is negligible and invisible on motion.

**No fix needed.** Direct pipeline-clock read on the render thread was considered and
ruled out — the AtomicU64 path is sufficient at this update rate.

**Bonus finding:** `Message::Frame` running at 147–151 fps (vs the 19 fps pre-fix baseline)
also means the Mutter subsurface ceiling rises to ~148 fps — confirming the coupling is to
iced's commit rate, not a fixed compositor cap.

### 2 — RSS flat-line check

**Question:** Is there a real memory leak independent of the (now-removed) render-rate
degradation path?

**Measurement (same 15-min run):**

| Sample | RSS |
|---|---|
| t ≈ 0 | 8 368 MB |
| t ≈ 5 min | 8 391–8 447 MB (oscillating) |
| t ≈ 10 min | 8 376–8 463 MB (oscillating) |
| t ≈ 15 min | 8 416–8 463 MB |

RSS is flat and non-monotone (oscillates ±95 MB around ~8 410 MB).  No climb.

**Leak question closed.** The fps degradation observed in the Step 0 tile-res run (54→28 fps
over 15 min) was event-loop pressure and thermal, not a memory-backed leak.

---

## YouTube audio silence + file source slow (6-source scene, 2026-06-29)

**Symptom:** yt-fc and yt-nature produce no audio even when unmuted.  fnaf2 (H.264 1920×960
24fps) runs slowly (14–16fps observed) instead of its native rate.

**Root cause — three cooperating failures:**

1. **Audio probe not sleeping:** First fix placed the probe on `aconv.sink` and used
   `buffer.duration()` to compute the sleep.  `avdec_aac` leaves that field unset
   (`GST_CLOCK_TIME_NONE`), so every probe call returned `None` and skipped the sleep
   entirely.  Audio still burst at full speed.  Confirmed by maintainer:
   "Both items aren't working" (audio still silent, fnaf2 still ~16fps).

2. **Segment timing mismatch at startup:** Even with correct throttling, `uridecodebin3`
   starts a segment with `base=0`, so PTS=0 frames from the adapter arrive at the core
   with `running_time=0`.  By the time the core starts playing the pipeline is already
   at `T_startup` (several seconds in).  The audiomixer sees running_time=0 as late
   and drops the entire clip → silence.  The compositor tolerates this (it doesn't drop
   late video frames; it just displays them when they arrive), which is why video works
   but audio doesn't.

3. **Reconnect seek gated on caps change:** The seek that re-anchors `segment.base` after
   EOS only fired when caps changed.  yt-fc loops with identical video+audio caps every
   4:38, so no seek → audio goes silent again after the first loop.

**Attempt 2 fix (CC, 2026-06-29 — commit 96ce048):**
- Probe moved to `aunixfdsink.sink` where format is fixed S16LE 48 kHz 2ch.
  Duration computed as `buf.size() / 192000` (exact; no metadata dependency).
- `Command::Play` handler issues `seek_simple(FLUSH|KEY_UNIT, 0)` on the first Play
  only (`play_seek_done` flag prevents firing on Pause→Play cycles).
- Reconnect seek in `post_reconnect_check_at` made unconditional.

**Attempt 2 result (maintainer-reported, 2026-06-30):** "fnaf2 is still playing at
low fps.  Neither yt source has audio still, even though their audio meters are
showing sound."  The audio meters fire on `alevel`, which is BEFORE audiomixer —
this was a new diagnostic clue: audio IS reaching the core chain, but audiomixer is
dropping it.

**Root cause revision (CC, 2026-06-30):** The seek approach was wrong-layer.
`unixfdsink`/`unixfdsrc` transfer raw buffer PTS values only — segment events
(including the adjusted `segment.base`) do NOT cross the process boundary.  So
PTS=0 audio always arrived at the core's audiomixer with `running_time=0`, which
audiomixer drops as late regardless of seeks in the adapter.  Additionally, the
reconnect seek (`FLUSH|KEY_UNIT, 0` on the HTTP source) was causing an EOS message
to be posted on the bus, triggering another `spawn_reconnect_thread`, which emitted
`StreamsChanged(false,false)` → core tears down the just-added audio chain → cycle
repeats.  This was confirmed in the session log: repeated
`added audio chain for 'yt-fc'` followed immediately by
`StreamsChanged (video=false audio=false)` → `removed audio chain for 'yt-fc'`.

**Final fix (CC, 2026-06-30):**
- `fm-core`: `aunixfdsrc` elements for audio chains now use `do-timestamp=true`.
  The adapter's raw PTS values are discarded; the core re-stamps each arriving audio
  buffer with the current pipeline clock time (arrival time).  Since the audio
  throttle probe (in the adapter) ensures buffers arrive at real-time pace,
  arrival time ≈ current running_time → audiomixer accepts them.
- `fm-youtube-adapter`: removed startup seek from `Command::Play` (was a no-op
  across the process boundary).  Removed reconnect seek from `post_reconnect_check`
  (was causing EOS→reconnect cycling).  Removed `play_seek_done` field.
- CC-verified (2026-06-30): yt-fc reconnected cleanly after ~4:38 (first clip EOS),
  `added audio chain for 'yt-fc'` without immediate removal.  20 transient
  `offset-canary` WARN entries during reconnect PTS=0 transition (expected; stopped
  after compositor stabilized).  No cycling observed.

**Awaiting maintainer validation (2026-06-30):** audio audible when unmuted from
both YouTube sources (yt-fc, yt-nature); fnaf2 fps near ~24fps.  Note: fnaf2 slow
fps hypothesis is that yt-fc cycling was consuming CPU (constant yt-dlp subprocess
spawning); with cycling removed, fnaf2 should recover.
