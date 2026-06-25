# Task Block — converge reconnect onto the cold-start chain-build path

Fixes the cam-77 reconnect freeze. Root cause (confirmed by the cold-start/replug asymmetry):
on replug, the compositor's cam-77 sink pad is session-old and holds an established PTS
timeline, so a stream that restarts at PTS≈0 reads as ~hundreds of seconds in the past and the
aggregator stalls (the ~20 s pulses are its latency-timeout). Cold-start avoids this because it
requests a fresh pad with no timeline. Fix: make reconnect rebuild the chain the same way
cold-start does, instead of reusing the stale pad.

No ADR — this implements ADR-0013 (core handles reconnect topology via `StreamsChanged`), it
doesn't decide anything new. The reasoning lives in a code comment at the rebuild site.

## Group 1 — confirm the PTS gap (cheap insurance before the fix)

- At reconnect, log the new stream's first PTS against the pipeline running time. Expect
  first-PTS ≈ 0 while running time ≈ session age (hundreds of seconds). If it matches, the
  diagnosis is nailed and you proceed. If it does **not**, stop and report — the fix below
  assumes this gap.

## Group 2 — the fix: reconnect tears down and rebuilds via the cold-start path

- On reconnect (adapter respawn → new stream available), **release the cam-77 compositor sink
  pad and tear down its video chain** (voff_q, vcaps, vscale, etc.), then **rebuild it through
  the same `add_video_chain` path cold-start uses** — fresh requested pad, fresh aggregator
  timeline. Same for the audio chain via its mixer pad.
- Concretely: route the reconnect's `StreamsChanged` through the identical teardown+build the
  cold-start path already runs, rather than the current reuse-the-existing-pad branch. The goal
  is that reconnect and cold-start converge on one code path, so the asymmetry can't reappear.
- The rebuild re-applies the source's pad offset fresh (the bounded live offset, ADR-0016) —
  verify it does, so a reconnect doesn't silently drop the user's offset.
- During the rebuild gap the tile shows the black floor (ADR-0018) — that's correct ("no
  signal"), not a bug to suppress.
- **Add a code comment at the rebuild site** explaining why reconnect rebuilds rather than
  reuses: a session-old aggregator pad holds a PTS timeline the restarted stream (PTS≈0)
  cannot satisfy, so the pad must be re-created to reset it. This is the durable record; no ADR.

## Group 3 — guard against disturbing other sources

- Cold-start already proves a pad request on the live compositor doesn't disturb cam-27. The
  new operation here is the **release** of a live pad — confirm releasing cam-77's pad and
  chain leaves cam-27 (and the floors) untouched: no glitch, no dropped frames, no aggregator
  re-negotiation hiccup on the other pads.

## Group 4 — validation (human-in-the-loop; you cannot move the cable)

**STOP-AND-WAIT, same as before: the unplug/replug is physical and only the maintainer can do
it. Set up, then stop and ask the maintainer to act, and wait for confirmation at each step. Do
not simulate or assume the cable moved.**

- Reconnect (the bug): cam-77 streaming → ask maintainer to **unplug**, wait → adapter goes
  Reconnecting, cam-27 unaffected → ask maintainer to **replug**, wait → confirm cam-77 video
  resumes at steady fps with **no ~20 s freeze/pulse pattern**.
- Reconnect + offset: with cam-77 at a +2000 ms offset, confirm the offset still holds after
  reconnect (not dropped, not divergent).
- Cold-start unchanged: offline-at-launch → plug in → still works (didn't regress the path you
  converged onto).
- cam-27 untouched throughout every reconnect.

## Group 5 — housekeeping

- CHANGELOG: cam-77 reconnect freeze fixed by rebuilding the source chain on reconnect (PTS
  timeline reset); note it implements ADR-0013, no new ADR.
- BUGS.md / troubleshooting.md: the reconnect-freeze entry resolves once Group 4 passes — move
  the outcome to CHANGELOG and clear the scratchpad entry.
- DoD checklist per commit.
