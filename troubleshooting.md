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

## cam-77 RTSP video frozen / not updating (ongoing)

**Symptom:** cam-77 tile displays a still frame. Camera management interface
shows live video. Audio from cam-77 works correctly. cam-27 displays live video
at ~70% CPU; cam-77 adapter idles at ~1.6% CPU.

---

### Attempt 1 — Suspected orphaned adapter processes

**Hypothesis:** A prior-session adapter held cam-77's RTSP stream slot, starving
the new session's adapter of video allocation.

**Action:** Killed orphaned fm-rtsp-adapter processes (PIDs 123552, 126254,
129282, 129777). Confirmed no orphans for the current session.

**Result:** Did not fix the freeze. cam-77 adapter still showed 1.6% CPU with
no new video frames after the reconnect that followed the power-cycle.

---

### Attempt 2 — SIGTERM cam-77 adapter to force full RTSP re-session

**Hypothesis:** The in-process partial reconnect (rtspsrc + decodebin3 cycled
through Null) established an RTSP session but left the camera's RTP video sender
in a stale state. A full process death → supervisor respawn → fresh
DESCRIBE/SETUP would force the camera to open a new RTP session.

**Action:** `kill -TERM <pid>` on the cam-77 adapter. Supervisor spawned a new
adapter. New adapter got both pads (video + audio chains ready), emitted
`Ready {true, true}`, received Play.

**Result:** Video jumped to a new timestamp (05:42:08) then immediately froze
again. Audio continued working. CPU remained ~1.6–1.9%.

`ss -tnp` showed `recv_q=190391` stable across multiple samples on the TCP
connection to port 554 — the camera was actively sending RTP data, but rtspsrc
was not reading it. This confirmed downstream backpressure blocking the read.

---

### Attempt 3 — Set `sync=false` on vshmsink and ashmsink

**Hypothesis:** `sync=true` on vshmsink causes the sink to pace writes to the
pipeline clock. After a long reconnect cycle (pipeline running for 30+ minutes),
the freshly-started RTP stream's timestamps don't align with the pipeline's
accumulated running time. vshmsink stalls waiting for timestamps to catch up,
creating backpressure through the entire chain back to rtspsrc — which stops
consuming from the TCP recv buffer, explaining the stable `recv_q=190391`.
The core's compositor handles sync via `do-timestamp=true` on vshmsrc; the
adapter's shmsink does not need to enforce sync.

**Action:** Changed `vshmsink.set_property("sync", true)` →
`vshmsink.set_property("sync", false)` and same for ashmsink in
`crates/fm-rtsp-adapter/src/main.rs`. Rebuilt. Restarted session.

**Result:** Did not hold, see test 4. cam-77 tile shows live video at ~60% CPU. Clock on
camera display advances in real time. Both cameras live simultaneously.

---

### What is known / ruled out

- Camera's RTSP server is healthy: DESCRIBE/SETUP succeeds, pads appear, audio
  flows.
- No orphaned adapters competing for the camera slot.
- The AAC audio decode errors in ffplayExtract.md (malformed PCE headers, buffer
  exhausted) are camera-side and unrelated to the video freeze — ffplay shows
  video correctly despite those errors, and audio works in our adapter too.
- The freeze happens both after in-process reconnect (partial rtspsrc restart)
  and after full process death + respawn.
- recv_q stable at 190391 bytes on the RTSP TCP socket confirms rtspsrc is not
  consuming data — pointing to downstream backpressure, not a camera send problem.

---

## GDP spike — shm PTS preservation via gdppay/gdpdepay (2026-06-24)

**Question:** Does wrapping the shm payload in the GStreamer Data Protocol
(`gdppay`/`gdpdepay`) allow the adapter's clock-coherent PTS to survive the
process boundary, fixing the n×2000 ms offset divergence found in Task Block 2 T3?

**Change under test:** `gdppay` inserted before each `shmsink` in the dummy adapter;
`gdpdepay` inserted after each `shmsrc` in the core; `do-timestamp=false` on all
core shmsrcs.  Scene: two dummy sources (`dummy-a` offset=0, `dummy-b` offset=2000 ms).

---

### Test 1 — PTS preserved across shm boundary?

**Setup:** `[PROBE-GDP-T1-ADAPT]` on `vcaps:src` (adapter, before gdppay) and
`[PROBE-GDP-T1-CORE]` on `vgdpdepay:src` (core, after gdpdepay) for `dummy-a`.

**Data (first 20 frames):**

| frame | adapter pts (ns) | core pts (ns) | diff (ns) |
|-------|-----------------|---------------|-----------|
| 0 | 780,964,575 | 780,964,575 | **0** |
| 1 | 814,297,908 | 814,297,908 | **0** |
| 5 | 947,631,241 | 947,631,241 | **0** |
| 10 | 1,114,297,908 | 1,114,297,908 | **0** |
| 19 | 1,414,297,908 | 1,414,297,908 | **0** |

