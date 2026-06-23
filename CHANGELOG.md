# Changelog

All notable changes to this project are documented here.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
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

### Changed
### Deprecated
### Removed
### Fixed
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
