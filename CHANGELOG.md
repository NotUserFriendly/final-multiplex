# Changelog

All notable changes to this project are documented here.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- **Mute state in scene config:** `SourceConfig` now has a `muted: bool` field
  (defaults to `false`).  The mute button reflects the scene's initial mute state
  on launch, and every toggle is persisted back to the scene TOML (same debounced
  flush path as `offset_ms`), so mute state survives app restarts.  The old
  workaround of setting `volume = 0.0` to silence a source is superseded — use
  `muted = true` instead.
- **Adapter reboot control (Block 5):** each external-source tile now has a
  "⟳ Reboot" button that triggers graceful adapter teardown and respawn via
  the existing supervisor path.  Offset and mute survive the reboot (stored
  in `source_layouts`, re-applied by `add_video/audio_chain` on reconnect).
- **Tile chrome + state overlays (Block 4):**
  - Each compositor cell now has three base layers: white fill at zorder 0
    (full cell), ~25% gray inset at zorder 1 (4 px inside each edge), and
    video at zorder 2 (full cell).  When a source is live the video covers
    both layers entirely — no white or gray is visible.  When a source is
    dead the gray tile with white border is exposed in the compositor output,
    so it appears in captured/streamed frames, not only in the live UI.
  - **SIGNAL LOST** overlay (50% translucent black, white text, centered)
    appears when an external source's adapter is reconnecting, restarting, or
    in a failed state.
  - **FILE TERMINATED** overlay appears for a finite file source once
    `fps_in` (per-source, probed at `vcaps:src`) has been silent for
    `compositor_latency_ms + 300 ms`, so the overlay fires after the last
    buffered frame has cleared the compositor rather than while it is still
    being displayed.  `fps_in` now correctly reports 0 after EOS by tracking
    `last_frame_at` per source; it previously held its last measured value
    indefinitely since `on_buffer()` is never called after EOS.
- **Output aspect ratio derived from grid geometry (2×1 bug fix):**
  `scene.grid.width`/`height` are now per-tile dimensions; the compositor
  canvas is computed as `columns × width` × `rows × height`.  A 2×1 grid
  of 1920×1080 tiles now produces a 3840×1080 (32:9) canvas instead of
  distorting each tile into 960×1080 (8:9).  The initial window size is now
  also derived from the canvas aspect ratio, so a 2×1 scene opens at 32:9
  rather than 16:9.
- **Overlay clamped to tile bounds:** the per-source control box now clips
  to its tile so it cannot paint outside the tile boundary at any grid size
  or aspect ratio.
- **Offset stepper bounds visible + enforced in UI:** the allowed range
  (e.g. `0..2000 ms` for a live source, `−60000..60000 ms` for a file
  source) is displayed below the stepper row; stepper buttons are disabled
  when already at the limit.
- **RTSP metrics: real `fps_in`, bad-frame counter, windowed rates:**
  - `fps_in` now reflects the actual camera frame rate (probe on `vconv:sink`,
    pre-`videorate`) instead of the configured/resampled output rate. A camera
    running at 25 fps now reports ~25, not 30.
  - `bad_frames` added to `SourceMetrics`: counts buffers arriving from the
    decoder with `GST_BUFFER_FLAG_CORRUPTED` set (RTP packet loss).
  - Both `dropped_frames` (videorate discards) and `bad_frames` are reported
    over a rolling 60-second window for live (RTSP) sources, so a long-running
    stream does not accumulate unbounded counts.
  - Off-rate canary tolerance now derived from the measured source frame period
    (`frame_period_ms + 100 ms`, floor 150 ms) so cameras running below 30 fps
    no longer trip false WARN lines.
- **Adapter discovery search path (ADR-0022):** adapters are now resolved via a
  defined three-tier search path instead of relying on `$PATH` or cwd.  Order:
  (1) scene `adapter_dir` config key, (2) `FM_ADAPTER_DIR` env var, (3) XDG data
  user dir (`~/.local/share/final-multiplex/adapters` on Linux), (4) bundled
  `adapters/` subdirectory next to the core executable.  Resolution is deterministic
  regardless of launcher or working directory.  The user data dir is created on first
  run if absent.  `Makefile` targets (`make dev` / `make release`) build the workspace
  and populate `target/{debug,release}/adapters/` so the bundled path resolves in dev.

