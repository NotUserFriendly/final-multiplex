# Phase 2.2 — Pre-Phase-3 cleanup (five blocks, do in order)

Mostly-independent cleanup. Work top to bottom, commit each block separately, hand a
near-complete 2.2 back for review. Human-in-the-loop (physical unplug/replug, RTSP) applies to
blocks 2, 4, 5 — set up, then stop and ask the maintainer to act and wait; do not simulate.

---

## Block 1 — Adapter discovery via a search path (ADR-0022)

The core resolves the adapter binary per-launch with no pinned location, so it "hunts" and
different launchers resolve differently. Replace with the ADR-0022 discovery path — and **not**
flat siblings of the core exe; adapters become user-serviceable.

- Resolve adapters from a defined **search path, first match wins**:
  1. Explicit override — `FM_ADAPTER_DIR` env var or config key.
  2. User adapter dir — XDG **data** (not cache): `$XDG_DATA_HOME/final-multiplex/adapters`
     (default `~/.local/share/...`); `%APPDATA%\final-multiplex\adapters` on Windows.
  3. Bundled — a dedicated `adapters/` subdir in the install layout (self-documenting, named).
- Centralize in one resolver the supervisor calls; no ad-hoc per-call paths.
- The build/install populates the bundled `adapters/` dir (a copy step), since they no longer
  just sit next to the exe. Create the user dir on first run (or skip if absent), don't fail.
- **Validation:** launch via `cargo run`, via the built binary, and from a different cwd —
  adapters resolve identically; a binary dropped in the user dir is found and takes precedence
  over the bundled one. CHANGELOG + DoD.

---

## Block 2 — RTSP metrics: real framerate, bad-frame counter, windowed rates

Implements the ADR-0008 "grow telemetry when a concrete need appears" — no new ADR.

- **`fps_in`:** stop reporting the nominal 30; measure the **actual** ingest rate (count buffers
  over a rolling window) and report that.
- **Bad/incomplete-frame counter (RTSP):** distinct from "dropped" — decode errors / corrupt or
  incomplete frames (RTP loss), surfaced from the adapter's decode path.
- **Windowing:** **live** sources report dropped + bad over a rolling interval (e.g. last 60 s)
  so a long stream doesn't show billions; **finite** media keep cumulative totals. Pick by
  source kind.
- **Off-rate canary close-out:** with `fps_in` now real, feed the measured frame period to the
  offset canary's tolerance (drop the 30 fps assumption) and run the previously-skipped off-rate
  canary test.
- **Validation (live RTSP):** `fps_in` reads a true rate **other than 30** on a real camera (a
  30-reading proves nothing — that's the bug relocated); a lossy feed increments bad-frame; live
  rates roll over the window, finite shows cumulative; off-rate canary correct. CHANGELOG + DoD.

---

## Block 3 — UI: output aspect from grid geometry, overlay clamp, visible bounds

- **Output aspect ratio (the 2×1 bug):** a 2×1 grid of 16:9 tiles is 32:9, but the output canvas
  is locked to 16:9, so tiles get crammed/distorted. Derive the **composited output dimensions
  from grid geometry** (cols×rows × tile aspect) so a 2×1 yields a 32:9 canvas. This is a core
  output-sizing fix; the UI display-region aspect-lock (Phase 1) then follows the corrected
  ratio. Not cosmetic — the distortion is at the compositor.
- **Overlay clamp:** the stats/control overlay falls to the bottom of the display area instead of
  staying within its tile. Clamp it to the tile's display region across grid sizes and aspect
  ratios.
- **Visible bounds:** controls (offset, etc.) are clamped in code (`MIN/MAX_OFFSET_MS`, the live
  ceiling per ADR-0016/0017) but the limits are invisible. Surface the allowed range and reflect
  the limit in the steppers (live = `0..ceiling`, file = signed range).
- **Validation:** a 2×1 scene renders at 32:9 with undistorted tiles; the overlay stays on its
  tile; the offset control shows its range and stops at the limit. CHANGELOG + DoD.

---

## Block 4 — Tile chrome + SIGNAL LOST / FILE TERMINATED overlay  (depends on Block 3)

Two related tile-visual changes.

- **Tile chrome:** change the black backdrop under feeds to **~25% gray (mostly black) with a
  white tile border**, so tiles are visually delineated and letterbox bars / black frames /
  empty tiles are distinguishable. The floor color is cosmetic — this does not change ADR-0018's
  decision (a floor exists), only its appearance.
- **State overlay** (translucent ~50%, white text, over the possibly-frozen last frame — an
  overlay, not a backdrop), keyed to per-source state:
  - **SIGNAL LOST** — buildable now: source not delivering (Reconnecting / Error / no frames).
    Maps to existing `IngestState`.
  - **FILE TERMINATED** — finite source at EOS. `IngestState` has no EOS/Ended variant today;
    add a minimal one (or detect EOS) and map it.
  - **PAUSED** — documented hook only; real pause arrives with play/pause (Phase 5).
- **Validation (human-in-the-loop):** tiles show the gray+border chrome; live RTSP → unplug →
  SIGNAL LOST over the frozen frame → replug → clears; a finite file → end → FILE TERMINATED.
  CHANGELOG + DoD.

---

## Block 5 — Adapter reboot control

A per-source UI control to manually 'down' and re-establish a misbehaving RTSP feed, for when a
feed is strange but hasn't tripped the delivery watchdog.

- Wire a **reboot** action (clean teardown → respawn) to the **existing supervisor respawn path**
  the watchdog already uses — no new recovery mechanism. One button is fine.
- During the down phase the tile shows SIGNAL LOST (Block 4).
- Offset and mute must survive the reboot (the reconnect path already preserves them — confirm,
  don't reimplement).
- **Validation (human-in-the-loop):** streaming feed → reboot → down (SIGNAL LOST) → recovers →
  offset and mute intact. CHANGELOG + DoD.

---

When all five are in: hand back a near-complete 2.2. Exit = metrics read true for live RTSP,
adapter launch is deterministic and user-serviceable, output aspect follows the grid, the UI gaps
are closed, and a flaky feed can be manually rebooted. Then tag **0.2.0** (ADR-0021).
