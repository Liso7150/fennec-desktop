![Fennec Desktop 2.5.0 — a local-first coding agent as a native Rust app (egui), talking to the local Fennec inference engine. Nothing is hosted. No Electron, no Node, no web view. Windows macOS Linux, MIT license.](docs/hero.png)

Nothing is hosted. The model, the agent loop, the tools and the index run on this device, and
the engine starts only when a message needs it. Inference comes from
[Fennec](https://github.com/Liso7150/fennec), the suite's native Rust engine;
[Colibri](https://github.com/JustVugg/colibri) still works as a fallback.

**Rewritten from Electron/React/TypeScript to native Rust.** The previous JS/TS app is preserved
on the `legacy-electron` branch; the tracked source is now Rust only.

## At a glance

| Language | UI | Backend | Parity tests | Default engine URL |
|---|---|---|---|---|
| Rust (egui/eframe) | native window | in-process Rust (config/engine/context/outlets/ACP) | 13 (44 handlers) | `http://127.0.0.1:8184` |

## Architecture

```
Native window (egui/eframe)
   | in-process calls (no IPC, no web view)
   v
Rust backend (fennec-desktop-shell)
   |- agent loop ---- path-jailed tools (read / edit / grep / run)
   |- context ------- fennec-economy (Rust compaction, in-process)
   |- outlets ------- fennec-outlets (loopback gate, redaction)
   `- engine -------- HTTP :8184 --> Fennec engine (spawned on first message)
```

## Features (every Electron handler ported)

| Area | Behavior |
|---|---|
| Chat and agent turns | Streamed answers, bounded tool loop, per-run approvals; read-only until writes/shell are allowed |
| On-demand engine | Attaches to a running engine, otherwise spawns one on the first message; stops the child it owns on quit |
| File tree / read | Path-jailed to the opened workspace |
| Models | Scans for `config.json` + weights |
| Outlets | `web_task` / `coworker_task` via the local core service; loopback-gated, confirmed per run |
| Setup wizard | Detect / validate / scan / save; writes config atomically; API keys stay in `FENNEC_API_KEY` |
| NPU / STT | Structured delegations to the engine sidecars (probe/index, faster-whisper) |
| Updates | Release feed check; `Update apps from GitHub` runs `git pull --ff-only` on suite checkouts |

Each maps to a Rust function in `rust/fennec-desktop-shell/src/parity.rs`; a parity test asserts
all 44 handlers resolve (never 404). Hardware surfaces (NPU/STT) return structured delegations.

## Build & run

```bash
cd rust
cargo build --release --offline
./target/release/fennec-desktop-shell      # native window
cargo test --release --offline             # 13 parity tests
```

The legacy Electron app (JS/TS, 106 tests) lives on the `legacy-electron` branch for reference.

## Which engine answers

Resolution order: native Fennec binary (`target/release`, `target/debug`, engine root) first,
then a legacy Colibri checkout (`c/coli`). An engine the app spawned is stopped with all
descendants on quit; one already running is attached to and left alone.

## Security posture

Tool paths are jailed to the opened workspace; outlets are loopback-gated and confirmed per
run; no secret is ever written to the config. The native UI has no web DOM, so no XSS surface.

## Credits

Engine: [Liso7150/fennec](https://github.com/Liso7150/fennec). Base idea:
[JustVugg/colibri](https://github.com/JustVugg/colibri) — device-tiered MoE inference.
Outlets: [jev-ultrafast](https://github.com/browser-use/jev-ultrafast),
[OpenBot](https://github.com/CopilotKit/OpenBot). Part of the
[Fennec suite](https://github.com/Liso7150/fennec-environment-setup).

## License

MIT — see [LICENSE](LICENSE).
