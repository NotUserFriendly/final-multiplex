# 0001. Record architecture decisions

- **Status:** Accepted
- **Date:** 2026-06-21

## Context

We want a durable, low-friction record of significant decisions, rather than
reconstructing them later from commit history.

## Decision

Record decisions as ADRs (Michael Nygard's format): numbered Markdown files in
`docs/decisions/`, from `0000-adr-template.md`. ADRs are immutable once Accepted; to
change one, write a superseding ADR and update the old status.

## Consequences

- Decisions are visible and reviewable in pull requests.
- ADRs capture *why*; the changelog captures *what shipped*; PLAN.md captures *what's next*.
- The discipline only works if we write them — see the working agreement in CLAUDE.md.
