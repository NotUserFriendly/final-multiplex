# 0006. UI toolkit: iced, with Slint as the documented fallback

- **Status:** Accepted
- **Date:** 2026-06-21

## Context

Windows is the near-term demo target; Linux is the dev box. The UI's video job is narrow:
display the single composited output the core produces (ADR-0004) plus chrome — it
doesn't play media or lay out tiles.

Video integration was the deciding axis:
- **GTK4:** first-party `gtk4paintablesink` — turnkey display, no bridge — but a heavier
  Windows runtime to ship.
- **iced:** no GStreamer sink; community player crates lag and are built on `playbin`,
  which doesn't fit our topology. Pure-Rust, near-single-binary.
- **Slint:** first-party, Windows-tested `gstreamer-player` example.

Because we display our own composited frame (not media playback), every non-GTK toolkit
needs the same small connector — `appsink` -> texture upload — so the community video
crates are irrelevant either way. That removes iced's main weakness, leaving its
single-binary shipping and paradigm fit decisive.

## Decision

Use **iced**, displaying the composited output through a thin, owned **appsink -> texture
bridge** (~100-200 lines), not the community crates. Treat the bridge as a deliberate,
replaceable seam.

**Slint is the documented fallback** — stronger purely on video integration (first-party,
Windows-tested), so it's the pre-vetted pivot if the iced display path proves too costly.
The engine is toolkit-agnostic, so a switch costs only the UI layer.

### Bridge implementation (Phase 1)

The bridge is `crates/fm-app/src/bridge.rs` + `crates/fm-app/src/video.rs`:

- `AppSink` callback writes decoded RGBA frames into `Arc<Mutex<Option<Arc<FrameData>>>>`.
  The UI thread does a reference-count bump to take the latest frame with no pixel copy.
  A `generation` counter lets the GPU upload step skip frames that haven't changed.
- Display uses iced's `shader` widget (`iced::widget::shader::Program` /
  `iced::widget::shader::Primitive`). `GpuState` (implementing `shader::Pipeline`) holds a
  persistent `wgpu::Texture`; `queue.write_texture` overwrites pixel data in-place each
  frame instead of allocating a new texture.

The original approach (`iced::widget::image::Handle::from_rgba`) was tried first and
discarded: it destroys and recreates the GPU texture on every frame, causing a
delete→re-upload gap (flickering) and a partial-upload race with the render command
(horizontal combing artifacts). The persistent-texture path resolved both.

## Consequences

- We own the display bridge by design — it's the replaceable connector and decouples us
  from the stale community crates.
- Pin the iced version; expect occasional migration work.
- Don't depend on `playbin`-based player crates.
- The texture-copy path risk (flagged as the Phase-1 exit check in PLAN.md) has been
  validated: no flickering or combing at 30 fps with a 1280×720 composited grid.
  A 1080p sustained-throughput check (with the fps/dropped-frames counters) remains
  the formal Phase 1 exit gate.
