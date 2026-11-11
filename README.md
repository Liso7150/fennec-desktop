# Fennec Desktop (desktop)

Local-first coding agent desktop app for Fennec models, with device-tiered MoE
inference whose base idea is [Colibri](https://github.com/JustVugg/colibri).
Electron + React + Vite.

## Features

- **On-demand local engine** — starts `coli serve` on the first message, attaches to an engine that is already running, and stops the engine it owns on quit.
- **Chat and agent turns** — streamed answers plus a bounded tool loop with per-run approvals.
- **Reasoning display** — engine `reasoning_content` streams into a collapsible block above each answer.
- **Vision** — attach one PNG/JPEG/WebP/GIF (or paste it) and it is sent as a base64 content part to the local engine.
- **File attachments** — attach up to three text files as bounded context instead of pasting them.
- **Speech** — dictate with the local faster-whisper sidecar and read answers aloud with the system voices.
- **Update from GitHub** — the release feed is checked at startup, on window focus and when an engine starts (cached, silent when offline); `Check release` re-queries it and `Update apps from GitHub` fast-forwards the suite checkouts with `git pull --ff-only`.
- **Path-jailed tools** — file reads, edits, grep and command runs stay inside the opened workspace.
- **NPU & accelerator sidecar** — OpenVINO device probe plus the batched workspace code index, off the token path (never a decoder). The app picks the first interpreter that can import OpenVINO — an explicit `FENNEC_PYTHON`, the configured one, or a plain `python` from PATH — and *Install sidecar deps* / *Install embedding model* provision exactly what is missing: pip for `openvino`, `openvino-tokenizers` and `numpy`, then a prebuilt OpenVINO IR embedding model fetched with `huggingface_hub`. No `optimum-cli` export step, so torch/nncf never enter that interpreter. The index file it writes is not consulted by the agent yet: retrieval is still keyword/grep.
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

The app spawns the Colibri engine with `python` from PATH and talks to its
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
- `jetbrains/` — moved to its own repo: `Liso7150/fennec-jetbrains`.

## Credits

Engine base idea: [JustVugg/colibri](https://github.com/JustVugg/colibri) —
device-tiered MoE inference. Fine-tuning:
[AirLLM](https://github.com/lyogavin/airllm). Part of the
[Fennec suite](https://github.com/Liso7150/fennec-environment-setup).