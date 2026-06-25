# For Review — Adapter stability gaps

Surfaced during Phase 2 hardware testing (cam-77, 2026-06-25).
Three distinct failure modes observed across multiple test sessions.
All share the same root gap: the supervisor handles process death correctly,
but has no watchdog for the case where an adapter is alive and internally
"OK" yet has failed to deliver a usable stream to the core.

---

## Issue 1 — In-process reconnect: "video reconnected" with no StreamsChanged follow-up

**RESOLVED** (commits a1c8be7, 989f79e)

Root cause: the post-reconnect stability timer only emitted `StreamsChanged(true)` if
`last_reported_caps != current`. When the pad re-linked fast enough that the timer saw it
as already linked, and `last_reported_caps` was still `(true,true)` from the previous
successful session, the check short-circuited. `StreamsChanged(true)` was never emitted.

Fix: the adapter now emits `StreamsChanged(false,false)` at reconnect start (before
`sync_state_with_parent`), pinning `last_reported_caps = (false,false)`. The stability
timer then always sees a change and emits `StreamsChanged(true)` on recovery. The core
supervisor holds `StreamsChanged(false)` for 3 s before tearing down the chain, so a
fast reconnect avoids a remove+add cycle.

Hardware validated: unplug → backoff → replug → `StreamsChanged(true)` → chain rebuilt
→ offset canary silent. (2026-06-25)

---

## Issue 2 — EOS loop: connect → EOS → chain removed → reconnect → repeat

**RESOLVED** (commits a1c8be7, 989f79e)

Fixes: adapter EOS path now applies the same exponential backoff as the error path
(was immediate restart). Core now debounces `StreamsChanged(false,false)` for 3 s —
a fast EOS+reconnect completes before the grace period expires, so no chain tear-down
occurs. Hardware validated same session as Issue 1 above.

---

## Issue 3 — GstNetClientClock never calibrates on supervisor respawn

Every supervisor-respawned adapter process timed out on `wait_for_sync`. The
seeded clock (commit a9a7aa1, ADR-0005 same-machine case) is now the
load-bearing path on every reconnect, not a fallback. Net calibration has never
succeeded on a respawned process across all test sessions.

```
[rtsp-adapter] clock sync: net calibration incomplete (5000ms); seeded clock proceeds — same-machine only
```

This fired on every respawn. Cold-start adapters (pid spawned fresh at session
start) synced in ~147ms consistently. Only respawned processes fail.

**Impact:** Cross-machine deployments cannot rely on the seed (ADR-0005 notes
this). If net calibration is permanently broken on respawn for an unknown reason
(suspected GStreamer child-process global clock state — root cause not confirmed),
any cross-machine or PTP path will have the same failure mode.

---

## T3 probe — keep or discard?

The T3 probe is a GStreamer pad probe on `voff_q:src` added inside
`add_video_chain`. On each buffer crossing that pad it computes
`running_time − pts`. In steady state (frames 60–79, after the voff_q fill
phase) this value converges to the active pad offset — because the compositor
creates backpressure: it accepts one buffer at a time per input, so the queue
blocks until the compositor pops the previous buffer, and the compositor pops
at `running_time ≈ pts + pad_offset`.

It fires 21 times per reconnect (frame 0 for the reconnect gap, frames 60–79
for steady state) then goes silent. Total log noise: 21 lines per cable pull.

**Argument for keeping it permanently:** it's a lightweight automatic regression
canary. Every reconnect produces a log entry that confirms the offset actually
applied to the rebuilt chain — not just that the code ran. It caught the
offset-reset bug numerically and confirmed the fix. Without it, a future offset
regression on reconnect is only detectable visually.

**Argument against:** it hardcodes the assumption that the fill phase is done by
frame 60, which holds for offsets up to ~2000ms at 30fps (2000ms / 33ms ≈ 60
frames) but breaks for higher offsets or lower framerates. It also currently
lives in `add_video_chain` as temporary scaffolding with no documentation; if
kept, it needs a clear name and a comment explaining the frame-window assumption.

**Recommendation:** keep it, but anchor the window to the ceiling — sample
starting at `ceiling_ms / frame_period_ms + 4` frames rather than hardcoding 60,
and log a `[T3-offset]` prefix so it's grep-able without being confused with
other probe output.
