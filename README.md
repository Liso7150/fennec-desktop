![Fennec Desktop 1.0.0 — a local-first coding agent as a native Rust app (gpui), talking to the local Fennec inference engine. Nothing is hosted. No Electron, no Node, no web view. Windows macOS Linux, MIT license.](docs/hero.png)

Fennec is a native Rust desktop app for the suite. The model, the agent loop,
the tools and the workspace discovery all run on this device; nothing is hosted, no account,
no API key, no cloud. It speaks to the two loopback services the suite already owns —
the Rust/Axum [coding-agent backend](https://github.com/Liso7150/fennec-coding-agent) and the
[Fennec](https://github.com/Liso7150/fennec) inference engine — and presents them in one
three-pane window. The Rust `fennec-agent` service in [fennec-coding-agent](https://github.com/Liso7150/fennec-coding-agent)
is the canonical task and event boundary. The older `fennec-core` service remains only for
compatibility routes that have not yet moved into the Rust contract.

The app was rewritten from Electron/React/TypeScript to native Rust. The previous JS/TS
source is preserved on the `legacy-electron` branch; the tracked source is Rust only.

Nothing published on GitHub is this app: the releases stop at `v0.9.0`, and every asset
there is an Electron installer — the `0.9.0` note calls it the last Electron build. The
Rust binaries are built from this tree, as the Build and run section shows.

The tree builds two Rust windows. `fennec-desktop-gui`, from `src/gui_main.rs` behind the
`gui` feature, is the app described on this page; `fennec-desktop`, from `src/main.rs`, is
the egui/eframe shell that preceded it and is kept buildable for A/B. A bare
`cargo build --release` makes only the older one — the feature is what selects the current
binary, and a release build of it needs `fxc.exe` from the Windows SDK to compile shaders.

## At a glance

| UI | Backend | Compaction | Tests | Platforms |
|---|---|---|---|---|
| native window, gpui-kit (`gui` feature) | Rust/Axum PWA/API `127.0.0.1:8787` (fallback `:8788`), engine SSE `127.0.0.1:8184` | `fennec-economy`, in-process | 47 | Windows, macOS, Linux |

## Architecture

```
fennec-desktop (this repo)          fennec-core/src (path dependency)
  src/gui_main.rs  gpui UI      →    fennec-client   agent server + engine SSE clients
  src/main.rs      egui shell       +               suite / repos.json discovery, git
  src/events.rs    timeline model   →    fennec-economy  context compaction
  src/fonts.rs     font lookup
```

The widgets both windows draw with — the markdown body, the grouped tool timeline, the
composer controls, the approval and delivery cards — live in the shared
[fennec-ui](https://github.com/Liso7150/fennec-ui) crate, so a rule fixed once shows up in
the desktop and in the coding-agent app.

Nothing here re-implements a protocol. The HTTP/SSE contract, the workspace list and the
engine/model resolution come from `fennec-client`, which reads
[fennec-environment-setup/repos.json](https://github.com/Liso7150/fennec-environment-setup)
for the suite root and repo set — the same manifest the installer fetches from, so what the
selector shows is what `fennec update` keeps current.

## What it does

| Pane | Behavior |
|---|---|
| Left rail | Fennec brand, real repository selector (manifest `repos[]` that exist on disk), Coding / My Attention / New Task / Search, recent sessions from the agent server, repo · branch · changed footer |
| Center | Task header, structured timeline: streamed reasoning and answers, every settled write folded into one expandable "Written N files" block and every other settled call into one "Ran N tools" block (a failed count is called out), with only the call still running shown apart and expandable while it works, one card per gated call (risk, reason, the four answers), copyable code blocks, persistent composer, the loop's four controls (edits / commands / web / ask each) |
| Right | Environment (core, engine, model, vision, context window, tokens/s, outlets — live values), Subagents, Artifacts (files the run actually wrote), Research, context compaction |

> [!NOTE]
> Panels report what the backend actually exposes. With only the engine running the app is in
> direct chat mode, so the tool timeline is empty and says so; features with no backend
> registry are shown as unavailable instead of being faked — the composer's image, record and
> read-aloud buttons stay disabled, each tooltip naming the reason. For images the reason comes
> from the engine: `/health` answers with either a `vision` flag or an `input_modalities` list
> (the Fennec engine declares `vision: false`; a Colibri server lists `image`), and an engine
> that never answered is reported as this shell having no attachment path, because that is the
> blocker either way. The Environment pane mirrors the same three states in its Vision row.

## Build and run

The app path-depends on the sibling `fennec-core` checkout, so fetch the repos together first
(`fennec update`, or the installer's bootstrap), then:

```bash
cargo build  --features gui --offline  # ./target/debug/fennec-desktop-gui(.exe)
cargo build  --release --features gui --offline   # needs fxc.exe on Windows
cargo test --offline --features gui                   # 47 tests
```

The engine and the Rust/Axum coding-agent backend start independently of the app; the
Environment panel offers `Start agent loop` when the backend is not reachable. The legacy
`fennec-core` service is a separate process only when a compatibility route requires it.
CI checks out `fennec-core` beside this repo and builds both binaries with `--features gui`, so the window CI
publishes is the current one rather than the egui shell.

The egui layout is tested without a window. One test paints the whole interface through
`egui::Context::run_ui` and measures the triangles the painter would have received, at the
default and the minimum window size, so a pane that collapses fails the suite instead of only
looking wrong on somebody's screen. The gpui window has no such headless paint test; it is
checked by Computer Use against the running app.

## Which engine answers

Resolution order comes from `fennec-client`: the manifest's `engine_root` first, then the home
engine checkout, then a legacy Colibri checkout only as a last resort. An already-running
engine is attached to, never restarted.

## Security posture

Tool paths are jailed to the selected workspace; writing, shell and web each stay off until
the corresponding switch is set in the composer, and those flags are the agent loop's own, not
app-side decoration. A fourth switch (ask each) lifts the flags and puts every gated call to
you instead — the run waits on the answer, and a timeout or cancel is a rejection. No secret
is read, written or displayed. The UI has no web view, so there is no DOM and no script
injection surface. Both services are loopback-only.

External connectors remain core-owned and do not change the native UI boundary. Nod connects through
Fennec Core's opt-in, read-only MCP endpoint. Obsidian is not integrated; a future vault connector
would be opt-in and approval-gated, with no embedded Node.js, Electron, or Obsidian runtime.

## Credits

Engine: [Liso7150/fennec](https://github.com/Liso7150/fennec). Base idea:
[JustVugg/colibri](https://github.com/JustVugg/colibri) — device-tiered MoE inference.
Agent server and outlets: [Liso7150/fennec-core](https://github.com/Liso7150/fennec-core).
Part of the [Fennec suite](https://github.com/Liso7150/fennec-environment-setup).

## License

MIT — see [LICENSE](LICENSE).
