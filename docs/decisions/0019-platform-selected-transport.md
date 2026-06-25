# 0019. Platform-selected transport behind the SDK/core seam (unixfd on Linux)

- **Status:** Accepted
- **Date:** 2026-06-25
- **Supersedes:** ADR-0015. Replaces the shm transport *mechanism* of ADR-0011 on Linux
  (ADR-0011's raw-frames decision is retained).

## Context

ADR-0015 chose GDP (`gdppay`/`gdpdepay`) to frame the shm payload so PTS, caps, and segment
survive the process boundary — selected over `unixfd` specifically for **universality**: GDP
makes no OS-specific syscalls, rides any byte transport, and would therefore serve every
platform on the roadmap (Linux → Windows → Mac → corporate Linux) with one transport, avoiding
per-OS transports.

That choice was validated only on the **dummy adapter**, which emits a minimal event stream.
On the real RTSP/`decodebin3` path, GDP on GStreamer 1.28.2 fails: `gst_dp_deserialize_event`
returns NULL for an event type `decodebin3` emits (e.g. `stream-collection` / `stream-start` /
`tags`). Caps deserialize correctly; the first event packet after caps fails with "could not
create event from GDP packet." Synthetic floors, `wait-for-connection`, and a 64 MB ring
buffer were each ruled out — the failure is GDP's deserialization coverage, not a race.

So GDP delivers **transport portability** (runs on every OS) but not **payload portability**
(it cannot carry `decodebin3`'s real event stream). The portability it was chosen for is not
portability usable on the primary platform's real source path.

Re-examining the cost that drove the original decision: a per-OS transport does **not** require
per-OS adapters. The transport endpoints sit at two seams — the SDK's output/sink builder (one
place, inherited by every adapter) and the core's receive-chain builder (one place). A per-OS
transport is therefore **two match arms total**, not a sprawl. And the project ships OS-specific
binaries regardless (packaging, plugin availability, signing), so the build is already branched
by OS; a transport branched by OS is absorbed into a split already being paid for. The
maintenance argument that favored universality is much weaker than it first appeared.

## Decision

- **Transport is platform-selected behind two seams, not universal.** The SDK exposes an
  output builder that selects the platform's sink; the core's receive-chain builder selects the
  matching source. **Adapters request "a video/audio output" and stay OS-agnostic** — they never
  name the transport element. Adding a platform is one match arm at each seam.
- **Linux uses `unixfd`** (`unixfdsink`/`unixfdsrc`): purpose-built to pass `GstBuffer`s between
  processes with full metadata — PTS, DTS, caps, segment, and events — intact, zero-copy via
  file-descriptor passing over a unix-domain socket. It carries `decodebin3`'s event stream
  natively, which is exactly what GDP failed to do, and needs **no framing layer**.
- On Linux this **replaces the shm + GDP path**: `shmsink`/`shmsrc` and `gdppay`/`gdpdepay` are
  removed in favor of `unixfdsink`/`unixfdsrc`. ADR-0011's *raw (unencoded) frames* decision is
  retained; only its shm *mechanism* is replaced, and only on Linux.
- **Windows, Mac, and corporate-Linux transports are deferred** to their roadmap steps. Each is
  a new implementation behind the same two seams, chosen when that platform is built. This is an
  explicit, accepted compromise: **universality is set aside in favor of the purpose-built tool
  per platform**, to get the primary platform behaving correctly now rather than stalling it to
  preserve an option for step two.

## Consequences

- `unixfd` carries PTS/caps/segment/events natively, so per-source offset (ADR-0016), the net
  clock (ADR-0005), and A/V sync work with no framing layer — and the `decodebin3` events that
  broke GDP are carried as a matter of course.
- The transport seam (SDK output builder + core receive builder) is the new abstraction point.
  A new platform changes two match arms and nothing in any adapter's logic.
- `unixfd` is zero-copy (fd passing), so it is likely *faster* than shm + GDP (no serialize /
  copy) — a side benefit, not the reason.
- Linux-only for now; Windows/Mac/corporate-Linux are known gaps behind the seam, each filled at
  its roadmap step. The Windows port becomes a larger, explicit effort rather than a free ride.
- The unix-domain socket fits the ADR-0014 runtime model: socket path under the per-PID runtime
  dir, 0700/0600 perms, credentials never in argv. Minor wiring, no new policy.
- Requires GStreamer ≥ 1.24 for `unixfd` (the dev box is 1.28.2). Confirm
  `unixfdsink`/`unixfdsrc` resolve before building.
- **Process lesson, recorded:** GDP's failure was missed because the dummy adapter emits a
  minimal event stream. The dummy adapter should be enriched to emit `decodebin3`-like events
  (stream-collection, tags) so transport-payload bugs surface on the cheap deterministic path,
  not only against a live camera.
