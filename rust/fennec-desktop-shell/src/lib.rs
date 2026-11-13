//! Rust desktop shell: the Electron agent logic over HTTP, serving the web
//! renderer and driving a bundled Chromium. Replaces fennec-desktop/electron/*.js.
//! Native desktop backend. Core logic is REUSED from the suite crates
//! (fennec-economy / fennec-outlets / fennec-acp / fennec-preflight, the same
//! crates fennec-cli's fennec-rs uses) and the fennec engine; only desktop UI
//! glue lives here. No reimplementation of config/engine.
pub mod parity;
pub mod agent;