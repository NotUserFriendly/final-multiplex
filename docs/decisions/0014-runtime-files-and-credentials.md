# 0014. Runtime files: location, cleanup, and credential handling

- **Status:** Accepted
- **Date:** 2026-06-24

## Context

Out-of-process adapters (ADR-0005) communicate over shm sockets that are real files on
disk. Config persistence (ADR-0010) writes `scene.toml`. Sources such as RTSP are
configured with URLs that embed credentials (`rtsp://user:pass@host`). Three risks follow:

1. **Scatter.** Runtime files with no single home are hard to clean up and easy to leave
   behind.
2. **Survival of poor exits.** A crash, kill, or uninstall can leave files littering the
   system — including, in the worst case, files containing camera passwords.
3. **Credential exposure.** The source URL is currently passed to the adapter as a
   `--uri` argv flag, which is visible to every user on the machine via `ps`. Credentials
   in argv, logs, or world-readable temp files are a leak.

A user who uninstalls the program should be left with nothing orphaned, least of all
anything sensitive.

## Decision

Confine all ephemeral runtime files to a single user-private root, and never let
credentials reach argv, logs, or shared locations.

**Location.** One runtime root: `$XDG_RUNTIME_DIR/final-multiplex/` when set (the correct
Linux home for runtime sockets — user-owned, `0700`, cleared on logout), else
`/tmp/final-multiplex/` created `0700`. On Windows, the per-user temp equivalent. The core
always supplies socket paths to adapters; the adapters' `/tmp/...` argv fallbacks are
removed.

**Per-run isolation.** Each instance uses a subdirectory keyed on its process id
(`.../final-multiplex/{pid}/`); sockets live inside. Concurrent instances coexist, and
orphans become identifiable.

**Cleanup on graceful exit.** Remove this run's subdirectory.

**Cleanup after a crash or poor exit.** On startup, scan the runtime root and remove any
run subdirectory whose owning pid is no longer alive — an orphan from a crashed prior run —
never touching a live instance's directory. The existing per-spawn stale-socket removal
stays as the inner safety net, since `SIGKILL` runs no cleanup handler.

**Permissions.** Runtime root and contents are user-only: `0700` directories, `0600`
files. Other users on the machine cannot read the sockets.

**Credentials never leave the config file.**
- The source URL (with any credentials) is delivered to the adapter **over the control
  channel** (a core→adapter `Configure { uri }` command on stdin), **not** as argv and not
  via an inheritable env var. The `--uri` flag is removed. This reorders startup slightly:
  spawn → adapter waits for `Configure` → connect + slave clock + open sockets → `Ready`.
- Credentials are never written to runtime files and never logged (stderr included);
  any URL must be credential-scrubbed before it reaches a log line.
- The ADR-0010 config round-trip writes its atomic temp file in the **config directory at
  `0600`**, never in `/tmp`, so a credential-bearing scene file is never briefly
  world-readable.

**Enumerated write surface.** The program writes in exactly two places: the user's config
file (where the user put it) and the runtime root above. Nothing else. Uninstall is
therefore: remove the binary and, at the user's option, the config file; the runtime root
is self-clearing and holds nothing persistent or sensitive.

## Consequences

- Predictable, auditable footprint; uninstall leaves nothing sensitive behind.
- Crash resilience without a daemon: startup orphan-reaping covers the `SIGKILL`/crash case
  no in-process handler can.
- Concurrent instances are safe; startup cleanup cannot clobber a running instance.
- The source URL moving off argv is a **contract change to the ADR-0012 launch surface**:
  the URL is no longer a launch flag, and the adapter must not connect until it receives
  `Configure`. The readiness gate already waits for `Ready`, so this fits, but the adapter
  lifecycle gains one step before `Ready`.
- Credential hygiene is a standing constraint that is easy to regress (a stray log of the
  full URL re-introduces the leak). Worth a review/lint note and a scrubbing helper used at
  every log site that might touch a URL.
- Orphan-reaping needs pid-liveness checking — cheap on Linux (`/proc` or `kill(pid, 0)`),
  a small shim on Windows.
- Cost: the core gains a small amount of directory/permission/lifecycle bookkeeping it did
  not have when sockets sat loose in `/tmp`.
