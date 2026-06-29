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

> *Provenance: items tagged **[build-out]** were added after this plan's earliest snapshot
> (mid-Phase-2), as the work surfaced them — not in the original outline. Tracking before that
> snapshot is imperfect: Phase 1's "Also shipped" extras and Phase 4's fit-mode design notes may
> also be later additions, but history can't confirm it.*

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

### Phase 2.1 — Recovery hardening  *(complete)*  **[build-out]**
- **Deliverable:** the reconnect/recovery path made solid on real hardware, well beyond
  Phase 2's exit bar. Transport moved from GDP-framed shm to **unixfd** (ADR-0019), carrying
  PTS/caps/events natively, behind a per-platform **transport seam** (SDK output builder +
  core receive builder). **Synthetic floor inputs** (ADR-0018) let live aggregators reach
  PLAYING with sources absent; Play is gated on PLAYING (cascade fix). **Live-source offset
  model** (ADR-0016: positive-only, bounded, tile-res buffering, configurable ceiling) with
  **adapter-declared capabilities** (ADR-0017); offset and mute **survive reconnect**
  (`source_layouts` kept in sync with live pads). Adapter clock **seeded with system time**
  so respawned processes timestamp correctly (single-machine); reconnect **rebuilds the
  chain** to clear the stale aggregator timeline. **Delivery watchdog** (ADR-0020) bounds
  adapter-recovery deference; ForReview Issues 1 (missing `StreamsChanged`) and 2 (EOS churn)
  fixed. Permanent **offset reconnect canary**, **test-run isolation** (refuse-launch +
  PID-tied `session.log`), dummy-adapter event enrichment.
- **Exit (met):** unplug/replug a live RTSP camera mid-session at depth — core survives,
  tile recovers, offset and mute persist, no respawn loop; the watchdog backstops a stuck
  divergence and stays silent against a genuinely-absent source. Hardware-validated.

### Phase 2.2 — Pre-Phase-3 cleanup  **[build-out]**
- **Deliverable:** close instrumentation and hygiene gaps before a second adapter type.
  - **RTSP metrics:** fix `fps_in` (pinned at 30 — report the *actual* rate); add a
    bad/incomplete-frame counter for RTSP; window dropped + bad over a rolling interval
    (e.g. last 60 s) for live sources, keep cumulative totals for finite media. Shares the
    "actual source framerate" dependency with the offset canary's tolerance — do them together.
  - **Adapter binary location:** a deterministic, permanent path the core always resolves,
    regardless of who launches it. Removes the per-launch "hunt" and the class of mistakes
    from launching the executable differently across runs.
  - **UI gaps:** clamp the stats/control overlay to its tile's region (currently falls to the
    display bottom); surface the existing per-feature min/max bounds visually (offset etc. are
    clamped in code but invisible to the user).
  - **SIGNAL LOST overlay:** a translucent state indicator over a tile so a dead stream is
    distinguishable from a live black frame. SIGNAL LOST is buildable now (Reconnecting/Error/
    no-frames states exist); FILE TERMINATED needs an EOS/Ended state; PAUSED waits for
    play/pause (Phase 5). Build SIGNAL LOST now, extend as the states arrive.
  - **Adapter reboot control:** a UI button to manually 'down' and re-establish a misbehaving
    RTSP feed (reuses the supervisor respawn the watchdog already drives).
- **Exit:** metrics read true for live RTSP; adapter launch is deterministic; the UI gaps are
  closed; a flaky feed can be manually rebooted.

### Phase 2.3 — Arbitrary and dynamic framerate  **[build-out]**
- **Deliverable:** sources compose at their **native** input rates (RTSP 10–35, YouTube/Twitch
  60, phone/pro cameras 120+) instead of a forced 30. The framerate-dependent code that assumes
  30 — offset-buffer sizing (`frames = ms × fps`), the offset canary's tolerance (frame period),
  the fps metrics — is made rate-correct per source.
- **Output framerate policy (ADR-0023):** a monotonic **ratchet-up high-water mark**. The output
  fixes to the max input rate seen across active sources and never falls back within a session:
  15 in → 15 out; a burst to 35 → 35 out; a fall to 24 leaves output at 35. Renegotiates only on
  a new high, then settles; resets to fresh discovery on scene reload.
- **Exit:** a mixed-rate scene (10 / 30 / 60 / 120) composes correctly; the output ratchets up to
  the active max and holds; canary and offset-buffer math use real per-source rates.
- **Position:** before Phase 3 — YouTube is a 60 fps source, so landing this first means the
  prototype handles native rates instead of downsampling YouTube to 30 and redoing it later.

### Phase 3 — GPU presentation rephase
- **Deliverable:** a GPU presentation path. Each source becomes its own GPU texture at native
  resolution and native rate, composited on the **GPU** (wgpu) instead of baked into one CPU
  frame by the GStreamer compositor. The shared clock (ADR-0005) stays the timing authority —
  what moves is *where* compositing and per-source presentation happen.
- **Additive, not a rip-out.** The GStreamer compositor stays as (a) the **fallback tier** for
  hardware without the GPU path (integrated/corporate, roadmap step 4) and (b) the
  **record / single-output tier** (recording forces a cohesive framerate + one stitched frame,
  which the per-source path otherwise loses). The validated machinery — offset model (0016),
  canary, watchdog (0020), floors (0018) — stays intact on the fallback path while the GPU path
  is built and proven beside it.
- **The hard core — a renderer-side presentation scheduler.** Today the compositor hands you
  frame-accurate alignment for free (all sources in one buffer at time T). Per-source, *you* own
  it: a per-source frame ring-buffer, and at each display refresh, select each source's correct
  frame for **(clock T − its offset)**. The offset *concept* is unchanged (0016: shared clock +
  per-source delay); the *mechanism* moves from `gst_pad_set_offset()` to scheduler
  frame-selection. This is where alignment correctness lives — design carefully, prove first.
