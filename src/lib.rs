//! Fennec desktop's reusable half: the translation layer between what the
//! backends report and what the timeline shows. The binary in `src/main.rs`
//! is the only consumer today, but keeping it a library means the same
//! interpretation can be reused (and tested) without starting a GUI.
//!
//! Protocol clients, suite/manifest discovery and the git helpers are NOT here
//! — they belong to `fennec-client` in fennec-core, which every native Fennec
//! surface shares.
pub mod events;
pub mod fonts;
