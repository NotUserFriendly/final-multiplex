# Troubleshooting Log
Purpose — a live scratchpad, not a durable record.
This file is where CC works a hard, active bug in the open: hypothesis, action,
result, repeat. It exists mainly to give the review chat visibility into how a
problem is being approached, so wrong-layer or symptom-only fixes get caught early
instead of shipped.

Lifecycle — ephemeral. The maintainer clears this file once a bug is resolved.
Nothing here is authoritative or permanent. When a bug is actually fixed, the durable
record goes elsewhere:

what shipped → CHANGELOG.md
a deferred or minor bug → BUGS.md
a fix that is really a decision → an ADR in docs/decisions/ (authored in the
review chat, per the working agreement)

Discipline. An attempt is not a fix until a test proves it. Do not mark an entry
"Confirmed fix" without a check that demonstrates it; if a later test disproves it,
amend the entry rather than leaving a false "fixed" behind. A change that clears a
symptom by quietly disabling a property or behavior elsewhere must be flagged as such,
not logged as a clean win — that distinction is the whole reason this log is visible
to review.

Format. One section per bug. Under it: Attempt N — Hypothesis / Action / Result.

## cam-77 offset reset to 0 after reconnect

**Symptom (discovered 2026-06-25):** A per-source offset set via the UI is silently
reset to 0 whenever the adapter's chain is rebuilt — i.e., on every adapter-process
restart (cable replug).

**Root cause confirmed by code inspection (no hardware test needed):**

`Pipeline::build()` populates `source_layouts: HashMap<String, SourceLayout>` once,
reading `offset_ns = source.offset_ms * 1_000_000` from the TOML.  This map is
**never updated** after that.

When the user changes the offset via the UI, `transport::set_source_offset()`
(transport.rs:92) calls `p.set_offset(offset_ns)` on the live vcaps_src and
acaps_src pads.  This works on the live pipeline, but `source_layouts` is not touched.

When `add_video_chain()` rebuilds the chain after a reconnect, it reads
`layout.offset_ns` from `source_layouts` (pipeline.rs:748) and calls
`vcaps_src.set_offset(layout.offset_ns)` (pipeline.rs:795).  This always gets the
stale TOML value — 0 for both cameras in scene-step5.toml.

**Where the fix goes:** `transport::set_source_offset` must also update
`source_layouts[source_id].offset_ns`.  The pipeline needs a method like
`update_source_layout_offset(&mut self, source_id, offset_ns)` for this.
Alternatively, `Pipeline` can expose a mutable reference to `source_layouts` or
move offset tracking into `Transport` itself.  Not patching here — flagged to review
chat to decide the right ownership model before implementing.
