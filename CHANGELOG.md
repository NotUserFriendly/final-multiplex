# Changelog

All notable changes to this project are documented here.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- **Adapter-owned recovery (ADR-0013):** adapters now signal in-process reconnects with
  `Reconnecting { attempt }` so the supervisor's frame-flow watchdog does not kill an
  adapter that is legitimately recovering.  Watchdog now kills only on total silence (no
  message of any kind for 30 s) or an `Error` message, never on `fps == 0` alone.
- **Live topology changes (ADR-0013):** new `StreamsChanged { has_video, has_audio }`
  message lets adapters report mid-session stream-set changes (camera reconnects with
  different codec, offline-at-startup source comes online).  Core builds or tears down
  shmsrc chains on the running pipeline without restarting the process.
- **Credential-safe URI delivery (ADR-0014):** source URIs (including credentials) are
  now delivered to adapters via `Command::Configure { uri }` on stdin rather than `--uri`
  argv — credentials no longer appear in `ps` output.  Both adapters wait for `Configure`
  before connecting to their source.
- **Runtime file isolation (ADR-0014):** all shmsink sockets now live under
  `$XDG_RUNTIME_DIR/final-multiplex/{pid}/` (fallback: `/tmp/final-multiplex/{pid}/`)
  with mode `0700`/`0600`.  Startup orphan-reaping removes dead prior-run directories.
  Graceful exit removes the run directory.
- **Graceful teardown on all core-initiated kills (ADR-0013):** watchdog and restart paths
  now send `Shutdown` and wait up to 3 s for the adapter to release its source (RTSP
  TEARDOWN) before force-killing.  Prevents orphaned camera sessions on the respawn path.
- **Protocol version bump:** `PROTOCOL_VERSION` → 2 (wire format changed: new message
  types, `--uri` flag removed from argv).
- `fm-rtsp-adapter`: credential scrubbing — `user:pass@` is masked in all stderr log
  lines; the raw URI never reaches log output.
- `fm-rtsp-adapter`: emits `StreamsChanged` after each reconnect's stability window if the
  stream set (video/audio presence) differs from what was last reported.

### Fixed
- `fm-rtsp-adapter`: `reconnect_count` is now incremented only after the
  `reconnecting` CAS succeeds.  Previously the counter was incremented before
  the `compare_exchange` gate, so the burst of ~5 GStreamer Error events that
  `rtspsrc` emits while tearing down during a reconnect each counted as a
  genuine reconnect attempt.  With `MAX_RECONNECTS = 8`, an adapter could hit
  the limit after as few as two real connection drops (8 teardown-burst errors ≈
  1–2 real reconnects).  The fix gates increment behind the CAS: skipped
  teardown-burst errors never reach the counter.
- `fm-core/supervisor`: frame watchdog now also fires when `has_video=true` but
  `last_frame_at` is `None` for `WATCHDOG_SECS`.  Previously, an adapter whose RTSP
  session was established while the camera was at connection capacity (no video
  allocation granted) would never produce a frame but also never trigger a restart —
  `last_frame_at` stayed `None` so the `if let Some(...)` guard silently skipped it.
  New `running_since` field tracks when the adapter entered Running state; watchdog
  fires on that elapsed time when no frame has ever arrived.
- `fm-rtsp-adapter`: reconnect partial-restart moved to a background thread.
  `rtspsrc.sync_state_with_parent()` performs RTSP DESCRIBE/SETUP during the
  READY→PAUSED transition; when the network is unreachable this blocks for the full
  OS connect timeout (~30 s).  Running it on the main loop stalled the 1 Hz Metrics
  emit, triggering the silence watchdog before recovery could complete.  Fix: sleep +
  state cycling now happen in a `std::thread::spawn` closure; an `AtomicBool` debounce
  flag (`reconnecting`) prevents concurrent reconnect threads on rapid error bursts.
- `fm-rtsp-adapter`: Ready emission no longer holds the `shared` mutex across the
  `emit()` call.  Previously, `emit(Ready)` was called while `shared` was locked; if
  a `pad-added` callback was concurrently building a chain (also holding `shared`),
  the Ready block and all subsequent Metrics were blocked until pad-added finished —
  triggering the silence watchdog spuriously at startup on sources with audio.
