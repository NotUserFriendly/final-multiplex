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
