# 0007. Config format: TOML via serde

- **Status:** Accepted
- **Date:** 2026-06-21

## Context

Scenes (sources + grid) are declared in a config file from Phase 1 (PLAN.md). The choice
was TOML vs YAML. YAML is more compact on nested lists but carries type-coercion footguns
(the "Norway problem", version/zero-leading values) and whitespace sensitivity — live
risks for a file full of URLs, ports, and paths. TOML spends a few more characters on
quotes and explicit `=` to stay unambiguous.

## Decision

Use **TOML**, parsed with `serde` + the `toml` crate. It is the Rust-native default, sits
beside `Cargo.toml`, and is what a Rust contributor expects.

## Consequences

- Zero-friction in the stack; no whitespace/coercion surprises in user-authored config.
- We give up YAML's brevity on deeply nested source lists.
- Export path stays open: TOML → YAML is lossless, so a YAML emitter can be added later if
  ever needed. The reverse (importing arbitrary YAML) is not guaranteed — YAML null,
  anchors, non-string keys, and multi-document files have no TOML equivalent.
