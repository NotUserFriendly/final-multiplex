# 0022. Adapter discovery and install layout

- **Status:** Accepted
- **Date:** 2026-06-25

## Context

The core launches out-of-process adapter executables (ADR-0005). It currently resolves the
adapter binary per-launch with no pinned location, so resolution varies by launcher and working
directory — the "hunt" — producing inconsistent behavior and launch mistakes.

Looking ahead, adapters will be **user-provided and user-serviceable** (third-party and custom
adapters), and in locked-down corporate environments users often cannot write to the install
directory but can write to their own profile. A flat "binaries next to the main executable"
layout is brittle, opaque, and not user-serviceable.

## Decision

Adapters are discovered from a **defined search path, first match wins**:

1. **Explicit override** — env var (`FM_ADAPTER_DIR`) or config — for dev and power use.
2. **User adapter directory** — the XDG **data** location (not cache; cache dirs get cleared):
   `$XDG_DATA_HOME/final-multiplex/adapters` (default `~/.local/share/final-multiplex/adapters`);
   `%APPDATA%\final-multiplex\adapters` on Windows; `~/Library/Application Support/
   final-multiplex/adapters` on macOS. User-writable, survives in locked-down corp environments,
   and is the drop-point for user-provided adapters.
3. **Bundled adapters** — a dedicated, self-documenting `adapters/` subdirectory in the install
   layout. **Not** flat siblings of the core executable.

The layout is self-documenting: a named `adapters/` directory, not binaries scattered next to
the core exe.

This is consistent with ADR-0005 (adapters are out-of-process *executables*, discovered as
binaries — not the dynamic `.so` plugin ABI that PLAN's non-goals exclude) and with the XDG
conventions already used for runtime files (ADR-0014). Data dir, deliberately, so user adapters
are not wiped by cache cleanup.

## Consequences

- Deterministic resolution regardless of launcher or cwd — removes the per-launch hunt and the
  class of launch mistakes it caused.
- User-serviceable: users drop custom adapters in the user directory without touching the
  install.
- Corp-friendly: the user directory is writable where the install directory is not.
- Sets Phase-3+ adapters (YouTube bundled now; user-provided later) on one discovery contract.
- The build/install must populate the bundled `adapters/` directory (a packaging step copies the
  adapter binaries in) — it is no longer "they happen to sit next to the exe."
- The user data directory may not exist on first run; the core creates it (or simply skips it if
  absent) rather than failing.
