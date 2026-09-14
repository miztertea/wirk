//! `wirk`'s library face: the daemon wire protocol/server module
//! (`wirkd`), compiled once here and shared as one ordinary crate
//! dependency between `main.rs` and every integration test binary
//! under `tests/`, each reaching it with `use wirk::wirkd;` — the same
//! `crate::wirkd` name a local `mod wirkd;` declaration would have
//! introduced, so no downstream `wirkd::Foo`/`wirkd::client::bar`
//! reference needs to change per consumer.
//!
//! An integration test binary is compiled with `--test`, which turns
//! on `cfg(test)` for its *whole* crate; a dependency it merely links
//! is never itself built with `--test`, so `wirkd::boundary`'s and
//! `wirkd::server`'s own `#[cfg(test)]` unit tests run exactly once,
//! as part of this library's own `--lib` test target, and are simply
//! absent from every integration binary that links it.
pub mod wirkd;
