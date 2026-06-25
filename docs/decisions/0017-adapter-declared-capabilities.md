# 0017. Adapter-declared capabilities; core builds and constrains the UI

- **Status:** Accepted
- **Date:** 2026-06-24

## Context

The core builds the per-source UI (offset controls today; more later). ADR-0016 needs to
know each source's offset constraints — positive-only with a ceiling for live sources, a
signed range for files. The first draft of 0016 had the core *guess* this from source kind
("adapter means live"), a heuristic that breaks the moment a counterexample exists (a file
served through an adapter, or a live source that happens to be seekable).

More generally, the core should not hardcode assumptions about what each source type can
do — the source knows its own constraints. The contract already works this way for streams:
`Ready { has_video, has_audio }` is a capability declaration the core uses to build the
pipeline. Generalizing that pattern — adapters declare their constraints, the core builds
UI from them — is the natural direction.

ADR-0004's invariant must hold throughout: offset and clock *logic* stay in the core; an
adapter must never set an offset or own timing.

## Decision

Adapters declare their per-source control constraints; the core reconciles them with its own
limits, builds the UI, and enforces the result. The adapter **informs**; the core **decides
and enforces**.

Minimal first step (this ADR):
- Extend the `Ready` declaration with offset capability: **polarity** (positive-only vs
  signed) and the source's own **max offset** (ms).
- The core treats the declaration as *information, not authority*. It reconciles the declared
  bounds against its own constraints (the ADR-0016 memory ceiling, user config), computes the
  **effective** bounds, builds and clamps the UI to them, and applies the pad offset. The
  adapter never sets an offset — it only describes what it supports.
- Where no adapter exists (in-core file sources), the core uses its own known defaults
  (signed ±60 s).

Scope is deliberately minimal: offset bounds only, carried in the existing `Ready`
handshake. This is **not** a generic declarative-UI framework. The schema grows one concrete
field at a time as real controls appear — the same discipline as ADR-0008's telemetry
("build the cheap concrete thing; grow when a concrete need appears"). The *direction* —
the core builds per-source UI from adapter-declared capabilities — is the target; offset
bounds are the first step.

## Consequences

- Replaces ADR-0016's source-kind heuristic with a declaration the source owns; the
  live/file distinction is no longer a core guess. This refines ADR-0016.
- ADR-0004 invariant preserved: a *declaration* crossing the boundary is not offset logic
  leaking — an adapter *setting* an offset would be. The adapter describes; the core acts.
- The core remains the authority on absolute limits: an adapter may declare a 10-minute max,
  but the core caps it at its own memory ceiling, so a greedy or misbehaving adapter cannot
  OOM the core.
- Generalizes the existing `Ready { has_video, has_audio }` pattern — capability declaration
  is already load-bearing; this adds a field, not a new mechanism.
- Future controls (volume range, fit modes, scrub availability) can follow the same
  declare → reconcile → build path when they become concrete — not built now.
- `Ready` changes shape, so the protocol version (already in the contract) is bumped.
