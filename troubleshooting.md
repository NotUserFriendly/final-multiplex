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

---

## Task Block 2 — Boundary clock-sync vs arrival-sync measurement (2026-06-24)

**Question:** With `do-timestamp=true` on vshmsrc/ashmsrc, is per-source timing
across the shm boundary driven by the shared net clock (frame-accurate) or by
arrival time (jitter-dependent)?

**Headline result (Test 1): PTS REWRITTEN.** The adapter's PTS does not cross the
shm boundary. The core re-timestamps every buffer with arrival time.

---

### Test 1 — Keystone: is the adapter's PTS preserved or discarded?

**Setup:** Buffer pad probes on `vshmsink:sink` (dummy-adapter side) and
`vshmsrc:src` (core side) for the same dummy source.  One dummy source at 30 fps.

**Data (first 20 frames):**

| frame | adapter pts (ns) | core pts (ns) | delta (ns) |
|-------|-----------------|---------------|------------|
| 0 | 1,078,017,823 | 98,007,210 | 980,010,613 |
| 1 | 1,111,351,156 | 131,386,242 | 979,964,914 |
| 5 | 1,244,684,489 | 264,756,791 | 979,927,698 |
| 10 | 1,411,351,156 | 431,387,029 | 979,964,127 |
| 19 | 1,711,351,156 | 731,421,449 | 979,929,707 |

