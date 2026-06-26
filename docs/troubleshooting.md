# Troubleshooting Log
Purpose — a live scratchpad, not a durable record.
This file is where CC works a hard, active bug in the open: hypothesis, action,
result, repeat. It exists mainly to give the review chat visibility into how a
problem is being approached, so wrong-layer or symptom-only fixes get caught early
instead of shipped.

Lifecycle — ephemeral. The maintainer clears this file once a bug is resolved.
Nothing here is authoritative or permanent. When a bug is actually fixed, the durable
record goes elsewhere:

what shipped → CHANGELOG.md
a deferred or minor bug → BUGS.md
a fix that is really a decision → an ADR in docs/decisions/ (authored in the
review chat, per the working agreement)

Discipline. An attempt is not a fix until a test proves it. Do not mark an entry
"Confirmed fix" without a check that demonstrates it; if a later test disproves it,
amend the entry rather than leaving a false "fixed" behind. A change that clears a
symptom by quietly disabling a property or behavior elsewhere must be flagged as such,
not logged as a clean win — that distinction is the whole reason this log is visible
to review.

Format. One section per bug. Under it: Attempt N — Hypothesis / Action / Result.

---

## Phase 2.3 validation log (native framerate + output ratchet)

### Gate 1 — Mid-session ratchet without freeze

**Test mechanism:** Added `--bump-fps-after SECS --bump-fps-to FPS` to
`fm-dummy-adapter` (live caps change on `vcaps` capsfilter mid-session) and
`extra_args: Vec<String>` passthrough to `SourceConfig`/`LaunchSpec` so scene
TOMLs can inject arbitrary adapter args. Two runs:

**Run 1 — dummy + FNAF2 (2 sources, `scene-gate1.toml`):**
- Mix started at 30 fps (scene configured). FNAF2 declared 50 fps caps at startup →
  ratchet fired immediately to 50. Dummy bumped to 60 fps at t+20 s →
  second ratchet fired: 50 → 60. Two-step escalation in a single session.
- Maintainer confirmed: no freeze, no multi-second stall; fps_out showing ~60 for all
  sources after the bump.

**Run 2 — dummy + FNAF2 + cam-27 + cam-77 (4 sources, `scene-gate1-full.toml`):**
- Same two-step ratchet sequence (30→50 at startup from FNAF2, 50→60 at t+20 s from
  dummy bump) with all four source types live simultaneously.
- Maintainer confirmed: fps ramped smoothly, no freeze across any tile during the
  mid-session 50→60 transition.

**Gate 1 result: PASS.** Live capsfilter renegotiation on a running compositor is
stable. The monotonic high-water mark holds after the bump.

---

### Gate 2 — Offset at 60 fps + reconnect

**Test scene:** `scene-gate2.toml` — single 60 fps dummy source, 1×1 grid,
`live_offset_ceiling_ms = 2000`.

**Pre-reconnect (buffer-sizing gate):**
- Offset exercised: 0 → +1500 ms → 0 → +2000 ms → +1500 ms, held 10 s at +1500 ms.
- Canary was **silent** throughout. No run-dry, no glitch, no drift.
- This confirms the time-based `voff_q` (`max-size-buffers=0`, bounded by
  `max-size-time = ceiling_ns`) is correctly sized at 60 fps for offsets up to the
  2000 ms ceiling. The old frame-count formula (`ceiling_ms * grid_fps / 1000 + 4`)
  would have sized for 30 fps and run dry at ~1280 ms.

**Post-reconnect (Kill + Reboot):**
- Canary fired immediately after the restarted adapter reached PLAYING:

  ```
  [dummy-adapter] WARNING: clock sync timed out
  [reconnect-pts] 'fast' first_pts=Some(0:00:00.000000000) pipeline_running=Some(0:05:32.272849118)
  [offset-canary] WARN 'fast' expected 1500ms got 334772ms (deviation 333272ms tolerance 150ms)
  ```

- Root cause: `NetClientClock::wait_for_sync(5 s)` timed out on the restarted adapter
  instance. With a bad clock, the adapter's videotestsrc produced frames at PTS ≈ 0
  instead of pipeline running time (~332 s). `reconnect-pts` added the full
  `pipeline_running` as a PTS-zero compensation, resulting in a pad offset of
  ~333772 ms instead of the intended 1500 ms.
