# Known bugs / minor issues

A batch-fix queue. Collect minor issues here as they're noticed, then clear them
in a dedicated pass **after** a feature lands — never mid-feature.

## Scope — what goes here

- **Bugs only:** wrong behavior, visual glitches, papercuts, small correctness gaps.
- **Not architecture.** A decision about *how the system is built* → write an ADR
  (`docs/decisions/`), don't fix it silently here.
- **Not features.** New capability → `PLAN.md`.
- **Not process.** Build/commit gates live in `CLAUDE.md`.

**Escalation rule:** if a "bug" turns out to need an architectural decision, move it
out of this file and into an ADR or PLAN.md. The Phase-1 mixed-source stall is the
cautionary example — it looked like a bug for four fix attempts before the real answer
turned out to be the Phase-2 process boundary. When a fix keeps not working, stop and
ask whether it's actually a bug.

**On fix:** a user-visible bug still gets a `CHANGELOG.md` entry when it's fixed — this
file is the scratchpad, the changelog is the durable record. Move the line to `## Fixed`
below *and* add the changelog entry.

This file is **not** a Definition-of-Done gate. It's a deferred-work queue; don't wire it
into the per-commit checklist.

---

## Open

<!-- One line each. Format:
- [ ] [area] symptom — where it shows / how to reproduce. (YYYY-MM-DD)
Areas: ui, pipeline, transport, metrics, audio, bridge, config, build
-->

- [ ] [ui] skipped (probe-failed) source still shows a tile slot with 0 fps and a
      no-op offset box — cosmetic; the source has no pipeline pads to control. (2026-06-22)
- [ ] [adapter-sdk] stdout-JSON fragility: any stray byte on an adapter's stdout (a
      GStreamer debug print routed to stdout, a library `println!`, or a Rust panic
      backtrace) corrupts the line-delimited JSON control channel and will be logged as a
      parse error by the supervisor.  Adapters must route all diagnostic output to stderr.
      The correct architectural fix is a dedicated control file-descriptor (not stdout),
      deferred until it bites in practice.  Documented in ADR-0012 Consequences. (2026-06-23)
- [ ] [pipeline] GStreamer criticals flood on adapter crash: when an external adapter dies,
      `shmsrc` enters error state with an invalid poll fd.  Setting the element to NULL to
      reconnect triggers `gst_poll_fd_has_error / gst_poll_remove_fd: assertion 'fd->fd >= 0'
      failed` hundreds of times in stderr before cleanup completes.  Non-fatal — core and all
      other sources continue, adapter restarts and shmsrc reconnects — but the noise obscures
      the log.  Root cause: GStreamer's shmsrc does not fully reset its poll set before posting
      the error event, so fd references are invalid by the time we call set_state(Null).
      Fix direction: send a GST_EVENT_FLUSH_START/STOP before the NULL transition, or defer
      the element reset until the fd is confirmed closed. (2026-06-23)

---

## Fixed

<!-- Move items here on fix. Format:
- [x] [area] symptom — fix summary. (fixed YYYY-MM-DD)
-->
- [x] [rtsp-adapter] Reconnect storm after SIGKILL: supervisor-initiated kills now send
      `Shutdown` and wait 3 s for RTSP TEARDOWN before force-killing.  Validated by
      Group F Gate 3: SIGKILL detected, 1 s backoff, respawn, Configure delivered,
      shmsrc reset — no orphaned camera session. (fixed 2026-06-24)
- [x] [rtsp-adapter] Stream did not resume after RTSP interruption: adapter emitted
      `Reconnecting`, supervisor held off, in-process partial restart (rtspsrc +
      decodebin3 only) recovered both video and audio without process death.  Validated
      by Group F Gate 2: iptables DROP → Reconnecting → iptables DELETE → full
      recovery (video + audio). (fixed 2026-06-24)
- [x] [rtsp-adapter] Camera offline at startup leaves a permanently dead tile: adapter
      emits `StreamsChanged { has_video, has_audio }` after each reconnect's stability
      window when the stream set changes; core calls `build_shmsrc_chain` live on the
      running pipeline.  Validated by Group F Gate 1: power-cycle camera → adapter
      reconnects → `StreamsChanged {true, true}` → core adds chains → tile populates.
      (fixed 2026-06-24)
- [x] [rtsp-adapter] Late audio pad excluded after stability window: `StreamsChanged`
      after a reconnect delivers the audio chain live even if audio arrived after the
      Ready stability window.  Original first-startup window unchanged.  Validated as
      part of Group F Gate 1 (StreamsChanged path confirmed working). (fixed 2026-06-24)