- **Build general from day one.** Per-source draw at an **arbitrary rect** (position + size), not
  grid-locked. That rect is the *mechanism* behind focus mode, per-source fit, and layout editing
  — building it general here makes all three cheap drop-ins later. (Those as *features* are
  Phase 5+; the rect + scheduler *mechanism* is here.)
- **Minimal now, full path target.** Minimal milestone: CPU decode → per-source texture upload →
  GPU composite. Target: full zero-copy (hardware decode → dmabuf → wgpu import → GPU composite,
  CPU never touches a pixel) — the discrete-GPU endgame, which folds in the decode-side stutter
  win. Driver/dmabuf dependencies make full-path the target, not the first step.
- **Payoffs (one body of work):** the measured compositor stutter goes (composite off the CPU —
  the 318%-at-4K core load); native per-source res and rate (no resampling to one universal
  rate); and **bad feeds stop dragging good ones** — per-source presentation decouples them, so a
  stalled source freezes alone instead of hitching the shared frame.
- **Exit:** N sources presented at native quality on the GPU path, **frame-accurately aligned**
  (the scheduler proven against a known offset, canary-style); compositor demoted to
  fallback/record tier. Acceptance proof: **draw one source large at native quality, correctly
  aligned** — 80% of focus mode's rendering with none of its UX, which de-risks the whole feature
  line behind it.

### Phase 4 — YouTube adapter (3rd source)
- **Deliverable:** yt-dlp resolver subprocess + stream ingest + periodic re-resolve on
  URL expiry, as an out-of-process adapter. **Lands on the Phase-3 GPU path** — the prototype's
  third source arrives on the final architecture, not a compositor it'd be rebuilt off.
- **Exit:** a YouTube URL plays in a tile and survives URL expiry without manual action.
- **Why here (before focus/fit):** the 3rd source is the make-or-break validation of the
  architecture; deferring it risks building focus/fit on assumptions a new source type breaks.

> **★ Prototype milestone — triggers the Windows build.** Reached when Final Multiplex
> can show **three distinct media types at once** (local file, RTSP, YouTube) and the
> whole scene is **driven from a config file**. This is the end of Phase 4.
> After this, the next goal is a Windows build before adding Phase 5+.

### Phase 5 — Focus mode + per-source fit
> The per-source-rect **mechanism** is built in the Phase-3 GPU path; this phase is the
> **feature** — the UX, transitions, fit shaders, presets. On the GPU path, fit is shader
> sampling and **zoom becomes a texture-coordinate crop** (cheap), retiring the `videocrop`
> question in the notes below — which now applies only to the compositor fallback tier.
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
  - **Focus is triggered by double-clicking a source.** **[build-out]** Double-click promotes a
    source into the focus layout's large tile; double-click again (or an explicit control)
    returns to equal split. The runtime sink-pad geometry control above is what lets this switch
    happen without restarting sources. (The broader layout-editing surface is a later phase.)

### Phase 6 — Manual audio sync (prerecorded)
- **Deliverable:** visible per-source waveform; drag to align a source against the others;
  "play all" honors it. There are **two distinct alignment tools here, not one** **[build-out]** — keep
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

**Interactive layout editing & preset library.** **[build-out]** Direct-manipulation layout:
click-and-drag a source to reorder it within the grid; an "add column" / "add row" menu to grow
the grid at runtime. Named layouts saved and restored through the existing scene system
(ADR-0010) — extend scene persistence from a single layout to a library. Ship a set of **default
layouts and their focused variants** (equal split, focus-with-thumbnails, etc.) as presets.
Double-click-to-focus (Phase 5) is the per-source trigger; this is the editing and preset surface
around it. Fancy UI, post-prototype.

**Cross-machine / distributed deployment.** **[build-out]** Net-clock calibration fails on supervisor-respawned
adapters (the respawn clock-calibration gap): the system-clock seed (ADR-0005, single-machine) is load-bearing on
every reconnect and masks it. Single-machine is unaffected — the system clock is the genuine
shared timebase. Cross-machine deployments cannot rely on the seed. **Trigger:** when core and
adapters run on different machines. Likely resolved by ADR-0005's `GstPtpClock` upgrade path
rather than root-causing the `GstNetClientClock` respawn failure (root cause unconfirmed,
suspected GStreamer child-process clock state).

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
- **Scrub control timing:** **[build-out]** the scrub (per-source file seek, symmetric jumps) is slated
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
- **File offset bound:** **[build-out]** files cost nothing to offset (seekable, not real-time), so they do
  not need the live buffering ceiling (ADR-0016) — but an *unbounded* value invites fat-finger
  errors and offsetting past a clip's own length is meaningless (the source just never overlaps
  the composition window). Recommendation: bound file offset to the **media's duration** where
  known, rather than the live ceiling or infinity. Small ADR-0016 follow-up if adopted; the live
  bound is unchanged.
- **Per-source volume tag (resolved):** **[build-out]** the scene already carries a per-source `volume`
  (default 1.0); `0.0` already produces silence on the audiomixer sink pad, so "0 = mute" holds
  today. Live volume *sliders* remain the separate open question below.
- **shm bandwidth:** **[build-out — evolved from the original "shm payload" question]** the core scales full-resolution frames per source (ADR-0012
  core-owned resize — adapter produces at full grid resolution, core scales to tile).
  At 1920×1080 @ 30 fps that is ~240 MB/s per source over shared memory.  Acceptable on
  the discrete-GPU target; if it bites on integrated-hardware camera walls, add an
  optional per-source production-resolution cap as a launch arg (addable without changing
  the ownership model).  Deferred until it is a measured problem.
