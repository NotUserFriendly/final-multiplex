# Stall troubleshooting log

## Problem
When a corrupt file or a video-only (no audio) file is in scene.toml, the
entire multiplex stalls: "Waiting for first frame" indefinitely. Good sources
are also blocked. 4-all-good-sources works; mixed good+bad does not.

---

## Attempt 1 — EOS + release_request_pad (transport.rs + pipeline.rs)
**What:** On bus ERROR, parse source id from debug string, call
`acaps_src.unlink + comp_sink.send_event(EOS) + compositor.release_request_pad`.
Same in a `connect_no_more_pads` handler for the no-audio case.
**Result:** Both code paths confirmed running via log. Aggregators still stalled.
**Learning:** `release_request_pad` alone does not wake an aggregator already
blocked mid-wait. EOS injection also did not help.

---

## Attempt 2 — Lazy pad request (inside pad-added)
**What:** Don't pre-request comp_sink/mix_sink at build time. Request them
inside `connect_pad_added` only once we know the stream type exists. Corrupt
files never fire pad-added, so they never get pads.
**Result:** Broke even the 4-good-sources case. A compositor that starts with
0 sink pads produces no output — it needs at least one pad to start its
aggregate loop.
**Learning:** Must pre-request pads at build time (or equivalent).

---

## Attempt 3 — force-live + latency=200ms + ignore-inactive-pads
**What:** GStreamer-native aggregator property combo. In live mode the
aggregator times out on pads that haven't received data, then skips them.
**Result:** `force-live` is construct-only (panic on set_property); fixed.
Then the compositor entered a tight latency-query-failed busy-loop and never
produced output. The latency query propagates upstream through the unlinked
vconv chain sinks and fails. Without a successful latency query the timeout
mechanism doesn't engage.
**Learning:** force-live doesn't work reliably when upstream chains have
unlinked sinks (as they do before pad-added fires for a failed source).

---

## Attempt 4 — GstDiscoverer pre-probe (current code)
**What:** Before building the pipeline, probe each source with GstDiscoverer
(2s timeout, parallel threads). Only request compositor pad if has_video,
only request audiomixer pad if has_audio. Corrupt files → no pads requested.
No-audio files → comp_sink only.
**Result:** Probe runs correctly (confirmed in log). Compositor pushes
stream-start and caps (it's alive). But still "Waiting for first frame."
4-good-sources still works with the same build. Mixed pipeline does not.
**Learning:** The stall is not the aggregator waiting for a missing pad —
those pads were never created. Something else in the mixed pipeline blocks
the compositor from producing frames after it has pushed caps.

---

## What we know
- 4 good sources: always works
- Mixed (3 good + corrupt + no-audio, probe-filtered): stalls
- The compositor has 3 correct pads and has pushed stream-start + caps
- The audiomixer has 2 correct pads
- The corrupt uridecodebin is still in the pipeline (but has no agg pads)
- The no-audio uridecodebin emits a video pad (linked) but no audio pad
- Pipeline does not appear to enter ERROR state (bus loop keeps running)

---

## Next diagnostic steps
1. Remove corrupt source from scene entirely, test: no-audio + 2 good
2. If still broken: remove no-audio too, test: 3 good sources
3. Add STATE_CHANGED bus message logging to see if pipeline reaches PLAYING
4. Consider: corrupt uridecodebin error may prevent pipeline from reaching
   PLAYING (async preroll issue in bin state machine)
