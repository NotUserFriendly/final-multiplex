# Changelog

All notable changes to this project are documented here.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Per-tile overlay controls: all per-source controls (title, filename,
  offset steppers, level meter, mute, fps readout) now live in a semi-
  transparent overlay anchored to the bottom-left of each video tile.
  The video display region is locked to the output aspect ratio so overlay
  cells align with compositor tiles without replicating letterbox math.
  A `tile_rect` layout function computes tile positions from the grid config
  and source index, structured so a future focus-mode layout swaps the
  function without touching the overlay code.
- Editable per-source offset: text box flanked by −10 ms / +10 ms and
  −1 s / +1 s stepper buttons replace the sliders. The offset string
  buffer is held separately from the committed i32 value; the box is only
  overwritten when a stepper fires, not on every keystroke, so mid-edit
  input is never clobbered. Range extended to ±60 000 ms; both text and
  stepper paths clamp to this limit.
- Per-source mute toggle: the audiomixer sink pad per source is stored at
  pipeline build time; `Transport::set_source_mute` sets the pad's `mute`
  property at runtime. The configured volume level is retained; mute and
  volume are independent. A [M] / [M] toggle button appears beside each
  level meter.
- Window resize / open events are subscribed so the video display area
  recomputes its dimensions on every resize.
- Per-source audio level meters in the UI. A GStreamer `level` element
  (`alevel_{id}`, `post-messages=true`) is inserted into each source's audio
  chain (`aconv → aresamp → level → acaps → audiomixer`). The bus loop parses
  the per-channel RMS and peak arrays (max across channels) and stores them in a
  shared `AudioStore`. `MetricsCollector::snapshot` exposes `audio_rms_db` and
  `audio_peak_db` on `SourceMetrics` (floored to `DB_FLOOR = -60.0 dBFS` when
  no data). Each source row in the UI shows a 20-segment LED-style meter driven
  by `audio_peak_db`: green < −12 dB, yellow −12…−3 dB, red ≥ −3 dB.
- Phase 1 in-core compositor: `Pipeline::build` wires N `uridecodebin` sources
  through `videoconvert`/`videoscale`/`audioconvert`/`audioresample` chains into
  GStreamer `compositor` + `audiomixer`; output goes to `appsink` (video) and
  `autoaudiosink` (audio). Equal-split tile grid computed from `scene.toml`.
- `Transport`: play / pause / `seek_all` / `set_source_offset` (pad offset on
  the capsfilter source pads feeding compositor/audiomixer, shifting A/V
  together — ADR-0004). Dedicated bus-loop thread handles EOS → seek-to-zero
  for continuous looping.
- `MetricsCollector`: BUFFER pad probes on capsfilter source pads (`fps_in`)
  and appsink sink pad (`fps_out`); QoS upstream events counted as
  `dropped_frames` (ADR-0008 always-on tier).
- `bridge` + `video`: `AppSink` callback writes RGBA frames as `Arc<FrameData>`
  into a `Arc<Mutex<Option<Arc<FrameData>>>>` store; the UI reads the latest
  frame via a reference-count bump. Display uses a persistent `wgpu::Texture`
  updated in-place via `queue.write_texture` through iced's `shader` widget
  (ADR-0006 appsink→texture seam, ADR-0009).
- `fm-app` UI: tile grid video display, Play/Pause button, per-source offset
  sliders (−5000 ms … +5000 ms) with live fps/dropped metrics readout; 60 fps
  `iced::time::every` subscription drives the frame and metrics refresh.
- Scene loaded from a TOML file (path from `argv[1]` or `scene.toml`).
- Cargo workspace with three crates: `fm-adapter-sdk`, `fm-core`, `fm-app`
  (binary `final-multiplex`).
- `fm-adapter-sdk`: `SourceMetrics` schema and `IngestState` enum (ADR-0008);
  `contract` module stub for the Phase 2 adapter trait (ADR-0005).
- `fm-core`: TOML scene config types (`SceneConfig`, `GridConfig`, `SourceConfig`)
  with `config::load` (ADR-0007); `Pipeline`, `Transport`, and `MetricsCollector`
  skeletons with documented `todo!()` stubs for Phase 1 implementation.
- `fm-app`: iced `App` skeleton + `bridge` module stub for the appsink→texture
  path (ADR-0006).
