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
- **Also shipped (beyond the exit bar):** per-source static volume in config; in-core
  per-source audio level meters with three-tone display + runtime mute; editable
  per-source offset (typed entry + ms/second steppers, sliders removed); per-tile
  control/debug overlays with the video display region locked to the output aspect ratio.
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

### Phase 4 — Focus mode + per-source fit
- **Deliverable:** focus layout (one large tile, others arranged around it); switch
  between equal split and focus at runtime. **Per-source fit mode**: each source can be
  set to *letterbox* (current default — preserve aspect, bars fill the leftover space),
  *stretch to fit* (fill the tile, ignore aspect), or *zoom to fit* (fill the tile,
  preserve aspect, crop the overflow).
- **Exit:** runtime toggle between equal-split and focus works without restarting sources;
  a source's fit mode can be changed at runtime and takes effect immediately.
- **Design notes:**
  - Fit mode is a per-source property of the **compositor sink pad**, set at runtime —
    the same runtime sink-pad geometry control focus mode needs, which is why the two
    live together. Today's per-source sizing is computed once at build from the grid;
    this phase makes it a live, per-source choice. Build that control once, here.
  - **Zoom != stretch in difficulty.** Stretch is a pad-property change (sink pad
    width/height = tile, no aspect constraint). Zoom (fill + crop, preserve aspect)
    generally needs a `videocrop`/`aspectratiocrop` element per source, or compositor
    pad properties that may not cover true crop — a real design question to settle in
    this phase, not a one-line toggle.
  - **Distinct from the Phase-1 UI aspect-lock.** That locks the *whole composited
    output's* display region to the output aspect ratio in the UI (so overlays align).
    This is per-*source* fit *inside the compositor*. Different layer — don't conflate.

### Phase 5 — Manual audio sync (prerecorded)
- **Deliverable:** visible per-source waveform; drag to align a source against the others;
  "play all" honors it. There are **two distinct alignment tools here, not one** — keep
  them separate:
  - **Sync offset (the Phase-1 pad offset):** shifts *when* a source presents on the
    shared timeline, in small amounts, for A/V alignment. Source-agnostic (a pad offset
    downstream of ingest), so it works for live sources too and survives the Phase-2
    boundary. Its visual feedback is **inherently asymmetric**: delaying a source
    (+offset) shows a brief freeze while presentation catches up; nudging back toward live
    (−offset) is smooth with no visible jump — a pad offset can only ever present
    already-decoded (older) frames, never frames the decoder hasn't produced yet. This is
    correct behavior for a sync primitive, *not* a defect. Do **not** pair it with a seek
    to force symmetric feedback: that reintroduces the file-seek path (and the
    double-offset bug hit in debugging) and breaks for live sources.
  - **Scrub (seek):** jumps *where in the file* a source is playing, in large amounts, to
    navigate a clip. Gives symmetric, immediate visual jumps in both directions — but only
    works for seekable sources (local files), so it **does not exist for RTSP/YouTube**.
    Build it as its own control, explicitly separate from the sync offset; the waveform
    drag is its natural input.
- **Exit:** two deliberately desynced clips can be aligned by eye/ear — fine alignment via
  the sync offset, coarse navigation via the scrub.

### Later
Twitch (streamlink), web pages (CEF), ONVIF discovery, text/program-view sources.

## Open questions

- **Runtime decode failure / stall resilience:** the build-time stall is **resolved** —
  a mixed scene (good sources plus a corrupt or video-only source) previously stalled the
  whole multiplex at "waiting for first frame"; good sources now play while unusable ones
  are skipped (mechanism lives in code + CHANGELOG). **Residual, untested:** a source
  whose container headers are valid but whose *encoded payload* fails at decode time
  passes the `GstDiscoverer` pre-probe and could still stall the compositor at runtime —
  this path has not been reproduced or verified since the fix. The Phase-2 process
  boundary makes it moot for out-of-process sources (a dead adapter can't stall the core),
  so revisit only if it surfaces for an in-core source.
- **Scrub control timing:** the scrub (per-source file seek, symmetric jumps) is slated
  for Phase 5 alongside the waveform, but the desire for it surfaced in Phase 1 — the
  sync offset's asymmetric feedback (no visible jump when nudging back toward live) reads
  as "nothing happened" to a user expecting an immediate undo. The sync offset is working
  as designed; the symmetric-jump expectation is the scrub's job. Slated Phase 5; pull
  earlier only if it becomes real friction before then, and keep it a *separate* control
  from the sync offset regardless.
- **Live volume control:** volume is currently static (set once on the audiomixer sink
  pad at build; mute is a live toggle on that same pad). If/when live volume *sliders* are
  wanted, decide then: keep adjusting the audiomixer sink-pad `volume`, or add a dedicated
  `volume` element per source (a cleaner handle for automation / ducking / fades). Mute
  alone did not force this choice.
- **Audio level measurement location (Phase 2):** level is measured in-core today (a
  `level` element per source, before the mixer). When audio goes out-of-process, decide
  whether the core keeps measuring post-`shmsrc` or the adapter measures and reports on
  the control channel. ADR-0008 says per-source telemetry originates in the adapter, but
  level is a cheap post-decode measurement that's simplest taken at one core-side spot —
  resolve when the Phase-2 contract is concrete.
- **shm bandwidth:** the core scales full-resolution frames per source (ADR-0012
  core-owned resize — adapter produces at full grid resolution, core scales to tile).
  At 1920×1080 @ 30 fps that is ~240 MB/s per source over shared memory.  Acceptable on
  the discrete-GPU target; if it bites on integrated-hardware camera walls, add an
  optional per-source production-resolution cap as a launch arg (addable without changing
  the ownership model).  Deferred until it is a measured problem.
