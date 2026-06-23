# 0010. scene.toml is round-trip read-write app state

- **Status:** Accepted
- **Date:** 2026-06-23

## Context

ADR-0007 chose TOML for the scene config and treated it as **input**: the app reads
`scene.toml` at boot and never writes it. Per-source values (offset now; volume and
other controls later) are adjustable live in the UI, but those adjustments are lost on
restart — the next boot re-reads the original file. We want live edits to persist, so a
scene the user tunes by hand stays tuned.

That turns the config from read-only input into state the app also writes, a force
ADR-0007 did not contemplate. Two hazards come with it:

- **Format destruction.** The file is hand-authored — comments, field order, URIs, ids,
  alignment. Re-serializing the whole `SceneConfig` through `toml::to_string` would
  discard all of that and hand the user back a reformatted file with their comments gone.
- **Corruption on interrupted write.** A crash or kill mid-write could leave `scene.toml`
  truncated or half-written, destroying the scene the user depends on to launch.

## Decision

Treat `scene.toml` as **round-trip read-write state**, owned by the app while it runs.

- Boot reads the file as before (ADR-0007 unchanged for the read path).
- A live control change persists the affected field back to the **same file** the scene
  was loaded from (the path from `argv[1]`, default `scene.toml`).
- Writes use **`toml_edit`**, not serde re-serialization, so edits are surgical and
  preserve comments, ordering, and formatting — only the changed value is rewritten.
- Writes are **debounced** (persist on commit after a short idle, and on clean exit), so
  live dragging does not thrash the file.
- Writes are **atomic**: write to a temp file in the same directory, then rename over the
  original, so an interrupted write cannot corrupt the live scene.

Scope now is `offset_ms`. The write path is structured as "persist field X for source id
Y" so per-source volume and future controls drop in without rework.

This **extends** ADR-0007; it does not supersede it. TOML-as-format and serde-for-read
still hold; this ADR adds the write half.

## Consequences

- Live adjustments survive restart; a tuned scene reproduces from its file, restoring the
  Phase-1 "reproduces from config" property for runtime-set values.
- The file stays human-readable and hand-editable — `toml_edit` keeps comments and layout
  intact across app writes.
- New dependency: `toml_edit` in `fm-core`. (It is already in the tree transitively via
  `toml`/`system-deps`, so this surfaces an existing crate rather than adding a new one.)
- The app now writes a user-owned file — a surprise if unexpected. Atomicity is therefore
  mandatory, not optional; the temp-and-rename path is part of the decision, not an
  implementation detail to skip.
- **Last-write-wins, app wins:** if the user hand-edits `scene.toml` while the app is
  running, the app's next persist overwrites that edit. Detecting external changes and
  reloading is out of scope here; revisit only if hand-editing-while-running becomes a
  real workflow.
- Follow-on: per-source volume persistence reuses this path. If the scene later grows
  fields that are only ever app-managed (never hand-authored), reconsider whether they
  belong in `scene.toml` or in a separate state file beside it.