- Adapter PTS starts at ~1,078 ms (pipeline has been running 1.078 s from base_time).
- Core PTS starts at ~98 ms (arrival time in core pipeline).
- Values bear no relation to each other: **PTS REWRITTEN, not preserved.**
- Delta (adapter − core) is constant at ~980 ms ± 0.1 ms across 20 frames — it
  does not drift; the gap is the shm depth at first read (adapter was producing
  frames for ~980 ms before the core's shmsrc first read one).
- Adapter frame-to-frame delta: exactly 33,333,333 ns (deterministic clock).
- Core frame-to-frame delta: ~33.3–33.5 ms (arrival jitter ±~300 µs).

**Confirming toggle (do-timestamp=false on vshmsrc):** Core-side PTS became 0 for
frame 0 and `u64::MAX` (NONE) for frame 1; pipeline became unschedulable and
stopped after 2 frames.  This confirms GStreamer's shmsink/shmsrc pair does **not**
carry buffer PTS in the shared memory ring buffer.  `do-timestamp=true` is what
supplies timestamps to the core; without it no PTS exists.

---

### Test 2 — A/V lock per source across the boundary

**Setup:** Video probe at vshmsrc:src and audio probe at ashmsrc:src for the dummy
source (48 kHz, 1024-sample audio buffers ≈ 21.3 ms per buffer; video at 30 fps
≈ 33.3 ms per frame).  A/V skew measured by matching each video PTS to the nearest
audio PTS by value over 300 video frames (~10 s).

**Skew data (nearest-PTS match, 5 windows of 60 frames each):**

| window | time range | min skew | max skew | mean | stdev |
|--------|-----------|----------|----------|------|-------|
| 0 | 98–2065 ms | −9.5 ms | +10.6 ms | +0.70 ms | 6.23 ms |
| 1 | 2098–4065 ms | −9.5 ms | +10.6 ms | +0.36 ms | 6.16 ms |
| 2 | 4098–6065 ms | −10.6 ms | +10.7 ms | +0.36 ms | 6.19 ms |
| 3 | 6099–8065 ms | −9.7 ms | +10.6 ms | +0.36 ms | 6.24 ms |
| 4 | 8099–10066 ms | −9.6 ms | +10.7 ms | +0.70 ms | 6.25 ms |

The ±10.6 ms range is the natural aliasing from mismatched video/audio cadences
(33.3 ms vs 21.3 ms); it is not jitter accumulation.  **Skew is stable and does
not drift over 10 s.** A/V lock is maintained across the boundary because both
shm channels are read promptly at approximately the same pipeline rate.

---

### Test 3 — Offset accuracy across the boundary

**Setup:** Two dummy sources (`dummy-a` offset=0, `dummy-b` offset=2000 ms).
Probes on `vcaps:src` (compositor input pad, where `gst_pad_set_offset` is
applied) for both sources.

**Observed:**

Frames 0–2 of `dummy-b` passed the probe before the pad offset took effect
(probe observed PTS ≈ 131–198 ms, same as dummy-a; expected 2132–2198 ms).
Starting at frame 3, the offset was active, but subsequent PTS values grew as
multiples of 2000 ms per burst:

| dummy-b burst | PTS range at compositor | implied offset |
|------|------------------------|---------------|
| frames 0–2 | 131–198 ms | 0 ms (pre-active) |
| frames 3–5 | 2165–2231 ms | +2000 ms |
| frames 6–8 | 4198–4265 ms | +4000 ms |
| frames 9–11 | 6231–6298 ms | +6000 ms |
| frames 12–14 | 8265–8332 ms | +8000 ms |
| frames 15–17 | 10298–10365 ms | +10000 ms |

Only 18 of ~360 expected frames reached the compositor (5%). The leaky queue
(`max-size-buffers=2, leaky=downstream`) keeps the 2 freshest frames. When
the compositor finishes waiting ~2 s for PTS to reach running_time, the freshest
frames in the queue are already 2000 ms further into the future (arrival_time
has advanced by the wait period). The displayed offset diverges as
n×2000 ms rather than holding at 2000 ms.

**Conclusion:** Arrival-sync PTS + `gst_pad_set_offset` does **not** produce
stable delay. The offset diverges on each compositor-wait cycle.  Frame-accurate
offset requires the adapter's clock-coherent PTS to survive the shm boundary
(i.e., `do-timestamp=false` plus the adapter writing valid PTS to shm — which the
current shmsink does not do).

---

### Test 4 — Reconnect drift (RTSP)

**Setup:** Both cameras (cam-27, cam-77) in scene-step5.toml.  SIGTERM cam-77
adapter after both cameras reach Ready.

**Observations:**

- `recv_q` for cam-77 was already **180,340 bytes** before the kill.  No cam-77
  video frames appeared at the core probe before the kill.  Cam-77 video was
  frozen despite the `sync=false` fix confirmed in Attempt 3 above.  (Cam-27
  was healthy: recv_q=0, core probe firing.)

- After SIGTERM: core saw `Control socket has closed` errors on vshmsrc_cam-77
  and ashmsrc_cam-77 immediately.  Supervisor scheduled restart in 1 s.

- New adapter (attempt=1): clock sync **timed out** ("WARNING: clock sync timed
  out — proceeding").  New adapter emitted `Ready {video=false, audio=true}` — no
  video pad arrived from decodebin3 in the 30 s deadline.  The camera's video
  data was accumulating (recv_q still 180,340) but not decoded.  Core removed
  the video chain (StreamsChanged consumed).

- PTS discontinuity: could not measure.  No cam-77 video frames at the core
  before OR after reconnect.  The frozen-video issue from Attempt 2 recurs even
  with the sync=false fix in place, possibly triggered by the clock-sync timeout
  or by a decodebin3 pad timing race.

- A/V after reconnect: audio-only; no video.

---

### Test 5 — Orphan / teardown coverage

**Setup:** scene-step5.toml, two fm-rtsp-adapter processes per session.

| exit path | orphan adapters? | stale runtime dir? |
|-----------|-----------------|-------------------|
| Clean quit (SIGTERM app) | **YES** (both adapters survive) | **YES** (dir + sockets remain) |
| SIGKILL app | **YES** (both adapters survive) | **YES** (dir + sockets remain) |
| Supervisor restart (adapter SIGTERM) | No — supervisor manages restart | Dir stays while app runs |
| Watchdog kill (graceful Shutdown) | No — adapter handles Shutdown | Dir stays while app runs |
| New session startup | n/a | `reap_orphans()` removes stale dirs whose PID is dead |

Notes:
- `shutdown_all()` (which calls `runtime::cleanup()` and sends Shutdown to all
  adapters) is **never called** from the UI.  There is no `Drop` impl on
  `Supervisor` and no `CloseRequest` handler in the iced app.  On any app exit
  (SIGTERM, SIGKILL, window close), adapters become orphans.
- `reap_orphans()` cleans up **directories** for dead PIDs at next startup, but
  does not kill surviving processes.  Orphaned adapter PIDs require manual kill.
- Confirmed: stale dir with socket files persists from a killed session and is
  cleaned by the next session's startup.  Adapter processes confirmed surviving
  after both SIGTERM and SIGKILL of the app via `kill -0 <pid>` check.
