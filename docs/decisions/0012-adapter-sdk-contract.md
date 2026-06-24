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
The following contract emerged from that implementation.  Recording it here freezes the
wire format so that adapters built by different contributors, in different languages,
remain interoperable without reading the Rust source.

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
| `--video-width` | integer | tile width in pixels |
| `--video-height` | integer | tile height in pixels |
| `--framerate` | integer | frames per second |
| `--base-time` | nanoseconds | core clock snapshot at supervisor start |

`--base-time` lets the adapter align its GStreamer base time to the core pipeline's
without a network round-trip.

### 2. Stream caps (adapter → core, shmsink/shmsrc)

Defined by ADR-0011; recorded here for completeness:

- **Video:** `video/x-raw,format=RGBA,width=W,height=H,framerate=F/1,pixel-aspect-ratio=1/1`
  where W, H, F come from the launch args.
- **Audio:** `audio/x-raw,format=S16LE,rate=48000,channels=2,layout=interleaved`

The core pins a `capsfilter` immediately after each `shmsrc` with these exact caps so
that caps negotiation is deterministic regardless of what the adapter's GStreamer graph
advertises.

### 3. Control channel (stdin/stdout, line-delimited JSON)

**Core → adapter (stdin):** one `Command` JSON object per line, flushed immediately.

```json
{"cmd":"play"}
{"cmd":"pause"}
{"cmd":"shutdown"}
```

**Adapter → core (stdout):** one `AdapterMessage` JSON object per line.

```json
{"msg":"ready"}
{"msg":"metrics","source_id":"…","fps_in":30.0,"dropped_frames":0,…}
{"msg":"error","description":"connection refused"}
```

**Lifecycle invariant:** the adapter must emit `{"msg":"ready"}` only after it has:
1. Slaved the net clock (`wait_for_sync` or equivalent),
2. Created and opened both shmsink sockets.

The core waits up to 10 s for `ready` from all adapters before advancing the core
pipeline to PLAYING, so the shmsrc elements connect to live sockets on the first attempt.

**After restart:** the supervisor sends `play` automatically on every `ready` message
when the pipeline is in the play state, so adapters need not distinguish initial startup
from a supervisor-triggered restart.

**Stderr** is left for human-readable logs and is never parsed by the core.

## Consequences

- Any language that can fork a process, write argv, and do line-buffered JSON on
  stdin/stdout can implement an adapter. The Rust crate `fm-adapter-sdk` provides the
  constants and serde types; bindings or re-implementations are equally valid.
- The `--base-time` flag means adapters can align buffer timestamps to the core clock
  without a clock query roundtrip, which matters for the first few frames at startup.
- Pinning caps on the core side (consequence of ADR-0011) means the adapter's GStreamer
  graph can produce slightly richer caps (e.g. with color-range metadata) without
  breaking negotiation; the capsfilter strips the extra fields.
- The 10 s Ready timeout is a hard upper bound for startup latency. Adapters that can't
  establish their sockets within 10 s will be treated as failed on the first attempt,
  then retried with backoff; there is no way to extend the window.
- `play`/`pause`/`shutdown` are the only commands. Frame-level control (seek, rate
  change) is not exposed over the control channel; offset is applied by the core's pad
  offset (ADR-0004), never communicated to the adapter.
