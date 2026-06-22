# Changelog

All notable changes to this project are documented here.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
_2026-06-21 22:45 −0500 · `37a9c69`_

- Cargo workspace with three crates: `fm-adapter-sdk`, `fm-core`, `fm-app`
  (binary `final-multiplex`).
- `fm-adapter-sdk`: `SourceMetrics` schema and `IngestState` enum (ADR-0008);
  `contract` module stub for the Phase 2 adapter trait (ADR-0005).
- `fm-core`: TOML scene config types (`SceneConfig`, `GridConfig`, `SourceConfig`)
  with `config::load` (ADR-0007); `Pipeline`, `Transport`, and `MetricsCollector`
  skeletons with documented `todo!()` stubs for Phase 1 implementation.
- `fm-app`: iced `App` skeleton + `bridge` module stub for the appsink→texture
  path (ADR-0006).

### Changed
### Deprecated
### Removed
### Fixed
### Security

<!--
Move items out of [Unreleased] into a versioned section on release, e.g.:

## [0.1.0] - 2026-06-21
### Added
- Initial project scaffold.
-->
