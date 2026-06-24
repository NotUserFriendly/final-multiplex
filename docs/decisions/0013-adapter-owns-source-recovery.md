# 0013. Adapter owns source recovery; core supervises the process

- **Status:** Accepted
- **Date:** 2026-06-24

## Context

ADR-0005 split recovery: the adapter owns source-specific reconnect (RTSP drops,
YouTube re-resolve), the core restarts a dead *process*. Phase 2 added a core
frame-flow watchdog — restart an adapter that produces no frames for N seconds — to
catch an adapter that is alive but wedged.

Against a real RTSP source, that watchdog overrides the adapter's own recovery. When a
stream drops, the adapter reconnects in-process (its job per ADR-0005), but the core sees
no frames and kills it — ungracefully (`SIGKILL`, no RTSP `TEARDOWN`), orphaning the
camera session so the respawned process cannot reconnect until the camera releases the old
session (10–120 s). The user-visible result is that an interrupted stream does not resume.

Separately, ADR-0012 fixes stream topology at the single startup `Ready`: the core wires
shmsrc chains from that one message and never revisits them. A camera offline at startup
(`Ready { false, false }`), an audio pad that appears after the readiness window, or a
topology change across a reconnect therefore leaves a permanently dead chain — the core
has nothing to receive frames that later arrive.

The common root: the contract has no state between "running" and "dead," and no way to
re-establish topology. The core cannot tell a reconnecting adapter from a wedged one, and
cannot adapt when streams appear after startup. Several open BUGS.md items
(resume-after-interruption, offline-at-startup dead tile, late audio pad, respawn
reconnect loop) are all facets of this.

## Decision

The adapter is the sole owner of source recovery; the core supervises only the process and
**defers to the adapter while it is recovering**. This reaffirms ADR-0005 and corrects the
watchdog overreach. It extends the ADR-0012 control channel — 0012's initial handshake is
unchanged; this adds the lifecycle *after* startup:

- **`Reconnecting` (adapter → core),** optionally with an attempt count: "my source
  dropped, I am recovering, do not kill me." While an adapter is in this state, the core
  frame-watchdog does not count it as stalled.
- **The watchdog kills only on:** process death, an `Error` message, or **total silence** —
  no message of any kind for a silence-timeout (a genuinely hung adapter) — never on
  `frames == 0` alone.
- **`StreamsChanged { has_video, has_audio }` (adapter → core):** the core builds or tears
  down the affected shmsrc chains **live** to match. `Ready` remains the one-time PLAYING
  gate at startup; mid-session topology changes use `StreamsChanged`. This resolves
  offline-at-startup and late/added streams.
- **All core-initiated stops are graceful:** send `Shutdown` (or `SIGTERM`) and wait a
  teardown window for the adapter to release its source (RTSP `TEARDOWN`) before `SIGKILL`
  — on the watchdog and restart paths too, not only clean shutdown.

## Consequences

- Reaffirms ADR-0005's division of labor; the core no longer does source recovery.
- The contract now depends on adapters being well-behaved status emitters: an adapter must
  emit `Reconnecting` promptly on source loss or the watchdog may still kill it. A truly
  hung adapter that emits nothing is still caught by the silence-timeout, so the safety net
  remains.
- **The hard part is live topology change.** Adding/removing shmsrc plus compositor and
  audiomixer pads on a PLAYING pipeline is the same dynamic-pad territory that produced the
  Phase-1 stalls. This is the implementation risk to validate carefully, not assume; expect
  it to be the slow part of the work, and treat a stall here as a known hazard rather than a
  surprise.
- Graceful teardown lengthens the kill path by the teardown window — cheap next to the
  orphaned-session reconnect storms it prevents.
- The watchdog's silence-timeout must exceed the adapter's status/metrics cadence (~1 Hz)
  so a healthy or reconnecting adapter is never mistaken for silent.
- The open rtsp/pipeline recovery items in BUGS.md should resolve under this model. The
  GStreamer `shmsrc` poll-fd critical-flood on reset is a separate cleanup-ordering bug and
  stays in BUGS.md.
