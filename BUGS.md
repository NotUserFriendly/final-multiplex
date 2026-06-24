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
- [ ] [rtsp-adapter] Camera offline at startup leaves a permanently dead tile: if no
      RTSP pads appear before the 30 s hard deadline, the adapter emits
      `Ready { has_video: false, has_audio: false }` and the core builds the tile with no
      shmsrc chains.  If the adapter then succeeds on an internal reconnect, streams flow
      into shmsink but the core has no shmsrc to receive them — the tile stays black for
      the lifetime of the process.  Fix requires either re-emitting Ready on reconnect and
      the core rebuilding the affected chains, or the supervisor detecting a no-stream Ready
      and restarting the process after a delay to retry discovery. (2026-06-23)
- [ ] [rtsp-adapter] Late audio pad excluded after stability window: the adapter emits
      Ready 3 s after the first decoded pad.  If a camera's video pad appears at T=0 and
      its audio pad at T>3 s, Ready fires with `has_audio: false` and the core never wires
      an audio shmsrc for this session.  Most cameras deliver both pads within ~500 ms so
      the 3 s window is adequate in practice, but it is not guaranteed.  Fix: use a
      per-stream-type "first-pad" timer so the window resets when any new media type
      appears, or extend the window. (2026-06-23)
- [ ] [rtsp-adapter] First respawn after SIGKILL can stick in an in-process reconnect
      loop: the adapter is killed without sending RTSP TEARDOWN, so the camera may hold
      the old session open for 10–120 s.  The new process immediately attempts RTSP PLAY;
      if the camera rejects or drops the connection before caps are negotiated, an
      in-process reconnect fires (1 s delay), then another, until the camera finally
      cleans up.  During this loop the adapter uses near-zero CPU and the tile shows the
      frozen last frame.  After 8 in-process failures the adapter emits Error and exits;
      the supervisor respawns, and that second fresh process typically succeeds.
      Fix direction: add a configurable post-crash startup delay (e.g. 5 s) before the
      first RTSP PLAY so the camera has time to release the old session, or send a
      graceful TEARDOWN on SIGTERM before the supervisor force-kills. (2026-06-24)

---

## Fixed

<!-- Move items here on fix. Format:
- [x] [area] symptom — fix summary. (fixed YYYY-MM-DD)
-->
