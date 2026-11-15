# Changelog

All notable changes to fennec-desktop, the native Rust (egui/eframe) desktop application.

The sections below carry the published tag each change actually shipped in
(`Liso7150/fennec-desktop`, `v0.3.0` … `v0.9.0`). An earlier revision of this
file numbered the same history on a 1.x/2.x ladder that was never published;
those tags still exist locally (`v1.0.0` … `v2.0.0`) and each one points at the
same commit as a published `v0.x` tag — except `v1.0.0`, `v1.1.0` and `v1.2.0`,
which mark pre-release commits that shipped inside `v0.3.0`. The first release
here was `0.3.0`; `0.1.0` and `0.2.0` were never published. Dates are the tag
publish dates.

## [Unreleased] — 1.0.0 in preparation

Nothing at 1.0.0 is published. The newest release here is `v0.9.0`
(2026-11-11T19:38Z). Measured on the working tree: 33 tests pass — 18 lib plus
15 bin (`cargo test --release --offline`, 2026-11-15).

### Added
- The app is native Rust (egui/eframe): no Electron, no Node, no web view
- Full Electron feature parity in Rust — every handler ported
- Reuse instead of re-implementation: `fennec-client` and `fennec-economy` come
  from the fennec-core workspace, the engine and CLI from their own repos
- Environment rail shows the outlet state the core actually reports
- README hero published

### Changed
- eframe upgraded 0.27 → 0.37; the macOS toolchain unpinned
- Event handling and the main loop expanded
- Scroll area uses `clamped_offset()` instead of the deprecated `offset()`
  heuristic
- The Electron leftovers and the redundant config/engine modules are gone; the
  legacy JS/TS source stays on the `legacy-electron` branch

### Fixed
- The README's test count corrected to the measured 33 (18 lib + 15 bin)

## [0.9.0] - 2026-11-11

### Changed
- Same app, faster core underneath

## [0.8.0] - 2026-11-11

### Added
- Browser outlets (Jev / OpenBot) in the desktop app

## [0.7.1] - 2026-11-11

Version bump only.

## [0.7.0] - 2026-11-11

### Added
- First-run setup wizard

## [0.6.0] - 2026-11-11

Version bump only.

## [0.5.0] - 2026-11-11

Version bump only.

## [0.4.1] - 2026-11-11

### Changed
- Spawn the engine with python from PATH

## [0.4.0] - 2026-11-11

### Added
- Orphaned engine processes swept on stop and quit
- Host expert pool defaulted to 4 GB: same speed, ~9 GB less RAM
- The GPU is used on backend auto when a Vulkan loader is present
- The desktop agent payload is compacted, and the saving shows in the header

### Changed
- CI granted `contents:write` so tag builds can publish releases

## [0.3.0] - 2026-11-10

The first published release. It carries the pre-tag history below.

### Changed
- Rebrand the desktop app to Fennec Desktop
- No drive letter assumed: every root defaults to the user home
- Speech input, read-aloud, vision, file attachments and GitHub updates
- The sidecar runs on any Python, and releases are checked on start and use
- Max tokens passed to serve as `--ngen`, the flag the engine accepts
- The engine starts on demand and the app stops only the one it owns
- Icon upscaled to 512×512 for macOS packaging

### Fixed
- The OpenVINO NPU compile artifact ignored, so it cannot reach a release diff

### Credits
- Initial commit as colibri-code — an Electron desktop app for the Colibri
  suite (JustVugg/colibri + lyogavin/airllm)
