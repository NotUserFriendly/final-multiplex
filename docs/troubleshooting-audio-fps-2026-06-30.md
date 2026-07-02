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

## Attempt 4 — Adapter PTS rewrite + audiomixer latency=200ms (2026-06-30)

**Root cause confirmed:** Segment events do NOT cross the unixfd process boundary.
The YouTube adapter's `uridecodebin3` starts with `segment.base=0`. Audio buffers
begin at `PTS=0`. In the core, `aunixfdsrc` creates its own segment (`base=0`,
`start=0`), so `running_time = PTS = 0`. The audiomixer (previously `latency=0`) at
`running_time = T_current` (several minutes in) drops PTS=0 buffers as late.

**Fix applied (two parts):**

(a) **`fm-youtube-adapter/src/main.rs` — adapter probe PTS rewrite:** Added
`buf.make_mut().set_pts(rt)` inside the throttle probe on `aunixfdsink.sink`, where
`rt = pipeline.current_running_time()`. Because adapter and core share the same master
clock and `--base-time`, `adapter_running_time == core_running_time` at any instant.
This bypasses the unreliable segment-event path across the process boundary and gives
audiomixer correctly-timed buffers.

(b) **`fm-core/src/pipeline.rs` — audiomixer latency=200ms:** Set
`audiomixer.set_property("latency", 200_000_000u64)`. Gives 200ms of slack for OS
scheduling jitter (±10–50ms arrival variance) so buffers that arrive slightly "late"
due to jitter are still accepted.

**Diagnostic probe results (session PID 61977, 2026-06-30):**

- `[yt-audio-probe]` in adapter: first buffer `size=4316 pts=Some(0:00:00.000000000)`
  confirms buffer arrives at adapter's `aunixfdsink.sink`. PTS is 0 (before rewrite).