### Changed
- **Transport seam realized (ADR-0019):** The platform-specific element names
  (`unixfdsink`, `unixfdsrc`) are now behind a single `cfg(target_os = "linux")`
  guard each.  Adapter output: `fm_adapter_sdk::transport::make_output_sink()` in
  `fm-adapter-sdk/src/transport.rs`.  Core receive: `make_transport_src()` in
  `fm-core/src/pipeline.rs`.  Both adapters (`fm-dummy-adapter`, `fm-rtsp-adapter`)
  call the SDK function; the core calls the pipeline-local function.  Adding a
  new platform is now one match arm at each seam, not an edit per adapter.
  Behavior-preserving — same elements, same properties, same T3 result.
- **Dummy adapter enriched with decodebin3-like events:** `fm-dummy-adapter` now
  pushes `stream-collection` (with video + audio `GstStream` entries) and `tags`
  events on its output pads when the pipeline first reaches PLAYING — matching the
  event shape a real `decodebin3` source produces.  Transport-payload bugs that only
  manifest on the event path (such as the GDP event deserialization failure) now
  surface on the cheap deterministic path, not only against live cameras.

### Added
- **`delivery_watchdog_ms` config knob (ADR-0020):** `[grid]` section; default 30 000 ms.
  When an adapter reports `fps_in > 0` but the core has no active chain for that source,
  and the divergence persists beyond the timeout, the supervisor force-respawns the adapter
  via the proven cold-start path.  Lower = faster backstop but more false-respawn risk;
  must exceed the normal recovery + RTSP connect window.  Hardware validated at 10 s:
  bark-test (suppressed recovery) → watchdog fires → respawn → chain rebuilt ✓;
  dead-source no-loop (camera absent, `fps_in = 0`) → watchdog silent across 60 s ✓.
- **Offset reconnect canary (`[offset-canary]`):** a permanent, always-on probe on
  `voff_q:src` verifies the applied pad offset matches `source_layouts` on every chain
  rebuild.  Samples 20 buffers after the voff_q fill phase (windowed by `ceiling_ms +
  500 ms` of elapsed running time — framerate-independent, adapts to the configured
  ceiling automatically).  Silent when `|running − pts − expected_offset| ≤ 150 ms`;
  emits one grep-able `[offset-canary] WARN` line per diverging sample.  Validated:
  correct 500 ms offset → silent; simulated apply-bug (0 ms applied, 500 ms expected)
  → deviation 336–420 ms → WARN fires on all 20 samples.  Off-rate source check
  untested (no off-rate source available); the time-window removes the framerate
  dependency.

### Fixed
- **In-process reconnect now emits `StreamsChanged(true)` (Issue 1):** the RTSP
  adapter now emits `StreamsChanged(false,false)` at the start of each reconnect attempt
  (before `sync_state_with_parent`), guaranteeing `last_reported_caps` is `(false,false)`
  when the post-reconnect stability timer fires.  Previously the stability timer
  short-circuited when `last_reported_caps` was already `(true,true)` and the pad had
  re-linked quickly, silently leaving the core chain torn down forever.
  Hardware validated: unplug → 5 backoff attempts → replug → `StreamsChanged(true)`
  → chain rebuilt → offset canary silent (500 ms offset survived).
- **EOS churn: backoff and grace period (Issue 2):** the RTSP adapter now applies
  the same exponential backoff to EOS-triggered restarts as to error-triggered ones,
  preventing a rapidly-EOSing source from hammering `rtspsrc`.  The core supervisor
  holds `StreamsChanged(false,false)` for 3 s (`STREAMS_GRACE_MS`) before tearing down
  the chain; a recovery event within the grace period cancels the tear-down, so a fast
  camera reconnect produces one rebuild rather than a remove+add cycle.
