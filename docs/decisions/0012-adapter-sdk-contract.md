# 0012. Adapter SDK contract: launch args, caps, and control channel

- **Status:** Accepted
- **Date:** 2026-06-23

## Context

ADR-0005 decided *that* adapters are out-of-process and *what* they must do at a high
level (slave to the net clock, write to shmsink, accept per-source offset from the
core).  ADR-0011 decided *what bytes* cross the shm boundary (raw RGBA video +
S16LE PCM audio).  Neither pinned the exact wire format: how the core launches an
adapter, how it tells the adapter where its sockets are, or how the two sides exchange
lifecycle signals.

Phase 2 (Steps 2–3) proved the boundary with a dummy adapter before RTSP code existed.
The TaskBlock2 hardening pass then corrected several details that emerged from the
implementation:

- **Core-owned resize** — tile geometry must not leak into the adapter contract; the
  adapter produces at a fixed production resolution and the core scales.
- **Optional streams** — most RTSP cameras are video-only; forcing audio causes the
  shmsrc to stall on a missing socket.
- **Protocol version** — a version field in `Ready` lets a future v2 adapter fail
  cleanly against a v1 core instead of silently mis-parsing.
- **BASE_TIME constant** — the flag was a hardcoded string in the supervisor, invisible
  from the SDK crate.
- **Configurable Ready timeout** — 10 s was hardcoded; RTSP cold-start can exceed it.

The following contract is the result of Steps 2–3 plus the hardening pass.  Recording it
here freezes the wire format so adapters built by different contributors, in different
languages, remain interoperable without reading the Rust source.

## Decision

We will define the adapter contract across three surfaces:

### 1. Launch arguments (core → adapter, argv)

The core supervisor passes all configuration as named CLI flags.  Constants live in
`fm-adapter-sdk::contract::args` so the spelling is never out of sync between the
supervisor and adapter implementations:

| Flag | Type | Meaning |
|------|------|---------|
| `--clock-addr` | `host:port` | GstNetClientClock endpoint (UDP) |
| `--video-shm` | path | shmsink socket for the video stream |
| `--audio-shm` | path | shmsink socket for the audio stream |
| `--source-id` | string | identifier echoed back in telemetry |
| `--video-width` | integer | **production resolution** width in pixels |
| `--video-height` | integer | **production resolution** height in pixels |
| `--framerate` | integer | frames per second |
| `--base-time` | nanoseconds | core clock snapshot at supervisor start |

**Production resolution vs tile size.**  `--video-width` / `--video-height` are the
resolution the adapter must produce at.  They are **not** the tile size.  The core
defaults both to the full grid output resolution (e.g. 1920×1080 for a 1920×1080 grid),
inserts a `videoscale → capsfilter(tile)` chain after the `shmsrc`, and scales to
whatever tile size it currently needs.  The adapter is never told tile dimensions and is
never reconfigured at runtime (ADR-0004: the core decides when frames present; the
adapter decides what they contain).

This decoupling enables Phase-4 focus mode to scale up a tile from real pixels instead
of upscaling a tile-sized frame.  The cost is that full-resolution frames cross the shm
boundary even for small tiles (see Consequences).

`--base-time` lets the adapter align its GStreamer base time to the core pipeline's
without a network round-trip, which matters for the first few frames at startup.

### 2. Stream caps (adapter → core, shmsink/shmsrc)

Defined by ADR-0011; recorded here for completeness:

- **Video:** `video/x-raw,format=RGBA,width=W,height=H,framerate=F/1,pixel-aspect-ratio=1/1`
  where W, H, F come from the launch args.  `pixel-aspect-ratio=1/1` is required; the
  core's `vshmcaps` capsfilter pins this field so negotiation is deterministic.
- **Audio:** `audio/x-raw,format=S16LE,rate=48000,channels=2,layout=interleaved`

The core pins a `capsfilter` immediately after each `shmsrc` with these exact caps.

### 3. Control channel (stdin/stdout, line-delimited JSON)

**Core → adapter (stdin):** one `Command` JSON object per line, flushed immediately.

```json
{"cmd":"play"}
{"cmd":"pause"}
{"cmd":"shutdown"}
```

**Adapter → core (stdout):** one `AdapterMessage` JSON object per line.

```json
{"msg":"ready","has_video":true,"has_audio":false,"protocol_version":1}
{"msg":"metrics","source_id":"…","fps_in":30.0,"dropped_frames":0,…}
{"msg":"error","description":"connection refused"}
```

**Protocol version.**  The `ready` message carries `protocol_version`.  The current
version is `1` (constant `fm_adapter_sdk::contract::PROTOCOL_VERSION`).  The core logs
an error and does not send `play` to an adapter that reports a mismatched version.  Bump
the constant when the wire format changes in a backward-incompatible way.

**Optional streams.**  `has_video` and `has_audio` tell the core which shm sockets are
active.  The core wires only the pads for present streams (same logic as the Phase-1
`GstDiscoverer` probe).  An adapter that produces video only must set `has_audio: false`
and **must not** create or open the audio shmsink socket.

**Lifecycle invariant:** the adapter must emit `ready` only after it has:
1. Slaved the net clock (`wait_for_sync` or equivalent),
2. Created and opened the shmsink sockets for all declared streams.

The core waits for `ready` from all adapters before advancing the core pipeline to
PLAYING, so the `shmsrc` elements connect to live sockets on the first attempt.  The
wait timeout is configurable in the scene's `[grid]` section
(`adapter_ready_timeout_secs`, default 30 s) because RTSP cold-start can comfortably
exceed 10 s.  `ready` still requires only sockets + clock — not frames flowing.

**After restart:** the supervisor sends `play` automatically on every `ready` message
when the pipeline is in the play state, so adapters need not distinguish initial startup
from a supervisor-triggered restart.

**Stderr** is left for human-readable logs and is never parsed by the core.

## Consequences

- Any language that can fork a process, write argv, and do line-buffered JSON on
  stdin/stdout can implement an adapter. The Rust crate `fm-adapter-sdk` provides the
  constants and serde types; bindings or re-implementations are equally valid.
- Tile geometry never crosses the adapter boundary. Resize stays in the core (a
  `videoscale → capsfilter(tile)` chain per external source). The adapter is simple and
  stable; layout changes (focus mode, per-source fit) require no adapter coordination.
- **shm bandwidth tradeoff:** every source ships full-resolution raw RGBA frames over the
  shm boundary even when displayed in a small tile.  At 1920×1080 @ 30 fps that is
  ~240 MB/s per source.  This is intentional (clean ownership, Phase-4 focus mode scales
  down from real pixels), but it will be the first thing to revisit on integrated-GPU
  hardware with many sources.  The relief valve — if boundary bandwidth becomes a
  measured problem — is an optional per-source production-resolution cap passed as a
  launch arg, addable without touching the ownership model.  Deferred until it is a
  measured problem (see PLAN.md Open questions).
- The `--base-time` flag means adapters can align buffer timestamps to the core clock
  without a clock query roundtrip, which matters for the first few frames at startup.
- Pinning caps on the core side (consequence of ADR-0011) means the adapter's GStreamer
  graph can produce slightly richer caps (e.g. with color-range metadata) without
  breaking negotiation; the capsfilter strips the extra fields.
- **stdout-JSON fragility:** any stray output on the adapter's stdout (e.g. from a
  GStreamer debug print, a library `println!`, or a panic) corrupts the line-delimited
  JSON protocol.  Adapters must ensure all debug/diagnostic output goes to stderr.  The
  correct long-term fix is a dedicated control file-descriptor (not stdout); deferred
  until a concrete breakage is seen in practice (see BUGS.md).
