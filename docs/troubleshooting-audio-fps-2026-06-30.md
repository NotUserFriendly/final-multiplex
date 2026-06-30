# Troubleshooting — YouTube audio silence + slow fps (2026-06-30)

Two symptoms from the 6-source scene (dummy + fnaf2 file + cam-27 RTSP +
cam-77 RTSP + yt-fc YouTube + yt-nature YouTube):
1. yt-fc and yt-nature: **no audio** even when unmuted (audio meters show activity)
2. fnaf2 and all sources: **~20fps output**, fnaf2 input fps below nominal 24fps

---

## What was worked on

### Attempt 1 — throttle probe on aconv.sink (prior session, 641f9ce)

**Hypothesis:** `uridecodebin3` decodes the HTTP audio stream at full speed;
without throttling, a burst of ~N minutes of audio arrives in seconds and mostly
gets dropped by `aqueue(leaky=downstream)`.

**Action:** Added a `BUFFER` pad probe on `aconv.sink` in the adapter that sleeps
`buf.duration()` between buffers.

**Result:** Ineffective. `avdec_aac` leaves `GstBuffer.duration` unset
(`GST_CLOCK_TIME_NONE`); `buf.duration()` returned `None` every call — no sleep,
burst unchanged. Maintainer confirmed: "Both items aren't working."

---

### Attempt 2 — move probe to aunixfdsink.sink + startup/reconnect seeks (96ce048)

**Three sub-fixes:**

(a) **Probe moved to `aunixfdsink.sink`**, where format is guaranteed S16LE 48 kHz 2ch
by `acaps`. Duration computed from `buf.size() / 192_000` (exact; no metadata needed).

(b) **Startup seek on `Command::Play`:** `seek_simple(FLUSH|KEY_UNIT, 0)` on
`uridecodebin3`, one-shot, to set `segment.base = T_play` — intended to map PTS=0
audio frames to `running_time ≈ T_play` so they wouldn't be dropped as late by
audiomixer.

(c) **Reconnect seek made unconditional:** Previously, the post-reconnect
`StreamsChanged` (and seek) fired only on caps change. yt-fc loops with identical
caps every 4:38, so the seek never fired after the first loop. Made it unconditional.

**Result (user-reported):** Audio meters showed activity. No sound heard. New clue:
"audio meters are showing sound" means audio is flowing through `alevel` (which is
BEFORE audiomixer) but audiomixer is not outputting it. Also: a new cycling bug
appeared — yt-fc was entering a tight add/remove loop:

```
[pipeline] added audio chain for 'yt-fc'
[supervisor] 'yt-fc': StreamsChanged (video=false audio=false)
[pipeline] removed audio chain for 'yt-fc'
... repeat immediately ...
```

**Root cause of cycling:** The `FLUSH|KEY_UNIT` seek on an HTTP-backed
`uridecodebin3` posts an EOS message on the GStreamer bus. The EOS handler
calls `spawn_reconnect_thread`, which immediately emits `StreamsChanged(false,false)`,
which tears down the just-added audio chain, creating an infinite loop.

---

### Attempt 3 — do-timestamp=true on aunixfdsrc + remove adapter seeks (7be6b03)

**Root cause revision:** The seek approach was wrong-layer. `unixfdsink`/`unixfdsrc`
transmit raw `GstBuffer` values (PTS, size, data) only. **Segment events (including
`segment.base`) do NOT cross the process boundary.** PTS=0 audio from the adapter
arrives in the core with PTS=0 regardless of any seek in the adapter's pipeline.

In the core, `aunixfdsrc` creates its own segment: `start=0, base=0`. A buffer
with PTS=0 has `running_time = 0 - 0 + 0 = 0`. The audiomixer (latency=0) is at
`running_time = T_current` (seconds or minutes in); it drops PTS=0 buffers as late.

**Fix:**

- `fm-core/src/pipeline.rs`: Added `make_audio_transport_src()` that sets
  `do-timestamp=true` on `aunixfdsrc`. This discards the adapter's PTS and
  re-stamps each buffer with `gst_clock_get_time(clock) - base_time` (i.e.,
  current pipeline `running_time`). Since the audio throttle probe ensures
  real-time delivery pace, arrival time ≈ running_time → audiomixer should accept.

- `fm-youtube-adapter/src/main.rs`: Removed startup seek from `Command::Play`
  handler (no-op across the boundary). Removed reconnect seek from
  `post_reconnect_check` (was causing the EOS cycling). Removed `play_seek_done`
  field.

**CC-measured results (session PID 31442, 2026-06-30):**

