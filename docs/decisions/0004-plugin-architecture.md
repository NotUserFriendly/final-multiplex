# 0004. Permissive core owns timing; sources are adapters

- **Status:** Accepted
- **Date:** 2026-06-21

## Context

We want a small permissive core and per-input plugins. The tempting framing — "core just
arranges windows, plugins do the rest" — undersells the core: the differentiators
(master transport, offset, sync) act across sources on one shared clock and timeline, and
cannot live in plugins without breaking sync.

## Decision

Split by responsibility:
- **Core (permissive):** owns *when* and *where* — master clock, transport, offset
  orchestration, compositor layout, global config.
- **Source adapter (plugin):** owns *what comes in* — produces one stream, obeys the
  core's timing. No clock, offset, or layout decisions.

The adapter interface is its own permissive SDK crate (the contributor entry point).

## Consequences

- Adding a source type is bounded; timing bugs stay in one place.
- GPL/proprietary plugins are possible without touching the core's license.
- Invariant to guard in review: offset/clock logic must not leak into an adapter.
- Concrete process and sync mechanism: ADR-0005.
