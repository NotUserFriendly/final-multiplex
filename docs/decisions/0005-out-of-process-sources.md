# 0005. Out-of-process source adapters, synchronized via a network clock

- **Status:** Accepted
- **Date:** 2026-06-21

## Context

The first two sources, RTSP and YouTube, are both unreliable (RTSP drops mid-session;
YouTube URLs expire), so a source fault must not crash the compositor. That makes crash
isolation a primary driver, pointing to a separate process per adapter. The risk:
out-of-process must not break the shared timeline. `ipcpipeline` was evaluated and
rejected — it shares no clock, is playback-oriented, and assumes the sink is master, the
opposite of our many-sources-to-one-compositor topology.

## Decision

Run each adapter as a separate process. Across the boundary:
- **Shared timeline:** core runs a `GstNetTimeProvider`; each adapter slaves via
  `GstNetClientClock` (localhost UDP).
- **Buffers:** adapter `shmsink` -> core `shmsrc`.
- **Per-source offset:** a pad offset in the core (never the adapter), per ADR-0004.

An adapter's contract: *given a net-clock address and a shm socket, slave to the clock
and produce frames.*

Supervision is split: the core restarts a dead adapter with backoff and holds its tile
meanwhile; source-specific recovery (RTSP reconnect, YouTube re-resolve) lives in the
adapter.

## Consequences

- A faulty source can't crash the mix; auto-reconnect becomes a first-class feature.
- Localhost net clock is cheap; `GstPtpClock` is a drop-in upgrade for cross-machine or
  hardware-grade sync.
- Adapters can be written in any language that can slave to the clock and write the shm
  format, widening the contributor pool.
- Build/sequencing (in-core first, then split) is tracked in PLAN.md.
