## Definition of done (run before every commit)
A task is not done until ALL of these are true:
- [ ] `cargo fmt --check` and `cargo check --workspace` pass
- [ ] CHANGELOG.md has an entry under `[Unreleased]` if the change is user-visible or alters behavior
- [ ] No Accepted ADR was edited (see Process rules)
- [ ] New architectural decision? A new ADR was written, not an edit to an old one
- [ ] Commit message states what changed and why

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

- **`PLAN.md`** — current objective, phases, and exit criteria. Read it before starting work; it is the source of truth for *what* to build next.
- **`CHANGELOG.md`** — running record of shipped changes (Keep a Changelog format).
- **`docs/decisions/`** — Architecture Decision Records (ADRs). One file per significant decision.

## Working agreements

- When a change is user-visible or alters behavior, add an entry under `## [Unreleased]` in `CHANGELOG.md`.
- When you make a decision that's hard to reverse or that a future reader would ask "why?" about (a dependency, a data model, an architectural boundary), write an ADR in `docs/decisions/` using `0000-adr-template.md`.
- Don't restate PLAN.md or CHANGELOG.md content here. Link, don't duplicate.

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
