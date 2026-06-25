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
