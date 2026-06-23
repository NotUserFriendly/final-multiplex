# Plan

## Objective

**Final Multiplex** — a cross-platform, open-source application that composites
arbitrary video/audio sources into a configurable multiplex, with frame-accurate
per-source offset, a master "play all" transport, and crash-isolated source plugins.
Priority use case: an individual watching many streams plus their own security cameras
on a dedicated/high-end box (assume a discrete GPU, not necessarily a strong one).
Secondary: a corp camera wall on integrated hardware. Linux first, Windows next.

## Success criteria

- [ ] Multiple sources arranged in an equal split, then a focus layout.
- [ ] One master transport drives all sources; a known per-source offset visibly shifts
      that source and holds.
- [ ] A source can crash or drop without stalling the mix, and recovers on its own.
- [ ] First real sources working out-of-process: RTSP and YouTube.

## Non-goals (for now)

- Multitrack NLE editing beyond per-source offset + waveform sync.
- Web-page and Twitch inputs (deferred; heaviest dependencies).
- A runtime dynamic-plugin ABI (`.so` drop-in). Sources are out-of-process processes,
  not dynamically loaded libraries — see ADR-0005.

## Phases

Each phase has a deliverable and an exit criterion. Don't start N+1 until N exits.

### Phase 0 — Scaffold  *(complete)*
- **Deliverable:** repo, CLAUDE.md, settings.local.json, .gitignore, licenses, ADRs.
- **Exit:** project builds empty; stack/license/architecture recorded (ADR-0002..0006).

### Phase 1 — In-core compositor proof  *(complete)*
- **Deliverable:** `compositor` + `audiomixer` equal split fed by N looping local-file
  sources, all in-core; master clock + transport (play / pause / seek-all); per-source
  pad offset shifting a source's **audio and video together**; iced UI displaying the
  composited output via the appsink→texture bridge (ADR-0006). Sources and grid declared
  in a **config file** (not hardcoded) — the minimal version of the "configurable" bar.
  Wire **basic per-source counters** (fps in/out, dropped frames, offset-vs-master) on the
  always-on tier (ADR-0008) — these are the instrument for the texture-copy check below.
- **Exit:** N tiles evenly arranged with mixed audio; one transport drives all; offsetting
  one tile by a known number of milliseconds visibly shifts its A/V and stays shifted;
  the same scene reproduces from the config file. Validate the texture-copy path holds
  frame rate at 1080p (the iced risk flagged in ADR-0006), measured via those counters.
- **Why first:** proves the differentiator with zero IPC risk (ADR-0005 sequencing).

### Phase 2 — Process boundary + RTSP
- **Deliverable:** extract a source into a subprocess; core `GstNetTimeProvider` +
  adapter `GstNetClientClock`; `shmsink`→`shmsrc` transport carrying **audio + video**;
  core supervisor with restart/backoff and tile-hold. First out-of-process adapter =
  RTSP, with auto-reconnect. (Test RTSP sources: available — user has multiple.)
  Extend per-source metrics (ADR-0008) with **transport timing at the boundary** (readback
  cost, buffers sent/dropped) — the number that decides shm vs unixfd later.
- **Exit:** kill the RTSP process mid-play — the core survives, the tile holds, the
  source auto-recovers, and A/V sync is intact after recovery.

### Phase 3 — YouTube adapter
- **Deliverable:** yt-dlp resolver subprocess + stream ingest + periodic re-resolve on
  URL expiry, as an out-of-process adapter.
- **Exit:** a YouTube URL plays in a tile and survives URL expiry without manual action.

> **★ Prototype milestone — triggers the Windows build.** Reached when Final Multiplex
> can show **three distinct media types at once** (local file, RTSP, YouTube) and the
> whole scene is **driven from a config file**. This is the end of Phase 3, not Phase 1.
> After this, the next goal is a Windows build before adding Phase 4+.

### Phase 4 — Focus mode
- **Deliverable:** focus layout (one large tile, others arranged around it); switch
  between equal split and focus at runtime.
- **Exit:** runtime toggle works without restarting sources.

### Phase 5 — Manual audio sync (prerecorded)
- **Deliverable:** visible per-source waveform; drag to set the per-source offset (which
  already exists and moves A/V together from Phase 1); "play all" honors it.
- **Exit:** two deliberately desynced clips can be aligned by eye/ear via the waveform.

### Later
Twitch (streamlink), web pages (CEF), ONVIF discovery, text/program-view sources.

## Open questions

- shm payload: raw frames vs encoded — bandwidth vs isolation. Decide at Phase 2.
- Source-adapter SDK crate shape: finalize when Phase 2 makes the contract concrete.
- Runtime decode failure / stall resilience: the current `GstDiscoverer` pre-probe
  skips sources with no readable streams (empty, wrong-format, network-timeout) before
  the pipeline is built. A source whose container headers are valid but whose encoded
  payload fails at decode time will still stall the compositor — uridecodebin errors
  at runtime, leaving an aggregator pad with no data flowing. Fix requires a runtime
  mechanism in the bus-error handler to detect the failing source, flush its aggregator
  pad, and keep the remaining sources playing. Needs investigation into why earlier
  attempts (EOS injection + `release_request_pad`, `force-live` + `ignore-inactive-pads`)
  did not reliably unblock the aggregator.
