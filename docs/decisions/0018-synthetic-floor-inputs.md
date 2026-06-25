# 0018. Synthetic floor inputs keep live aggregators running

- **Status:** Accepted
- **Date:** 2026-06-25

## Context

The cascade fix (ADR-driven Play-gating: send Play to adapters only after the pipeline
reaches PLAYING) exposed a chicken-and-egg. A live `GstAggregator` (`audiomixer`,
`compositor`) only produces output once it has at least one input delivering buffers. When
real sources are absent or not yet flowing, the aggregator never produces, so the pipeline
never reaches PLAYING, so the gate times out. The data that would unblock it does not flow
until the gate releases Play — deadlock.

This is not an edge case:
- A **video-only camera** (the common case — most cameras carry no audio) leaves the
  `audiomixer` with zero real inputs.
- A **camera offline at startup** declares `Ready { video:false, audio:false }`, so neither
  aggregator gets that source.
- **All sources of a type absent** at cold start leaves that aggregator with nothing.

## Decision

Give each live aggregator a permanent **synthetic floor input** that always produces, so the
aggregator always produces and reaches PLAYING regardless of real-source presence.

- **Audiomixer:** a permanent silent `audiotestsrc` (`is-live=true`, silence). Silence is the
  additive identity in the mix — inaudible — and gives the audiomixer a perpetual heartbeat.
  This is required, because a video-only camera leaving the audiomixer with no real input is
  the normal case, not the exception.
- **Compositor:** first verify whether it produces output with zero sink pads (it has
  `background=black`). If it does, no video floor is needed. If it stalls with zero pads, add
  a black floor at the lowest zorder for the all-video-sources-absent cold-start case.

The floor is a bootstrap heartbeat: it provides data → the aggregator reaches PLAYING → the
gate releases → adapters receive Play → real source data flows. The Play-gate stays; the
floor guarantees the pipeline can reach the state the gate waits for. The gate timeout remains
as a rare safety net.

More broadly, synthetic inputs are adopted as a sanctioned technique for keeping the pipeline
alive and well-behaved across absent, rough, or flaky sources. This is expected to recur —
bootstrapping aggregators, holding the pipeline stable while a source drops in and out,
smoothing flaky inputs — not a one-off for the no-audio case.

## Consequences

- The pipeline reaches PLAYING in every cold-start permutation (no sources, video-only, all
  absent); the cascade fix works universally rather than only when a source happens to be
  flowing.
- The Play-gate timeout becomes a rare safety net, not a normal path.
- Permanent low cost: a silent audio source (and possibly a black video floor) always in the
  pipeline — negligible CPU/memory.
- **Floors are infrastructure, not sources.** They are excluded from source enumeration: no
  tile, no metrics, no offset control, not counted in grid layout.
- Floors are permanent — created at build, never torn down, not subject to ADR-0013
  `StreamsChanged` topology changes.
- The audio floor must be true silence (additive identity) so it never colors the mix; the
  video floor, if needed, sits at lowest zorder so any real tile covers it.
- Establishes synthetic inputs as a reusable tool for the flaky-source cases ahead (RTSP
  churn, YouTube re-resolve gaps).