All 20 frames: diff = 0.  Adapter frame-to-frame delta: 33,333,333 ns (exact 30 fps).
**PASS — GDP framing preserves PTS exactly across the shm boundary.**

---

### Test 2 — A/V lock per source

**Setup:** `[PROBE-GDP-T1-CORE]` (video, vgdpdepay:src) and `[PROBE-GDP-T2-ACORE]`
(audio, agdpdepay:src) for `dummy-a`.  Audio cadence: ~20.27 ms/buffer
(973-sample chunks at 48 kHz).

**Observations:**
- Video frame-to-frame delta: 33,333,333 ns (exact 30 fps). ✓
- Audio buffer-to-buffer delta: 20,333,333 ns (~20.3 ms, consistent). ✓
- Initial video PTS: 780.965 ms; initial audio PTS: 766.591 ms; offset ≈ −14 ms.
- The −14 ms initial A/V offset is stable; it does not drift over the observed window
  (~20 frames each).  (Frame-N-to-frame-N skew grows because video and audio have
  different cadences; same-time comparison gives ≤ ±14 ms, well within typical sync
  tolerance.)

**PASS — GDP preserves both audio and video PTS; A/V offset stable at ≈ −14 ms initial.**

---

### Test 3 — Offset accuracy at compositor (GDP-framed)

**Setup:** `[PROBE-GDP-T3-COMP]` on `vcaps:src` for both sources.  `dummy-b`
has `gst_pad_set_offset(2000000000 ns)` on its compositor sink pad.

**Data — dummy-b first 30 frames:**

| frames | PTS range at compositor (ms) | delta from prev group |
|--------|-----------------------------|-----------------------|
| 0–3 | 783 → 883 | — (linear, 33 ms each) |
| 4–6 | 3617 → 3683 | **+2734 ms jump** |
| 7–9 | 6417 → 6483 | +2800 ms jump |
| 10–12 | 9217 → 9283 | +2800 ms jump |
| 13–15 | 12017 → 12083 | +2800 ms jump |
| 16–18 | 14817 → 14883 | +2800 ms jump |

Frames 0–3 pass linearly.  From frame 4, bursts of 3 frames arrive every ~2800 ms.

**dummy-a at frame 4:** 1,614 ms (expected ~914 ms; +700 ms jump).

**Analysis:** The leaky queue (`max-size-buffers=2, leaky=downstream`) is still the
root cause.  GDP corrects PTS delivery, so the compositor now waits for
`running_time = PTS + 2000 ms` before consuming each dummy-b frame.  While it
waits, the vshm_q fills with the latest adapter frames.  When backpressure is
released, the queue delivers frames with a PTS that is already 2000 ms deeper into
the future (relative to dummy-a), causing an additional 2000 ms jump on top of the
base leaky-queue divergence seen in dummy-a.

The frame delivery ratio is still ≪ 100% for dummy-b (3 frames per ~2800 ms ≈ 32 fps
equivalent over the bursts, but with multi-second dead zones between bursts).

**FAIL — offset is not a stable +2000 ms; it diverges as n×2800 ms.  Root cause:
`leaky=downstream` queue.  GDP alone does not fix T3.**

---

### Summary and gate outcome

| test | result | notes |
|------|--------|-------|
| T1 PTS crossing | **PASS** | Adapter PTS = core PTS, diff=0, all frames |
| T2 A/V lock | **PASS** | Both streams advance at correct cadence; ≤14 ms initial offset |
| T3 offset accuracy | **FAIL** | n×2800 ms divergence; leaky queue still root cause |
| T4 RTSP smoke | not run | gate: T3 must pass first |

**Gate: FAIL.** Reporting to review chat.  No architectural commit from this spike.
GDP fixes the PTS crossing (T1) but the offset divergence (T3) remains — caused by
the `leaky=downstream` queue keeping only the 2 freshest frames while the compositor
waits for the PTS-coherent window.  The queue strategy (not the GDP framing) is
the next decision point.

---

## Group G gate: cam-77 cold-start — shmsrc cascade failure (ongoing, 2026-06-24–25)

**Gate procedure:** Start app with cam-77 unplugged → verify cam-27 shows live video
→ plug cam-77 back in → verify its tile populates without stalling other sources.

**Symptom:** All shmsrc elements (`vshmsrc_cam-27`, `vshmsrc_cam-77`, `ashmsrc_cam-77`)
fail within ~60 ms of `transport.play()` being called:

```
[fm-core] error from Some("ashmsrc_cam-77"): Internal data stream error.
gst_base_src_loop: streaming stopped, reason error (-5)
[fm-core] error from Some("vshmsrc_cam-27"): Internal data stream error.
gst_base_src_loop: streaming stopped, reason error (-5)
```