- This failure is **framerate-independent** — it would occur identically at 30 fps.
  It is a pre-existing bug in the dummy adapter reconnect path, not a Phase 2.3
  regression. Real RTSP cameras are unaffected (they use a source-internal clock, not
  the shared net clock).
- Source also showed frozen video and pegged VU meter after the bad reconnect, consistent
  with frames arriving with PTS misaligned relative to the pipeline's running time.

**Gate 2 result: PARTIAL.** Buffer-sizing half PASS; reconnect half FAIL (pre-existing
dummy-adapter clock-sync bug, not a Phase 2.3 regression). See BUGS.md for the deferred
entry.

---

## Phase 2.2 session log (Blocks 2–5 implementation + bugs found during validation)

### Block 2 — RTSP metrics (fps_in, bad_frames, windowed rates)

**Missing `bad_frames` field in two SourceMetrics initializers**
- `fm-dummy-adapter/src/main.rs` and `fm-core/src/metrics.rs` both constructed
  `SourceMetrics { … }` without the `bad_frames` field added in the SDK.
- Fix: added `bad_frames: 0,` to both.

**Doc-comment warnings in rtsp adapter**
- `///` (doc comments) on local `let` bindings produced rustc warnings.
- Fix: changed to `//`.

**Off-rate canary tolerance**
- Old canary hardcoded 30 fps frame period → false WARNs at 15 fps / other rates.
- Fix: measured `fps_in` is now fed to the canary's tolerance calculation at chain
  build time (`frame_period_ms = 1000 / fps`), replacing the hardcoded assumption.

**Validation result (live RTSP, 2-camera scene)**
- `fps_in` confirmed non-30 on both cameras via temporary `[metrics-dbg]` eprintln
  (added, confirmed, removed — never committed).

---

### Block 3 — Output AR fix, overlay clamp, visible bounds

**2×1 aspect-ratio bug**
- `scene.grid.width/height` were treated as canvas dimensions; they are per-tile.
  A 2×1 grid of 1920×1080 tiles produced a 1920×1080 (16:9) canvas instead of
  3840×1080 (32:9), squashing tiles.
- Fix: hoisted grid geometry before output caps; canvas = `cols×tile_w × rows×tile_h`.
  `grid_ar` in ui.rs updated to match.

**Offset stepper not clamping display on Enter**
- Typing an out-of-range value and pressing Enter left the text field showing the
  invalid value rather than snapping back to the clamped `offset_ms`.
- Fix: added `Message::OffsetNormalise` + `.on_submit()` wiring on the text input;
  handler sets `offset_buf = offset_ms.to_string()`.

---

### Block 4 — Tile chrome + SIGNAL LOST / FILE TERMINATED

**Border layering — three attempts, resolved on third**

Attempt 1 — conditional iced overlay border (border only when `is_dead`):
- Result: worked visually but the border was in the iced overlay layer (above video).
  Maintainer flagged: border must be in the compositor base layer so it appears in
  captured/streamed output, not just the live UI.

Attempt 2 — gutter-expanded canvas (1 px gutters between tiles, white floor visible
  as border in the gaps):
- Result: rendered; maintainer rejected. Gutters mean white lines are permanently
  visible between live tiles. Spec requires no white at all when all sources are live.
  Also polluted source_layouts with gutter-offset xpos/ypos.

Attempt 3 — per-cell compositor layers, no gutters (TaskBlock-block3-chrome.md):
- Canvas stays `cols×tile_w × rows×tile_h` — no gutter expansion, no source_layouts
  changes.
- zorder 0: white solid-color full-canvas floor (was gray).
- zorder 1: ~25% gray inset per cell, `border_w = 4` px inside each corner — always
  present, visible only when video does not cover it.
- zorder 2: video, full cell size — covers z0+z1 entirely when live.
- Removed iced overlay border entirely.
- Result: **confirmed** — live tiles show full-bleed video with no white visible.
  Killing cam27's adapter for 10 s showed gray+white-border tile with SIGNAL LOST
  overlay; recovery cleared both. Validated with 2-RTSP scene.

**SIGNAL LOST detection**
- `signal_lost` flag set when adapter state ≠ Running or `is_reconnecting`.
- Polled via supervisor status handle every ~500 ms (30 ticks at 60 Hz).

**FILE TERMINATED detection**
- `has_ever_had_frames` latched on first non-zero `fps_out`.
- `file_terminated = !is_external && has_ever_had_frames && fps_out == 0.0 && playing`.

---

### Block 5 — Adapter reboot control

