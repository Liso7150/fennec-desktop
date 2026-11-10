# Colibri Code (desktop)

Local-first coding agent desktop app for [Colibri](https://github.com/JustVugg/colibri)
models, with device-tiered MoE inference. Electron + React + Vite.

## Features

- **On-demand local engine** — starts `coli serve` on the first message, attaches to an engine that is already running, and stops the engine it owns on quit.
- **Chat and agent turns** — streamed answers plus a bounded tool loop with per-run approvals.
- **Reasoning display** — engine `reasoning_content` streams into a collapsible block above each answer.
- **Vision** — attach one PNG/JPEG/WebP/GIF (or paste it) and it is sent as a base64 content part to the local engine.
- **File attachments** — attach up to three text files as bounded context instead of pasting them.
- **Speech** — dictate with the local faster-whisper sidecar and read answers aloud with the system voices.
- **Update from GitHub** — check the latest release, open it, and fast-forward the suite checkouts with `git pull --ff-only`.
- **Path-jailed tools** — file reads, edits, grep and command runs stay inside the opened workspace.
- **NPU embeddings** — optional Intel NPU sidecar for semantic search (never a decoder).
- **Cross-platform builds** — NSIS/MSI, dmg, AppImage/deb, with a Windows/macOS/Linux CI matrix.
- **Local-only** — models, caches, logs and settings never leave this device.

## Develop

```bash
npm install
npm run dev          # Vite renderer + Electron
```

## Build standalone apps

```bash
npm run dist:exe     # Windows NSIS installer (.exe)
npm run dist:msi     # Windows MSI
npm run dist:mac     # macOS dmg
npm run dist:linux   # Linux AppImage/deb
```

The app spawns the Colibri engine from the shared venv and talks to its
OpenAI-compatible server; the agent loop and tools live in `electron/`.

## On-demand engine

Nothing listens in the background. The first message in a chat starts
`coli serve` on the app's configured loopback URL (shared config, default
`http://127.0.0.1:8184`) using the shared suite config, and an engine the app
spawned is stopped when the app quits. An engine that was already running is
attached to and left alone. Models, caches, logs and settings stay on this
device.

## Layout

- `electron/` — main process, engine backend, agent loop, tools, NPU sidecar.
- `src/` — React renderer.
- `build/` — packaging resources (icons).
- `jetbrains/` — moved to its own repo: `Liso7150/colibri-jetbrains`.

## Credits

Engine: [JustVugg/colibri](https://github.com/JustVugg/colibri). Fine-tuning:
[AirLLM](https://github.com/lyogavin/airllm). Part of the
[Colibri suite](https://github.com/Liso7150/colibri-environment-setup).