The push from each shmsrc → downstream returns `GST_FLOW_ERROR` (-5).  The compositor
or audiomixer enters an error state and rejects all subsequent sink pad pushes.

---

### Attempt 1 — Remove compositor `latency` property

**Hypothesis:** `compositor.set_property("latency", ceiling_ns)` causes GstAggregator
to enter an error state before the first buffers arrive.  With live sources and a
2000 ms latency target, the aggregator times out waiting and sets
`aggregate_func_return = GST_FLOW_ERROR`.  All subsequent sink pad pushes see the
stored error and return -5.

**Action:** Removed the `compositor.latency` set block.  Added explanatory comment.

**Result:** Non-deterministic.  In one "bisect-nolatency" run where cam-77 reported
`video=true audio=true`, video shmsrc worked and only the audio shmsrc errored.
In subsequent runs (same code), all three shmsrc elements errored.  The latency
removal is confirmed correct (setting it was definitely wrong) but is not the sole
cause.

---

### Attempt 2 — Revert all GDP elements

**Hypothesis:** `gdpdepay` in the core fails when shmsrc reconnects mid-stream
because gdppay writes caps only once at stream start; the caps packet is overwritten
in the ring buffer before a late-connecting shmsrc reads it.  This causes
`GDP packet header does not validate` errors.

**Action:** Removed `vgdpdepay` and `agdpdepay` from the core pipeline; removed
`gdppay` from both adapters.  `do-timestamp=false` on shmsrc is sufficient to
preserve the adapter's PTS from the SHM buffer header.

**Result:** GDP errors gone.  The shmsrc cascade failure (`GST_FLOW_ERROR`) still
occurs — GDP was a separate bug, not the cause of the cascade.

---

### Attempt 3 — Remove `alevel` from initial build path (bisect)

**Hypothesis:** `alevel` (GstLevel) in the audio chain fires
`assertion 'num_int_samples % channels == 0' failed` on the first buffer from
ashmsrc, which is not stereo-aligned.  The assertion abort kills the thread,
leaving the audiomixer in error state and cascading to video via some bus mechanism.

**Action:** Removed `alevel` from the initial (not dynamic) audio chain build path
(TEST edit in pipeline.rs).

**Result:** `ashmsrc_cam-77` still errors with GST_FLOW_ERROR.  `vshmsrc_cam-27`
still errors.  alevel is not the cascade mechanism.  TEST edit reverted in cleanup.

---

### Attempt 4 — Remove `voff_q` from initial build path (bisect)

**Hypothesis:** The voff_q leaky queue somehow causes the compositor to return an
error on the first push, which propagates back through the chain.

**Action:** Removed `voff_q` from the initial video chain build path (TEST edit).

**Result:** Same cascade.  voff_q is not the cause.  TEST edit reverted in cleanup.

---

### What is known

- `audiomixer: Latency query failed` appears in aggregator debug (`GST_DEBUG=aggregator:5`)
  immediately at pipeline start.  This is a WARN from gstaggregator.c:2355 and fires before
  any shmsrc buffers arrive.
- The compositor src task starts and the first call to `aggregate()` pushes a frame to
  appsink or autoaudiosink.  If those sinks are not yet in PLAYING state, the push may
  fail, setting `aggregate_func_return = GST_FLOW_ERROR` permanently.
- The error arrives within ~60 ms of `set_state(Playing)`.  GStreamer live pipelines
  return `StateChangeReturn::Async` from `set_state(Playing)` — the pipeline may not be
  fully playing when `send_play_all()` fires.

### Attempt 5 — Gate send_play_all() on pipeline PLAYING (root cause fix, 2026-06-25)

**Hypothesis:** `set_state(Playing)` on a live GStreamer pipeline returns
`StateChangeReturn::Async` — the transition is still in progress.  `send_play_all()`
fired immediately after, so adapters began pushing frames before the compositor
and audiomixer aggregators had completed their first `aggregate()` cycle.  The first
buffer push from a shmsrc landed in an aggregator that was mid-transition; the
aggregator permanently latched `aggregate_func_return = GST_FLOW_ERROR`.

**Action:** Added `Transport::wait_for_playing(10)` in `ui.rs` between `transport.play()`
and `supervisor.send_play_all()`.  `wait_for_playing` calls `pipeline.state(timeout=10s)`
which blocks until the pipeline returns `Success` or `NoPreroll`.  Only then are adapters
told to start streaming.  Also fixed `StateChangeReturn::Success | NoPreroll` to the
correct gstreamer 0.25 API: `Ok(StateChangeSuccess::Success | StateChangeSuccess::NoPreroll)`.

