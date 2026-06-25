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

## cam-77 reconnect freeze (~20s chunks, single frame updates)

**Symptom:** After cam-77 is unplugged mid-session and replugged, its tile freezes
for ~20-second stretches, delivers a single frame, then freezes again. cam-27 is
unaffected throughout.

**Not present on the cold-start path.** If cam-77 is offline at launch and plugged in
later (the `StreamsChanged` / `add_video_chain` path), video is live immediately.
Confirmed 2026-06-25: session age ~208s, cam-77 connected at T+60s via `StreamsChanged`,
socket clean (RecvQ=0 SendQ=0), adapter at 47.5% CPU, frames moving on screen.

**Present on the replug / adapter-restart path.** Confirmed 2026-06-25:
session age ~14 minutes, cam-77 unplugged → adapter exhausted retries → supervisor
respawned adapter → new process connected → `resetting vunixfdsrc_cam-77` → freeze.
Socket snapshot during freeze:
```
cam-77.vid.sock  adapter side  RecvQ=1712   SendQ=213504  ← adapter blocked
                 core side     RecvQ=21406  SendQ=164352  ← core recv backed up
cam-27.vid.sock  adapter side  RecvQ=0      SendQ=0       ← clear, unaffected
```

**What the two paths differ on:** On cold-start, the compositor's cam-77 sink pad is
newly requested (`add_video_chain` calls `compositor.request_pad_simple("sink_%u")`).
The aggregator has no prior PTS expectation for that pad — it starts accepting buffers
from whatever PTS they arrive with.

On replug, the compositor's cam-77 sink pad was created at `build()` time and has been
live for the full session. The aggregator has an established PTS timeline for cam-77.
When the adapter restarts and the new RTSP stream begins at PTS≈0, the aggregator
stalls waiting for cam-77's PTS+offset to reach the current session running time
(e.g., 14 min − 2 s = 13:58). Frames arrive at PTS=0 and must advance ~838 seconds
before the aggregator will present them — which never happens within a human timescale.

The ~20s single-frame pulses are the aggregator's timeout path forcing one frame out,
then stalling again.

**Socket backlog is a symptom, not the cause.** unixfdsrc consumes from the socket
continuously; the vshm_q (leaky=downstream) and voff_q (leaky=upstream) absorb and
drop the frames. Backpressure only propagates to the socket once both queues are full
AND the aggregator's thread stops pulling entirely.

**Hypothesis for fix:** When `restart_shmsrc` resets the unixfdsrc element after a
reconnect, also reset the compositor sink pad's PTS expectations. Three options worth
discussing:
1. Release and re-request the compositor sink pad (same as the cold-start path — the
   aggregator treats it as a new source).
2. Inject a `GST_EVENT_FLUSH_START/STOP` + new `GST_EVENT_SEGMENT` on the compositor
   sink pad to reset its PTS base to "now."
3. Update the pad offset at reconnect time to compensate for the PTS discontinuity
   (`new_offset = session_running_time − new_stream_pts`).

Option 1 is the most defensible because it matches the already-working cold-start
behavior exactly. Options 2 and 3 are more surgical but require careful segment/offset
arithmetic. This is a decision for the review chat — flagged.

**Attempt 1 — Option 1 implemented; partial result 2026-06-25.**

Action: On `Ready` with `is_restart=true`, supervisor now always pushes to
`streams_changed` (not `restarted`). `build_shmsrc_chain` tears down the existing
video/audio chains before rebuilding if they are present. Code compiles and the
Group 1 PTS diagnostic probe fires on every `add_video_chain` call.

Result: The 20-second freeze / single-frame-pulse symptom is **gone** — the compositor
pad teardown+rebuild is working as intended, and the old stale-pad PTS stall does not
reappear. However, cam-77 still does not recover after reconnect. A second bug was
discovered during the same test run.

---

## cam-77 reconnect: adapter clock sync timeout on respawn → PTS=0 → respawn loop

**Symptom (discovered 2026-06-25):** After the compositor pad fix, cam-77 enters a
120-second respawn loop instead of the 20-second pulse pattern. Video never resumes.

**Root cause confirmed by log:** Every *respawned* adapter process (attempt≥1) emits
`WARNING: clock sync timed out — proceeding`, then connects and begins producing frames.
Cold-start adapters (attempt=0) do NOT emit this warning — their clock sync succeeds.

Without a successful clock sync the adapter's pipeline base-time does not match the
core's. All frames are timestamped relative to a fresh clock starting at zero.
The Group 1 PTS probe confirms this:

```
[reconnect-pts] 'cam-77' first_pts=Some(0:00:00.000000000) pipeline_running=Some(0:08:10...)
```

PTS=0 frames arrive at the compositor when its running time is 8:10. The compositor
correctly discards them as ~8 minutes late. The socket fills, `fps_in` drops to 0,
and the 120-second frame watchdog fires. The respawn loop repeats indefinitely.

**Why cold-start adapters sync but respawned ones time out:** unknown. The NetClock
server port is unchanged. Adapter code and arguments are identical. The clock sync
timeout in the adapter may be too short for an already-running provider, or
GstNetClientClock may require more rounds to converge when calibrating against a clock
that has been ticking for minutes. Needs investigation in the adapter source.

**This is a separate bug from the stale compositor pad.** The compositor pad fix is
correct and necessary; this clock sync issue would cause a different failure (frames
silently dropped as late rather than the 20-second pulse freeze) even once the pad fix
lands.

**Options for review chat:**
1. Increase or remove the clock sync wait timeout in the adapter so respawned processes
   block until GstNetClientClock actually converges (simplest; adds startup latency on
   respawn but that is fine — recovery is already slow).
2. After clock sync timeout, fall back to using `pipeline_current_running_time()` at
   the moment the first RTSP pad is linked to set a corrective pad offset
   (`session_running_time - first_rtp_pts`) on the transport source. Surgical but
   requires per-reconnect arithmetic in the adapter and/or core.
3. Accept clock desync; compensate at the pipeline boundary: in `add_video_chain`, read
   the pipeline's current running time and set it as the vcaps_src pad offset, effectively
   making the compositor treat the reconnected stream as "starting now."
   Downside: the offset calculation happens at chain-build time; there is a small window
   before the first buffer arrives where the pad offset may be stale.

Flagged to review chat before any fix is attempted.
