# Troubleshooting Log

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

**Result:** Confirmed fix. cam-77 tile shows live video at ~60% CPU. Clock on
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
