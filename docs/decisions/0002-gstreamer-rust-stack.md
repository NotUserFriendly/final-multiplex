# 0002. Build on GStreamer, written in Rust

- **Status:** Accepted
- **Date:** 2026-06-21

## Context

The app composites heterogeneous sources (RTSP/ONVIF, file loops, YouTube/Twitch, web
pages, program views) with a differentiator: precise per-source offset, a master
transport, and waveform audio sync. Linux-first, Windows next, open source.

Options weighed — fork, OBS plugin, or build on a media framework:
- Forking: open options are ffmpeg/VLC scripts (no sync) or closed commercial
  multiviewers (monitoring only); none provide the differentiator.
- OBS plugin: excellent inputs for free, but it's a live switcher with no master
  timeline, so the differentiator fights its design — and it's GPLv2.
- GStreamer: native multi-stream sync (clock + `gst_pad_set_offset()`), `compositor`
  for the multiplex, cross-platform, LGPL.

## Decision

Build a dedicated app on GStreamer in Rust (`gstreamer-rs`). The bindings are
first-class and maintained, plugins are crates, and memory/thread safety matters for a
long-running daemon handling many unreliable streams. CC writing most of the code
offsets the cost of a less-familiar language.

## Consequences

- Cross-platform expansion is a build target, not a rewrite.
- We own input handling OBS would have given free; heaviest inputs are deferred (PLAN.md).
- C++ stays viable (and mandatory if we ever reverse into an OBS plugin). C#/.NET
  (`gstreamer-sharp`) rejected as less maintained.
