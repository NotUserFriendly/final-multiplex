# Troubleshooting Log — 0.4
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
amend the entry rather than leaving a false "fixed" behind.

Format. One section per bug. Under it: Attempt N — Hypothesis / Action / Result.

---

## Phase 4 Block 1 — fm-youtube-adapter bringup (2026-06-29)

**What was built:**
New crate `crates/fm-youtube-adapter`.  Resolves a YouTube watch URL to a direct
stream URL via `yt-dlp`, ingests through `uridecodebin3`, and emits RGBA video +
S16LE audio over the existing unixfd transport — the same SDK contract as
`fm-rtsp-adapter` (ADR-0014, ADR-0022).

**yt-dlp format selection:**
`-f "18/22/best[vcodec!=none][acodec!=none]/best"` — prefers muxed formats 18
(360p mp4) or 22 (720p mp4) so a single URL is returned; falls back to any
muxed, then absolute best.  First output line only (avoids split-stream two-URL
output confusing uridecodebin3 into treating only the video half as input).

**`uridecodebin3` vs `rtspsrc + decodebin3`:**
YouTube streams arrive as HTTP/HLS, not RTSP.  `uridecodebin3` handles protocol
negotiation internally; the same `pad-added` callback pattern links video and audio
chains once pads arrive.  `PAD_STABILITY_SECS = 3` delay before emitting `Ready`
gives the element time to deliver both pads before the core marks the source live.

**Scene credential issue — `scene-youtube-test.toml` committed without RTSP credentials, then re-added:**
Initial scene had dummy + file + YouTube only.  Maintainer requested a real RTSP
source be added (dummy doesn't count toward the three-source prototype milestone).
RTSP URI (`rtsp://monitor:...@10.0.0.27`) was added to the scene, but by that
point the file was already tracked by git.  Remediation:
```
git rm --cached scenes/scene-youtube-test.toml
echo "scenes/scene-youtube-test.toml" >> .gitignore
```
The file now has four sources (dummy + file + RTSP + YouTube) and is gitignored.
`exampleInputs.md` and `scenes/scene-mixed-dummy.toml` remain gitignored as before.

**Block 1 EOS/Error handling:**
On EOS or GStreamer Error, Block 1 emits `AdapterMessage::Error` and exits cleanly.
URL-expiry re-resolution was deferred to Block 2 (now implemented).

**Status:** Block 1 compiles clean.  Runtime validation below (Block 2 session, 2026-06-29).

---

## Phase 4 Block 2 — URL-expiry re-resolution (2026-06-29)

**What was built:**
On GStreamer Error or EOS the adapter now:
1. Checks and sets `reconnecting` flag (prevents concurrent reconnect attempts).
2. Increments `reconnect_count`; if count > `MAX_RECONNECTS` (8), emits `Error` and exits.
3. Emits `AdapterMessage::Reconnecting { attempt }`.
4. Spawns a background thread that waits `reconnect_delay_secs(attempt)` (backoff:
   1 → 2 → 4 → 8 → 16 → 30 s, then capped at 30 s for subsequent attempts).
5. Sets `uridecodebin3` to `NULL` (severs dynamic pads, unlinking chain sinks).
6. Emits `StreamsChanged { has_video: false, has_audio: false }` to the core.
7. Re-runs `resolve_ytdlp()` for a fresh signed URL; sets it on the element's `uri` property.
8. Calls `sync_state_with_parent()` to restart the source.
9. Sets `post_reconnect_check_at` so the main loop can emit `StreamsChanged(true, true)` once
   pads re-stabilise (`PAD_STABILITY_SECS` after the last `pad-added` fires).

**Re-entrant `pad-added`:**
The callback checks `chain.sink.is_linked()`.  After a reconnect, `uridecodebin3`'s old pads
are gone and new pads arrive; since the chain sink is now unlinked, the callback re-links
it and calls `sync_state_with_parent()` on the chain elements.  Chains are never torn down
— only the source element cycles through NULL → PLAYING.

**SIGUSR1 forced re-resolve (deterministic test hook):**
A static `AtomicBool FORCE_RECONNECT` is set by a `sigusr1_handler` installed at startup.
The main loop checks it each iteration via `compare_exchange`.  On match it calls the same
`trigger_reconnect()` helper as the Error/EOS paths, so the full recovery sequence
(backoff → re-resolve → restart → StreamsChanged) runs on demand without waiting for real
URL expiry.

**SIGUSR1 validation — confirmed by CC, 2026-06-29 (PID 153263):**
```
kill -USR1 153301   # fm-youtube-adapter pid
```
Log showed full re-resolve cycle:
```
[yt-adapter] SIGUSR1 — forcing re-resolve
[yt-adapter] forced re-resolve #1/8 — immediate
[yt-adapter] fresh URL (truncated): https://rr7---sn-bvvbaxivnuxq5uu-q4fs.googlevideo.com/…
[yt-adapter] video reconnected
[yt-adapter] audio reconnected
[supervisor] 'yt-map' StreamsChanged grace expired — tearing down chain
[pipeline] removed video chain for 'yt-map'
[pipeline] removed audio chain for 'yt-map'
[yt-adapter] StreamsChanged (video=true audio=true)
[pipeline] added video chain for 'yt-map'
[pipeline] added audio chain for 'yt-map'
[gpu-path] native-res probe reinstalled on vdeint_yt-map
```
dummy, fnaf2, and cam-27 emitted no errors during teardown/rebuild.
Visual confirmation (freeze+recovery on tile, other three continuous) — pending maintainer.

**`MAX_RECONNECTS = 8`, backoff table:** `[1, 2, 4, 8, 16, 30]` seconds, then 30 s for
attempts beyond 6.  Chosen to avoid hammering yt-dlp while recovering in reasonable time
for an expired signed URL (typical expiry: several hours into a long session).

**Status:** Block 2 compiles clean; `cargo check --workspace` and `cargo fmt --check` pass.
Commit `0ab396d`.  SIGUSR1 re-resolve sequence confirmed in logs 2026-06-29.
Visual validation: maintainer confirmed other three tiles kept playing during re-resolve, but
the YouTube tile "recovered strangely" (2026-06-29).  Root cause identified — see bug below.

---

## Bug: YouTube reconnect resumes from burst-offset PTS, causing tile to go black (2026-06-29)

**Symptom:** After SIGUSR1 re-resolve (and presumably after a real URL expiry), the YouTube
tile initially appears to recover (pads re-link, `StreamsChanged(true)` emitted, chain
rebuilt), but the tile goes black and stays black instead of showing video.

**Root cause — two compounding issues:**

1. **HTTP CDN burst:** `uridecodebin3` decodes YouTube progressive MP4 (format 18/22) 10–13×
   faster than real-time.  After 8 minutes of session time, the CDN had delivered ~94 minutes
   of video content (measured: `first_pts=1:34:35.574658689` after reconnect in a session
   running for `pipeline_running=0:08:09`).

2. **GStreamer state-reset does not rewind the demuxer:** When `uridecodebin3.set_state(Null)`
   is called and the element is then restarted with a fresh URL, the internal `qtdemux` or
   `souphttpsrc` HTTP range state resumes from the burst offset (byte ~300 MB into the MP4),
   not from byte 0.  The first frame after reconnect carries `PTS ≈ 1:34:35`.

**Why the tile goes black:** The core compositor's pipeline running time at reconnect was
≈ 8 min = 480 s.  The incoming frames had `PTS ≈ 5,675 s` (1:34:35).  `PTS > running_time`,
so all frames are in the FUTURE from the compositor's perspective.  The `voff_q` (max
2000 ms, `leaky=upstream`) holds these future frames, but the compositor finds no frame
matching its current running time (480 s) and renders the tile black.

