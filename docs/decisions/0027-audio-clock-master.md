# 0027. Audio hardware clock is the pipeline master and net-time source

- **Status:** Accepted
- **Date:** 2026-06-30

## Context

ADR-0005 established a net clock (`GstNetTimeProvider` / `GstNetClientClock`) for multi-process
A/V sync, forced as the pipeline clock. The audio sink (`pulsesink`/`alsasink`) runs on the
sound-card hardware clock. With the net clock forced, the sink is a clock **slave** and must
reconcile the rate difference between the net clock and the sound card.

Investigation (2026-06-30) confirmed this is the burst cause and that no sink setting fixes it:
`audiobuffersplit` made the GStreamer chain through `audiomixer.src` gapless, so the burst is
entirely in the sink clock-slave layer. A slave-method sweep (`skew`, `resample`, `none`, on both
`pulsesink` and `alsasink`) only changed the burst *cadence* — none removed it, because none
removes the root tension: the net clock does not track the sound card.

## Decision

When an audio device is present, make the **audio hardware clock the pipeline master**:

- The core's audio sink provides the pipeline clock (audio sinks are high-priority clock
  providers; stop forcing the net client clock on the core pipeline so the audio clock is
  selected, or set it explicitly).
- The `GstNetTimeProvider` serves **that** clock as the net time to adapters. Adapters' net-client
  clocks then track the sound-card clock.
- The audio sink is now its own master — no slave correction, no drift.

**Fallback:** if no audio device is available (headless), fall back to the system/monotonic clock
as master and net source (the prior arrangement). The clock master is conditional on an audio sink
existing.

## Consequences

- Audio drift eliminated at the root; the burst resolves without sink workarounds. The
  `slave-method` / `buffer-time` experiments and the `sync=false` option are no longer needed —
  the sink isn't a slave anymore.
- Adapters, the compositor/record tier, and the GPU scheduler now ride the audio hardware clock
  via net time. Video alignment is preserved because everything still derives from **one** shared
  clock — it's just the audio one now. **Must be validated post-switch** (alignment is the
  regression risk).
- `audiobuffersplit` in the per-source chain stays — it's the correct, independent chain fix.
- **Single machine: correct and complete.** Multi-machine/distributed (future): adapters on other
  machines would track the display machine's audio clock; a multi-output distributed setup needs
  further design — known debt, deferred.
- Startup ordering: the net provider must wrap the audio clock after the sink exists; audio-device
  absence and runtime device change are edge cases to handle (absence → system-clock fallback).

## Relationship

Supersedes the **pipeline-clock-source** choice in ADR-0005 (the net time now wraps the audio
clock rather than the system clock). The net-time-distribution mechanism of ADR-0005 is unchanged.
ADR-0005 gets a one-line forward-pointer.
