# 0011. shm transport carries raw frames

- **Status:** Accepted
- **Date:** 2026-06-23

## Context

ADR-0005 chose `shmsink`→`shmsrc` as the media transport across the adapter process
boundary but deferred the payload question: raw decoded frames or encoded (e.g. H.264)?

The options are:

- **Raw frames** — adapter decodes video to RGBA (or NV12) and audio to PCM and writes
  those buffers straight to the shm socket. The core's `shmsrc` reads them with no
  further decode. Simple contract, zero codec dependency in the core.
- **Encoded** — adapter writes a compressed stream (e.g. H.265); core decodes again.
  Lowers inter-process bandwidth but re-introduces a decoder in the core and couples the
  two sides to a shared codec choice.

Relevant constraints:
- Priority target is a dedicated or high-end machine with a discrete GPU (PLAN.md §Objective).
- The shm transport is on localhost — shared memory, effectively a memcpy, no network.
- Phase 1 already decodes in-core; no second decode pass is desirable.
- The adapter contract must stay source-agnostic (ADR-0004/0005). RTSP adapters
  re-encode → decode introduces an extra codec layer that complicates reconnect timing.

Bandwidth at the boundary for reference (worst-case, all raw):
- 1920×1080 RGBA (4 B/px) @ 30 fps ≈ 238 MB/s per source.
- 4 sources at 1080p ≈ 950 MB/s — within the memory bandwidth of any machine running
  a discrete GPU (typically 20–50 GB/s VRAM bandwidth; host RAM bandwidth ≥ 50 GB/s
  on modern hardware).

## Decision

The shm transport carries **raw decoded frames**: video as RGBA (same format the
Phase-1 compositor already accepts) and audio as PCM (S16LE 48 kHz stereo, same as
the audiomixer input).

The adapter's job is: decode its source → shmsink (raw RGBA video, raw PCM audio).
The core's job is: shmsrc → existing compositor/audiomixer chain, unchanged from Phase 1.

Encoded payloads remain a future option; if boundary bandwidth becomes the constraint
(measured via ADR-0008 transport timing), a new ADR supersedes this one and the encoded
path is introduced behind the boundary without touching the core compositor.

Per-source telemetry (ADR-0008) is measured adapter-side and reported on the control
channel. No new ADR required — this is the intended path from ADR-0008.

## Consequences

- The core has zero codec dependency introduced at Phase 2; `shmsrc` output feeds the
  existing compositor chain with no structural changes.
- Adapter contract is simple: produce raw frames in a fixed format. Any language that
  can call GStreamer can implement an adapter.
- Boundary bandwidth is higher than an encoded path. This is acceptable on the target
  hardware; the ADR-0008 transport-timing metric is the future tripwire if it stops
  being acceptable.
- Format choice (RGBA vs NV12) may be revisited for a GPU-upload adapter (NV12 is more
  efficient for hardware decoders); that is a later optimisation, not a now decision.
