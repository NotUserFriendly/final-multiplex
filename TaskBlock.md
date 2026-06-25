# Task Block — Phase 2 close-out: seam, reconnect-freeze, dummy enrichment, log clear

Phase 2's gate is met (T3 flat offset on live RTSP). These are the close-out items before
Phase 3. None should change the validated offset behavior — Group 1 is a behavior-preserving
refactor, and T3 on live RTSP is the regression gate for it.

## Group 1 — finish the transport seam (ADR-0019)

Right now both adapters call `make("unixfdsink")` directly and the core builds `unixfdsrc`
inline — the per-platform element choice is hardcoded, not behind the seam ADR-0019 specified.
Realize the seam so adding a platform is one match arm at each end, not an edit per adapter:

- **SDK output builder.** Add a function in `fm-adapter-sdk` that, given a video/audio output
  request (socket path + caps), creates and returns the platform's sink — `unixfdsink` on
  Linux, behind a single `cfg(target_os)` / platform switch. Both adapters call this instead
  of constructing the sink themselves.
- **Core receive builder.** The matching selection for the source side: `unixfdsrc` on Linux,
  behind the same single switch, in the one place the receive chain is built.
- **Refactor `fm-dummy-adapter` and `fm-rtsp-adapter`** to use the SDK builder; remove their
  direct `unixfdsink` construction.
- This is a **behavior-preserving refactor** — same elements, same properties, relocated. Do
  not change the transport behavior. **Regression gate: T3 must still pass flat at +2000 ms on
  live RTSP after the refactor.**

## Group 2 — confirm the cam-77 reconnect-freeze (HUMAN-IN-THE-LOOP)

The original cam-77 freeze (recv_q backpressure after a long reconnect cycle) was never
root-caused and predates unixfd. unixfd changes the backpressure dynamics, so re-confirm
whether it recurs.

- **STOP-AND-WAIT: this test needs a physical unplug/replug of cam-77, which only the
  maintainer can do. You cannot perform it yourself.** Set up the test, then **stop and
  explicitly ask the maintainer to unplug the camera, and wait for confirmation before
  continuing. Do not simulate the disconnect, do not assume it happened, do not proceed as if
  the cable moved.** Same again for the replug. The maintainer is not able to teleport on your
  schedule — pause and hand control back at each physical step.
- Procedure: run with cam-77 streaming and confirm live → ask maintainer to **unplug cam-77**,
  wait → observe the adapter goes Reconnecting, other sources unaffected → ask maintainer to
  **replug cam-77**, wait → confirm video resumes and does **not** freeze.
- If it freezes: capture `recv_q` (via `ss -tnp` on the RTSP socket) and adapter CPU, and
  **report back** — do not chase it solo. This is the multi-attempt bug from before; if it
  survives unixfd it's a review-chat discussion, not a quick patch.

## Group 3 — enrich the dummy adapter with real events

The dummy adapter emits a minimal event stream, which is exactly why it hid the GDP bug for
three rounds. Make it emit `decodebin3`-like events so transport-payload bugs surface on the
cheap deterministic path:

- Emit `stream-start`, `stream-collection`, and `tags` events on the dummy adapter's output,
  matching the shape a real `decodebin3` source produces.
- After enrichment, re-run **T1 and T3 on the dummy path** and confirm the events pass through
  unixfd cleanly (they should — unixfd carries events natively) and the offset still holds.
  This proves the dummy now exercises the event path a real source would.

## Group 4 — housekeeping

- **Clear `troubleshooting.md`.** The cam-77-freeze and GDP-spike sections are resolved or
  superseded (unixfd replaced GDP; cold-start fixed). Reset the file to just its header/purpose
  block so it's a clean scratchpad for the next active bug. (The maintainer has delegated this
  clear to you for this round.)
- **ADR-0016** has been corrected by the maintainer (the `leaky=downstream` text was a factual
  error; the correct mode is `leaky=upstream`, which the code already does). No code change —
  just be aware the ADR text now matches the code.
- CHANGELOG: transport seam realized (ADR-0019), dummy adapter event enrichment, and the
  cam-77 reconnect-freeze result (once Group 2 produces one).
- DoD checklist per commit; commit in chunks (seam refactor; dummy enrichment; housekeeping).
  Group 2's result is reported to the review chat, not self-resolved.
