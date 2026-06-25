# 0015. GDP-framed shm payload to preserve PTS and caps

- **Status:** Superseded by ADR-0019
- **Date:** 2026-06-24

## Context

ADR-0011 carries raw decoded frames over the `shmsink`→`shmsrc` boundary. Phase 2
measurement (the clock-sync findings and the GDP spike) found that a bare `shmsink`/
`shmsrc` pair **does not carry buffer PTS**: with the core `shmsrc` at `do-timestamp=false`
the pipeline became unschedulable (no timestamps at all), and with `do-timestamp=true` the
core re-stamped every buffer with its *arrival* time, discarding the adapter's
clock-coherent PTS.

The consequences were structural: per-source offset diverged across the boundary (offset
applied to arrival-stamped buffers compounds rather than holds), and the net clock
(ADR-0005) was rendered vestigial — the adapter's careful `GstNetClientClock` slaving was
overwritten on arrival. Frame-accurate per-source timing, the entire reason the Phase-2
clock machinery exists, was not actually crossing the boundary.

A spike wrapped the shm payload in the GStreamer Data Protocol (`gdppay`/`gdpdepay`) and
measured the result: adapter PTS equalled core PTS exactly (diff = 0 across all frames),
and A/V lock was preserved. `gdppay` serializes each buffer with its PTS/DTS, caps, and
segment into the byte stream the shmsink writes; `gdpdepay` reconstructs them on the core
side.

## Decision

Frame the shm payload with the GStreamer Data Protocol:

- Adapter: `… → gdppay → shmsink`.
- Core: `shmsrc → gdpdepay → …`, with **`do-timestamp=false`** on the `shmsrc` so the
  PTS restored by `gdpdepay` passes through unmodified.

This **extends ADR-0011** — raw (unencoded) frames over shm still hold; GDP only adds
framing so PTS, caps, and segment survive the boundary. It does not reverse the
raw-vs-encoded decision.

GDP is chosen over `unixfdsink`/`unixfdsrc` because it is transport- and platform-portable:
it rides the existing shm transport and works on the Windows demo target, where unixfd's
unix-domain-socket model does not fit.

## Consequences

- The net clock (ADR-0005) is load-bearing again: adapter PTS is meaningful in the core,
  and the `GstPtpClock` cross-machine upgrade path is real rather than decorative.
- Per-source offset and A/V sync are frame-accurate across the boundary — subject to the
  buffering bound for live sources (ADR-0016).
- The `shmsrc do-timestamp=true` arrival-stamping workaround is removed. `sync=false` on the
  adapter `shmsink` stays: the adapter pushes timestamped buffers as fast as decoded, and
  the core presents per PTS against the shared clock.
- `gdpdepay` restores caps, so the post-`shmsrc` capsfilter is now a redundant assertion —
  keep it only if it agrees with `gdpdepay`'s output, otherwise drop it.
- Small per-buffer serialization overhead (GDP headers + a copy), negligible against raw
  frame size.
- GDP is older than `unixfd` and serializes rather than passing file descriptors. If
  zero-copy fd passing becomes necessary on Linux later, `unixfd` is the upgrade, behind a
  new ADR.
