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
Visual validation pending maintainer.

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
