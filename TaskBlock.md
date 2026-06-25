# Task Block — SPIKE: GDP-framed shm to carry PTS across the boundary

**Measure first. Commit nothing architectural until the data passes.** The bet: wrapping
the shm payload in the GStreamer Data Protocol (`gdppay`/`gdpdepay`) makes buffer PTS, caps,
and segment survive the process boundary — which should fix the offset divergence (Test 3)
and likely the cam-77 freeze. Prove it on the dummy path, then report back. The ADR is
authored in the review chat after the measurements, per the working agreement — **do not
author or edit an ADR in this pass.**

Why GDP over `unixfd`: GDP keeps the existing shm transport (ADR-0011) and just frames the
payload, and it is platform-portable — it works over any byte transport including on the
Windows demo target, where `unixfdsink`'s unix-domain-socket model does not fit. So this is
an *amendment* to ADR-0011 (shm retained; payload now GDP-framed), not a transport swap.

## Pre-check
- Confirm `gst-inspect-1.0 gdppay` and `gdpdepay` resolve on the dev box (they're in
  gst-plugins-bad). If absent, stop and report — do not substitute silently.

## The change (minimal, dummy path first)
- **Adapter** (`fm-dummy-adapter` first): insert `gdppay` just before each shmsink —
  `... -> vcaps -> gdppay -> vshmsink`, same for audio. `gdppay` serializes each buffer with
  its PTS/DTS/caps/segment into the byte stream the shmsink writes.
- **Core** (`build_shmsrc_chain` and the initial build path): insert `gdpdepay` right after
  each shmsrc — `shmsrc -> gdpdepay -> vshmcaps -> queue -> ...`. `gdpdepay` reconstructs the
  buffers with their original PTS/caps.
- **Set `do-timestamp=false` on the core `vshmsrc`/`ashmsrc`.** This is the crux: we now
  *want* the adapter's PTS (restored by gdpdepay) to pass through, not be overwritten by
  arrival stamping. (Test 1's toggle failed before *because* bare shm carried no PTS;
  gdpdepay now supplies it, so do-timestamp=false should work.)
- **Leave adapter shmsink `sync=false`.** With PTS meaningful end-to-end, the core presents
  per-PTS against the shared clock; the adapter should still push timestamped buffers ASAP.
  Pacing at the adapter sink (`sync=true`) is what caused the reconnect-freeze, so keep it
  off here.
- **Caps:** `gdpdepay` restores caps, so the `vshmcaps`/`ashmcaps` capsfilter after it may
  now be redundant or could conflict. Verify negotiation works; keep the capsfilter only if
  it agrees with gdpdepay's output, otherwise drop it.

## Measurements (re-run the keystone tests on the dummy path)

1. **Test 1 redux — does PTS now cross?** Probe PTS at the adapter (before `gdppay`) and at
   the core (after `gdpdepay`) for the same source. **Pass = the two PTS series now match**
   (preserved), where before they were unrelated (arrival-stamped).
2. **Test 3 redux — does the offset hold?** Two dummy sources, offset one by 2000 ms.
   **Pass = a flat, stable +2000 ms shift** (not the old n×2000 ms divergence), with the
   normal share of frames reaching the compositor (not 5%).
3. **Test 2 redux — A/V still locked?** Confirm per-source A/V skew is still stable (now via
   real PTS rather than coincidental arrival timing).
4. **RTSP smoke (after dummy passes):** point one adapter at a live camera, confirm it plays
   and the `recv_q` backpressure freeze does not recur. Quick smoke only — not full RTSP
   integration.

## Gate
- **All pass (esp. 1 and 3):** stop and report the data. The review chat authors the ADR
  amending ADR-0011 (shm retained, payload GDP-framed, do-timestamp off), then you implement
  fully across both adapters and the topology-build path.
- **Fail (PTS still not crossing, or offset still diverges):** report the data, commit
  nothing, and we reassess (e.g. `unixfd` on Linux as a fallback). Do not paper over a
  failure by re-enabling arrival stamping.

## Scope — do NOT do in this pass
- No live topology-build fix (the cam-77 cold-start case) — that comes after, against the
  new framed chain.
- No metrics rework, no `fps_in` fix, no visual state overlays — separate passes.
- No full RTSP integration beyond the smoke test.

## Housekeeping
- Record the measured PTS series and offset numbers in `troubleshooting.md` (active
  investigation), numbers not conclusions.
- Temporary probes reverted (or behind a debug flag) before finishing.
- No ADR authored/edited by CC. No CHANGELOG entry yet — nothing ships from a spike until
  the architecture decision is made.