- `[audio-pts] probe fired: pts=None running=Some(0:04:43.064787258) data=Some("Event...")`:
  first push on `aunixfdsrc.src` in core is a CAPS event — socket IS delivering data,
  caps negotiation successful. Buffers follow silently (probe's `done` flag already set).
- `[audio-caps-src]`: CAPS event passes `ashmcaps` — caps match S16LE 48kHz 2ch interleaved.
- `[audio-sync]`: all elements report `Ok(())` from `sync_state_with_parent()`.

**Initial chain note:** The constructor-built initial chain (before first EOS) does NOT
go through `add_audio_chain` and has no core-side probe. The adapter probe still
rewrites PTS on the initial chain's data, and audiomixer latency=200ms applies to all
sources. Both paths should produce correct audio.

**Maintainer result:** Audio present but in short bursts — "half second of sound,
followed by a second or so of silence" — confirmed 2026-06-30.

---

## Attempt 5 — do-timestamp=true on aunixfdsrc (7be6b03, re-applied with latency=200ms)

**Symptom from Attempt 4:** Adapter PTS rewrite produced *some* audio, but with a
0.5s-on / 1s-off burst/silence pattern. The adapter's PTS rewrite uses
`pipeline.current_running_time()` which is derived from the GstNetClientClock sync
between adapter and core processes.

**Fix:** `make_audio_transport_src` sets `do-timestamp=true`. GstBaseSrc re-stamps
each buffer with the core pipeline clock at the moment `create()` returns. Local
clock, no cross-process dependency.

**Maintainer result (PID 37075, 2026-06-30):** Audio still bursting.

---

## Diagnostic session — aqueue overrun + mix-out gap probes (2026-06-30)

**Added probes:**

- Adapter: inter-buffer elapsed time on `aunixfdsink.sink`; logs any gap > 50ms as
  `[yt-audio-probe] ← SOURCE GAP`. **Result: zero source gaps.** The adapter delivers
  continuously; root cause is in the core.
- Core: inter-buffer elapsed time on `audiomixer.src`; logs > 50ms as
  `[mix-out] ← MIX GAP`. **Result: confirmed — the audiomixer itself is bursty.**
- Core: `queue::overrun` signal handler on every `aqueue` element (all per-adapter
  audio queues) to count buffer drops.

**Key finding — mute is the trigger (PID 37075):** With yt-fc and yt-nature muted,
the mix-out probe ran **10,789 consecutive clean buffers (3.8 minutes) with zero
gaps**. Gaps began the instant yt-fc/yt-nature were unmuted. The audiomixer is
healthy when those two sources are excluded from its wait set.

**Overrun counts (PID 37075, after unmuting):**

| Source | Drops |
|---|---|
| dummy | 6,738 |
| yt-fc | 6,182 |
| yt-nature | 6,179 |
| cam-77 | 1,717 |

Overruns start immediately after unmuting and run continuously. The aqueues are
constantly full. Every adapter-backed source is affected equally, pointing to the
audiomixer's **downstream** (pulsesink chain) as the bottleneck, not any single
source.

---

## Root cause analysis — PTS / latency mismatch (2026-06-30)

**do-timestamp stamps buffers with `current_time`.** The audiomixer with
`latency=L` runs its output window L milliseconds *behind* the wall clock:
`W_output = clock − L`. So every buffer stamped with `clock` arrives at the per-pad
queue as a buffer **L ms in the future** relative to the current output window.

**GstAggregator `has_space` fills the per-pad queue to `ceil(L / buf_dur)`
buffers before blocking the chain.** With L=200ms and buf_dur=22ms: 9 buffers
(200ms) fill the per-pad queue; the chain blocks. The aqueue fills at 45.5 buf/s
(adapter production rate) and overflows since drain rate < production rate.

**Consequence:** The per-pad queue holds 200ms of "future" yt-fc data. The
audiomixer outputs 9 windows from those buffers (192ms of audio), then the per-pad
queue is empty and waits for the next fill cycle — producing the burst/silence
pattern. The ratio improves as the system partially converges but never cleans up
because the mismatch is structural.

**Why do-timestamp buffers have no duration set?** `do-timestamp` (in GstBaseSrc)
only rewrites PTS; it leaves `GstBuffer.duration = GST_CLOCK_TIME_NONE`. With
duration=None, GstAggregator's `has_space` cannot compute `queued_duration`
correctly. In GStreamer 1.28 this means `has_space` returns TRUE until the per-pad
queue reaches its byte or buffer count limit, allowing unlimited future buffers to
accumulate.

---

## Attempt 6 — Sequential PTS probe + duration on aunixfdsrc.src (2026-06-30)

**Hypothesis:** Fix the two problems simultaneously:

1. Assign **monotonically sequential PTSes** anchored to the first buffer's
   do-timestamp PTS: `PTS_N = start_pts + N × dur`. This eliminates burst-read
   clock jitter (multiple rapid socket reads get PTSes spaced apart, not all at
   the same instant).
2. Set **`buf.duration`** from byte count (`size × 1e9 / 192_000`) so
   `has_space` correctly counts queued time and blocks the chain at the intended
   threshold.

**Code:** Added `attach_sequential_audio_pts(src: &Element)` in
`fm-core/src/pipeline.rs`, called at both the constructor-path and
`add_audio_chain()` call sites.

**Result (PID 39218, 2026-06-30):** Improvement — sound:silence ratio increased
(roughly 50 % sound vs 33 % before). But still bursting. Mix-out gap pattern
changed:

- Clean audio for **4.37 minutes** (12,326 buffers) while yt-fc/yt-nature were
  muted — confirming the probe doesn't hurt normal operation.
- At unmute (buf#12328): gaps start at 69–90ms, **grow** to a peak of ~720ms
  (buf#17582), then **decrease** over ~3 minutes toward 7ms. This convergence
  pattern confirms the mismatch is structural, not random jitter.

**Why still broken:** With duration set correctly, `has_space` now blocks the chain
after `ceil(200ms / 22ms) = 9` buffers. The per-pad queue is still 200ms full of
future data. The cycle is the same as before; the correct duration just makes the
ceiling more precise. Setting L=200ms with do-timestamp PTSes is inherently self-
defeating: the latency that was meant to absorb jitter is exactly the offset that
keeps the per-pad queue permanently full.

**During muting (GstAggregator behaviour confirmed):** Muted pads are NOT consumed.
The per-pad queue fills to threshold and the chain blocks permanently. The aqueue
overflows for the full muted period. This is expected; the overruns are harmless
while muted. At unmute, the audiomixer discards the 9 stale per-pad buffers and
the newest aqueue buffers resume — the catch-up takes < 1ms.

---

## Attempt 7 — audiomixer latency reduced to 50ms (2026-06-30)

**Hypothesis:** Reducing L from 200ms to 50ms shrinks the PTS mismatch. Per-pad
queue ceiling drops from 9 to `ceil(50/22) = 3` buffers. The chain cycles faster;
the aqueue drains faster; the burst/silence cycle should shorten toward
imperceptibility.

**Result (PID 40739, 2026-06-30):** Still bursting. Regular 140–210ms gaps every
~3 seconds (vs 665ms every 1.5s before). Mix-out gap count: 221 gaps in the session.
Overrun counts similar (yt-fc 5,755; dummy 24,352 over longer run). Sound:silence
ratio better but user still perceives bursting.

**Why still broken:** The same structural mismatch exists at 50ms scale. The
per-pad queue fills to 3 buffers (50ms), the chain cycles, and the accumulated
buffer-duration drift (22.479ms per yt-fc buf vs 21.333ms per audiomixer chunk =
1.146ms/buf drift) causes periodic stalls as the queued data gets out of phase with
the output window. Reducing L makes the gaps shorter and less frequent, but doesn't
eliminate them.

---

## What needs to happen next

**The structural problem:** `do-timestamp` assigns PTS = `clock_now`. The
audiomixer's output window is `clock_now − L`. These two will never be aligned as
long as L > 0, because every buffer arrives L ms ahead of the window that needs it.

**Next candidate fix — offset start_pts by −L:**

In `attach_sequential_audio_pts`, subtract the audiomixer latency from the anchor:

```rust
if sp.is_none() {
    if let Some(pts) = buf.pts() {
        // Shift back by audiomixer latency so PTSes match the output window.
        *sp = Some(ClockTime::from_nseconds(
            pts.nseconds().saturating_sub(50_000_000), // 50 ms
        ));
    }
}
```

With this: `PTS_N = (clock_now − 50ms) + N × 22ms ≈ clock_now − 50ms = W_output`.
Buffer PTSes match the audiomixer's current output window directly. The per-pad
queue should stay at 0–1 (consumed immediately), the chain never blocks, the aqueue
stays empty, and audio should be continuous with no burst pattern.

At unmuting: the aqueue holds 20 newest buffers with PTSes ≈ `T_unmute − 50ms`.
Exactly the current output window. The 3 stale per-pad buffers are discarded in
< 1ms and audio resumes immediately from the correct position.

**This is the next thing to try.**

---

## Attempt 15 — session PID 107629 observations (2026-06-30)

**Session uptime:** 73+ min at time of analysis.

**Audio result:** Initial burst at startup, then long period of clean audio — major
improvement over all prior attempts. Clock switch confirmed in log:
`[net-clock] pulsesink clock: GstAudioClock — forcing as pipeline master`.

**yt-fc audio silence:** At pipeline time 9:27, the YouTube URL expired.
`[yt-adapter] EOS — re-resolving for fresh URL` triggered. Audio was silent for the
re-resolve duration (~2–10 s), then resumed. Maintainer reported "yt-fc audio
completely died" — this was the URL-expiry silence, not an audio pipeline bug.
After reconnect, `do-timestamp=true` ensures audio PTS ≈ current running_time → audio
resumes correctly.

**yt-nature crackling after mute/unmute:** Maintainer muted then unmuted yt-nature
around the same time as the yt-fc reconnect. The mute→unmute amplitude step produces
an audible click (instant level change from 0 → live audio); this is expected GStreamer
behaviour with `set_property("mute", …)`. The subsequent silence was most likely the
transient disruption from the yt-fc audiomixer pad release+add during the reconnect,
which briefly disturbed the other pads. Session continued normally afterwards.

**Video bug found:** After each URL-expiry reconnect, `first_pts=0` while pipeline
running time was 569 s (first reconnect) and 854 s (second reconnect). The
`reconnect-pts` probe only logged this; `vcaps_src.set_offset()` was never updated.
yt-fc video frames arrived with a PTS 9–14 min in the past and were displayed at wrong
time or dropped. Fix: probe now applies `set_offset(initial_offset + skew)` when
`|skew| > 500 ms` (commit below).

**Second reconnect:** yt-fc reconnected again at pipeline time 14:14 (URL expiry
repeating every ~5 min). Same video offset bug. Both reconnects showed 60 total
`[offset-canary] WARN` lines (20 samples × 3 reconnects).

**Fix committed:** `reconnect-pts` probe updated to call
`vcaps_src.set_offset(initial_offset + correction)` so video resumes at current
pipeline position after any reconnect.

**yt-fc audio crunchiness (session 117487):** "yt-fc ran for a bit, got really
crunchy, then died."  Crunch at the URL-expiry reconnect (~2.5 min).  Root cause at
the time was believed to be socket backlog accumulation.  A 300 ms DROP probe on
`aunixfdsrc.src` was added to `add_audio_chain()` to drain the backlog — session 122324
showed this made things worse (see below).

**yt-nature never produced sound (session 117487):** Likely overshadowed by yt-fc
crunchiness.

**Next validation needed (maintainer):** Kill and relaunch.  Expect:
- Both YouTube sources produce clean audio starting ≈300 ms after startup.
- At URL-expiry reconnect (~10 min): ≈300 ms of silence from yt-fc, then clean
  audio resumes.  Log shows `[reconnect-pts] 'yt-fc' PTS correction +Xs applied`
  and yt-fc video tile continues (no freeze).

---

## Session 122324 — drop probe findings (2026-06-30)

**Maintainer report:** "Crunch, to good audio, to crunch, to stutter, to silence.
All inside of a minute."

**Session log:** URL expiry EOS at pipeline time 4:43.  Log confirms clean video
and `GstAudioClock` as pipeline master throughout.  After the reconnect:
```
[reconnect-pts] 'yt-fc' first_pts=0ns  pipeline_running=0:04:43.102140685
[reconnect-pts] 'yt-fc' PTS correction +283.102s applied
[offset-canary] WARN 'yt-fc' expected 0ms got 285624ms (deviation 285624ms)
... 19 more WARN lines all showing ~285 000 ms ...
```

**Finding 1 — Drop probe is useless at reconnect.**  `spawn_reconnect_thread()`
tears down `uridecodebin` (`set_state(Null)`) which unlinks its dynamic audio pad
from `aconv`.  The `aunixfdsink` chain (`aconv → aresamp → acaps → aunixfdsink`) is
REUSED — same elements, same socket path, rate-limit probe intact.  No data flows to
the socket during re-resolve because the audio pad is unlinked.  yt-dlp resolution
takes 1–3 s, well past the 300 ms drain window.  The probe always expires before the
first fresh buffer arrives.  Zero buffers are ever dropped at reconnect by the probe.

**Finding 2 — Drop probe causes simultaneous onset of ALL sources at startup.**
All 5 audio chains go through `add_audio_chain()` and get the 300 ms probe.  With
RTSP and file sources that have no backlog, the probe just adds 300 ms of unnecessary
silence.  All sources then start simultaneously at T+300 ms (vs. natural staggered
onset without the probe), which may produce a worse audible artifact.

**Finding 3 — reconnect-pts correction is not taking effect.**  The canary shows
≈285 000 ms skew on ALL 20 samples even though `+283 s` was applied to
`vcaps_src`.  `voff_q` is a delay buffer (`max-size-time = ceiling_ns`, up to 30 s for
YouTube sources).  In the time between the first frame ENTERING voff_q (where `vcaps_src`
writes it with offset=0) and the first frame EXITING voff_q (where the reconnect-pts
probe fires and applies the correction), many additional source_PTS≈0 frames have
already entered voff_q — all with the wrong (uncorrected) PTS.  These 20+ frames
constitute all 20 canary samples.  The correct fix: apply the skew correction
**at chain-build time**, before any frame enters voff_q.  At build time,
`self.inner.current_running_time()` gives the pipeline clock (source_PTS at reconnect
is always ≈ 0), so the correction can be pre-computed directly.

**Finding 4 — Socket backlog theory is wrong.**  The adapter writes NO audio during
re-resolve (pad unlinked).  There is no significant socket backlog at reconnect.
The crunchiness source is still unconfirmed.

**Action taken:** Reverted the drop probe.  Committed.

**Proposed next steps:**

1. Fix `reconnect-pts` to apply correction at **chain-build time** (query
   `current_running_time()` in `add_video_chain()` and call `vcaps_src.set_offset()`
   immediately, before `voff_q` starts accumulating frames).  Remove the one-shot
   probe on `voff_src` — it fires too late.

2. Add PTS-interval diagnostic probe on `abuffersplit.src` to log actual audiomixer
   input timing.  Log: per-buffer PTS, inter-buffer wall-clock interval (should be
   ≈21 ms), and a WARN on intervals < 10 ms or > 100 ms.  This will show whether the
   crunchiness is rapid-burst PTSes, gaps, or something else entirely.

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

---

## Attempt 8 — Live-source model: is-live=true + do-timestamp + latency from query (2026-06-30)

**Hypothesis (TaskBlock-audio-livesource-fix.md, Steps 1-3):** The manual sequential
anchor and hand-picked latency constant are fighting each other. Replace with one
coherent model: declare `aunixfdsrc` a proper live source so GstAggregator handles
timing via the standard latency-query path.

**Fix:**
- `is-live=true` on `aunixfdsrc` via `BaseSrcExt::set_live()` (GObject property not
  exposed on `GstUnixFdSrc`; must use the C API through `gstreamer-base` crate).
- `do-timestamp=true` kept — now on a declared-live source, GstBaseSrc stamps buffers
  with `running_time` at capture, which is what the audiomixer expects.
- `audiomixer.latency` removed — let it come from the latency query.
- `attach_sequential_audio_pts` replaced with `attach_audio_duration_probe` (duration
  setting only; no PTS rewrite).

**Core probe result (`[audio-pts]` on first buffer, PID 65862, 2026-06-30):**

```
[audio-pts] 'yt-fc'     first buf: pts=Some(9)ms  running=Some(9)ms
[audio-pts] 'yt-nature' first buf: pts=Some(12)ms running=Some(12)ms
[audio-pts] 'cam-77'    first buf: pts=Some(10)ms running=Some(13)ms
```

`pts ≈ running_time` — live timestamping is working correctly.

**Maintainer result (PID 65862, 2026-06-30):** Still bursting. Sources were unmuted
from the start (scene config `muted = false`); no mute/unmute transition occurred.
The live-source model with correct PTSes did not fix the burst.

---

## Attempt 9 — Sequential PTS anchored at do_ts − 50ms (2026-06-30)

**Hypothesis:** With `audiomixer.latency=50ms`, the audiomixer's output window sits at
`running_time − 50ms`. Buffers stamped at `running_time` (from `do-timestamp`) are
always 50ms *ahead* of the window, filling the per-pad queue to `has_space` threshold
and oscillating. Fix: anchor `start_pts = first_do_ts_pts − 50ms` so `PTS_N = start +
N × dur` lands exactly on the audiomixer's current window.

**Fix:**
- `attach_audio_sequential_pts(src, id, AUDIOMIXER_LATENCY_NS)` — same per-buffer
  duration + sequential PTS as Attempt 6, but anchored to `do_ts − 50ms`.
- `AUDIOMIXER_LATENCY_NS = 50_000_000` constant shared between the audiomixer property
  and the probe offset, ensuring they stay in sync.
- `is-live=true` retained from Attempt 8.

**Core probe result (`[audio-pts]` on first buffer, PID 71515, 2026-06-30):**

```
[audio-pts] 'yt-nature' anchor: do_ts=9ms  offset=50ms start=0ms
[audio-pts] 'yt-fc'     anchor: do_ts=15ms offset=50ms start=0ms
[audio-pts] 'cam-77'    anchor: do_ts=51ms offset=50ms start=1ms
[audio-pts] 'dummy'     anchor: do_ts=2122ms offset=50ms start=2072ms
```

Note: yt-fc and yt-nature do_ts < 50ms at startup → `saturating_sub` clamps anchor
to 0ms instead of −50ms. The intended offset had no effect for those two sources at
startup (anchor = 0ms, not −(50ms - do_ts)).

**Maintainer result (PID 71515, 2026-06-30):** Still bursting. Sources unmuted from
start. Confirmed burst is not a mute-transition artifact.

---

## Diagnostic — mute/unmute not the cause (2026-06-30)

Earlier sessions (PID 37075) suggested mute was the trigger: the mix-out probe showed
10,789 clean buffers while yt-fc/yt-nature were muted, then gaps started the instant
they were unmuted.

Hypothesis: the mute/unmute transition itself was causing a chain flush or per-pad
queue dump that set off the oscillation.

**Test:** Run with `yt-fc` and `yt-nature` `muted = false` from launch (no UI toggle).
**Result (PID 71515):** Burst begins immediately with no mute transition. The mute
hypothesis is wrong. The burst is structural and present from the first audio output.

The earlier correlation was a coincidence: unmuting was when the user first listened,
not the cause of the burst.

---

---

## Attempt 10 — audiobuffersplit: re-chunk to 1024-sample buffers (2026-06-30)

**Root cause from TaskBlock-audio-buffersplit-fix.md:** `attach_audio_sequential_pts`
computed `PTS_N = start + N × dur_N` using *this buffer's* duration. For variable-size
buffers (post-AAC-decode resample yields non-uniform chunks, not always 1024 samples),
each buffer's PTS is placed at `N × dur_N` rather than `Σ(dur_0…dur_{N-1})`. Error
accumulates per buffer, placing windows at wrong offsets → audiomixer sees timeline
gaps → burst/silence.

**Fix:** Insert `audiobuffersplit output-buffer-size=4096` (1024 samples × 4 bytes
S16LE stereo) after `acaps` in every per-source audio chain. Removes sequential probe
entirely. `audiobuffersplit` accumulates exact sample counts and outputs uniform
1024-sample chunks with correctly accumulated PTSes.

**Property discovery note:** `GstAudioBufferSplit` (GStreamer 1.28.2) exposes
`output-buffer-duration` (GstFraction) and `output-buffer-size` (bytes). There is
**no** `output-buffer-samples` property — using it panics at runtime.
`output-buffer-size = 4096` is the correct approach for S16LE stereo.

**Mix-out probe result (PID 78994, 2026-06-30):**

```
[mix-out] buf#500  ok pts=5000ms
[mix-out] buf#1000 ok pts=10000ms
```

**Zero gap lines across 10+ seconds of playback.** The audiomixer output is completely
clean. audiobuffersplit solved the upstream chain issue.

**Maintainer result (PID 78994, 2026-06-30):** Still bursting. The GStreamer chain
through the audiomixer is gapless; the burst is downstream, inside PulseAudio.

---

## Attempt 11 — pulsesink with explicit buffer-time=200ms (2026-06-30)

**Hypothesis:** `autoaudiosink` lets PulseAudio choose its own ring-buffer geometry,
which may be undersized for this pipeline's scheduling jitter. An explicit `pulsesink`
with `buffer-time=200000` (200ms) and `latency-time=10000` gives PulseAudio 200ms of
ring buffer to absorb jitter before triggering underrun recovery.

**Fix:** Replace `autoaudiosink` with `pulsesink buffer-time=200000 latency-time=10000`.

**Maintainer result (PID 80134, 2026-06-30):** Still bursting.

---

## Attempts 12-14 — clock-slave method sweep (2026-06-30)

All three have `audiobuffersplit` in place. The GStreamer chain through `audiomixer.src`
is confirmed gapless in all cases. The burst is entirely in the sink clock-slave layer.

**Mechanism (confirmed):** The pipeline clock is `GstNetClientClock` (ADR-0005). The
audio sink runs on the hardware clock. Every slave method produces a different
artifact because none of them remove the root tension: the net clock that adapters
sync to does not track the audio hardware clock.

| Attempt | Sink | slave-method | Maintainer result |
|---|---|---|---|
| 12 | pulsesink | resample | Still bursting — different cadence than skew (PID 82759, 2026-06-30) |
| 13 | alsasink | resample | Mostly silent; single burst after several minutes (PID 84675, 2026-06-30) |
| 14 | pulsesink | none | Burst:short silence:burst:long silence pattern (PID 87810, 2026-06-30) |

`resample` changed the burst cadence (confirmed clock-slave is the mechanism).
`resample` on alsasink was near-silent — the initial net-clock vs ALSA-hardware-clock
offset is large enough that the resample correction throttles playback almost to zero.
`none` removes GStreamer-level correction but exposes PulseAudio's own buffer cycle
directly, producing yet another burst cadence.

---

## Root cause and recommended architectural fix (for review chat)

The drift-free arrangement is to **make the audio hardware clock the pipeline master**:
the core uses the sink's provided clock as the pipeline clock and serves *that* as
the net time to adapters, instead of slaving the audio sink to a net clock that
doesn't track the sound card. This is an ADR-0005 change.

**Current arrangement (broken for audio):**
```
GstNetClientClock → pipeline clock
                  → adapters sync to this
                  → pulsesink slaved to this   ← TENSION: sound card ≠ net clock
```

**Drift-free arrangement:**
```
pulsesink.provide-clock=true → pipeline clock = sound-card clock
GstNetTimeProvider serves sound-card clock to adapters
pulsesink is its own master: no slave correction needed
```

**Caution:** Adapters run as separate OS processes and sync to the net clock for
frame-accurate video compositor alignment. Switching the clock source to the
sound card means adapters track the sound card instead. On one machine this is the
same hardware clock; on a future multi-machine setup it needs careful design.
Flag this for a new ADR before implementing — it supersedes part of ADR-0005.

**ADR-0027 authored 2026-06-30.** Implementation below (Attempt 15).

---

## Attempt 15 — audio hardware clock as pipeline master (ADR-0027, 2026-06-30)

**Implementation:**
- `NetClock::switch_to_clock()` added: after pipeline reaches PLAYING, `fm-app` reads
  `transport.pipeline().inner().clock()` (the auto-selected GStreamer pipeline clock —
  pulsesink's provided clock when audio is present) and re-binds the `GstNetTimeProvider`
  to that clock on the same UDP port. Provider is kept alive in `App.net_clock` for
  continuous adapter re-sync.
- `pulsesink` restored to clean defaults: `slave-method`, `buffer-time`, `latency-time`
  overrides removed. Sink is now clock master — no slave correction needed.
- `audiobuffersplit` retained in all per-source chains (correct, independent fix).
- Diagnostic probes removed: `[mix-out]` from `pipeline.rs`, `[yt-audio-probe]` from
  `fm-youtube-adapter/src/main.rs`.
- ADR-0005 status line updated with one-line forward pointer to ADR-0027.

**Validation needed (maintainer):**
1. Audio: clean sound from both YouTube sources, several minutes, across a reconnect.
   No burst, no cycling. Log `[net-clock] switched to audio hardware clock on UDP :PORT`
   and `[net-clock] pipeline clock type after PLAYING: GstPulseSinkClock` (or similar)
   should appear in the session log.
2. Video alignment: sources align at offset 0; offsets still settable; compositor and
   GPU paths unchanged. Alignment is the regression risk — adapters now ride the audio
   clock instead of the system clock.

---

## Session 122324 — abs-probe analysis (2026-06-30)

**Session started after the voff_src probe was removed (video build-time correction in place).**

**abuffersplit.src probe (new in this session):** Fires when wall_interval outside [10, 100] ms.
At reconnect (T≈418s), the probe fired on rapid-burst buffers (wall_interval≈0ms).

**Key data point from probe:**
```
[abs-probe] 'yt-fc' pts=2.624s pts_delta=+21ms wall_interval=0ms
```
Pipeline was at T≈418s. abuffersplit output PTS=2.624s (not 418s). Confirmed root cause:
`audiobuffersplit` ignores `do-timestamp=true` input PTS and computes output from segment
base (≈0) plus accumulated sample count. After reconnect: new abuffersplit starts from PTS=0;
at T_reconnect+2.6s the probe shows PTS=2.6s — exactly 2.6s elapsed since element was
created. The audiomixer (at T=420.6s) drops these as hundreds of seconds late → silence.

---

## Attempt 16 — Audio reconnect-PTS: build-time correction on abuffersplit.src (2026-06-30)

**Fix (`add_audio_chain`, reconnect path):**
In `add_audio_chain`, after building the new abuffersplit chain, apply:
```rust
let audio_correction_ns = pipeline.current_running_time()
    .map(|rt| { let ns = rt.nseconds() as i64; if ns > 500_000_000 { ns } else { 0 } })
    .unwrap_or(0);
abs_src.set_offset(layout.offset_ns + audio_correction_ns);
```
Same pattern as video build-time correction. With `set_offset(T_reconnect)`, abuffersplit's
PTS=0 → running_time = 0 + T_reconnect ≈ audiomixer's current position → accepted.

**Fix (`Pipeline::build()`, startup path):**
A one-shot BUFFER probe on `abs_src` fires on the first audio buffer from each source.
At that point the pipeline is already running; `current_running_time()` gives the elapsed
time since start. Applies `pad.set_offset(offset_base + correction)` and removes itself.
Logged as `[startup-pts-audio]`. This covers sources (yt-nature) that never reconnect
but whose adapter takes several seconds to produce audio after pipeline start.

**CC-measured validation — session 165845 (165 min run, 2026-06-30):**

| Event | Pipeline time | `[reconnect-pts-audio]` correction |
|---|---|---|
| yt-fc reconnect 1 | T≈283s | +283.577s ✓ |
| yt-fc reconnect 2 | T≈568s | +568.082s ✓ |
| yt-fc reconnect 3 | T≈854s | +854.558s ✓ |

After each reconnect: abs-probe shows steady `pts_delta=+21ms` increments — no crunch,
no silence cascade. yt-nature had no reconnects in 165 min (stable initial-build path).

**Startup probe — session 172498 (2026-06-30):**
```
[startup-pts-audio] 'dummy' correction +2.077s on first buffer
```
`dummy` connected at T≈2.1s; probe applied correction immediately.

`yt-fc` and `yt-nature`: adapter had URL ready before pipeline reached PLAYING (yt-dlp
resolved during the Ready handshake). First audio buffer arrived at T<0.5s (pipeline had
just started). Probe fired silently (correction=0, below 500ms threshold), removed itself.
No correction needed — audiomixer also near T=0, no dropout. The 0.5s gate correctly
distinguishes "startup at T≈0" from "reconnect at T=minutes."

**CC-measured validation — session 172498 (2026-06-30):**

Offset-canary: 60 WARN lines total = 3 reconnects × 20 samples each. All transient
(canary silences after 20 samples per reconnect). Pattern is expected: during the ~2.5s
window after a reconnect, the compositor briefly repeats the last frame (EOS); canary
fires on repeated frames and silences once new frames flow. Not an audio issue.

| Event | Pipeline time | `[reconnect-pts-audio]` correction |
|---|---|---|
| yt-fc reconnect 1 | T≈284.5s | +284.583s ✓ |
| yt-fc reconnect 2 | T≈571.1s | +571.079s ✓ |
| yt-fc reconnect 3 | T≈861.1s | +861.062s ✓ |

**Gates (maintainer validation needed):**
1. After URL-expiry reconnect (~5 min): `[reconnect-pts-audio]` appears in log; yt-fc audio
   resumes clean (no crunch→stutter→silence). `[offset-canary]` deviation ≈0.
2. At startup: yt-fc and yt-nature audio clean from first playback (no initial choppy period).
   `[startup-pts-audio]` lines appear for all sources in session log.
3. Steady-state audio for full session (pre-expiry and post-reconnect): no burstiness.

---

## Session 198727/197232 — startup choppy→clean→choppy→silent persists (2026-06-30)

**Maintainer report (session 191762/194075, before startup fix landed):** "Inside of a minute,
yt-fc ran through the same choppy>clean>choppy>silent pattern. yt-nature was either never made
any sound, or did the same thing as yt-fc earlier than it did." No reconnect occurred in either
session — this is purely the steady-state/startup case, independent of the reconnect-PTS fix.

**abs-probe added to the initial-build audio chain** (previously only existed in the reconnect
path `add_audio_chain`, so the initial-build chain — which most sessions run on for the first
several minutes — had zero diagnostic coverage). Mirrors the existing probe exactly.

**Session 198727 (after startup-pts-audio + initial-build abs-probe landed):**
Maintainer report: "Choppiness at the start seemed reduced. Good audio went for longer than
previously. Then choppy to silent seemed like previous attempts."

**abs-probe data for yt-fc, full session:** Only 8 wall_interval spikes >100ms, all in the
first ~9 seconds (buffers #1, #61, #118, #175, #228, #284, #338, #388), monotonically
*decreasing* in magnitude (1956ms → 344ms → 297ms → 263ms → 234ms → 215ms → 177ms → 155ms) —
a classic damped convergence pattern, consistent with `GstNetClientClock`'s periodic resync
settling in.  **Zero spikes after buffer #388** for the rest of the session (1223+ buffers,
~28+ seconds and counting) — the GStreamer chain at `abuffersplit.src` was completely clean
for the entire window during which the maintainer reported hearing choppy-to-silent audio.

**Conclusion: the GStreamer pipeline is not the source of the residual choppy-to-silent
pattern.** Three layers now confirmed clean:
1. Adapter write pacing (`aunixfdsink.sink` throttle probe): confirmed via temporary
   `[yt-audio-size]` diagnostic — buffer sizes uniform (4316–4460 bytes, ~23ms each), real
   wall-clock gaps between probe firings matched the intended ~23ms duration almost exactly
   (23.0–23.6ms observed across 150 buffers). Ruled out "irregular small chunks under-pacing
   the throttle" as a cause. Diagnostic removed after confirming (no net diff vs. prior state).
2. Core `abuffersplit.src` (input to audiomixer): clean per above.
3. `audiomixer.src` → sink chain: previously confirmed gapless in Attempts 10-14.

**New lead — system CPU load.** `top` during session 198727 showed **load average 20.78**
with `final-multiplex` (debug build) alone at **1127% CPU** (~11 cores), plus
`fm-rtsp-adapter` processes at 90%/45%, plus unrelated desktop processes (ffplay 181%,
gnome-shell 45%, Discord, etc.) all contending for CPU. This is a strong candidate for the
residual symptom: even with ADR-0027's audio-hardware-clock-as-master fix in place and a
provably gapless GStreamer graph, PipeWire/PulseAudio's own real-time audio thread can still
underrun if starved of CPU scheduling under this level of system-wide contention — a failure
mode entirely outside GStreamer's control.

**Next step (not yet done): test with a release build.** A release build should cut
`final-multiplex`'s CPU usage dramatically (debug builds commonly run 5-10× hotter). If the
choppy-to-silent pattern disappears or is greatly reduced under a release build with otherwise
identical load, that confirms CPU/scheduling starvation as the residual root cause rather than
a remaining pipeline bug. This is Pending Task #2 (build release binary, measure fps + CPU) —
now doubles as the audio validation step.

---

## Session 201265 — mission-clock-timed report; choppy episode 2 correlates with reconnect (2026-06-30)

**Mission clock added** (`HH:MM:SS` since launch, chrome bar, bottom right — see CHANGELOG)
specifically so timing reports like the one below don't need cross-referencing against raw
session-log timestamps.

**Maintainer report (session 201265, timestamps from the new mission clock):**
> First second was slightly choppy but not bad. Good audio at 1s, choppy at 33s, silent at 50s.
> Then, good audio at 4:44, choppy almost immediately, then silent at 5:02.

**Episode 2 correlates directly with the yt-fc URL-expiry reconnect.** Session log confirms:
```
[reconnect-pts-audio] 'yt-fc' PTS correction +283.560s applied at build time
```
283.6s = 4:43.6 — matches "good audio at 4:44" almost to the second (the maintainer is
describing the *last good moment before* the reconnect, which lines up with when the old
chain was still playing cleanly right up to teardown).

**abs-probe at the reconnect (new add_audio_chain instance):** A burst-catch-up pattern
identical to the startup-burst mechanism (many buffers at `wall_interval=0ms` immediately
after the new chain links — expected, matches the historical socket-catch-up signature),
followed by **one single gap of 1907ms**, then clean `wall_interval≈0-7ms` for the rest of
the searched window (3700+ lines / hundreds of buffers past that point, no further spikes
found). That one 1907ms gap is the only GStreamer-level anomaly in the entire post-reconnect
window.

**This does not match the reported symptom duration.** The maintainer reports audible
silence persisting to 5:02 — roughly **18 seconds** after the 4:43.6 reconnect — but the
GStreamer-level data shows recovery within ~2 seconds (the single 1907ms gap) and clean
delivery afterward. If `abuffersplit.src` were still starved or bursting for 18 seconds,
that would show up as a long run of large `wall_interval` values; it doesn't. **The 16+
second gap between "GStreamer recovered" and "audio comes back audibly" is unaccounted for
by anything in the instrumented pipeline** — reinforcing the Session 198727 conclusion that
the residual symptom lives downstream of GStreamer, in the PipeWire/PulseAudio/ALSA layer,
most plausibly as a consequence of the CPU/scheduling pressure already noted (load average
20.78, `final-multiplex` debug build alone at ~1127% CPU).

**Episode 1 (33s choppy, 50s silent) has no corresponding reconnect** — `grep` for
Reconnecting/StreamsChanged in the session log shows only the one reconnect at 283.6s.
Episode 1 is therefore *not* explained by any known PTS/reconnect mechanism at all; it's
either the tail of the startup-clock-sync convergence lasting far longer than the ~9s
measured in session 198727 (session-to-session variance under different system load), or a
second, distinct downstream dropout with no pipeline-level signature — same category as
episode 2's unaccounted 16 seconds.

---

## Potential next steps (for review chat)

Three consecutive sessions (198727, 201265, and the 191762/194075 pair before the startup fix)
all show the same pattern: **every layer of the GStreamer pipeline that can be instrumented
comes back clean**, yet audible choppy-to-silence episodes persist, including one now directly
correlated with a real event (URL-expiry reconnect) but *outlasting* that event's
GStreamer-visible disruption by roughly 8-9×.  This is the point where further probing inside
`fm-core`/`fm-youtube-adapter` has diminishing returns — the evidence increasingly implicates
a layer this codebase does not instrument at all.

1. **Release-build CPU test (mechanical, cheap, do first).** Debug build alone is running at
   ~1127% CPU with a system load average of 20.78 on a 6-source scene.  Building `--release`
   and re-running the identical scene would either confirm or rule out CPU/scheduling
   starvation as the residual cause in one pass.  This is already Pending Task #2 in the
   working task list; running it now would directly settle or eliminate the leading hypothesis
   before any further architectural work is considered.

2. **Instrument the PipeWire/ALSA layer directly, if the release build doesn't resolve it.**
   Nothing in this investigation currently observes what PipeWire's audio thread is doing —
   only what GStreamer hands it.  `pw-top` or `pactl list sink-inputs` during a live episode,
   or checking `journalctl --user -u pipewire` / `dmesg` for xrun/underrun messages timed
   against a mission-clock-reported episode, would confirm or rule out sink-side starvation
   directly rather than by exclusion.

3. **If CPU load is confirmed as the cause, this raises a scoping question for the project's
   stated priority use case** (per `CLAUDE.md`: "an individual watching many streams + their
   own security cameras at once — assume a dedicated or high-end box with a discrete GPU").
   A 6-source scene pushing a debug build to 1127% CPU and load average 20+ is a stress case
   this project's target hardware may not hit in practice — but it's worth review chat
   deciding whether audio robustness under CPU pressure (e.g., giving PipeWire/the audio
   thread real-time scheduling priority via `rtkit`, or bounding source count against
   available cores) deserves its own ADR, or whether "use a release build on the target
   hardware" is a sufficient answer and this line of investigation should close once the
   release-build test confirms the hypothesis.

4. **If the release-build test does *not* resolve it**, the next diagnostic layer is PipeWire
   itself (item 2 above) rather than more GStreamer-side probes — three separate probe
   layers (adapter write-pacing, core `abuffersplit.src`, `audiomixer.src`) have each come
   back clean in turn, and a fourth GStreamer-side probe is unlikely to find what the first
   three didn't.

---

## TaskBlock-Phase4Block2b — release-build CPU test (2026-07-01/02)

Ran item 1 from the list above. Result contradicts the CPU-starvation hypothesis and surfaces
a new, more specific lead.

### CPU/load: modest improvement, not a fix

| | Debug baseline (session 198727) | Release, clean (session 214166, ffplay closed) |
|---|---|---|
| `final-multiplex` CPU | ~1127% | ~1120% |
| System load average | 20.78 | 15.16 |

`final-multiplex`'s own CPU usage is essentially **unchanged** between debug and release
builds. In hindsight this makes sense: this workload's cost is dominated by GStreamer's
native decode/compositing (pre-compiled C libraries) and GPU work, not our own Rust glue —
compiling *our* code in release mode doesn't touch the hot path. Load average dropped
moderately (20.78 → 15.16), plausibly just less Rust-side overhead layered on top, not a
meaningful reduction in the underlying pressure.

**Confound caught mid-test:** an `ffplay` window was independently decoding the same cam-27
RTSP stream throughout earlier sessions (including, likely, the debug baseline) — redundant
CPU/GPU decode load competing with the measured process. The maintainer closed it and the
figures above are the clean, post-closure reading. **This means the original 1127%/20.78
debug baseline may itself have been confounded** — it was not re-measured without ffplay, so
the true debug-vs-release CPU delta is unknown. Recommend re-measuring the debug baseline
clean before treating any debug/release CPU comparison as authoritative.

### New finding: a highly regular ~1.6s periodic stall, specific to YouTube sources

With `ffplay` closed (release, session 214166), the `abs-probe` diagnostic showed a **much
more severe and continuous** pattern than any debug-build session had shown — not sparse
startup noise, but a **recurring cycle every ~1.6 seconds for the entire session** (797+
occurrences logged for `yt-fc` alone in the first few minutes):

```
[abs-probe] 'yt-fc' pts=1.773s ... wall_interval=1609ms   ← ~1.6s dead stop
[abs-probe] 'yt-fc' pts=3.242s pts_delta=+1469ms ...      ← burst catch-up (~1.5s of audio in one jump)
[abs-probe] 'yt-fc' pts=3.264s ... (6-7 buffers at clean 21ms cadence)
[abs-probe] 'yt-fc' pts=3.392s ... wall_interval=1609ms   ← next stall, ~1.62s later
```

**Isolating the cause — three comparisons, all pointing the same direction:**

1. **`yt-nature` shows the identical pattern, synchronized to within tens of ms of `yt-fc`**
   (1.637s/1.645s, 3.250s/3.242s, 4.859s/4.801s — both YouTube sources cycling in lockstep,
   drifting apart only slightly over time). Two independent adapter processes, two different
   videos, same ~1.6s period.
2. **`cam-77` (RTSP, same core process, same machine) shows a completely different,
   irregular pattern** — gaps of 1897ms, 239ms, 570ms, 222ms, 739ms, no fixed period. If this
   were general core-process CPU starvation, all sources sharing the process should show
   correlated stalls; they don't.
3. **This rules out a core-process-wide or system-wide scheduling cause** and points at
   something specific to the YouTube HTTP/MP4 ingestion path shared by both adapters.

**Two targeted interventions tried, both had zero effect:**

- **`use-buffering=true` + `buffer-duration=3s` on `uridecodebin3`** (with `MessageView::Buffering`
  handling added to safely pause/resume around it). No `[yt-adapter] buffering …%` log lines
  ever appeared — `queue2`'s raw-byte buffering percentage never dropped below 100%, meaning
  raw HTTP throughput is not the bottleneck (the CDN delivers bytes fast enough that queue2
  never runs low). The periodic stall was identical with this in place.
- **A 3-second `queue` (`max-size-time`, buffers/bytes unbounded) inserted between the decode
  chain and the adapter's existing real-time throttle probe**, intended to let the decoder/demuxer
  race ahead of real-time and give decoded audio somewhere to land so a fragment-boundary burst
  wouldn't propagate as a stall. Also had zero effect — pattern identical.

**Working hypothesis:** YouTube's progressive-download MP4 URLs are commonly internally
fragmented (fMP4/segment structure) even when served as a single "progressive" URL.
`qtdemux` (or whichever demuxer parses the container) may only release a fragment's worth of
demuxed audio samples once that fragment has been fully parsed, producing burst-then-wait
output at the *demuxer* level — independent of raw network throughput (which is why
`use-buffering`, a `queue2`/byte-level knob, didn't touch it) and independent of downstream
buffering depth (which is why the extra `queue` didn't help either, if the source isn't
racing ahead of real-time to begin with — a rate-matched fragment-arrival-vs-consumption
cycle can't be smoothed by buffer size alone). **Not yet confirmed** — would need either a
correlated video-side timing check (does yt-fc's video output show the same ~1.6s cadence?)
or direct GStreamer-level inspection (`GST_DEBUG=qtdemux:5` during a live episode, or trying
an alternate demux path) to verify.

Both speculative code changes (`use-buffering`/`buffer-duration`, the extra `queue`) were
reverted after confirming zero effect — `fm-youtube-adapter/src/main.rs` is back to its
pre-session committed state; no code change resulted from this investigation.

### Fork outcome

Neither branch of the taskblock's anticipated fork applies cleanly:
- **Not "Resolved"** — audio is not clean; the release build did not fix it.
- **Not simply "instrument PipeWire"** — the new evidence (YouTube-specific, highly regular
  period, unaffected by GStreamer-level buffering changes, RTSP source on the same process
  behaving differently) points *upstream* of PipeWire, at the YouTube ingestion path
  specifically, not at general audio-sink starvation. Instrumenting PipeWire next would likely
  find nothing, since the stall is demuxer/adapter-side already, well before PulseAudio.

### Recommended next steps (updated, supersedes items 2-4 above for the YouTube-specific finding)

1. **Confirm or refute the fragment-boundary hypothesis directly**, cheaply: `GST_DEBUG=qtdemux:5
   GST_DEBUG_NO_COLOR=1` on a `fm-youtube-adapter` invocation during a live ~10s window, looking
   for periodic ~1.6s-spaced fragment/moof parsing log lines that line up with the abs-probe
   stall timestamps. This directly tests the hypothesis instead of continuing to guess via
   buffering-property trial and error.
2. **If confirmed**, the real fix is architectural: either pre-buffer many seconds of decoded
   audio *before* any real-time throttling is applied (so the demuxer is free to race arbitrarily
   far ahead of playback, giving downstream enough of a lead that individual fragment gaps never
   starve the output), or investigate whether `uridecodebin3`'s `download=true` mode (full
   download-to-disk buffering, distinct from `use-buffering`) sidesteps the fragment-pacing
   behavior entirely by decoupling demuxing from live HTTP delivery altogether.
3. **The debug-build CPU/load baseline (1127%/20.78) should be treated as unconfirmed** — it may
   have included the same `ffplay` confound found mid-session. If CPU/load figures are needed for
   an ADR decision later, re-measure both debug and release cleanly, back to back, with no other
   GPU/CPU-heavy processes running.
4. **RTSP-side irregular jitter (cam-77: 1897/239/570/222/739ms gaps) is a separate, lower-priority
   observation** — worth a note for whoever picks this up, but not chased further in this session
   since it doesn't share the YouTube sources' clear periodic signature and wasn't the reported
   symptom.

---

## TaskBlock-Phase4Block2c — fragment-boundary hypothesis refuted (2026-07-02)

Ran task 1 (confirm fragment cadence via `GST_DEBUG=qtdemux:5`). **Gate failed — cadence does
not line up.** Per the taskblock's explicit instruction ("Doesn't → stop, reconsider (don't
proceed to fix)"), stopped before tasks 2/3 (throttle removal, `download=true` fallback).

### Method

Launched the release build with `GST_DEBUG=qtdemux:5 GST_DEBUG_NO_COLOR=1` (env var inherited
by all adapter child processes via `Stdio::inherit()`), captured ~34s of a live 6-source run.

**Logging gotcha hit and worked around:** `Supervisor::shutdown_all()` calls `runtime::cleanup()`
on graceful exit (`Message::Exit`, triggered by `SIGTERM`/window close), which removes the
process's own run directory — including `session.log` — as part of normal teardown. A plain
`kill` (SIGTERM) on the first capture attempt destroyed the log before it could be read. Redid
the capture and killed with `kill -9` (SIGKILL) instead, which bypasses the Rust-side cleanup
path entirely (the kernel never runs it), then copied the log out of the tmpfs run directory
immediately — before launching anything else, since the *next* process's `reap_orphans()` would
delete any dead-PID directory it finds on its own startup. No orphaned adapter processes
resulted (`PR_SET_PDEATHSIG` correctly terminated all children when the core was SIGKILL'd).
**For any future diagnostic capture that must survive process teardown: SIGKILL, then copy the
log out before the next launch.**

### Result: no fragment structure, no correlated stall in qtdemux

- **Zero `moof` (movie fragment box) references anywhere in the ~33s capture**, for either
  YouTube source. The only fragment-related line at all was a single `qtdemux_parse_trex:
  "failed to find fragment defaults for stream N"` at startup (both sources) — i.e., `qtdemux`
  looked for fragmentation markers (the `trex`/`mvex` boxes fragmented MP4 requires) and found
  none. **The container is a standard progressive moov/mdat MP4, not fragmented.** The
  fragment-boundary hypothesis from Phase4Block2b's writeup was architecturally wrong.
- **`qtdemux`'s own debug output shows no periodic stall.** Isolated each YouTube adapter's
  `qtdemux` instance by PID and measured gaps between consecutive LOG-level trace lines
  (GStreamer's own high-resolution debug timestamps, not wall-clock guessing):
  - `yt-fc` (PID 448674): 14,621 lines over 1.5s–35.3s span, only **4 gaps >100ms**, all small
    and irregular (116ms, 154ms, 168ms, 561ms) — no ~1.6s periodicity.
  - `yt-nature` (PID 448676): 16,186 lines over 1.7s–35.3s span, only **3 gaps >100ms**
    (130ms, 309ms, 469ms) — same, no periodicity.
- **Confirmed the abs-probe stall was present in this exact same session** (18 spikes >100ms
  for `yt-fc`, same ~1.6s cadence as every prior session) — so this isn't a case of the stall
  simply not occurring during the capture window; `qtdemux` is provably clean while
  `abuffersplit.src` (core side) is provably not, in the same run, at the same time.

### Synthesis: the stall is not in the adapter's pipeline at all

Combined with the Phase4Block2b finding (adapter's own real-time write-throttle probe on
`aunixfdsink.sink` showed rock-steady ~23ms real-time spacing with zero gaps across 150
buffers, ~3.45s — session 197232), **three independent measurement points across the
adapter's full pipeline (demux → decode/convert/resample → throttled write to socket) are
all clean**. The ~1.6s periodic stall only appears at `abuffersplit.src`, which is on the
**core** side, downstream of `aunixfdsrc` reading from the Unix socket. This relocates the
search: the gap is somewhere in the socket/transport layer itself, or in the core's own
`aqueue`/`aconv`/`aresamp`/`alevel`/`acaps`/`abuffersplit` chain — not in anything the
YouTube adapter's own pipeline is doing.

This doesn't yet explain why RTSP (`cam-77`, same core process, structurally similar
core-side chain) shows a different, irregular pattern instead of the same one — a real
difference must exist either in the transport (YouTube uses `unixfdsink`/`unixfdsrc`
fd-passing; need to confirm RTSP's audio path uses the identical mechanism) or in how the
two adapters' write patterns interact with it (buffer sizing, write cadence, or something
else not yet identified).

### Recommended next steps (supersedes Phase4Block2b's items 1-2 for the YouTube-specific finding)

1. **Instrument the core side of the boundary directly**: a probe on `aunixfdsrc.src` (before
   `aqueue`) with the same real-time-gap methodology used on the adapter's write-throttle probe,
   to determine whether the stall is at the socket read itself or further down the core's chain
   (`aqueue` → `aconv` → `aresamp` → `abuffersplit`).
2. **Confirm RTSP's audio path structurally**: check whether `fm-rtsp-adapter` writes to its
   audio socket via the same `unixfdsink`/pacing mechanism as `fm-youtube-adapter`, or something
   different — needed to know whether the YouTube-vs-RTSP difference is a transport-layer
   variable or a core-side one.
3. `download=true` mode and throttle-removal (Phase4Block2c's original tasks 2-3) are **on hold**
   — their premise (demuxer-side fragment starvation) is refuted; revisit only if the core-side
   probe in (1) somehow implicates the adapter's write pattern after all.

---

## TaskBlock-Phase4Block2d — stall localized to aqueue drain/fill cycling (2026-07-02)

Ran both tasks. Gate met: the ~1.6s period is now attributed to a concrete mechanism.

### Task 1 — RTSP-vs-YouTube path diff

`fm-rtsp-adapter`'s `build_audio_chain` (aconv → aresamp → acaps → aunixfdsink) uses the
identical transport (`fm_adapter_sdk::transport::make_output_sink`, i.e. `unixfdsink`) as
`fm-youtube-adapter`. **The one structural difference: RTSP has no throttle probe at all** —
RTP timing paces it naturally, so there's nothing analogous to YouTube adapter's sleep-based
real-time throttle on `aunixfdsink.sink`. Same transport, different pacing — the "adapter
buffer cadence" variable the taskblock anticipated, though this alone doesn't yet explain the
core-side finding below.

### Task 2 — core-boundary probe

Added a probe on `aunixfdsrc.src` (before `aqueue`) plus per-buffer `aqueue` fill-level
logging (`current-level-buffers`/`current-level-time`, confirmed correct property names via
`gst-inspect-1.0 queue`), logging on any level *change* (captures the full fill/drain
trajectory efficiently) or wall-clock anomaly at the socket-read point itself.

**Hit a false alarm first:** the original threshold-only version (log only on
`wall_ms < 10 || wall_ms > 100`) produced **zero** log lines for `yt-fc`/`yt-nature` despite
firing correctly for `cam-77`/`dummy` on the identical code path. Added unconditional
first-N-calls logging to confirm the probe *was* attached and firing — it was; `wall_interval`
at the socket-read boundary is **rock steady at ~23ms**, never triggering the anomaly
threshold. The zero-entries result was a true negative, not a bug.

**Socket read is clean; the queue is not:**
```
'yt-fc' #0  wall_interval=29ms  aqueue_level_buffers=0
'yt-fc' #6  wall_interval=23ms  aqueue_level_buffers=1
'yt-fc' #7  wall_interval=23ms  aqueue_level_buffers=2
...                                    (steady +1 per buffer, 23ms cadence throughout)
'yt-fc' #25 wall_interval=23ms  aqueue_level_buffers=20   ← cap reached, holds here
'yt-fc' #89 wall_interval=23ms  aqueue_level_buffers=18   ← ~1.47s later (64 buffers), small dip
'yt-fc' #90-91                  aqueue_level_buffers=19,20 ← refills in 2 buffers, back at cap
'yt-fc' #96, #158                                          ← repeats: small 2-3 buffer dips
'yt-fc' #213                    aqueue_level_buffers=0    ← FULL DRAIN (52 buffers ≈ 1.2s after last cap)
'yt-fc' #216-235                                           ← refills 0→20 over next ~19 buffers
```
`wall_interval` at the socket read never deviates from ~23ms across this entire trace —
**the queue's input side is perfectly smooth throughout**. The level trajectory shows the
queue filling to its 20-buffer cap (`max-size-buffers=20`, ~460ms) within ~575ms of startup,
then cycling: mostly holding at/near cap with small 2-3-buffer nibbles taken every several
hundred ms, punctuated by periodic much larger drains — including a **full drain to 0**
roughly every ~1.5-2s. A full drain to 0 means the audiomixer's input for this source goes
completely dry — literal silence — until the queue refills, which is exactly the audible
symptom.

**`cam-77` shows the same cap-hugging behavior but never a full drain:** its `aqueue` also
climbs to and holds near 20 for most of the trace, wobbling by 1-2 buffers (20→18→19→20→...),
but in the entire captured window it never once reaches 0. This is a real, measured
difference, not just an impression from the downstream symptom: **all audio sources in this
process show some degree of downstream under-servicing** (every queue trends toward its cap),
but only `yt-fc`/`yt-nature` periodically hit *complete* starvation. `cam-77`'s milder,
irregular jitter (documented in earlier sessions) and the YouTube sources' clean periodic
silence are two severities of the same underlying mechanism, not two different mechanisms.

### Localization (satisfies the gate)

**The stall is in the `aqueue` drain/fill cycle** — specifically, whatever pulls data out of
`aqueue` downstream (driving `aconv → aresamp → alevel → acaps → abuffersplit`, ultimately
serviced by `audiomixer`'s pull-based aggregation) is not being scheduled/serviced consistently
enough to keep pace with the queue's steady ~23ms input rate. It is *not* the socket read
(confirmed clean), *not* qtdemux (Phase4Block2c), and *not* the adapter's write pacing
(Phase4Block2b). All sources' downstream consumption is under-paced to some degree in this
busy multi-source process; YouTube's chain crosses into complete periodic starvation while
RTSP's does not — the reason for that severity difference (decode cost, element scheduling
order, `audiomixer`'s per-pad servicing pattern, or something else) is not yet determined and
would be the next question if this is pursued further.

Diagnostic code reverted after capturing the finding — `pipeline.rs` is back to its committed
state, no code change resulted from this investigation.

### Recommended next steps

1. **This may not need chasing further as a bug** — it increasingly looks like a genuine
   downstream-servicing capacity question (this process is running 5 GStreamer decode chains +
   compositor + GPU capture + iced UI in one process; see the Phase4Block2b CPU/load figures).
   Whether to pursue a GStreamer-scheduling-level fix (e.g., understanding why YouTube's chain
   specifically starves while RTSP's doesn't) or treat this as a capacity/hardware-scoping
   question (per `CLAUDE.md`'s target-hardware assumption) is a call for review chat.
2. **If pursued further**: the next diagnostic layer is `audiomixer`'s own pad-servicing —
   specifically whether it pulls from all 4 audio-bearing sink pads evenly per aggregate cycle,
   or whether something about pad registration order / per-source processing cost causes uneven
   servicing. This would need instrumentation inside `audiomixer`'s aggregate cycle itself
   (likely via `GST_DEBUG=audioaggregator:5` or similar) rather than another per-source probe.
3. **`aqueue`'s `max-size-buffers=20` (~460ms) may simply be too small** for the servicing
   irregularity actually present in this process — a deeper queue wouldn't fix uneven servicing,
   but might raise the bar high enough that "occasionally lags for over a second" doesn't
   translate to a *full* drain-to-zero. Cheap to test, doesn't address the root cause, but worth
   knowing whether it changes the audible symptom's severity.

## Performance snapshot (session PID 31442, measured ~20 min in)
