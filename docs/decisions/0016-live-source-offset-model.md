# 0016. Live-source offset: positive-only, bounded by a configurable buffer ceiling

- **Status:** Accepted
- **Date:** 2026-06-24

## Context

ADR-0004 puts per-source offset in the core as a pad offset. In Phase 1 (in-core file
sources) the range was ±60 s and worked at any magnitude, because a file source is not
real-time-bound: the decoder produces frames on demand, so delaying presentation costs
nothing.

Once ADR-0015 made PTS cross the boundary, the GDP spike showed offset still failing for
**live** sources. A live source produces frames in real time, so a positive offset —
"present this frame N ms later than it arrived" — requires **holding the frame in a buffer
for N ms**. The buffer must therefore store offset-worth of frames. The existing ±60 s
range would demand up to ~1800 frames/source of raw RGBA (gigabytes) — infeasible. The
spike's 2-frame leaky queue held ~66 ms and leaked everything beyond, so a 2 s offset
compounded as n×~2.8 s divergence.

Live and file offsets are different in kind:
- **Live:** positive-only (you cannot present frames that have not arrived yet), and
  bounded by buffer memory.
- **File:** signed, and effectively unbounded (the source is seekable / not real-time).

## Decision

Treat live-source offset as a bounded, positive-only delay backed by an explicit buffer:

- **Positive-only, bounded.** Live (boundary/adapter) sources get a positive offset with a
  default ceiling of **2000 ms** — enough to align live cameras' latency differences.
  In-core file sources keep the existing signed ±60 s range and Phase-1 semantics.
- **Buffer at tile resolution.** The offset-buffering queue sits after `videoscale`, near
  the compositor sink pad where the offset is applied, so it holds small tile-sized frames,
  not full production-resolution frames. Size it to the ceiling, non-leaky within the
  ceiling, `leaky=downstream` beyond it (the anti-stall safety the original queue provided).
- **Compositor latency** is set to the ceiling so the live aggregator actually waits for the
  most-delayed source.
- **The ceiling is a configurable parameter** (config-driven), defaulting to 2000 ms. A user
  with an extreme case — aligning very out-of-sync feeds, or deliberately "seeing into the
  past" — can raise it and accept the memory cost, rather than being capped by a hardcoded
  limit. Design it as a config value now; surfacing it in the UI is deferred, but raising it
  later must be a setting change, not a rearchitecture.

A live source's offset constraints (positive-only, ceiling) are **declared by the adapter**
in its capability handshake (ADR-0017) and reconciled by the core against its own memory
ceiling — the core does not guess them from source kind. In-core file sources are known to
the core directly and keep the signed range.

## Consequences

- Live offset works within a memory-bounded range; the offset differentiator holds for
  cameras across the process boundary.
- Memory cost is explicit and the user's to spend: at tile resolution, 2 s ≈ 60 frames ≈
  ~120 MB/source worst-case; raising the ceiling scales the buffer linearly. Document this
  wherever the config value lives, so a user raising it understands the cost.
- Live offset is positive-only; the UI must reflect that for live sources (no negative
  offsets, or clamp them). File sources keep the signed range.
- Phase 5 (manual waveform sync of prerecorded clips) uses the file-source range and
  semantics, not the live model — the two are now explicitly distinct.
- A future "see into the past" / instant-replay feature has a natural home: raise the buffer
  ceiling. Noted as the forward path, not built now.
- The offset/queue construction branches on the source's declared constraints (ADR-0017) for
  live sources and core-known defaults for in-core files.
