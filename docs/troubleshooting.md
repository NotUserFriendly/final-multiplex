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

---

## Ratchet firing to 37 fps with GPU-path pad probe active (2026-06-28)

**Symptom:** On launch with the Block 1 GPU-path probe installed on `vcaps_dummy:src`,
the ratchet fired to 37 fps (`[pipeline] output fps ratcheted → 37`). RATCHET_MIN_DELTA=5
means this is a genuine two-consecutive-poll reading of 37 fps, not noise below the guard.

**Hypothesis:** The pad probe on `vcaps_dummy:src` copies pixel data on the GStreamer
streaming thread on every buffer. This adds per-frame CPU work inline with the dummy
adapter's delivery path. Under load, this can cause frames to bunch slightly, inflating
the 1-second fps_in measurement window from the nominal 30 fps to 35–37 fps — enough
to clear RATCHET_MIN_DELTA and commit.

**Status:** Open. Not yet confirmed whether the probe is the cause or whether it is
coincidental measurement noise. If stutter is observed, check fps_out — a compositor
running at 37 fps against a 23.976 fps file source produces the same 37/24 judder
pattern seen in the pre-RATCHET_MIN_DELTA era. Potential fixes:
- Off-thread copy: have the probe enqueue a buffer reference and do the pixel copy on
  a dedicated thread, removing the inline CPU cost from the streaming thread.
- Increase RATCHET_MIN_DELTA (e.g. to 8) to absorb probe-induced jitter at 30 fps
  while still passing a genuine 48/50/60 fps source (≥18 fps above baseline).

---
