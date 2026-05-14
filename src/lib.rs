//! llamatui library — TUI + CLI + daemon for managing local llama.cpp
//! servers. The binary at `src/main.rs` is a thin wrapper around the
//! modules exposed here so integration tests (in `tests/`) can drive the
//! same code paths the binary uses.

#![warn(rust_2018_idioms)]
#![deny(clippy::shadow_unrelated)]
// Unit 2 lands the IPC + daemon layer; later units (3-9) consume the rest.
// Allow dead code crate-wide while the scaffold is incomplete; remove this
// allow once Unit 6+ start consuming these surfaces.
#![allow(dead_code)]

pub mod banner;
pub mod cli;
pub mod config;
pub mod daemon;
pub mod ipc;
pub mod theme;
pub mod util;
