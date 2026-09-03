//! Integration test for the `wirk claim` stub (ruling 0001 D9#4; brief
//! `p0-skeleton` W2). Runs the actual built binary via
//! `env!("CARGO_BIN_EXE_wirk")` (cargo-native, R4 — confirmed via ctx7:
//! cargo sets `CARGO_BIN_EXE_<name>` only while running integration
//! tests/benchmarks that select a binary target, building it alongside
//! the test run) rather than calling `main` in-process, so the test
//! proves the same env-inheritance path the Herdr spike will exercise.

use std::process::Command;

fn wirk_bin() -> &'static str {
    env!("CARGO_BIN_EXE_wirk")
}

#[test]
fn claim_prints_triple_and_exits_zero_when_present() {
    let output = Command::new(wirk_bin())
        .arg("claim")
        .env("WIRK_ESTATE_ROOT", "/estate")
        .env("WIRK_WORK_ID", "work-1")
        .env("WIRK_RUN_ID", "run-1")
        .output()
        .expect("wirk claim should run");

    assert!(
        output.status.success(),
        "expected exit 0, got {:?}",
        output.status
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        stdout,
        "WIRK_ESTATE_ROOT=/estate\nWIRK_WORK_ID=work-1\nWIRK_RUN_ID=run-1\n"
    );
}

#[test]
fn claim_fails_and_names_each_missing_variable() {
    let output = Command::new(wirk_bin())
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
    let output = Command::new(wirk_bin())
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