- `fm-rtsp-adapter`: `reconnect_count` moved to a separate `AtomicU64` so the
  1 Hz Metrics emit no longer needs the `shared` mutex.  Previously, if a
  `sync_state_with_parent()` call inside `pad-added` stalled, the `shared`
  mutex was held for the duration and Metrics could not emit — triggering the
  silence watchdog spuriously at startup on sources with audio streams.
- `fm-core/supervisor`: silence watchdog threshold raised from 30 s to 60 s.
  30 s was too tight when the camera side takes ~20–25 s to deliver RTSP pads,
  leaving only a 5–10 s margin for Metrics to start flowing.
- `fm-core/supervisor`: `do_spawn()` now resets `last_frame_at` to `None` on
  every spawn.  Previously a stale frame timestamp from attempt N was
  inherited by attempt N+1, causing the 120 s frame watchdog to fire on the
  fresh process based on the prior process's last frame — an unnecessary
  kill-restart cycle.
- `fm-rtsp-adapter`: force TCP-only RTSP transport (`protocols = "tcp"`) — eliminates
  repeated `not-negotiated (-4)` errors from `rtspsrc`'s internal `udpsrc` elements that
  occurred when cameras required TCP but the adapter tried UDP first.
- `fm-rtsp-adapter`: partial pipeline restart on reconnect — only `rtspsrc` and `decodebin3`
  are cycled through NULL; the shmsink chains stay in PLAYING so their sockets remain open
  and the core's shmsrc does not see a socket-closed event during in-process reconnect.
  Previously the full pipeline was cycled, causing shmsrc errors in the core that the
  supervisor never recovered from (it only calls `restart_shmsrc` on process death, not on
  in-process reconnects).
- `fm-rtsp-adapter`: add `videorate` to the video chain before `vcaps` — converts the
  camera's native framerate to the configured grid fps so that the capsfilter downstream
  emits fully-fixed caps.  Without this, removing the framerate field from vcaps caused
  the core's `vshmcaps` to see a framerate range `[0, MAX]` and error with "output caps
  are unfixed".

### Added
- Phase 2 Step 6 — boundary throughput metrics (ADR-0008 always-on tier):
  - Both `fm-rtsp-adapter` and `fm-dummy-adapter` now install a GStreamer
    `BUFFER` pad probe on the `vcaps:src` pad (output of the capsfilter, just
    before `shmsink`).  Each passing buffer increments an `AtomicU64` counter.
    The 1 Hz metrics loop computes `fps_in` from the counter delta, which is
    the "buffers sent" rate across the shm boundary called for in Step 6.
  - `fps_in` in `SourceMetrics` is now populated for both adapters.  The
    supervisor's B7 frame-flow watchdog activates as soon as the first frame
    is seen (`fps_in > 0`), then fires if no frames arrive for 120 s.
  - `dropped_frames` remains 0 — GStreamer's `shmsink` does not expose a drop
    counter via pads or properties.  Measuring true shmsink drops requires
    comparing shmsink's `bytes-written` property against expected byte totals;
    deferred until there is evidence of drops in practice.
  - "Readback cost" (time from shm write to shm read on the core side) is also
    deferred; it requires paired timestamps across the process boundary and will
    be added in a later pass if shm vs. unixfd becomes a real question.
- Phase 2 Step 5 — `fm-rtsp-adapter`: out-of-process adapter that streams one
  RTSP camera into the Final Multiplex compositor via shared memory.
  - Uses `rtspsrc → decodebin3` with dynamic pad-added callbacks so video and
    audio chains are built on the fly when the RTSP server describes the streams.
    No assumptions about codec or number of streams; `has_video` / `has_audio`
    are determined by which pads actually appear.
  - Emits `Ready` after a 3-second stability window from the first decoded pad
    (gives time for a second stream to appear), or after a 30-second hard deadline
    if no pads arrived (emits `has_video=false, has_audio=false` so the core
    can still proceed).
  - In-process reconnect: on `GstMessageError` the pipeline cycles NULL → PLAYING;
    existing shmsink chains are kept in the pipeline so sockets stay open and
    the core's shmsrc does not need to reset.  On reconnect the chains' sink pads
    are re-linked to the new decoded pads from decodebin3.  After 8 failed
    consecutive reconnects the adapter emits `Error` and exits; the supervisor
    restarts the process with backoff.
  - Slaved to the core's `GstNetClientClock` (same as the dummy adapter).
  - `--uri` added to `contract::args` as a typed constant; supervisor passes it
    whenever `SourceConfig.uri` is set on an external source.
  - `scene-step5.toml`: test scene using the two known LAN cameras (`cam-27` and
    `cam-77`) in a 1×2 grid at 1920×1080@30.  `adapter_ready_timeout_secs = 45`
    to accommodate RTSP cold-start.
