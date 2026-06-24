# Changelog

All notable changes to this project are documented here.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Phase 2 Steps 0–3: process boundary with crash isolation proven.
  - Step 3: crash isolation gate confirmed — killing the dummy adapter mid-play
    leaves the core running with all other sources unaffected.  The supervisor
    detects the exit, applies the backoff delay, respawns with attempt=1, the new
    adapter auto-receives Play on its Ready message, and the core pipeline resets the
    `shmsrc` elements so they reconnect to the new adapter sockets.
    Known issue: GStreamer `gst_poll_fd_*` criticals flood stderr during the NULL
    transition of a disconnected shmsrc (see BUGS.md — non-fatal). (2026-06-23)
- Phase 2 Steps 0–2: process boundary with a dummy adapter.
  - ADR-0011: shm transport carries raw decoded frames (RGBA video + PCM audio).
    Resolves the ADR-0005-deferred payload question. Raw = no decode in core, simple
    adapter contract, higher boundary bandwidth — acceptable on the discrete-GPU target.
    Encoded remains a future option behind a new ADR if bandwidth becomes the constraint.
  - Per-source telemetry (ADR-0008) measured adapter-side, reported on the control
    channel. No new ADR required — this is the ADR-0008 intended path.
  - `fm-adapter-sdk`: contract module fleshed out with launch arg constants
    (`contract::args`), boundary caps constants (`VIDEO_CAPS_TEMPLATE`, `AUDIO_CAPS`),
    `Command` (core → adapter), `AdapterMessage` (adapter → core), and JSON
    encode/decode helpers. Control channel is line-delimited JSON on stdin/stdout.
  - `fm-core`: new `net_clock` module wraps `GstNetTimeProvider` (serves the pipeline
    clock over localhost UDP on an OS-chosen port); new `supervisor` module spawns
    adapter processes with the contract launch args, reads their stdout for
    `AdapterMessage` on a per-adapter thread, and restarts dead processes with
    exponential backoff (`[1, 2, 4, 8, 16, 30]` seconds).
  - `fm-core/config`: `SourceConfig.uri` is now `Option<String>` (external sources
    have no URI); new `source_type` field (`file` default, `external` for
    out-of-process adapters); new optional `adapter` field (binary name/path).
  - `fm-core/pipeline`: external sources get `shmsrc` elements (one for video, one
    for audio) wired into the existing compositor/audiomixer chain in place of
    `uridecodebin`. Pad-offset, mute, and metrics probes work identically to
    in-core file sources — the Phase-1 compositor chain is unchanged (ADR-0004).
  - `fm-app`: `Supervisor` is created after `transport.play()` (pipeline clock
    available), adapters spawned once per external source, Play sent to all adapters
    immediately. `Tick` polls the supervisor every ~500 ms. `TogglePlay` sends
    Play/Pause to all adapters in sync with the pipeline state.
  - `fm-dummy-adapter`: new binary crate. Produces `videotestsrc pattern=ball`
    (RGBA) + `audiotestsrc` (S16LE 48 kHz stereo) to `shmsink` sockets at the
    tile dimensions supplied by the supervisor. Slaves to the core's
    `GstNetTimeProvider` via `GstNetClientClock`. Responds to `Play`/`Pause`/
    `Shutdown` on stdin; emits `Ready` and `Metrics` on stdout.
  - `scene-step2.toml`: test scene with 3 local-file sources + 1 external dummy
    source in a 2×2 grid. Run with `final-multiplex scene-step2.toml`.
- scene.toml round-trip persistence (ADR-0010): live offset changes are written
  back to the scene file so a tuned scene reproduces from its config on next
  launch. Writes use `toml_edit` for surgical, format-preserving edits —
  comments, key ordering, and alignment are unchanged; only the affected value
  is rewritten. Writes are debounced (500 ms idle after last change) so rapid
  steppers do not thrash the file. Writes are atomic: temp file in the same
  directory then renamed over the original, so an interrupted write cannot
  corrupt the scene. `ConfigPersist::Drop` flushes any pending dirty state on
  clean exit. `toml_edit` is surfaced as a direct `fm-core` dependency (it was
  already present transitively via `toml`). Scope: `offset_ms`; structure is
  "persist field X for source id Y" so volume drops in later without rework.
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
- Window opens at the correct size for the configured grid aspect ratio so
  no black bars are visible on launch.
### Deprecated
### Removed
- Per-source offset sliders replaced by editable text box + stepper buttons.
### Fixed
- Per-source offset inconsistency: seeks past a source's file duration silently
  returned `Ok` but parked the element at its last frame, triggering an immediate
  EOS loop reset that reset all source positions — appearing as "nothing happens"
  or "resets to beginning." Reverted to `gst_pad_set_offset` on the capsfilter
  source pads (ADR-0004). The pad-offset approach is a compositor-timeline shift
  (no file seek issued) so it is source-agnostic, unaffected by file duration or
  seekability, and survives the Phase-2 RTSP boundary. Pad offset properties
  persist across EOS loop seeks as they are pad-level properties, not pipeline state.
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
