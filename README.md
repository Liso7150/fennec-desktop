![Fennec Desktop 1.0.0 — a local-first coding agent as a native Rust app (gpui), talking to the local Fennec inference engine. Nothing is hosted. No Electron, no Node, no web view. Windows macOS Linux, MIT license.](docs/hero.png)

Fennec Desktop is the suite's native Rust/GPUI application. It presents repository, session, timeline, approval, and environment data in one three-pane window. The model, supervised agent loop, and tools run locally through [`fennec-core`](https://github.com/Liso7150/fennec-core); the desktop is a client, not a second agent runtime.

## Architecture

| Service | Address | Desktop use |
|---|---|---|
| `fennec-core/server.py` | `127.0.0.1:8787` | Canonical coding backend: supervised sessions, tools, approvals, polling |
| `fennec-agent` | `127.0.0.1:8788` | Optional auxiliary/chat and integration service; never a coding route |
| Fennec engine | `127.0.0.1:8184` | Inference and health/model telemetry; supervised by core |

The supported binary is `fennec-desktop-gui`, built from `src/gui_main.rs` with the `gui` feature. It uses `fennec-client` for HTTP/SSE protocols and suite discovery, `fennec-economy` for shared context budgeting, and [`fennec-ui`](https://github.com/Liso7150/fennec-ui) for native widgets and themes. The older egui binary is retained only for comparison; build the GPUI binary explicitly.

## Strict core-first coding

Coding is available only when `GET /api/capabilities` on the configured core URL reports protocol version 1 and all four features:

- `supervised-agent`
- `tool-execution`
- `per-call-approvals`
- `agent-polling`

Health alone is not coding readiness. The Rust service's `chat-only` capability and the inference engine cannot make a coding prompt routable.

The desktop fails closed when core is stopped, unreachable, or advertises an incomplete capability set:

- A new coding prompt is recorded as unavailable with the remedy shown; it is not sent to the engine or Rust chat service.
- Restored queue entries and session resumes wait for capable core supervision.
- Approval answers and cancellation require a live supervised core session.
- Missing integrations and unavailable controls remain visibly disabled rather than being simulated.

Engine health and model metadata may still appear in the Environment pane. That telemetry is not a chat fallback or an alternate coding path.

## Workspace

| Pane | Contents |
|---|---|
| Left rail | Repository selector, Coding / My Attention / New Task / Search, recent core sessions, branch and changed-file footer |
| Center | Streamed reasoning and answers, grouped tool and write summaries, per-call approval cards, code blocks, persistent composer, permission controls |
| Right | Environment, subagents, artifacts, research, outlets, and context compaction |

Tool paths are jailed to the selected workspace. Write, shell, and web permissions are off unless enabled; ask-each mode submits gated calls for an explicit decision. A timeout or cancellation rejects the request. The window has no web view, DOM, or embedded Node.js runtime.

## Build and run

Fetch the suite together so the `fennec-core`, `fennec-economy`, and `fennec-ui` sibling crates are available, then run in this repository:

```bash
cargo build --features gui --bin fennec-desktop-gui --offline
cargo build --release --features gui --bin fennec-desktop-gui --offline
cargo test --offline --features gui                   # 59 tests
```

The debug binary is `target/debug/fennec-desktop-gui` (`.exe` on Windows). Windows release shader compilation requires `fxc.exe` from the Windows SDK.

Start the canonical service in the core checkout:

```bash
python server.py serve
```

Then launch `fennec-desktop-gui`. Core starts the Fennec engine on demand. The optional Rust service may run separately for chat and integrations; it is not required for coding.

## Service configuration

| Setting | Environment override | Default |
|---|---|---|
| Core coding URL | `FENNEC_CORE_URL` | `http://127.0.0.1:8787` |
| Auxiliary Rust URL | `FENNEC_RUST_URL` or `FENNEC_AGENT_URL` | `http://127.0.0.1:8788` |
| Engine telemetry URL | `FENNEC_ENGINE_URL` | `http://127.0.0.1:8184` |

Suite and repository discovery reads `fennec-environment-setup/repos.json`. Only Fennec engine binaries are resolved; another inference engine is never substituted. An attached engine is not restarted by the desktop.

## External connectors

Nod remains a separate native Rust stdio endpoint in `fennec-coding-agent`. Obsidian remains an opt-in local-vault writer. Neither connector is embedded in the desktop or adds Electron, a web view, Node.js, or an Obsidian plugin runtime.
