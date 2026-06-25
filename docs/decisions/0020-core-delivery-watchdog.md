# 0020. Core delivery watchdog bounds adapter recovery deference

- **Status:** Accepted
- **Date:** 2026-06-25
- **Refines:** ADR-0013.

## Context

ADR-0013 makes the adapter the owner of source recovery and has the core defer during
reconnect. The supervisor watches for process *death* and restarts. Phase-2 hardware testing
surfaced a failure class neither covers: the adapter process is **alive and self-reports
healthy** — telemetry shows frames being produced — yet the core is **not receiving a usable
stream**. Observed instances:

- The adapter reconnects its source internally and logs success, but never emits
  `StreamsChanged(video=true)`. The core tore the chain down on the earlier
  `StreamsChanged(video=false)` and has no reason to rebuild it. Result: a blank tile,
  telemetry still showing ~0.6 fps, and no automatic recovery.
- EOS churn on replug can leave the source stuck, compounding with the above.

ADR-0013's "core defers to the adapter during reconnect" is **unbounded** — if the adapter's
recovery silently fails, the core defers forever, and the failure is invisible to the existing
watchdogs. For a security-camera product, a silent blank tile is the worst failure mode: a
crash gets restarted, silence does not.

## Decision

The core runs a per-source **delivery watchdog** that reconciles what the adapter says it is
delivering against what the core is actually receiving:

- **Adapter-reported production** comes from the existing telemetry channel (ADR-0008).
- **Core-observed delivery** is a frame-arrival counter on the source's receive chain, plus
  whether a chain exists at all.
- **Trigger:** the adapter reports it is producing a stream, but the core is not receiving it —
  no chain built (the `StreamsChanged`-never-arrived case), or a chain present but no frames
  advancing (stalled) — and the divergence persists beyond a configurable timeout. The core
  then **force-respawns the adapter**, routing recovery through the reliable cold-start path.

Two properties make this safe:

- It only fires when the adapter is **actually producing** the stream, so it never respawns
  against a genuinely-absent source — a camera that is truly gone produces nothing, the core's
  empty state is correct, and no respawn loop occurs.
- The same reconciliation signal distinguishes a *stuck* divergence from *in-progress* messy
  recovery: during EOS churn the adapter isn't steadily producing, so the watchdog stays quiet.

This **bounds ADR-0013's deference**: the adapter remains the primary recovery owner, but the
core's deference is backstopped — if delivery doesn't resume within the watchdog window, the
core stops waiting and forces the proven reset.

## Consequences

- Catches the alive-but-not-delivering class (the blank-tile case, the stuck-EOS case, and
  future modes), not just the specific bugs. The specific bugs are still fixed directly so the
  watchdog firing stays rare and exceptional — frequent firing signals an unfixed bug
  underneath.
- Recovery action is force-respawn — heavier than in-process recovery, but the cold-start path
  is proven reliable. A gentler re-query of stream state is a possible future refinement, not
  built now.
- Risk: false respawns if the timeout is too aggressive. Mitigated by the reconciliation
  trigger only holding under genuine divergence, plus a configurable timeout set safely longer
  than the normal recovery/stabilization window.
- Watches **core-observed delivery, never adapter self-report alone** — the failure mode is
  precisely the adapter reporting healthy while not delivering.
- Adds a core-side monitoring component, per-source frame-arrival observation, and a timeout
  config knob. Reuses the ADR-0008 telemetry channel for the production signal; no new protocol
  unless telemetry proves insufficient.