- Phase 2 TaskBlock2 hardening pass (Groups A–D) — all changes below run before RTSP.
  - **A1 — Optional streams in `Ready`:** `AdapterMessage::Ready` is now a struct
    `{ has_video: bool, has_audio: bool, protocol_version: u32 }`.  The core wires only
    the pads for present streams (same pattern as the Phase-1 discoverer probe), so a
    video-only RTSP camera no longer needs an audio shmsink or shmsrc.
  - **A2 — Core-owned resize (ADR-0012):** `--video-width`/`--video-height` now carry the
    full grid output resolution (the adapter's *production resolution*), not the tile size.
    The core inserts `videoscale → capsfilter(tile)` after each `shmsrc` to scale to tile
    dimensions.  Focus-mode zoom later scales *down* from real pixels instead of upscaling
    a tile-sized frame.  Recorded in ADR-0012 Consequences and PLAN.md Open questions.
  - **A3 — `BASE_TIME` constant in SDK:** `contract::args::BASE_TIME = "--base-time"` is
    now exported from the SDK crate; the supervisor uses it instead of a string literal.
  - **A4 — Protocol version:** `PROTOCOL_VERSION: u32 = 1` constant in
    `fm_adapter_sdk::contract`; carried in every `Ready` message.  The core logs an error
    and withholds `play` from an adapter that reports a mismatched version.
  - **A5 — Caps reconcile:** `VIDEO_CAPS_TEMPLATE` now includes `pixel-aspect-ratio=1/1`;
    the adapter vcaps capsfilter pins this field to match the core's vshmcaps.
  - **B6 — Backoff reset on healthy run:** if an adapter ran for > 60 s before dying, the
    supervisor resets its `restart_count` to 0 so the next restart uses the shortest
    backoff delay rather than capping at 30 s forever.
  - **B7 — Frame-flow watchdog:** if `fps_in` stays 0 for 120 s while an adapter is
    Running + playing (and at least one frame has previously arrived), the supervisor kills
    and restarts it.  The 120 s threshold is generous for RTSP cold-start; once a first
    frame has arrived, prolonged silence is treated as a stall.
  - **B8 — Configurable Ready timeout:** the wait-for-Ready timeout is now a `[grid]`
    field `adapter_ready_timeout_secs` (default 30 s, was hardcoded 10 s).  RTSP
    cold-start can comfortably exceed 10 s.
  - **C9 — `--no-frames` mode in dummy adapter:** `fm-dummy-adapter --no-frames` opens
    shmsink sockets and emits `Ready` but keeps the pipeline in PAUSED so no frames enter
    the shm ring buffer.  Used to test the alive-but-silent RTSP reconnect window before
    any RTSP code exists.
  - **D10/D11 — ADR-0012 revised:** body updated to reflect the corrected contract
    (optional-stream Ready, protocol version, core-owned resize, BASE_TIME constant,
    configurable timeout) and accepted.  Consequences section now records the
    shm-bandwidth tradeoff (full-resolution frames cross the boundary per source; relief
    valve is an optional per-source production-resolution cap, deferred until measured).
  - **D12 — Docs/known limitations:** stdout-JSON fragility added to BUGS.md and
    ADR-0012 Consequences.  PLAN.md Open questions updated with shm-bandwidth note.
- Phase 2 Steps 0–4 complete.
  - Step 4: ADR-0012 — adapter SDK contract. Freezes the three wire surfaces
    (launch args, stream caps, control channel) now that the boundary is proven.
    Any language that can fork a process and do line-buffered JSON on stdin/stdout
    can implement an adapter.
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
