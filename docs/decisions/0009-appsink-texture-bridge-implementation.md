# 0009. appsink→texture bridge: persistent wgpu texture via iced shader widget

- **Status:** Accepted
- **Date:** 2026-06-22

## Context

ADR-0006 committed to a thin owned bridge between the GStreamer `appsink` and the iced
display, and designated it a deliberate replaceable seam. Phase 1 required a concrete
implementation to validate that the texture-copy path can sustain the target frame rate
(the primary risk flagged in ADR-0006).

The first implementation used `iced::widget::image::Handle::from_rgba`. Under that API,
every new frame triggers a full GPU texture lifecycle: the old texture is destroyed, a new
one is allocated, and pixels are uploaded. Two artifacts resulted:

- **Flickering** — the gap between destruction and re-upload leaves the widget without a
  valid texture for one or more render commands.
- **Horizontal combing** — the partial upload races with the in-flight render command,
  which reads a texture that is only partially written, producing alternating rows from the
  old and new frames.

Both artifacts were confirmed by saving a raw frame to disk (pixel data was clean) and
observing that they disappeared when the image widget was hidden, ruling out pipeline or
stride issues as the cause.

## Decision

Implement the bridge as a **persistent `wgpu::Texture`** updated in-place via
`queue.write_texture`, exposed to iced through a custom `shader` widget
(`iced::widget::shader::Program` / `Primitive` / `Pipeline`).

The texture is allocated once when the first frame arrives (dimensions are not known
before that) and reused for every subsequent frame. `queue.write_texture` overwrites the
pixel data without touching the texture object itself, so the render command always sees a
complete, valid texture. A `generation` counter on `FrameData` lets the GPU upload step
skip frames that haven't changed between UI ticks, avoiding redundant writes.

The appsink callback stores the latest decoded frame as `Arc<FrameData>` in a
`Arc<Mutex<Option<Arc<FrameData>>>>`. The UI thread takes ownership via a reference-count
bump — no pixel copy on the CPU side. The `shader::Pipeline` (`GpuState`) is created once
by the iced runtime and held for the lifetime of the widget; `Primitive::prepare` receives
it by mutable reference on each frame, performs the conditional `write_texture`, and
`Primitive::draw` issues the render-pass draw call into the existing iced render pass.

## Consequences

- Flickering and combing are eliminated; the bridge is validated at 30 fps / 1280×720.
  A sustained 1080p throughput check (using the fps/dropped-frames counters from ADR-0008)
  remains the formal Phase 1 exit gate.
- The `wgpu` crate is now an explicit workspace dependency (version must track
  `iced_wgpu`'s transitive dependency to avoid duplicate instances).
- `Handle::from_rgba` must not be reintroduced for video frames; the symptom (combing +
  flickering) is subtle and might be attributed to the wrong cause.
- The seam described in ADR-0006 is preserved: `bridge.rs` and `video.rs` together are
  still the only files that touch the appsink→GPU path, and swapping to a different
  renderer (Slint or GTK4) still costs only those two files.
