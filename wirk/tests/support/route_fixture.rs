//! Shared test support (p2-route-files W2, R2 — the `#[path]`-included
//! module convention `wirk-herdr/tests/support/live_herdr.rs` already
//! uses for a helper shared across several test binaries). A bare
//! `--route <name>` resolves against `<estate_root>/routes/<name>.json`
//! (`server.rs::resolve_route_path`), never the repo the fixture text
//! lives in — so every test that submits `--route smoke` or `--route
//! proving` must first put that file where the estate expects it.

// Included via `#[path]` into several test binaries (`wirkd_process.rs`
// uses both `install_route_fixture`; `run_verb.rs`'s per-test
// distinctive intents use `write_route` only): whichever one a given
// binary doesn't call would otherwise warn as dead code there, so this
// mirrors `wirk/src/wirkd/mod.rs`'s own module-level allow for the same
// reason.
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/routes")
}

/// Copies the canonical `<name>.json` fixture (`wirk/tests/fixtures/
/// routes/`) into `<estate>/routes/<name>.json`, creating the directory
/// as needed — ready for a bare `--route <name>` submit.
pub fn install_route_fixture(estate: &Path, name: &str) {
    let text = fs::read_to_string(fixture_dir().join(format!("{name}.json")))
        .unwrap_or_else(|err| panic!("read fixture {name}.json: {err}"));
    write_route(estate, name, &text);
}

/// Writes `text` verbatim as `<estate>/routes/<name>.json` — for a
/// Route authored inline by the test itself (a distinctive intent per
/// call, say), rather than a shared fixture file.
pub fn write_route(estate: &Path, name: &str, text: &str) {
    let routes_dir = estate.join("routes");
    fs::create_dir_all(&routes_dir).expect("create estate routes/ dir");
    fs::write(routes_dir.join(format!("{name}.json")), text)
        .unwrap_or_else(|err| panic!("write estate route {name}.json: {err}"));
}
