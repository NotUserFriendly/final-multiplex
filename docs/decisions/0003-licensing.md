# 0003. Dual MIT/Apache-2.0 license; quarantine GPL codecs

- **Status:** Accepted
- **Date:** 2026-06-21

## Context

The project is open source and wants to stay maximally usable, including by third-party
proprietary or GPL plugins. GStreamer core is LGPL, which allows this. The real
constraint is codec plugins: `x264enc` links GPL libx264, and `gst-libav` may be GPL —
linking either makes that combined binary GPL.

## Decision

- License our code (core and adapter SDK) dual **MIT OR Apache-2.0**.
- Keep GPL-encumbered elements (`x264enc`, GPL `gst-libav`) out of the default
  distribution; rely on system/hardware/good-tier decoders for RTSP/file paths.
- Keep the adapter SDK permissive so third parties can write GPL/proprietary plugins
  without relicensing the core.

## Consequences

- Anyone can adopt or embed the core under MIT/Apache terms.
- Patent-encumbered codecs (H.264/H.265) are a distribution concern separate from
  copyright; shipping source or using the system install sidesteps most of it.
- A contributor adding `x264enc` to the default build silently relicenses it — guard in
  review.
