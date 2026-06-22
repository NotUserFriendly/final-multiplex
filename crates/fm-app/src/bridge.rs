// appsink → iced wgpu texture bridge  (ADR-0006, ~100–200 lines)
//
// Pulls `gst::Sample`s from the compositor's AppSink, converts the raw frame
// buffer to an `iced::widget::image::Handle`, and sends it to the UI via a
// channel so the render loop can blit it without touching the GStreamer thread.
//
// This is the deliberate, replaceable seam described in ADR-0006. If the
// texture-copy cost on a given platform is too high, only this file changes.
//
// Populated in Phase 1 implementation.