- **Per-source offset and mute survive reconnect:** `transport::set_source_offset` and
  `set_source_mute` now write back to `source_layouts` (via `Pipeline::update_source_layout_offset`
  / `update_source_layout_mute`) in addition to updating the live pads.  Previously,
  `add_video/audio_chain` re-applied the stale TOML value on every chain rebuild, silently
  discarding any UI-set offset or mute state.  Hardware-confirmed (2026-06-25, full Group 3):
  offset set to 500 ms via UI → supervisor respawned adapter → T3 probe steady-state
  418–420 ms (≈ 500 ms − one frame period) vs 1967 ms if reverted to TOML → visual delay
  confirmed vs real life → mute confirmed still active post-reconnect.
- **cam-77 replug — adapter clock seeded with system time; respawn loop eliminated
  (ADR-0005):** Respawned adapter processes consistently failed `GstNetClientClock`
  calibration: `wait_for_sync` timed out every time while cold-start adapters synced in
  <400 ms.  Measurement (Group 1) confirmed no NTP offset was ever applied on respawn —
  `net_clock` stayed at `ZERO + elapsed` even after 60 s.  No UDP sockets for the
  provider port were detectable on respawn, confirming the packets never arrive (not
  "arrive but rejected").  Root cause unknown; may be a GStreamer child-process global
  clock state issue.  Fix: seed `GstNetClientClock::new` with
  `gstreamer::SystemClock::obtain().time()` instead of `ClockTime::ZERO`.  On the same
  machine, adapter and core share the same monotonic clock, so the NetClientClock reads
  ≈correct immediately — `first_pts ≈ pipeline_running_time` confirmed (119 ms gap vs
  8+ minutes before the fix).  Net calibration is still attempted (5 s) for refinement;
  timeout is non-fatal since the seed is load-bearing.  Implements ADR-0005 for the
  same-machine case.  Cross-machine deployments cannot rely on this seed and would need
  the NTP calibration actually working, or PTP (ADR-0005 upgrade path).
- **cam-77 replug — compositor chain rebuilt on adapter restart; 20-second freeze
  eliminated:** On cable replug, the supervisor-respawned adapter produced a new RTSP
  stream starting at PTS≈0.  The compositor's cam-77 sink pad had an established PTS
  timeline from the original session (e.g., 14 min of running time); the aggregator
  stalled waiting for PTS to advance from 0 to the current time — producing 20-second
  freeze / single-frame pulses.  Fix: supervisor always routes adapter-restart `Ready`
  messages through `streams_changed`, causing `build_shmsrc_chain` to tear down and
  rebuild the compositor sink pad.  The restart path now follows the same code as the
  already-working hot-add path (fresh pad with no PTS history).  The `restarted` queue
  and `restart_shmsrc` method are removed.
- **Cold-start: offline source now populates tile on reconnect:** When a source reports
  `Ready(video=false audio=false)` at startup (camera offline), its tile layout (xpos,
  ypos, tile dimensions, offset) was not stored, so `add_video_chain`/`add_audio_chain`
  failed with "no layout" when the source later came online via `StreamsChanged`.  Fix:
  store layout for all configured sources before the no-streams skip.  Validated: cam-77
  offline at startup → cam-27 live and unaffected → cam-77 plugged in → both chains added
  cleanly without stalling cam-27.
- **T3 offset accuracy validated on live RTSP (2026-06-25):** With the unixfd transport,
  voff_q leaky=upstream, and compositor latency=ceiling_ns, both cameras deliver at steady
  30 fps (33 ms frame intervals, no burst-gap pattern).  The n×2800 ms PTS divergence from
  the leaky=downstream bug is confirmed absent.  Measured via T3-COMP probes on voff_q:src
  for cam-27 (offset=0) and cam-77 (offset=2000 ms) over 20 frames each.