- Per-source static volume via `volume` field in `[[source]]` config blocks
  (linear scale: 0.0 silent, 1.0 unity, >1.0 amplifies). Applied to the
  `audiomixer` sink pad at pipeline build; omitting the field defaults to 1.0.
- Phase 1 exit gate confirmed: 4-source 1920×1080 @ 30 fps scene sustained
  fps_out ≈ 30 (sub-frame jitter only) and dropped_frames near zero,
  confirming the `queue.write_texture` path does not bottleneck compositor
  output at 1080p (ADR-0006 risk item).

### Changed
- Per-source offset is now seek-based (moves the file read head) rather than
  compositor-timestamp-based (`gst_pad_set_offset`). The old approach shifted
  when frames appeared on the compositor timeline without changing what content
  was shown; the new approach causes the video to visibly jump to the requested
  position, as expected from a multi-camera sync tool. Uses `ACCURATE` seek flag
  so the position is exact rather than snapping to the nearest keyframe.
- Offset range changed from ±60 s to 0–600 s (10 minutes). Negative offsets
  are not meaningful for file-position seeks and are now rejected at all entry
  points (text input, steppers, config load).
- Window opens at the correct size for the configured grid aspect ratio so
  no black bars are visible on launch.
### Deprecated
### Removed
- Per-source offset sliders replaced by editable text box + stepper buttons.
### Fixed
- `transport`: audio level meters now light up correctly. The GStreamer `level`
  plugin posts peak/rms values as `G_TYPE_VALUE_ARRAY` (`GValueArray`), not
  `GST_TYPE_ARRAY`. The previous `get::<gstreamer::Array>()` call silently
  returned `Err` on every message, so `parse_level_array` always returned
  `DB_FLOOR` and no segments lit.
- `transport` / `metrics`: audio level meters now return to floor when a source
  stops playing. `AudioLevel` entries are timestamped; `snapshot()` treats any
  entry older than 300 ms (3× the 100 ms `level` interval) as stale and floors
  the meter. This handles individual source EOS, sources of unequal length,
  pause, and error without depending on pipeline-level EOS timing.
- `video` / `ui`: resizing the window no longer stretches the video.
  The vertex shader now applies a per-frame letterbox/pillarbox scale
  uniform (written via `queue.write_buffer` every prepare call) so the
  composited output is always drawn at its native aspect ratio.  The
  shader widget is wrapped in a black container so the bar areas are
  filled rather than transparent.
- `pipeline`: corrupt or unreadable sources (empty file, wrong format, network
  timeout) no longer stall the compositor. A `GstDiscoverer` pre-probe runs for
  each source before the pipeline is built; sources with no detectable streams
  are skipped entirely — no uridecodebin, no aggregator pads, no error event that
  could block the pipeline's async state change. Sources confirmed as video-only
  get a compositor pad but no audiomixer pad; the idle audio chain is not added,
  preventing a dangling unlinked chain in the pipeline. Remaining sources continue
  playing normally with their tiles composited; a skipped source's tile shows the
  compositor background colour (black by default).
- `SourcePads` fields are now `Option<gstreamer::Pad>` to reflect that a source
  may have video, audio, both, or neither; `set_source_offset` and metrics probes
  skip absent pads gracefully.
- `bridge` + `video`: replaced `iced::widget::image::Handle::from_rgba` (which
  destroys and recreates the GPU texture on every frame) with a persistent
  `wgpu::Texture` updated in-place via `queue.write_texture`. Eliminates the
  delete→re-upload gap that caused per-frame flickering and the partial-upload
  race that produced horizontal combing artifacts.
- `bridge`: RGBA row copy now reads stride from `VideoInfo::from_caps` and
  copies row-by-row when stride > width×4, preventing corrupted frames on
  odd tile widths where GStreamer pads rows to an alignment boundary.
- `bridge`: bounds-check buffer length against `stride × (h−1) + row_bytes`
  before slicing; a short/truncated buffer now returns `FlowError::Error`
  (dropped frame) instead of panicking the streaming thread.
- `pipeline`: `gst_pad_set_offset` moved from compositor/audiomixer sink pads
  to the capsfilter source pads that feed them — the only side where GStreamer
  guarantees reliable offset behaviour. Eliminates startup warnings and makes
  per-source offset sliders actually take effect.
### Security

<!--
Move items out of [Unreleased] into a versioned section on release, e.g.:

## [0.1.0] - 2026-06-21
### Added
- Initial project scaffold.
-->
