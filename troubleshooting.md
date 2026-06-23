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

## Attempt 4 — GstDiscoverer pre-probe, pads filtered but source still in pipeline
**What:** Before building the pipeline, probe each source with GstDiscoverer
(2s timeout, parallel threads). Only request compositor pad if has_video,
only request audiomixer pad if has_audio. Corrupt files → no agg pads requested.
No-audio files → comp_sink only. All elements (including uridecodebin) still
added to the pipeline regardless of probe result.
**Result:** Probe ran correctly (confirmed in log). Compositor pushed
stream-start and caps (it's alive). Still "Waiting for first frame."
4-good-sources still works with the same build. Mixed pipeline does not.
**Learning:** The stall is not the aggregator waiting for a missing pad —
those pads were never created. The corrupt uridecodebin is still in the
pipeline and errors; this error during the async PAUSED→PLAYING state change
prevents the pipeline from completing the transition and reaching PLAYING.
Also: the no-audio source's idle audio chain (aconv→aresamp→acaps with
unlinked sink and no output) was still in the pipeline unnecessarily.

---

## Attempt 5 — Skip source entirely from the pipeline when probe returns (false, false)
**What:** If GstDiscoverer returns no video and no audio for a source, don't
add any elements to the pipeline at all for that source — no uridecodebin,
no conversion chains, nothing. For sources with only video (no audio), only
add the video chain; don't add the audio chain. Added STATE_CHANGED bus
logging and set_state return logging to confirm the pipeline reaches PLAYING.
**Result:** Pipeline reached PLAYING state. Working sources played normally.
Corrupt source tile showed compositor background (black). No stall.
Confirmed with the mixed scene: rdt-apr, no-audio (johnny-bravo), and
like-a-glove all played; corrupt slot was black.
**Learning:** The root cause was the corrupt uridecodebin posting a bus ERROR
during the pipeline's async state change, which blocked the PAUSED→PLAYING
transition. Removing the element entirely from the pipeline eliminates the
error event before it can interfere. Secondary issue: idle chains with
unlinked sinks and no aggregator output were also removed for cleanliness,
avoiding any NOT_LINKED flow errors propagating back through those chains.

---

## Resolution
**Fix shipped in commit `6034435`.**

The fix has one known gap: a source whose container headers are valid but
whose encoded payload fails at runtime (e.g. valid MP4 container wrapping
corrupt H.264 data) will still stall — GstDiscoverer sees the headers and
returns `(true, true)`, so the source gets aggregator pads, but uridecodebin
errors at decode time leaving a pad with no data. This case requires a
runtime mechanism in the bus-error handler. Noted in PLAN.md Open Questions.
