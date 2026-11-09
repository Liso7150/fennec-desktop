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

## Layout

- `electron/` — main process, engine backend, agent loop, tools, NPU sidecar.
- `src/` — React renderer.
- `build/` — packaging resources (icons).
- `jetbrains/` — moved to its own repo: `Liso7150/colibri-jetbrains`.

## Credits

Engine: [JustVugg/colibri](https://github.com/JustVugg/colibri). Fine-tuning:
[AirLLM](https://github.com/lyogavin/airllm). Part of the
[Colibri suite](https://github.com/Liso7150/colibri-environment-setup).