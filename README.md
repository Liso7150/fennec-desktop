# Colibri Code (desktop)

Local-first coding agent desktop app for [Colibri](https://github.com/JustVugg/colibri)
models, with device-tiered MoE inference. Electron + React + Vite.

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