**Contrast with initial play:** On first play, YouTube frames start at `PTS = 0`.  The
compositor's running time is also small (seconds), so frames are slightly in the past.
The compositor uses the latest available past frame → tile plays normally at 30 fps.  After
reconnect the burst means PTS >> running_time and no frame matches.

**Fix approach — seek to position 0 after reconnect:**
In `spawn_reconnect_thread` (or in the main loop at the `post_reconnect_check_at` gate),
after `uridecodebin3` is stable in PLAYING state, issue:
```rust
let _ = uridecodebin.seek_simple(
    gstreamer::SeekFlags::FLUSH | gstreamer::SeekFlags::KEY_UNIT,
    gstreamer::ClockTime::ZERO,
);
```
This resets the demuxer to byte 0, flushing the burst offset.  After the seek, new frames
start at `PTS = 0` and the compositor can display them (same as initial play).  The seek
should be issued BEFORE emitting `StreamsChanged(true)`, so the core rebuilds its chain onto
a source that's already at position 0.

**Attempted (no effect):** `deep-element-added` signal on `uridecodebin3` to cap the internal
`queue2` buffer — this signal does not fire for `uridecodebin3`'s internal elements (it is not
a standard GstBin in this respect).  `uridecodebin3` exposes only `source-setup` as a signal.

**Fix implemented (2026-06-29):** `seek_simple(FLUSH | KEY_UNIT, ClockTime::ZERO)` issued on
`uridecodebin` immediately before `emit(StreamsChanged(true, true))` in the
`post_reconnect_check_at` gate (`fm-youtube-adapter/src/main.rs`, ~line 469).

**Confirmed by CC, 2026-06-29 (PID 212385):**
`first_pts` after SIGUSR1 re-resolve changed from `1:34:35.574658689` to `0:00:02.002063327`
(pipeline running time 1:06); YouTube tile displayed video immediately after reconnect.
Visual confirmation by maintainer pending (see milestone checkpoint).

---

---

## Bug: File-loop burst + RTSP EOS drove ratchet to 235→240 fps (2026-06-29)

