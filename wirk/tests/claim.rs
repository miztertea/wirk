//! Integration test for `wirk claim`'s env-triple handling (ruling 0001
//! D3, D9#4; brief `p0-skeleton` W2, carried into W3). Runs the actual
//! built binary via `env!("CARGO_BIN_EXE_wirk")` (cargo-native, R4 —
//! confirmed via ctx7: cargo sets `CARGO_BIN_EXE_<name>` only while
//! running integration tests/benchmarks that select a binary target,
//! building it alongside the test run) rather than calling `main`
//! in-process, so the test proves the same env-inheritance path the
//! Herdr spike will exercise.
//!
//! W3 (0023 D81) makes `claim` real: with the triple present it now
//! locates wirkd via `WIRK_ESTATE_ROOT`'s pointer file rather than
//! printing the triple, so `claim_prints_triple_and_exits_zero_when_present`
//! (the P0-era assertion) is replaced below by
//! `claim_with_no_wirkd_running_is_a_transport_error` — the
//! missing/blank-variable cases are usage errors caught before wirkd is
//! ever contacted and are unaffected, so they stay as they were.
//! The full accepted/refused wire path is `wirk/tests/wirkd_process.rs`'s
//! job (a real wirkd, not this file's no-wirkd-running case).

use std::process::Command;

fn wirk_bin() -> &'static str {
    env!("CARGO_BIN_EXE_wirk")
}

#[test]
fn claim_with_no_wirkd_running_is_a_transport_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    // No `.wirk/wirkd.json` under this estate root — wirkd was never
    // started here, so `client::locate` fails and `claim` exits 2
    // (transport error), never printing a verdict.
    let output = wirk_cli()
        .arg("claim")
        .env("WIRK_ESTATE_ROOT", dir.path())
        .env("WIRK_WORK_ID", "work-1")
        .env("WIRK_RUN_ID", "run-1")
        .output()
        .expect("wirk claim should run");

    assert_eq!(
        output.status.code(),
        Some(2),
        "expected exit 2, got {:?}",
        output.status
    );
    assert!(
        output.stdout.is_empty(),
        "stdout should be empty on a transport error, got {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("wirkd.json"),
        "expected the pointer-not-found error, got: {stderr}"
    );
}

#[test]
fn claim_fails_and_names_each_missing_variable() {
    let output = wirk_cli()
        .arg("claim")
        .env_remove("WIRK_ESTATE_ROOT")
        .env_remove("WIRK_WORK_ID")
        .env_remove("WIRK_RUN_ID")
        .output()
        .expect("wirk claim should run");

    assert!(
        !output.status.success(),
        "expected nonzero exit, got {:?}",
        output.status
    );
    assert!(
        output.stdout.is_empty(),
        "stdout should be empty on failure, got {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("WIRK_ESTATE_ROOT"), "stderr: {stderr}");
    assert!(stderr.contains("WIRK_WORK_ID"), "stderr: {stderr}");
    assert!(stderr.contains("WIRK_RUN_ID"), "stderr: {stderr}");
}

#[test]
fn claim_treats_blank_variable_as_missing() {
    let output = wirk_cli()
        .arg("claim")
        .env("WIRK_ESTATE_ROOT", "")
        .env("WIRK_WORK_ID", "work-1")
        .env("WIRK_RUN_ID", "run-1")
        .output()
        .expect("wirk claim should run");

    assert!(
        !output.status.success(),
        "expected nonzero exit, got {:?}",
        output.status
    );
    assert!(
        output.stdout.is_empty(),
        "stdout should be empty on failure, got {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("WIRK_ESTATE_ROOT"), "stderr: {stderr}");
    assert!(
        !stderr.contains("WIRK_WORK_ID"),
        "stderr should not name WIRK_WORK_ID, got {stderr}"
    );
    assert!(
        !stderr.contains("WIRK_RUN_ID"),
        "stderr should not name WIRK_RUN_ID, got {stderr}"
    );
}

/// Ruling 0145's own rule, still enforced after the ruling 0212
/// correction: one name cannot be claimed from both places at once.
/// This is a usage error caught before the triple is even read (the
/// duplicate check runs on the parsed flags alone), so no wirkd needs
/// to be running.
#[test]
fn claim_refuses_a_name_claimed_as_both_artifact_and_output() {
    let output = wirk_cli()
        .args([
            "claim",
            "--artifact",
            "report.md=report.md",
            "--output",
            "report.md",
        ])
        .env_remove("WIRK_ESTATE_ROOT")
        .env_remove("WIRK_WORK_ID")
        .env_remove("WIRK_RUN_ID")
        .output()
        .expect("wirk claim should run");

    assert_eq!(
        output.status.code(),
        Some(1),
        "expected exit 1 (usage), got {:?}",
        output.status
    );
    assert!(
        output.stdout.is_empty(),
        "stdout should be empty on a usage error, got {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("report.md")
            && stderr.contains("--artifact")
            && stderr.contains("--output"),
        "expected the conflicting-name usage error naming both flags, got: {stderr}"
    );
}

/// The `wirk` CLI with the *test runner's own* actor triple removed from
/// the child's environment.
///
/// `resolve_scope` reads `WIRK_ESTATE_ROOT`/`WIRK_WORK_ID`/`WIRK_RUN_ID`
/// to decide whether a call is an actor's own or an operator's, and a
/// test process inherits whatever its runner had. This suite is run from
/// inside a real actor pane often enough that an inherited triple makes
/// a fixture's administrative call against its own temp estate refuse as
/// a cross-estate read — so the fixture has to say which it is rather
/// than depend on who started it.
///
/// Sites that mean to act *as* an actor set the three back explicitly on
/// the returned command; a later `env` overrides this removal.
fn wirk_cli() -> Command {
    let mut command = Command::new(wirk_bin());
    command
        .env_remove("WIRK_ESTATE_ROOT")
        .env_remove("WIRK_WORK_ID")
        .env_remove("WIRK_RUN_ID");
    command
}
