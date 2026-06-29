# 0024. GPU presentation path with a renderer-side scheduler

- **Status:** Accepted
- **Date:** 2026-06-28

## Context

The app composites N sources into one output. That output is produced by the GStreamer
`compositor` on the **CPU** (ADR-0002/0004), then uploaded as a single texture to wgpu for
display (ADR-0009). The GPU only does the final blit.

The Phase-2.3 stutter investigation measured the cost: software compositing scales with canvas
pixel count and runs ~3–5 CPU cores at 4K (a 2×2 grid of 1080p tiles — 318% core load at 4K,
near-linear in pixels until it brushes a per-core ceiling and goes super-linear), tipping into
frame-drop stutter on an 8-core box. A discrete GPU does this same blend trivially. The app's
primary use case **targets a discrete-GPU box**, yet does all pixel work on the CPU.

The single composited frame also **couples** all sources in two ways the product doesn't want:
one universal output framerate (every source resampled to it — ADR-0023's ratchet manages this
but can't escape it), and a slow or stalled source hitches the shared frame, so **a bad feed
drags the good ones down**. Both are the same root cause: one CPU-composited output frame.

## Decision

Add a **GPU presentation path**. Each source becomes its own GPU texture at native resolution and
rate; compositing happens on the **GPU** (wgpu) by drawing each source's texture at an
**arbitrary rect** (position + size). This becomes the primary display path.

- **The shared clock (ADR-0005) remains the timing authority.** Per-source GPU presentation
  otherwise *loses* the frame-accurate alignment the compositor gave for free, so a
  **renderer-side presentation scheduler** re-establishes it explicitly: a per-source frame
  ring-buffer, and at each display refresh, select each source's frame for **(shared clock −
  that source's offset)**. The offset *concept* (ADR-0016: shared clock + per-source delay) is
  unchanged; its *mechanism* on this path moves from `gst_pad_set_offset()` + tile-res buffering
  to scheduler frame-selection. Frame-accuracy correctness lives in the scheduler.

- **Additive, not a replacement.** The GStreamer compositor stays as two tiers: the **fallback**
  path for hardware without the GPU path (integrated/corporate, roadmap step 4), and the
  **record / single-output** path (recording and streaming want one cohesive-framerate stitched
  frame, which per-source presentation doesn't produce). The validated machinery — offset model
  (0016), offset canary, delivery watchdog (0020), synthetic floors (0018) — stays intact on the
  compositor path while the GPU path is built and proven beside it.

- **Minimal milestone first, zero-copy as the target.** Minimal: CPU decode → per-source texture
  upload → GPU composite. Target: full zero-copy (hardware decode → dmabuf → wgpu import → GPU
  composite, CPU never touches a pixel) — the discrete-GPU endgame, which also retires the
  decode-side CPU cost. Driver/dmabuf dependencies make the full path the target, not the first
  step.

- **Build the rect interface general from day one** (arbitrary position + size, not grid-locked).
  That rect is the shared mechanism behind focus mode, per-source fit, and layout editing.

## Consequences

- **Stutter resolved at its source** — the per-pixel composite leaves the CPU; the measured 4K
  CPU-composite cost becomes near-free GPU work.
- **Native per-source rate and resolution** — no resampling to a universal output rate. ADR-0023's
  ratchet (the universal output framerate) becomes a **compositor/record-tier concern only**, moot
  on the GPU path.
- **Sources decouple** — a stalled source freezes in its own rect (the scheduler keeps presenting
  its last frame) instead of hitching a shared frame.
- **The scheduler is new owned complexity** — what the compositor gave free (atomic alignment),
  the renderer now owns. This is the rephase's primary risk; prove it first in the smallest scope
  (one source, against the compositor path) before generalizing.
- **Bandwidth interaction with ADR-0012** — the core-owned resize-to-tile was a deliberate
  bandwidth measure. On the GPU path the GPU scales per-rect, so the transport may carry larger
  (native-res) frames — heavier, but bounded by on-screen size, ultimately addressed by the
  zero-copy dmabuf target. Sending only what each rect needs (tile-res for small tiles, native
  for focused/large) is an available optimization.
- **Floors (ADR-0018) are a compositor-path concern** — they keep the GStreamer aggregators at
  PLAYING despite absent sources; the GPU path has no single aggregator waiting on all pads, so
  that problem doesn't arise there.
- **The delivery watchdog (ADR-0020) still applies** — it watches core-observed delivery; on the
  GPU path "delivery" is frames reaching the scheduler. Concept holds, observation point adapts.
- **Two display paths to maintain** until the GPU path covers all target hardware — the accepted
  cost of the additive approach, in exchange for keeping the validated machinery intact and the
  rephase de-risked.

## Relationships (forward-pointers to add to the older ADRs)

- **0009** (appsink→texture bridge): extended — one composited texture becomes N per-source
  textures composited in wgpu.
- **0016** (offset model): mechanism forks by path; the GPU path uses this ADR's scheduler.
- **0023** (output framerate ratchet): scoped to the compositor/record tier under this ADR.
- **0012** (core-owned resize): compositor-path; the GPU path scales per-rect (see bandwidth
  consequence).
