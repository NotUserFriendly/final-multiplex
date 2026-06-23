# Troubleshooting Log

## Per-source offset seek not working reliably

**Symptom 1 (iteration 1 — `gst_pad_set_offset` only):**
Pressing +1s/−1s caused a brief stutter then the video resumed at exactly the
same frame. No visible content change.

**Root cause:** `gst_pad_set_offset` on the capsfilter src pads changes the
compositor _timestamp mapping_ only — it shifts when each frame appears on the
compositor timeline but does not move the file read head. The content shown is
identical to before; only the compositor clock offset changes.

---

**Symptom 2 (iteration 2 — `uri_elem.seek_simple(FLUSH | KEY_UNIT)`, no pad offset):**
Inconsistent: sometimes the video scrubbed forward, sometimes nothing happened.

**Root cause:** `KEY_UNIT` seeks to the _nearest_ keyframe to the target. For
videos with sparse keyframes (e.g., one keyframe every 2–3 s), a 1 s step from
position 0 has no keyframe between 0 and 1 s, so `KEY_UNIT` snaps back to the
keyframe at 0 s — visually "nothing happens." When the step happened to cross a
keyframe boundary, it appeared to work.

---

**Symptom 3 (iteration 3 — seek + `pad_offset = running_now`, KEY_UNIT):**
Pressing +1s/−1s consistently reset the video to the beginning, with occasional
exceptions.

**Root cause (double-offset):** After a per-element `FLUSH` seek, GStreamer sets
the new segment's `base` field to the current pipeline running time T. Frames
from the seeked source therefore arrive at the compositor at running time T
(correct, no adjustment needed). Adding `pad_offset = T` on the capsfilter src
pad doubled the offset, sending frames to running time 2T — T seconds in the
future. The compositor froze that tile waiting; other sources continued playing,
eventually reaching EOS; the pipeline EOS handler fired and sought everything
back to position 0.

**Root cause (KEY_UNIT snapping):** Same as iteration 2 — KEY_UNIT snapping to
keyframe 0 caused the visible "reset to beginning" in the non-double-offset case.

---

**Fix (iteration 4 — `seek_simple(FLUSH | ACCURATE)`, no pad offset):**
- `ACCURATE` flag: seek lands at exactly the requested timestamp; no keyframe
  snapping. Slightly slower than KEY_UNIT (decoder must decode from previous
  keyframe) but correct for a frame-accurate sync tool.
- No pad offset: GStreamer's segment `base` field already compensates for
  running time, so frames arrive at the compositor on schedule without manual
  offset. Adding a manual pad offset introduced the double-offset bug.

**Status:** deployed, awaiting verification.

---

## Known limitation — per-source offsets not preserved across loops

When all sources reach end-of-file, the bus loop seeks the entire pipeline back
to position 0 (`seek_all`). Per-source offsets are lost; the UI still displays
the previously set values but the sources have all returned to position 0. The
user must re-apply offsets after a loop. Fix: pass source-offset state into the
bus loop and re-seek each source after the pipeline loop seek. Deferred.