**Symptom:** After the SIGUSR1 re-resolve (seek fix confirmed working), the file source
(`fnaf2`) appeared to play at very low framerate.  Log showed:
```
[pipeline] output fps ratcheted → 235
[pipeline] output fps ratcheted → 240
```
Both commits happened while `yt-map` was fully above-cap (300–500 fps, blocked).
The output compositor was running at 240 fps, starving and destabilizing the tile display.

**Root cause:**  Two independent burst paths both slipped through the cap.

1. **File loop burst (~line 691, ~4 min after SIGUSR1):** `fnaf2` reached EOS → bus loop
   `seek_simple(FLUSH | KEY_UNIT, 0)` → file restarts from PTS=0 while pipeline running
   time is ~5 min.  All 5 min × 30 fps = 9 000 past frames were decoded as fast as
   possible (~235 fps).  This cleared the ratchet settle timer (fps_in→0 briefly) and
   restarted it; 3 s later the burst readings (235 fps) committed since `235 > 240` is
   FALSE with the old cap of 240.

2. **Second file loop burst (~line 1691, ~10 min):** same mechanism; second loop produced
   readings at 240 fps exactly; `240 > 240` is FALSE so it also committed.

**Fix:** `MAX_RATCHET_SOURCE_FPS` lowered from 240 to 65 in `transport.rs`.  65 fps covers
standard cameras (30 fps) and YouTube content (max 60 fps) with a 5 fps jitter margin.
Burst readings from file loops (100–250 fps) and HTTP streaming (300–500 fps) are now
blocked by the cap.  Log message updated from "HTTP burst?" to "burst".

**Status:** Fixed in `transport.rs` (2026-06-29).  Dead `deep-element-added` signal
connection also removed from `fm-youtube-adapter/src/main.rs` — `uridecodebin3` does not
emit this signal for its internal elements (confirmed via `gst-inspect-1.0 uridecodebin3`;
only `source-setup` is exposed).

**Confirmed by CC, 2026-06-29 (PID 226290):**
Full four-source session (dummy + fnaf2 + cam-27 + yt-map).  After SIGUSR1 re-resolve at
1:50 session time:
- Only one ratchet commit in the full session: `output fps ratcheted → 35` (legitimate,
  from fnaf2 settling).  No commits at 235/240 fps.
- `reconnect-pts 'yt-map' first_pts=Some(0:00:00.000000000) pipeline_running=Some(0:01:50)`.
  Post-seek burst (300–400 fps) entirely blocked by the new 65 fps cap.
- File source continued at normal framerate throughout; no tile disturbance observed.
  Visual confirmation by maintainer: all four tiles playing simultaneously, YouTube tile
  froze briefly during re-resolve then recovered (video, not black), file source at normal
  framerate throughout — confirmed 2026-06-29.

---

## Phase 4 milestone checkpoint (pending)

**Milestone definition (from taskblock):**
Three distinct media types simultaneously from config: file + RTSP + YouTube, stable over
a sustained run, offsets settable per source.

**Scene:** `scenes/scene-youtube-test.toml` — four sources:
- `dummy` (fm-dummy-adapter, sanity check)
- `fnaf2` (file:///home/locad/Desktop/FNAF2-cropped.mkv)
- `cam-27` (fm-rtsp-adapter, rtsp://10.0.0.27)
- `yt-map` (fm-youtube-adapter, https://www.youtube.com/watch?v=9vntypeV5QU)

**Validation criteria:**
- All four sources (at minimum: file + RTSP + YouTube) play simultaneously on the GPU path.
- Stable over a sustained run — no source-type interaction, decouple holds.
- Offsets settable per source across the mix.
- YouTube tile survives at least one forced re-resolve (SIGUSR1) without disturbing the others.

**Log-confirmed (CC, 2026-06-29, PID 226290):**
- All four adapters reached `Ready` and their chains were built (`gpu-path` probe installed for
  each: dummy, fnaf2, cam-27, yt-map).
- Output ratchet settled at 35 fps; single commit for the entire session.  No spurious commits
  from YouTube burst (300–420 fps, all blocked by 65 fps cap) or file-loop burst.
- SIGUSR1 → full YouTube re-resolve cycle → chain rebuilt; `first_pts=0:00:00.000000000` at
  `pipeline_running=1:50`; tile recovered to video immediately.  dummy/fnaf2/cam-27 unaffected.

**Note — YouTube HTTP burst (CPU overhead):**
`uridecodebin3` still delivers 300–420 fps to the core probe; all are blocked by the ratchet
cap.  CPU overhead (~800% on RTX 3090 Ti, ~25% of capacity) is acceptable for the milestone.
Proper rate-limiting (e.g. `uridecodebin3` buffer depth via `source-setup` signal) is deferred.

**Milestone criteria — confirmed by maintainer 2026-06-29 (PID 226290):**
- [x] All four tiles visible and playing correct content simultaneously.
- [x] YouTube tile froze during SIGUSR1 re-resolve, recovered (other tiles continuous).
- [x] File source at normal framerate throughout (ratchet held at 35 fps, no spurious commits).
- [x] GPU overlay correctly composited (no torn or frozen tiles).

---
