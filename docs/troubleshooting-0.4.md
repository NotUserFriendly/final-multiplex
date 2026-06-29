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

**Not yet attempted:** seek to position 0 after reconnect (above).

**Status:** Bug confirmed by maintainer visual 2026-06-29.  Fix designed, not yet implemented.

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

**Log-confirmed (CC, 2026-06-29, PID 153263):**
- All four adapters reached `Ready` and their chains were built (`gpu-path` probe installed for
  each: dummy, fnaf2, cam-27, yt-map).
- Output ratchet settled at 35 fps; compositor ran at ~53 fps present (GPU Fifo path).
- SIGUSR1 → full YouTube re-resolve cycle → chain rebuilt; dummy/fnaf2/cam-27 unaffected.
- No errors on any source over 5+ minutes of sustained run.

**Known issue (deferred) — YouTube HTTP burst:**
`uridecodebin3` decodes YouTube MP4 content 10–13× faster than real-time (HTTP CDN delivery
speed).  The burst floods the core pipeline's `vcaps:src` probe at 300–420 fps.  The
`MAX_RATCHET_SOURCE_FPS = 240` cap in `transport.rs` prevents the ratchet from committing a
false high-fps lock.  On a 32-core/RTX 3090 Ti machine the CPU overhead (~800% = ~25% of
capacity) is acceptable for the milestone; proper rate-limiting (e.g. limiting `uridecodebin3`
buffer depth) is deferred.  See BUGS.md when filed.

**Status — pending maintainer visual confirmation:**
- [ ] All four tiles visible and playing correct content simultaneously.
- [ ] YouTube tile froze during SIGUSR1 re-resolve, recovered (other tiles continuous).
- [ ] GPU overlay is correctly composited (no torn or frozen tiles).

---