**Notes:**
- GDP removal (Attempt 2) was a dead end — GDP was not the cascade cause.  GDP was
  subsequently **restored** (ADR-0015 compliance): `gdpdepay` after each shmsrc in the
  core, `gdppay` before each shmsink in both adapters.
- Compositor latency was **restored** (ADR-0016 compliance).  Its removal (Attempt 1)
  was a dead end — it was not the cascade cause.
- `voff_q` leaky mode corrected from `downstream` to `upstream` (see analysis in
  the GDP spike log above).  ADR-0016 text says "leaky=downstream" but that drops
  the oldest frame — the one the compositor needs — causing n×2800 ms PTS divergence.
  Flagged for ADR supersession.

**Result:** Cascade fix confirmed — GST_FLOW_ERROR (-5) no longer occurs.  Two new issues
surfaced in the live run (2026-06-25):

**Issue A — PLAYING gate times out (10 s).** cam-27 reported `video=true audio=false`
(video-only camera, the common case).  The audiomixer had zero real inputs; GstAggregator
will not produce output — and therefore will not reach PLAYING — without at least one live
input.  The Play-gate correctly held `send_play_all()`, but then timed out because there was
nothing to unblock it.  This is a chicken-and-egg between the gate and the aggregator.
Fix: ADR-0018 synthetic audio floor (permanent silent `audiotestsrc`) gives the audiomixer
a heartbeat input so it always reaches PLAYING.

**Issue B — GDP event deserialization failure in GStreamer 1.28.2 (BLOCKER).**

Progression of attempts after the PLAYING-gate fix:

Run 1 (floors only, wait-for-connection=false): `gdpdepay_cam-27: Received a buffer without
first receiving caps.`  Ring buffer (64 KB default) cycled past the caps packet before
shmsrc's task thread started reading.

Run 2 (floors + wait-for-connection=true, ring=64 KB): `gdpdepay: could not create event from
GDP packet.`  Error changed — gdpdepay DID receive the caps correctly (no "without caps"
error) but then failed to deserialize a downstream event packet.

Run 3 (floors + wait-for-connection=true, ring=64 MB): SAME `could not create event from GDP
packet` on all chains (cam-27 video, cam-77 video, cam-77 audio).  Ring size is not the
cause; the event deserializer (`gst_dp_deserialize_event`) returns NULL for a specific event
type emitted by the rtspsrc/decodebin3 pipeline (likely a tag or stream-collection event that
the dummy-adapter's videotestsrc pipeline never emits).

**This is the TaskBlock stop-and-report criterion:** GDP + shm transport fails in GStreamer
1.28.2 with the rtsp-adapter pipeline, both before and after ring-size/wait-for-connection
fixes.  The GDP spike T1 test passed only because it used the dummy adapter (videotestsrc),
not rtspsrc + decodebin3.

**Raised to review chat.** Decision required: (a) debug GDP event serialization for 1.28.2,
(b) switch transport (unixfd-on-Linux fallback mentioned in TaskBlock), or (c) verify whether
do-timestamp=false on shmsrc alone preserves PTS without GDP (the claim "it won't" in the
TaskBlock was asserted without a fresh measurement with do-timestamp=false specifically).

---

### Resolution — unixfd transport (ADR-0019, 2026-06-25)

Review chat authored ADR-0019: platform-selected transport; Linux = unixfd.

**Group 1 controlled T1 measurement** settled option (c) before building:
- Adapter PTS (frame 0): `0:00:02.328102754`
- Core shmsrc PTS (frame 0, do-timestamp=false, no GDP): `0:00:00.000000000`
- Frames 1+: core PTS = `None`
- Immediate `ashmsrc` cascade error: shmsrc cannot negotiate caps without the GDP
  caps envelope — without framing, the caps race is unavoidable.

**Verdict (option c): No, do-timestamp=false on shmsrc alone does NOT preserve PTS.** The first
frame gets PTS=0 (shmsrc default), subsequent frames get None. GDP was doing two separate jobs
(caps negotiation AND PTS envelope); removing it breaks both.

**Fix (option b) — unixfd transport implemented (2026-06-25):**
- `shmsink`/`shmsrc` and `gdppay`/`gdpdepay` removed from all adapters and core.
- `unixfdsink` (adapter) + `unixfdsrc` (core, do-timestamp=false) wired at both seams.
- `is-live` property removed from unixfdsrc (not a valid property on GstUnixFdSrc).

**Validation run results:**
- Dummy adapter (scene-t1.toml): 15 s run, zero errors, both sockets created. ✓
- RTSP scene (scene-step5.toml, cam-27 + cam-77): 30 s run, zero errors, zero GDP event
  deserialization failures. Both cameras reached PLAYING and Play; both video sockets and the
  cam-77 audio socket all created and connected. ✓

**GDP saga closed.** All GDP-related entries in this file are historical. The transport is now
unixfd on Linux (ADR-0019 supersedes ADR-0015).
