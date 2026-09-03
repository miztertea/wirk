//! wirk binary entrypoint.
//!
//! `wirk claim` is a stub for the P0 Herdr spike (ruling 0001 D3, D9#4;
//! brief `p0-skeleton` W2 "What triple means"): its only purpose is to
//! prove, running inside a Herdr pane, that the execution triple Herdr
//! injects into the pane's env at creation is inherited by a process
//! wirkd launches there. No validation, no journal, no Herdr call, no
//! other subcommand — those arrive with the claim contract (plan item 4).

use std::env;
use std::process::ExitCode;

/// The injected execution triple: ruling 0001 D3 ("the execution
/// identity injected into the pane env at creation"), names fixed by
/// D5 (`WIRK_ESTATE_ROOT`, `WIRK_WORK_ID`, `WIRK_RUN_ID`), shape from
/// the predecessor's causation contract (sergeant-rs v0.3.0, W1
/// hierarchical execution contract §6:
/// `SERGEANT_ESTATE_ROOT`/`SERGEANT_WORK_ID`/`SERGEANT_EXECUTION_ID`,
/// "a transport hint, not trusted lineage"). Order is print order.
const TRIPLE_VARS: [&str; 3] = ["WIRK_ESTATE_ROOT", "WIRK_WORK_ID", "WIRK_RUN_ID"];

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("claim") => claim(),
        _ => {
            eprintln!("usage: wirk claim");
            ExitCode::FAILURE
        }
    }
}

/// Print each variable of the injected triple as `NAME=value`, one per
/// line, in `TRIPLE_VARS` order, and exit 0. If any is absent, name
/// every missing one on stderr (`wirk claim: missing NAME`) and exit
/// nonzero — nothing is printed to stdout in that case.
///
/// Read exactly the way the predecessor's CLI reads its causation env
/// (sergeant-rs v0.3.0, W1 hierarchical execution contract §6,
/// `claimed_causation`/`origin`, R5): `std::env::var`, with an empty
/// value treated as absent so an exported-but-blank variable still
/// counts as missing, never as `""`.
fn claim() -> ExitCode {
    let mut missing = Vec::new();
    let mut lines = Vec::new();
    for name in TRIPLE_VARS {
        match env::var(name).ok().filter(|v| !v.trim().is_empty()) {
            Some(value) => lines.push(format!("{name}={value}")),
            None => missing.push(name),
        }
    }
    if !missing.is_empty() {
        for name in &missing {
            eprintln!("wirk claim: missing {name}");
        }
        return ExitCode::FAILURE;
    }
    for line in lines {
        println!("{line}");
    }
    ExitCode::SUCCESS
}
