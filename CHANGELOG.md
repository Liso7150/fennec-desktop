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
(2026-11-11T19:38Z). Measured on the working tree: 37 tests pass — 18 lib plus
19 bin (`cargo test --release --offline`, 2026-11-15).

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
- The README's test count corrected to the measured 33 (18 lib + 15 bin), then to
  the measured 35, then to the measured 37 (18 lib + 19 bin)
- A prompt sent while the engine is cold is no longer refused: `/api/agent/start`
  answers 503 until the model listens, so the prompt now waits in the app and the
  2 s status poll re-sends it once the engine answers. It waits inside a 300 s
  window, and past it — or once the agent loop stops answering — the text goes
  back to the composer rather than vanishing. Found and verified through the real
  window: a typed prompt plus Ctrl+Enter on an offline engine, the engine warming
  on its own, the run appearing in the timeline. The queue holds one prompt: a
  second send while the model is loading stays in the composer instead of
  replacing what the queue is already waiting to deliver.
- The warm-up ask is no longer treated as an error the app can see. The service
  answered `POST /api/engine/start` slower than the client's own timeout because
  the route probed the dead engine's `/health` first, so every cold send reported
  a failure while the engine started anyway and the prompt was thrown away. A
  send whose ask does not confirm now stays queued; the route's probe is gone in
  `fennec-core`, where the same request measured 26 ms.
- The Send button says what the shortcut is: `{Cmd,Ctrl}+Enter sends`, and on a
  cold engine it says the first send starts the model and the prompt waits.
- Reopening the app no longer forgets what a run did. The rail listed the
  service's sessions, but a timeline was built only from the events this window
  watched, so a finished task selected after a restart read "Nothing has
  happened in this task yet". Selecting a session now asks the loop for the steps
  it kept — once per session — and puts the prompt at the head of the replay with
  no timestamp invented for it. Verified through the real window: after a
  restart, the run's answer, its plan card and its summary are there again.

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
