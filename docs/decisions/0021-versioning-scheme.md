# 0021. Versioning: Semantic Versioning with phase-driven minors

- **Status:** Accepted
- **Date:** 2026-06-25

## Context

Development has been organized by phases (PLAN.md) with no formal version scheme. The first
real release is about to be tagged (0.2.0, after Phase 2.2), so the project needs a predictable
versioning scheme and a definition of what triggers each bump.

## Decision

Adopt **Semantic Versioning** (semver.org).

- **Pre-1.0 (0.x):** each completed **phase** drives a **minor** bump — Phase 2.x cleanup →
  `0.2.0`, Phase 3 → `0.3.0`, and so on. Bug-fix-only releases between phases bump **patch**.
  Per SemVer, `0.x` explicitly disclaims stability, so "phase = minor" is simple and correct
  pre-1.0 — every phase ships as a minor regardless of what it changes.
- **Compatibility surfaces** (what a breaking change means once stable): the **scene-file
  format** (ADR-0007/0010) and the **adapter SDK contract** (ADR-0012, including its
  `PROTOCOL_VERSION`). Post-1.0, breaking either is a **major** bump.
- The adapter SDK contract carries its **own** `PROTOCOL_VERSION` (already at 3), versioned
  independently of the app's user-facing SemVer. The two coexist and must not be collapsed: the
  app version tracks user-facing releases; `PROTOCOL_VERSION` tracks adapter↔core wire
  compatibility (an adapter built against v3 either speaks v3 or it doesn't).
- **1.0 is deliberately left undefined for now.** It is the point at which scene-file and
  SDK-contract stability are committed to — plausibly the prototype milestone or the first
  Windows release, decided when closer. Guessing the 1.0/2.0 line now would over-constrain, and
  projects commonly revise their versioning approach at a major release anyway.

## Consequences

- Predictable cadence tied to phases; `0.2.0` is the first tagged release.
- Pre-1.0, all changes ship under minor bumps (0.x disclaims stability), so the scheme is
  trivial to apply until 1.0.
- The compatibility surfaces are named now, so when 1.0 lands, "what is a breaking change" is
  already recorded rather than argued.
- App version and `PROTOCOL_VERSION` evolve on independent clocks.
- The scheme itself may be revised at a future major release — an accepted, common practice.
