# Documentation

## Architecture Decision Records

Significant, hard-to-reverse decisions live in [`decisions/`](decisions/) as ADRs.

- Copy `decisions/0000-adr-template.md` to the next number: `0002-...md`, `0003-...md`.
- Number monotonically; never renumber. Title in kebab-case.
- One decision per file. Status starts `Proposed`, moves to `Accepted` when agreed.
- To reverse a past decision, write a new ADR and mark the old one
  `Superseded by [NNNN](NNNN-...md)`.

ADRs answer *why*. The top-level `CHANGELOG.md` answers *what shipped, when*.
`PLAN.md` answers *what we're doing next*. `BUGS.md` tracks deferred and known
bugs. `troubleshooting.md` is the active hardware/runtime scratchpad.