**Reboot button wired to existing supervisor respawn path**
- `Supervisor::request_reboot(source_id)` calls `graceful_shutdown_live`; supervisor
  poll detects exit and respawns using the same stored command-line args.
- Offset and mute survive reboot: `source_layouts` holds offset_ns, mute flag lives
  in the transport layer — neither is re-read from scene on respawn.
- Reboot button rendered for external sources only.

**Validation:** SIGNAL LOST appeared during the down phase; source recovered cleanly;
offset and mute were intact after reconnect.

---

### Mute button not reflecting scene mute status

**Symptom:** The mute button (`M` / `[M]`) always started unmuted regardless of whether
the scene intended sources to be muted. Toggling mute in the UI worked for the session
but state was lost on relaunch.

**Root cause — no `muted` field in `SourceConfig`:** `SourceRow.muted` was hardcoded to
`false` at startup. The only way to silence a source at launch was `volume = 0.0`, which
silenced audio via gain but left the mute button showing the wrong state. Mute state was
also not written back to the scene TOML, so it didn't survive app restarts.

**Fix:**
- Added `muted: bool` (`#[serde(default)]`) to `SourceConfig`.
- `SourceLayout.muted` now initialised from `source.muted` instead of hardcoded `false`;
  applied to the audiomixer sink pad for file sources at build time (`mix_sink.set_property("mute", source.muted)`).
- `SourceRow.muted` in ui.rs initialised from `s.muted` instead of `false`.
- `ConfigPersist::set_source_muted()` added; called from `ToggleMute` handler so every
  toggle writes back to the scene TOML (same debounced flush path as offset_ms).

**Validation:** launched 4-tile scene with `muted = true` on all sources — all four
buttons showed `[M]`. Relaunched with cam-77 `muted = false` — only cam-77 showed `M`
and was audible; others showed `[M]` and were silent.

---

### Validation notes — kill-and-recover test mechanics

Approach that worked: kill cam27's adapter PID, use a temp-script hold-loop to
suppress supervisor auto-respawns for 10 s (killing each respawn as it appears),
then exit the loop and let the next respawn run to completion.

Earlier one-liner attempt failed (exit 144): the loop's own cmdline contained the
search pattern `fm-rtsp-adapter.*cam-27`, causing `pgrep -f` to match and kill the
bash process itself. Fixed by writing the loop to `/tmp/hold_dead.sh`.

---

### FILE TERMINATED timing bug (Block 4)

**Symptom:** FILE TERMINATED overlay appeared a few frames before the file stopped
playing — visible on top of still-moving video at the end of the clip.

**Root cause — wrong metric:** `file_terminated` was keyed on `fps_out`, which is
the global compositor output rate (probe on the appsink sink pad). The compositor
keeps running regardless of whether a file source has ended, so `fps_out` never
drops to 0. Fixed to use `fps_in` (per-source, probed on `vcaps:src`).

**Root cause — stale fps never zeros:** `SourceCounter.fps` is only updated inside
`on_buffer()`. After EOS, `on_buffer()` is never called again, so `fps` holds its
last non-zero value indefinitely. Fixed by adding `last_frame_at: Instant` to
`SourceCounter` (updated on every buffer) and checking staleness at snapshot time:
if `last_frame_at.elapsed() > fps_stale_ms`, report `fps_in = 0`.

**Root cause — compositor latency:** `fps_stale_ms` was initially set to a flat
1500 ms. When external sources are present the compositor has a `latency` property
set to `ceiling_ms` (default 2000 ms), which buffers all sources — including file
sources — by up to 2000 ms. With `fps_stale_ms = 1500`, FILE TERMINATED fired
500 ms before the last frame was actually displayed. Fixed: `fps_stale_ms =
compositor_latency_ms + 300` (300 ms margin for downstream pipeline + iced latency).

**Stale fps side-effects audit:**
- `fps_in` shown in per-source stats display (ui.rs:440): correctly reads 0 after
  a file ends — desired.
- `has_ever_had_frames` latch (ui.rs:172): uses `fps_in > 0` to set; once latched
  true, stale→0 has no effect.
- `StreamsChanged` handler (ui.rs:210): reads from `sup.status_handle()` —
  adapter-reported telemetry, not MetricsCollector. Unaffected.
- Supervisor delivery watchdog (supervisor.rs:415, 779): also reads adapter
  telemetry, not MetricsCollector. Unaffected.
No unintended side-effects from the stale zeroing.
