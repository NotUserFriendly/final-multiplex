# Task Block — swap Linux transport to unixfd (ADR-0019), then finish the offset path

**What changed:** GDP can't carry decodebin3's event stream on 1.28.2. Per ADR-0019 the
transport becomes platform-selected behind two seams; Linux uses `unixfd`. This removes the
shm + GDP path on Linux and unblocks everything that was stuck behind it.

**Done — do NOT redo:** synthetic floors (ADR-0018, PLAYING timeout resolved), the cascade
fix (Play gated on PLAYING). Both confirmed working.

**Still pending (was blocked behind the transport):** compositor latency, `voff_q`
correction, cam-77 cold-start, validation, test isolation.

Order: settle the premise (Group 1), confirm the tool (Group 2), build the seam + unixfd
(Group 3), then the pending offset items, then validate on **real RTSP**.

## Group 1 — settle the do-timestamp question, once, with a measurement

Before building anything, run the controlled test that was asserted but never cleanly
isolated. Dummy adapter, **no framing**, plain `shmsrc` `do-timestamp=false`, probe core PTS
against adapter PTS.
- **If PTS does NOT cross** (expected — documented shm behavior): proceed to unixfd. The
  question is now settled; do not revisit it.
- **If PTS DOES cross** (unexpected): **stop and report** — the whole framing/transport swap
  may be unnecessary and ADR-0019's premise needs revisiting before you build.

## Group 2 — confirm unixfd is available

- `gst-inspect-1.0 unixfdsink` and `unixfdsrc` resolve on the dev box (need GStreamer ≥ 1.24;
  box is 1.28.2). If absent, stop and report — do not substitute silently.

## Group 3 — transport seam + unixfd (ADR-0019)

- **Introduce the seam.** SDK exposes an output builder ("give me a video/audio output") that
  selects the platform sink; the core's receive-chain builder selects the matching source.
  Adapters call the builder and **never name the transport element** — they stay OS-agnostic.
- **Linux path = unixfd.** Adapter side: `… → unixfdsink`. Core side: `unixfdsrc → …`.
- **Remove the shm + GDP path on Linux:** drop `gdppay`/`gdpdepay` and `shmsink`/`shmsrc` from
  the Linux build. `unixfd` carries PTS/caps/segment/events natively — no framing, and
  `do-timestamp` is irrelevant (metadata arrives intact).
- **Socket lives in the runtime dir** (ADR-0014): unix-domain socket path under the per-PID
  dir, 0600, credentials never in argv. Reuse the existing runtime-dir plumbing.
- Carry over the connection-ordering guarantee (`unixfd`'s equivalent of wait-for-connection /
  the adapter pushing only after `Play`) so a fresh stream never pushes into an unconnected
  consumer.

## Group 4 — compositor latency + voff_q correction (ADR-0016) — still pending

- **Re-add compositor `latency` = ceiling** (still unset in pushed code). With floors + the
  Play-gate + unixfd, re-test it no longer errors. If it still errors, stop and report.
- **`voff_q` is `leaky=downstream` always** (~line 358) with a comment claiming "voff_q alone
  is sufficient — the compositor uses the latest frame." That comment *is* the T3 failure:
  "uses the latest frame" is the leaky drop that makes the offset diverge. Make `voff_q`
  **non-leaky within the ceiling**, leaky only beyond it, and delete that comment. The offset
  holds only with both the latency (aggregator waits) and the non-leaky-within-ceiling queue.

## Group 5 — cam-77 cold-start (still pending)

- `build_shmsrc_chain` (now an `unixfdsrc` chain) builds a complete, queued, offset-capable
  chain **live** on `StreamsChanged` and links it to the compositor/mixer. Validate offline →
  online populates the tile without stalling others; dummy adapter first, then real RTSP.

## Group 6 — test-run isolation (carry-over, still not landed)

Polluted logs from surviving instances already caused one wrong conclusion. Land this:
- **Refuse to launch if an instance is already running** (scan runtime root for live-PID dirs
  via `runtime::is_pid_alive`; clear message + non-zero exit).
- **PID-tied `session.log`** under the per-PID runtime dir.

## Group 7 — validation — ON THE REAL RTSP PATH, not just dummy

The dummy path is exactly what hid the GDP bug. Validate the transport against a live camera's
full event stream.
- **T1:** PTS crosses (adapter PTS == core PTS) over unixfd.
- **T3 (the gate):** offset holds **flat at +2000 ms** within the ceiling on a **live RTSP
  source**, normal frame delivery — not n×2800 divergence. Phase 2 is not done until this
  passes on a live source.
- **Beyond ceiling** clamped; **live negative** clamps to 0, no dead input.
- **cam-77 cold-start:** offline → start → online → tile populates, others unstalled.
- **RTSP smoke:** live camera plays, a 1–2 s offset visibly and stably delays it, no freeze,
  no caps/event errors (the unixfd path should have none).

## Group 8 — enrich the dummy adapter (process fix)

- Make the dummy adapter emit `decodebin3`-like events (stream-collection, tags, stream-start)
  so transport-payload bugs surface on the cheap deterministic path next time, instead of only
  against a live camera. This is the lesson from the GDP miss, made permanent.

## Group 9 — housekeeping

- ADR-0019 authored (review chat) — implement against it. **Change ADR-0015's status line to
  "Superseded by ADR-0019"** — that one-line status edit is the only permitted change to an
  accepted ADR.
- CHANGELOG: transport seam + unixfd on Linux, GDP removed, latency + voff_q correction.
- BUGS.md: move offset-divergence and cam-77 cold-start to `## Fixed` once Group 7 confirms.
- troubleshooting.md: close out the GDP saga (superseded by unixfd), keep nothing stale.
- **Standing rule (still in force):** don't remove validated architecture to clear a symptom —
  flag it. (This time the architecture genuinely was the problem, and it was diagnosed and
  escalated correctly rather than ripped out — that's the right pattern.)
- DoD checklist per commit; commit in chunks (seam+unixfd; offset items; cold-start; isolation).
