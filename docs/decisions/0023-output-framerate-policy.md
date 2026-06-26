# 0023. Output framerate: ratchet-up high-water mark

- **Status:** Accepted
- **Date:** 2026-06-25

## Context

Sources have widely varying native framerates: RTSP cameras 10–35, YouTube/Twitch 60, phone
and pro cameras 120+. Phase 2.2 made `fps_in` measure the real per-source rate, retiring the
pinned-30 assumption in the metric — but the pipeline still needs a single **output** framerate,
and the rest of the framerate-dependent code (offset-buffer sizing, the offset canary's
tolerance) still assumes 30.

Three candidate output policies:
- **Fixed low (today, ~30):** downsamples every faster source. Wrong for a 60/120 world.
- **Fixed high (assume 60 or 120):** wastes resources when every source is slow, and still caps
  anything faster than the guess.
- **Fully dynamic (track the current max, up *and* down):** thrashes — the output rate
  renegotiates constantly as source rates fluctuate, producing ongoing jitter.

## Decision

The output framerate is a **monotonic high-water mark within a session**: it ratchets **up** to
the maximum input rate observed across active sources and **never comes back down**.

- A source arriving at 15 fixes the output at 15. A burst to 35 ratchets the output to 35. A
  later fall to 24 leaves the output at 35.
- Renegotiation happens **only when a new maximum appears**, then settles. After the initial
  ramp the output rate is stable.
- The mark is per-session: it only rises while running, and resets to fresh discovery on scene
  (re)load.
- Per-source input rates are tracked individually; a source slower than the output rate has its
  frames repeated up to it.

## Consequences

- **Discovers the real ceiling** instead of assuming 60/120 — efficient for an all-slow wall,
  captures fast sources when present.
- **Stable after settling.** Renegotiation fires only on a new high, so the wall isn't
  continuously jittering. Some initial jitter as it ramps is accepted.
- **Trades efficiency for stability:** removing the fastest source does *not* lower the output —
  it keeps running at the high-water rate (repeating frames for slower sources). The right trade
  for a display meant to run unattended; churn-avoidance beats marginal efficiency. A manual
  re-evaluate/reset is a possible future knob, not built now.
- **Runtime caps renegotiation on the live compositor is required**, but bounded — it occurs on
  ratchet-up events only, never per-frame. (Implementation hazard in the same class as dynamic
  pads, but rare.)
- The framerate-dependent code keys off **real** per-source and output rates: offset-buffer
  sizing (`frames = ms × fps`), the canary's frame-period tolerance, the fps metrics. The 30 fps
  assumption is retired here.
- The output rate is **discovered at runtime**, not known at config time — downstream consumers
  (encoders, the canary window) must read it dynamically rather than assume a constant.

This is the policy Phase 2.3 implements.
