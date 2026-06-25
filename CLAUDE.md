## Definition of done (run before every commit)
A task is not done until ALL of these are true:
- [ ] `cargo fmt --check` and `cargo check --workspace` pass
- [ ] CHANGELOG.md has an entry under `[Unreleased]` if the change is user-visible or alters behavior
- [ ] No Accepted ADR was edited (see Process rules)
- [ ] New architectural decision? Flagged for the review chat — not authored or edited here
- [ ] Commit message states what changed and why

## Claude Code UI/UX Behaviors
- Before starting any multi-step task, create a todo checklist of all steps, then mark each in_progress → completed as you go.

## Process rules
- **ADRs are immutable once `Status: Accepted`.** Never edit the body of an accepted ADR.
  To change, reverse, or extend a decision, write a NEW ADR that supersedes it and set the
  old one's status to `Superseded by ADR-XXXX`. The only edit permitted to an accepted ADR
  is a one-line status change pointing at its successor.
- **Implementation detail does not go in ADRs.** ADRs capture the decision and its rationale.
  Code lives in code; what-changed lives in CHANGELOG.md. Link, don't duplicate.
- When implementation teaches you something that changes a decision, that's a signal to write
  a new ADR — not to quietly amend the old one.

# Project memory — Final Multiplex

> **Final Multiplex** composites arbitrary video/audio sources into a configurable
> multiplex with frame-accurate per-source offset and a master "play all" transport.
> Two use cases: an individual watching many streams + their own security cameras at
> once (the **priority** — assume a dedicated or high-end box with a discrete GPU), and a
> corp security-camera wall on integrated hardware. Open source, Linux-first, Windows next.

## Steering documents

- **`docs/PLAN.md`** — current objective, phases, and exit criteria. Read it before starting work; it is the source of truth for *what* to build next.
- **`CHANGELOG.md`** — running record of shipped changes (Keep a Changelog format).
- **`docs/BUGS.md`** — deferred and known bugs; log issues here when deferring a fix.
- **`docs/troubleshooting.md`** — active hardware/runtime scratchpad.
- **`docs/decisions/`** — Architecture Decision Records (ADRs). One file per significant decision.

## Working agreements
- When a change is user-visible or alters behavior, add an entry under `## [Unreleased]` in `CHANGELOG.md`.
- When you hit a decision that's hard to reverse or that a future reader would ask "why?" about (a dependency, a data model, an architectural boundary), STOP and flag it for the review chat to author the ADR — do not write or edit the ADR yourself. The test: would you answer "why is it built this way?" differently than the existing ADRs in `docs/decisions/`? If you can't tell, flag it anyway. You still write `CHANGELOG.md` entries and code comments, and you read and implement against existing ADRs.
- If a step will run longer than ~5 minutes without visible output — long builds, soak/duration
  tests, clock-convergence or reconnect waits, anything where you'll go quiet — say so up front:
  that it'll take a while, a rough duration, and what's running. A silent 20-minute wait is
  indistinguishable from a hang to the maintainer; "thinking…" with a stalled token count reads
  as broken, not intentional. If a step you expected to be quick runs long, say so as soon as
  that's apparent, and prefer emitting interim progress over going silent.
- Prefer reading the completed PID-tied `session.log` after a run over live-tailing it. The log
  is isolated per run (instance check + PID path), so reading it once the run finishes is faster
  and less wasteful than blocking on a sometimes-empty live feed.
- When you're blocked waiting on the maintainer (a physical action like an unplug/replug, or a
  decision), state plainly that you're now waiting and on what — don't sit silent.
- Don't restate docs/PLAN.md or CHANGELOG.md content here. Link, don't duplicate.

## Conventions

- Rust + GStreamer (`gstreamer-rs`); UI in iced. See ADR-0002 / ADR-0006.
- Crate/binary name: `final-multiplex` (display name "Final Multiplex").
- License: dual `MIT OR Apache-2.0`. Don't add GPL-encumbered codec plugins
  (`x264enc`, GPL `gst-libav`) to the default build — ADR-0003.
- Config: TOML via `serde` + the `toml` crate — ADR-0007.
- Per-source metrics schema lives in the adapter SDK crate; telemetry rides the control
  channel, not the media path — ADR-0008.
- Build: `cargo build`
- Check (no link): `cargo check --workspace`
- Lint: `cargo clippy --workspace`
- Test: `cargo test --workspace`
- Format: `cargo fmt --check` (CI gate) / `cargo fmt` (apply)