- **Synthetic floor inputs (ADR-0018):** a permanent silent `audiotestsrc` (wave=silence,
  volume=0 on mixer pad) now feeds the audiomixer, and a permanent black `videotestsrc`
  (pattern=black, zorder=0) now feeds the compositor.  Both are infrastructure, not sources:
  they are excluded from tile layout, metrics, offset controls, and grid source enumeration.
  They give each GstAggregator a live heartbeat so the pipeline reaches PLAYING in every
  cold-start permutation (video-only cameras, all-sources-absent).  The Play-gate
  (wait_for_playing) remains as a safety net but is no longer the normal path.
- **cam-77 cold-start GST_FLOW_ERROR cascade (root-cause fix):** `send_play_all()` now
  blocks until the GStreamer pipeline confirms it has reached PLAYING state before telling
  adapters to start pushing frames.  Previously, `set_state(Playing)` returned `Async` and
  adapters began pushing while the compositor and audiomixer aggregators were still in their
  async transition — the first buffer push permanently latched `GST_FLOW_ERROR` (-5) on the
  aggregator, and every subsequent push failed silently.  Fix: new `Transport::wait_for_playing(10)`
  calls `pipeline.state(timeout)` to block until `Success` or `NoPreroll` before `send_play_all()`.
  A warning is logged (but startup continues) if PLAYING is not confirmed within 10 s.
- **Instance lock:** a second `final-multiplex` process now refuses to start if another is
  already running, printing the incumbent PID.  Prevents two instances competing for the same
  shmsink sockets and polluting logs.
- **Session log:** stderr is now redirected to `$XDG_RUNTIME_DIR/final-multiplex/{pid}/session.log`
  at startup on Linux.  All subsequent `eprintln!` output from that session (including GStreamer
  warnings) goes to the log file.
- **Platform-selected transport: unixfd replaces shm+GDP on Linux (ADR-0019):**
  `shmsink`/`shmsrc` and `gdppay`/`gdpdepay` removed from all adapters (`fm-dummy-adapter`,
  `fm-rtsp-adapter`) and from all three core receive-chain builders (`build()`,
  `add_video_chain()`, `add_audio_chain()`).  Replaced with `unixfdsink` (adapter side)
  and `unixfdsrc` (core side).  `unixfd` transfers full GstBuffers across the process
  boundary — PTS, DTS, caps, segment, and events — intact, zero-copy via fd passing.
  This eliminates the GDP event deserialization failure (`gst_dp_deserialize_event`
  returning NULL for `decodebin3`/`rtspsrc` events) that blocked live RTSP validation.
  ADR-0015 superseded by ADR-0019.  Controlled T1 PTS measurement confirmed shmsrc
  without GDP does not preserve PTS (frame 0: adapter=2.33 s, core=0; frames 1+: None)
  — settling that question before building.
- **Compositor latency restored (ADR-0016):** `compositor.set_property("latency", ceiling_ns)`
  reinstated in `pipeline.rs` for scenes with external sources.  Had been removed during debugging
  of the cascade (it was not the cause).
- **`voff_q` leaky mode corrected:** `leaky` changed from `downstream` to `upstream` for all
  offset buffer queues.  `leaky=downstream` drops the *oldest* buffered frame when the queue is
  full — the exact frame the compositor waits to consume — causing n×frame_duration PTS
  divergence.  `leaky=upstream` drops the *incoming* frame when the queue is full, preserving
  the buffered delay window.  (The ADR-0016 text says "leaky=downstream"; that text is incorrect
  for a delay buffer and is flagged for review/supersession.)

- **Adapter orphaning on app exit:** adapters no longer survive app termination.
  Four-layer fix: (1) `prctl(PR_SET_PDEATHSIG, SIGTERM)` in both adapters so the
  kernel signals them when the supervisor process dies — covers app `SIGKILL` where
  no app-side code can run; (2) app-side `SIGTERM` handler sets a flag checked in
  the iced `Tick` loop to call `shutdown_all()` before exiting; (3) iced
  `CloseRequested` event now calls `shutdown_all()` via `Message::Exit` instead of
  relying on the default exit path; (4) `Drop` on `Supervisor` as backstop for
  unwind paths. Verified: SIGTERM and SIGKILL of the app both leave zero orphan
  adapter processes; stale runtime dirs from a hard-killed session are reaped at
  next startup.

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
- **Protocol version bump:** `PROTOCOL_VERSION` → 3 (Ready message extended with
  `offset_polarity` and `max_offset_ms`; GDP-framed shm transport; see below).
