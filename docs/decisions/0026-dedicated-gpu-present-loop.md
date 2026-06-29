# 0026. GPU path owns its own wgpu present loop; iced becomes chrome

- **Status:** Accepted
- **Date:** 2026-06-29

## Context

The GPU presentation path renders through iced's `shader()` widget (ADR-0009), driven by iced's
reactive event loop. Measurement (troubleshooting.md, 2026-06-29): the render cycle is ~53 ms
(~19 fps, degrading to ~28 fps sustained), and the cost is iced's per-frame event dispatch, not
upload (~50 µs) or GPU composite (62 % util). iced's reactive model is structurally unfit for
frame-paced video presentation; no upload-size change can lift it to display refresh.

## Decision

The GPU compositor runs on a **dedicated render thread with its own `wgpu::Surface` and a
vsync-locked present loop** (`PresentMode::Fifo`), outside iced's event dispatch. The scheduler's
frame selection (`running_time − offset` against the shared clock, ADR-0005) runs on that thread,
per present. iced is retained as **UI chrome** (controls, panels, text, layout) only; it no longer
drives or gates GPU-path presentation.

The integration mechanism — how iced chrome and the GPU surface share the window (iced composited
over the GPU surface, GPU-owns-window with iced as an overlay layer, or split regions) — is the
implementation's first question; settle it with a spike before the full build.

## Consequences

- Render rate decouples from iced's cadence and can reach display refresh; the measured ~19 fps
  ceiling is removed.
- Supersedes the ADR-0009 iced-shader-widget bridge **for the GPU path** (the compositor/record
  tier and the chrome are unaffected). ADR-0006 (UI = iced) holds but narrows: iced is chrome, not
  the video surface.
- New owned complexity: a render thread, surface lifecycle (resize, occlusion, device loss), and
  thread-safe access to the per-source rings.
- Compositor/fallback/record tier unchanged; additive philosophy intact.
- The session render-rate *degradation* (54→28 fps over ~15 min) is tracked separately in
  troubleshooting and is **not** rationale here — a dedicated loop inherits any leak, so it is
  checked before/during implementation, not assumed fixed by this change.

## Sequencing

Gate before Phase 4 — the 3rd source should not land on a 19 fps loop.

## Relationship

Resolves the render-rate bottleneck left open by ADR-0025's scope note. Forward-pointers:
ADR-0009 (superseded for the GPU path), ADR-0006 (narrowed to chrome).
