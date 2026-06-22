# 0008. Per-source telemetry as part of the adapter contract

- **Status:** Accepted
- **Date:** 2026-06-21

## Context

Performance must be measurable per source, both during development (find the readback
cost, catch regressions, decide shm vs unixfd) and in deployment (a camera-wall operator
needs to see which feed is degraded). Because each adapter owns ingest and decode, the
per-source numbers can only originate there — bolting metrics on at the core would miss
exactly the data that matters.

## Decision

Make per-source telemetry part of the adapter contract, carried on a **control channel
beside the media channel** (the supervisor already needs one per ADR-0005), off the hot
media path so measurement never backpressures frames.

Two tiers:
- **Always-on counters** (cheap, ~1 Hz): ingest state, reconnect count, fps in/out,
  dropped frames, offset-vs-master drift, transport/readback timing, adapter CPU/RSS.
- **Deep tracers** (per-frame, dev-only, toggled): GStreamer's tracer subsystem
  (`latency`, `rusage`, queue levels) plus `tracing` logs.

Reuse what exists: GStreamer QoS messages and tracers supply most metrics; the net clock
(ADR-0005) is the reference for latency/drift. Define the metric schema once in the
adapter SDK crate so every adapter and the UI agree on shape.

## Consequences

- Basic counters land in Phase 1–2 — they *are* the instrument for the transport and
  texture-copy decisions deferred "to a measurement" (PLAN.md).
- Export surface grows by audience, later: a per-tile health overlay for deployment, and
  an optional Prometheus/OpenTelemetry endpoint for 24/7 walls.
- Scope discipline: build the cheap counters + uniform schema early, not a full
  observability stack; grow exports when a concrete need appears.
