# For Review — Adapter stability gaps

Surfaced during Phase 2 hardware testing (cam-77, 2026-06-25).
Three distinct failure modes observed across multiple test sessions.
All share the same root gap: the supervisor handles process death correctly,
but has no watchdog for the case where an adapter is alive and internally
"OK" yet has failed to deliver a usable stream to the core.

---

## Issue 1 — In-process reconnect: "video reconnected" with no StreamsChanged follow-up

The RTSP adapter reconnects its internal source and logs success, but never
emits `StreamsChanged(video=true audio=true)` to the core. The core's chain
stays torn down (removed on the earlier `StreamsChanged(video=false)`). The
tile is blank. Metrics show frames flowing (~0.6 fps via the control channel)
so the adapter appears healthy. Required a manual `kill` of the adapter process
to force a clean supervisor respawn.

```
[rtsp-adapter] video reconnected
[rtsp-adapter] audio reconnected
                                     ← StreamsChanged(video=true) never arrives
```

**Impact:** Blank tile with no automatic recovery. Core and adapter have
diverged on stream state with no detection mechanism.

---

## Issue 2 — EOS loop: connect → EOS → chain removed → reconnect → repeat

After replug, the camera came up but the RTSP stream dropped almost immediately
on each attempt, cycling through chain add/remove repeatedly before (eventually)
stabilising. When the loop coincided with Issue 1, the adapter got permanently
stuck in the broken state.

```
[pipeline] added video chain for 'cam-77'
[reconnect-pts] 'cam-77' first_pts=Some(0:04:04.276323840) pipeline_running=Some(0:04:04.374703360)
[rtsp-adapter] EOS — restarting source
[supervisor] 'cam-77': Reconnecting (attempt 8)
[pipeline] removed video chain for 'cam-77'
[rtsp-adapter] video reconnected
[rtsp-adapter] audio reconnected
                                     ← chain never re-added (Issue 1 triggered)
```

**Impact:** Composes with Issue 1 to produce a permanently blank tile after a
replug where the camera takes time to stabilise its RTSP stack.

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
