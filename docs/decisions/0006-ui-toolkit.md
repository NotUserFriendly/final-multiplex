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

## Consequences

- We own the display bridge by design — it's the replaceable connector and decouples us
  from the stale community crates.
- Pin the iced version; expect occasional migration work.
- Don't depend on `playbin`-based player crates.
- The texture-copy path (esp. on Windows) is the risk to validate early — tracked as a
  Phase-1 exit check in PLAN.md.
