# Fennec Desktop (desktop)

Local-first coding agent desktop app for Fennec models, with device-tiered MoE
inference whose base idea is [Colibri](https://github.com/JustVugg/colibri).
Electron + React + Vite.

## Features

- **First-run setup wizard** — on a machine with no `~/.fennec/config.json` the app asks for the install folder, the Python interpreter, the engine checkout and the models folder, checks and installs the core Python dependencies, and offers to install Python/Git with the platform's own package manager. It runs the same on Windows, macOS and Linux because it lives in the renderer, not in the OS installer. Re-runnable from the Files tab or the Fennec menu.
- **On-demand local engine** — starts `coli serve` on the first message, attaches to an engine that is already running, and stops the engine it owns on quit.
- **Chat and agent turns** — streamed answers plus a bounded tool loop with per-run approvals.
- **Reasoning display** — engine `reasoning_content` streams into a collapsible block above each answer.
- **Vision** — attach one PNG/JPEG/WebP/GIF (or paste it) and it is sent as a base64 content part to the local engine.
- **File attachments** — attach up to three text files as bounded context instead of pasting them.
- **Speech** — dictate with the local faster-whisper sidecar and read answers aloud with the system voices.
- **Update from GitHub** — the release feed is checked at startup, on window focus and when an engine starts (cached, silent when offline); `Check release` re-queries it and `Update apps from GitHub` fast-forwards the suite checkouts with `git pull --ff-only`.
- **Path-jailed tools** — file reads, edits, grep and command runs stay inside the opened workspace.
- **Browser outlets** — `web_task` and `coworker_task` hand one goal to a browser agent on this machine: [jev-ultrafast](https://github.com/browser-use/jev-ultrafast) (Jev) drives pages, [OpenBot](https://github.com/CopilotKit/OpenBot) coworkers answer web tasks. The desktop keeps no browser code: it asks the local Fennec service (`/api/outlets`, `/api/outlets/run`), advertises a tool only while that outlet answers, and confirms each run in the approval dialog unless *Run browser tasks without asking* is on. Neither upstream is modified or vendored, and Jev spends the key inside its own checkout.
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

## First run

When `~/.fennec/config.json` is missing the setup wizard opens before the chat:

1. **Prerequisites** — Python 3.10+ and Git, with winget on Windows, Homebrew on macOS, or the exact package-manager command on Linux (no silent elevation).
2. **Install folder** — default `~/Fennec`, with engine, suite, models and cache derived under it. Free space is checked because a 35B int4 container needs about 23 GB.
3. **Python** — the first `python`/`python3` on PATH that is 3.10+, or a Browse-picked absolute interpreter; version, venv and pip are reported.
4. **Dependencies** — `fastapi`, `uvicorn`, `psutil` and `pydantic` are probed by import; missing ones are installed with `python -m pip install --user` (or into the active venv) with `ensurepip` offered when pip is absent.
5. **Engine** — validates `c/coli` and `c/openai_server.py`, and can clone upstream `JustVugg/colibri` into an empty folder.
6. **Models** — scans folders for `config.json` and weights, lets you pick one, and allows finishing without a model.
7. **Review** — writes the config atomically (backing up an existing file), then restarts the app.

No secret is ever written to the config; API keys stay in `FENNEC_API_KEY`.
The wizard is bypassed when a readable config already exists.

## On-demand engine

Nothing listens in the background. The first message in a chat starts
`coli serve` on the app's configured loopback URL (shared config, default
`http://127.0.0.1:8184`) using the shared suite config, and an engine the app
spawned is stopped when the app quits. An engine that was already running is
attached to and left alone. Models, caches, logs and settings stay on this
device.

## Layout

- `electron/` — main process, engine backend, agent loop, tools, outlet client, NPU sidecar.
- `src/` — React renderer.
- `build/` — packaging resources (icons).
- `jetbrains/` — moved to its own repo: `Liso7150/fennec-jetbrains`.

## Credits

Engine base idea: [JustVugg/colibri](https://github.com/JustVugg/colibri) —
device-tiered MoE inference. Fine-tuning:
[AirLLM](https://github.com/lyogavin/airllm). Part of the
[Fennec suite](https://github.com/Liso7150/fennec-environment-setup).