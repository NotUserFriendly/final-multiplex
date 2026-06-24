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