- **GDP-framed shm transport (ADR-0015):** adapters now wrap each buffer in a GStreamer
  Data Protocol envelope (`gdppay`) before writing to `shmsink`; the core unwraps with
  `gdpdepay` after `shmsrc`.  PTS, DTS, and caps survive the process boundary without
  re-timestamping (`do-timestamp=false` on `shmsrc`).  Eliminates PTS discontinuities
  that caused the compositor to stall after adapter reconnects.
- **Live source offset model (ADR-0016):** a configurable offset-buffer queue
  (`live_offset_ceiling_ms`, default 2000 ms) is inserted after `videoscale` for each
  external source.  The compositor `latency` is set to the ceiling so it waits for the
  most-delayed source.  Positive offsets up to the ceiling are now stable and frame-
  continuous; the n×2800 ms PTS divergence from the pre-fix leaky-queue design is fixed.
- **Adapter-declared capability (ADR-0017):** `AdapterMessage::Ready` now carries
  `offset_polarity` (`positive_only` | `signed`) and `max_offset_ms`.  The core
  reconciles against the ceiling (`effective_max = min(declared_max, ceiling_ms)`) so a
  greedy adapter cannot force an oversized buffer.  Per-source offset controls in the UI
  now reflect these effective bounds: live sources clamp to `[0, effective_max]`, file
  sources keep `[−60 000, 60 000]`.
- **Protocol version bump:** `PROTOCOL_VERSION` previously bumped to 2; now corrected
  history — current version is 3.
- `fm-rtsp-adapter`: credential scrubbing — `user:pass@` is masked in all stderr log
  lines; the raw URI never reaches log output.
- `fm-rtsp-adapter`: emits `StreamsChanged` after each reconnect's stability window if the
  stream set (video/audio presence) differs from what was last reported.

### Fixed
- `fm-rtsp-adapter`: `vshmsink` and `ashmsink` now use `sync=false`.
  `sync=true` caused these transport sinks to pace writes against the pipeline
  clock.  After a partial in-process reconnect (rtspsrc/decodebin3 cycled
  through Null), the fresh RTP session's buffer timestamps did not align with
  the pipeline's accumulated running time (30+ min into a session).  vshmsink
  blocked waiting for timestamps to arrive, creating backpressure all the way
  back to rtspsrc, which stopped consuming the TCP recv buffer — confirmed by
  a stable `recv_q` on the RTSP socket.  The core's compositor handles sync via
  `do-timestamp=true` on vshmsrc; adapter shmsinks are pure transports and must
  not enforce clock synchronisation.
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
- `fm-core/pipeline`: `audiomixer` now always has at least one input — a permanent
  live `audiotestsrc wave=silence` is wired as a seed input at build time.  Without
  it, a scene where every source is video-only or offline at startup left the
  `audiomixer → autoaudiosink` chain with zero inputs; the chain could not negotiate
  audio caps, blocking the pipeline from reaching PLAYING state and preventing video
  from rendering at all.
- `fm-core/supervisor`: adapter process restart with changed stream caps now
  triggers a `streams_changed` notification in addition to the `restarted` reset.
  Previously, if a source was skipped at startup (Ready sent `video=false,
  audio=false`) and its adapter process exhausted all reconnect retries and was
  respawned, the fresh adapter's Ready `video=true, audio=true` updated the status
  map but never pushed to `streams_changed`.  `restart_external_source` was called
  (no-op — no chain existed), and the source remained absent from the display for the
  rest of the session.  Fix: capture previous caps before updating; push to
  `streams_changed` when `is_restart && prev_caps != new_caps`.

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
