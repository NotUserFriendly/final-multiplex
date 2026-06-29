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
- **`docs/troubleshooting-0.3.md`** — active hardware/runtime scratchpad.
- **`docs/decisions/`** — Architecture Decision Records (ADRs). One file per significant decision.

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

## Working agreements

- **Plan multi-step work.** Before starting any multi-step task, create a todo checklist of
  all steps, then mark each in_progress → completed as you go.
- **CHANGELOG.** When a change is user-visible or alters behavior, add an entry under
  `## [Unreleased]` in `CHANGELOG.md`.
- **Don't go silent.** If a step will run longer than ~5 minutes without visible output — long
  builds, soak/duration tests, clock-convergence or reconnect waits — say so up front: that it'll
  take a while, a rough duration, and what's running. A silent 20-minute wait is indistinguishable
  from a hang; "thinking…" with a stalled token count reads as broken, not intentional. If a step
  you expected to be quick runs long, say so as soon as that's apparent, and prefer interim progress
  over silence. When you're blocked waiting on the maintainer (a physical action like an
  unplug/replug, or a decision), state plainly that you're now waiting and on what.
- **Record validation results, attributed — especially maintainer-run ones.** When a task
  includes validation, record the outcome in `docs/troubleshooting-0.3.md`: who verified it and what
  they observed (e.g. "maintainer killed cam-27 → tile froze alone, others kept running,
  recovered on reboot — confirmed 2026-06-29"). This is CC's responsibility whether or not the
  task block restates it. CC already logs what it can see from its own runs; the gap is the
  maintainer-run tests CC *can't* self-observe — physical actions (kill/reboot/unplug), on-screen
  alignment or visual checks, anything needing a human at the keyboard. Those evaporate unless CC
  writes them down. A passed test that isn't recorded is, to everyone downstream, a test that
  never happened.
- **Read logs after, not during.** Prefer reading the completed PID-tied `session.log` after a
  run over live-tailing it. The log is isolated per run (instance check + PID path), so reading it
  once the run finishes is faster and less wasteful than blocking on a sometimes-empty live feed.
- **Don't duplicate the steering docs.** Don't restate `docs/PLAN.md` or `CHANGELOG.md` content
  here. Link, don't duplicate.

## Architecture Decision Records (ADRs)

- **CC does not author or edit ADRs.** When you hit a decision that's hard to reverse or that a
  future reader would ask "why?" about (a dependency, a data model, an architectural boundary),
  STOP and flag it for the review chat to author the ADR. The test: would you answer "why is it
  built this way?" differently than the existing ADRs in `docs/decisions/`? If you can't tell,
  flag it anyway. You still write `CHANGELOG.md` entries and code comments, and you read and
  implement against existing ADRs.
- **ADRs are immutable once `Status: Accepted`.** Never edit the body of an accepted ADR. To
  change, reverse, or extend a decision, the review chat writes a NEW ADR that supersedes it and
  sets the old one's status to `Superseded by ADR-XXXX` — a one-line status change is the only
  edit ever made to an accepted ADR. When implementation teaches you something that changes a
  decision, that's the signal to flag for a new ADR — not to quietly amend the old one.
- **Implementation detail does not go in ADRs.** ADRs capture the decision and its rationale.
  Code lives in code; what-changed lives in `CHANGELOG.md`. Link, don't duplicate.

## Definition of done (run before every commit)

A task is not done until ALL of these are true:
- [ ] `cargo fmt --check` and `cargo check --workspace` pass
- [ ] `CHANGELOG.md` has an `[Unreleased]` entry if the change is user-visible or alters behavior (see Working agreements)
- [ ] No accepted ADR was edited, and any new architectural decision was flagged — not authored — here (see ADRs)
- [ ] Commit message states what changed and why

- **Cutting a release:** run `scripts/release.sh X.Y.Z` from a green tree (DoD passed). It bumps
  the workspace version, rolls `[Unreleased]` into a dated `[X.Y.Z]` section, refreshes
  `Cargo.lock`, commits, and creates an annotated `vX.Y.Z` tag — it does **not** push (review,
  then push manually). `--dry-run` previews. Per ADR-0021.