| Source | Reconnects | Audio chain status |
|---|---|---|
| yt-fc | 4 (EOS every ~4:43 — correct) | add/remove per EOS cycle; no tight cycling |
| yt-nature | 0 | chain added at startup; never removed |
| cam-77 | 0 | chain added at startup; never removed |

Cycling is gone. yt-fc reconnects are clean EOS-based reconnects. yt-nature has
a stable, never-torn-down audio chain for the full session.

**offset-canary warnings:** 20 warnings per reconnect (60 total across 3 reconnects).
Transient: appear during the ~3s post-reconnect window while video PTS=0 buffers
arrive; stop once the compositor stabilizes. Not continuous.

**User result:** Audio still not heard. Heard briefly at some point during the
session, but not sustained.

---

## Current state (awaiting fix)

**Audio flow confirmed up to alevel:** UI audio meters show activity for both YouTube
sources, confirming buffers flow through `aunixfdsrc → ashmcaps → aqueue → aconv →
aresamp → alevel`. The problem is between `alevel → acaps → audiomixer`.

**`do-timestamp=true` evidence unclear:** The `[reconnect-pts]` probe logs show
`first_pts=Some(0:00:00.000000000)` for yt-fc after every reconnect. However, this
probe is on the **video** chain (`voff_q.src`), NOT the audio chain. It confirms
video PTS=0 (which is expected and handled by the compositor). Whether `do-timestamp`
is actually overriding audio PTSes is unconfirmed — there is no analogous probe on
the audio chain.

**Brief audio:** User heard audio from YouTube sources briefly at one point during
the session. This is consistent with `do-timestamp=true` occasionally producing
correctly-timed buffers that the audiomixer accepts, followed by them going late.
Could indicate scheduling jitter that causes buffers to arrive after the audiomixer's
current output window has closed (which latency=0 does not tolerate).

---

## Performance snapshot (session PID 31442, measured ~20 min in)

```
Process                          CPU%    RSS
final-multiplex (core + UI)      963%    15.1 GB
fm-rtsp-adapter cam-27           80%     0.4 GB
fm-rtsp-adapter cam-77           64%     0.5 GB
fm-dummy-adapter                 18%     1.0 GB
fm-youtube-adapter yt-nature     12%     0.2 GB
fm-youtube-adapter yt-fc          9%     0.2 GB
```

UI render rate (`present fps`): **50–60 fps** (healthy — no iced event-loop stall).

Source output rate (user-reported): **~20 fps all sources**, fnaf2 input below 24fps
nominal. 963% main-app CPU on a debug build with 6 sources may be the primary
bottleneck — the compositor, 6 GStreamer decode chains, audio mixing, GPU capture,
and iced render all share the same process.

---

## Next step (for review chat)

**Suspected root cause:** `audiomixer` with `latency=0` drops any buffer whose
`running_time` falls before its current output window. Even if `do-timestamp=true`
stamps buffers with `T_arrival`, OS scheduling jitter (1–50ms) may cause a buffer
to arrive after the audiomixer has already closed and output the window covering
`T_arrival`. With `latency=0` there is no tolerance for this.

**Proposed fix A — audiomixer latency:**
Set `audiomixer.set_property("latency", 200_000_000u64)` (200ms). This makes the
aggregator wait 200ms past a window's nominal end before declaring it closed and
moving on. A buffer that arrives 10–50ms "late" (due to OS jitter) would be
included. Downside: 200ms of output latency for all audio (acceptable for security
camera monitoring).

**Proposed fix B — `is-live=true` on `aunixfdsrc`:**
`unixfdsrc` is not declared live (`is-live=false` by default). GStreamer's
`do-timestamp` behaviour may not apply the clock correctly when the source is
non-live. Setting `is-live=true` makes the element declare itself live to the
pipeline, which ensures the clock is properly applied and the element participates
in latency queries. May interact with `audiomixer`'s aggregation differently. Should
be tried alongside or independently of fix A.

**Proposed fix C — audio PTS probe in core for diagnostic:**
Add a one-shot `BUFFER` probe on `aunixfdsrc_{id}.src` (analogous to
`reconnect-pts` on video) to log `first_pts` and `pipeline_running` after a YouTube
audio chain is built or rebuilt. This would confirm whether `do-timestamp=true` is
actually modifying the PTS or silently failing. If `first_pts ≈ 0`, the property
is not working; if `first_pts ≈ pipeline_running`, it is working and the problem
is audiomixer latency.

**Proposed fix D — root-cause fnaf2/all-sources slow fps:**
963% main-app CPU on a debug binary with 6 sources is likely the ceiling. If the
compositor thread is starved, output fps drops below the configured 30fps. This
is a build-mode symptom — a release build (`cargo build --release`) should be
measured before attributing it to a code defect.
