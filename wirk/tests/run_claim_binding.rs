//! Ruling 0235: a late Claim from a Run this Work has since moved past
//! must not (a) address the *current* Waypoint's output contract instead
//! of its own bound Waypoint's, and must not (b) reach in and flip the
//! Work's own state out from under whatever Run is actually current.
//!
//! Reproduces, at this repository's own base and against a real `wirkd`
//! and a real `wirk` binary (0040 D127, no fake service), the actual
//! failed trial preserved at `work-18d4c248f13b6ccb-0`: a `review`
//! Waypoint's Run auto-advances a Work to `publish`, then the same old
//! Run's own automatic bare Claim (the Claude/OpenCode Stop-hook shape,
//! `wirk-herdr/src/claim_hook.rs`) fires again. `outputs_two_stage`
//! (`survey.md`/`change.md`, both managed, both required) is the same
//! shape with distinctive names, so a wrong-contract selection is
//! unmistakable in the assertions rather than merely plausible.

#[path = "support/nested_harness.rs"]
mod harness;
#[path = "support/route_fixture.rs"]
mod route_fixture;
#[path = "../src/wirkd/mod.rs"]
mod wirkd;

use std::fs;
use std::path::Path;

use harness::*;

/// `wirk output dir` for an already-known triple — the harness's own
/// `output_dir` lives in `work_owned_outputs.rs`, a different test
/// binary; duplicated here rather than shared across binaries (R6).
fn output_dir(estate: &Path, work_id: &str, run_id: &str) -> std::path::PathBuf {
    let out = std::process::Command::new(wirk_bin())
        .args(["output", "dir"])
        .env("WIRK_ESTATE_ROOT", estate)
        .env("WIRK_WORK_ID", work_id)
        .env("WIRK_RUN_ID", run_id)
        .output()
        .expect("wirk output dir runs");
    assert_eq!(
        out.status.code(),
        Some(0),
        "wirk output dir: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    std::path::PathBuf::from(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Submits `outputs_two_stage`, materializes `survey`'s worktree, writes
/// and bare-claims `survey.md` so the Work auto-advances to `change`.
/// Returns `(estate, wirkd handle, socket, work_id, survey's run, worktree)`.
fn advanced_past_survey(
    dir: &tempfile::TempDir,
) -> (
    std::path::PathBuf,
    KillOnDrop,
    std::path::PathBuf,
    String,
    String,
    std::path::PathBuf,
) {
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    route_fixture::install_route_fixture(&estate, "outputs_two_stage");
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let repo = dir.path().join("repo");
    init_repo(&repo);
    let submitted = submit_kind(
        &estate,
        "outputs_two_stage",
        &repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .expect("submit outputs_two_stage");
    let worktree = materialize_actor(
        &pointer.socket,
        &estate,
        &submitted.work_id,
        &submitted.run_id,
    );

    let staging = output_dir(&estate, &submitted.work_id, &submitted.run_id);
    fs::write(staging.join("survey.md"), b"# survey\n").expect("write survey.md");

    let (code, stdout) = claim(&estate, &submitted.work_id, &submitted.run_id, &[]);
    assert_eq!(code, Some(0), "survey's own bare claim: {stdout}");
    assert_eq!(stdout, "Validated");

    let after = status(&pointer.socket, &submitted.work_id);
    assert_eq!(
        after["current_waypoint"].as_str(),
        Some("outputs-two-stage/change"),
        "the route must have auto-advanced past survey: {after}"
    );

    (
        estate,
        wirkd_child,
        pointer.socket,
        submitted.work_id,
        submitted.run_id,
        worktree,
    )
}

/// RED before ruling 0235: the actual observed defect. `survey`'s own
/// Run, already Validated and superseded by advance to `change`, fires a
/// second bare Claim (`fetch_output_contract_names` asked `status` for
/// the *current* Waypoint's contract — `change`'s `change.md` — and
/// addressed that against `survey`'s own Run, which never declared it).
/// Before the fix this was `Refused: OutOfBoundary` naming `change.md`
/// and latched the whole Work `needs_input`, blocking the live `change`
/// Run's own progress. After the fix, `fetch_output_contract_names` asks
/// for THIS Run's own bound Waypoint (`survey`'s `survey.md`), and
/// `survey`'s Run — already Done — is refused `AlreadyClaimed`, a fact
/// of its own stale Claim that never touches the Work's live state.
#[test]
fn a_superseded_runs_late_bare_claim_never_addresses_the_current_waypoints_contract() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (estate, wirkd_child, socket, work_id, survey_run, _worktree) = advanced_past_survey(&dir);

    let before = status(&socket, &work_id);
    let change_run = before["run_id"]
        .as_str()
        .expect("change's own Run is open")
        .to_string();
    assert_ne!(change_run, survey_run, "advance must open a fresh Run");

    // The stale Run's own automatic Stop-hook Claim, exactly as fired —
    // no `--artifact`/`--output`/`--question` at all.
    let (code, stdout) = claim(&estate, &work_id, &survey_run, &[]);
    assert_eq!(
        code,
        Some(3),
        "a superseded Run's second bare claim must be refused, not honored: {stdout}"
    );
    assert!(
        !stdout.starts_with("Refused: OutOfBoundary"),
        "the producer must never again reach for the CURRENT Waypoint's contract on a Run \
         bound to a DIFFERENT Waypoint — got: {stdout}"
    );

    let after = status(&socket, &work_id);
    assert_eq!(
        after["state"].as_str(),
        Some("active"),
        "a stale Run's own re-claim must never flip the Work to needs_input while a live, \
         current Run is progressing: {after}"
    );
    assert_eq!(
        after["current_waypoint"].as_str(),
        Some("outputs-two-stage/change")
    );
    assert_eq!(
        after["run_id"].as_str(),
        Some(change_run.as_str()),
        "the live change Run must remain the one the Work is on"
    );
    assert!(after.get("needs_input").is_none(), "{after}");

    stop_wirkd(&estate, wirkd_child);
}

/// Negative control, same fixture: an explicit, genuinely malformed
/// `--output` from the SAME stale, superseded `survey` Run (not the
/// producer's own bug — a hand-typed wrong name) is still refused
/// `OutOfBoundary` as a fact of that Claim, but must likewise never
/// reach in and latch the Work `needs_input` out from under the live
/// `change` Run.
#[test]
fn an_explicit_undeclared_output_from_a_superseded_run_stays_out_of_boundary_but_does_not_latch_the_work()
 {
    let dir = tempfile::tempdir().expect("tempdir");
    let (estate, wirkd_child, socket, work_id, survey_run, _worktree) = advanced_past_survey(&dir);

    let (code, stdout) = claim(&estate, &work_id, &survey_run, &["--output", "change.md"]);
    assert_eq!(code, Some(3), "{stdout}");
    assert!(
        stdout.starts_with("Refused: OutOfBoundary"),
        "an explicitly wrong name is still a genuine refusal on its own Claim: {stdout}"
    );

    let after = status(&socket, &work_id);
    assert_eq!(
        after["state"].as_str(),
        Some("active"),
        "a stale Run's own explicit misaddress must not latch the Work either: {after}"
    );
    assert_eq!(
        after["current_waypoint"].as_str(),
        Some("outputs-two-stage/change")
    );

    stop_wirkd(&estate, wirkd_child);
}

/// Negative control: the CURRENT (`change`) Run's own genuine
/// `OutOfBoundary` refusal must still latch the Work `needs_input`
/// exactly as before — this ruling narrows *which* Run's refusal counts,
/// it does not stop counting the one the Work is actually waiting on.
#[test]
fn the_current_runs_own_out_of_boundary_refusal_still_latches_needs_input() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (estate, wirkd_child, socket, work_id, _survey_run, _worktree) = advanced_past_survey(&dir);

    let before = status(&socket, &work_id);
    let change_run = before["run_id"]
        .as_str()
        .expect("change's own Run is open")
        .to_string();

    let (code, stdout) = claim(
        &estate,
        &work_id,
        &change_run,
        &["--output", "not-declared.md"],
    );
    assert_eq!(code, Some(3), "{stdout}");
    assert!(stdout.starts_with("Refused: OutOfBoundary"), "{stdout}");

    let after = status(&socket, &work_id);
    assert_eq!(after["state"].as_str(), Some("needs_input"), "{after}");
    assert_eq!(
        after["needs_input"]["run"].as_str(),
        Some(change_run.as_str())
    );
    assert_eq!(
        after["needs_input"]["reason"].as_str(),
        Some("out_of_boundary")
    );

    stop_wirkd(&estate, wirkd_child);
}

/// The brief's own named gap: "a guard comparing waypoint names only
/// does not handle an old Run after same-waypoint retry." `survey`'s
/// first Run is refused `OutOfBoundary` (a real checkout escape — its
/// declared `boundary` is empty, so any change refuses), a human
/// retries onto a fresh Run of the SAME `survey` Waypoint, then the
/// ORIGINAL (now superseded, same-Waypoint) Run separately fires an
/// explicit malformed `--output`. Both Runs carry the identical
/// Waypoint id, so a guard that only compared `current_waypoint` to the
/// Claim's own Waypoint would wrongly call the stale Run "current" and
/// re-latch `needs_input` naming it — clobbering the retried Run's own
/// live progress. The Run-identity guard (`latest_run_per_waypoint`)
/// must refuse it that authority.
#[test]
fn a_same_waypoint_superseded_runs_out_of_boundary_does_not_reclaim_the_retried_runs_waypoint() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    route_fixture::install_route_fixture(&estate, "outputs_two_stage");
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let repo = dir.path().join("repo");
    init_repo(&repo);
    let submitted = submit_kind(
        &estate,
        "outputs_two_stage",
        &repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .expect("submit outputs_two_stage");
    let worktree = materialize_actor(
        &pointer.socket,
        &estate,
        &submitted.work_id,
        &submitted.run_id,
    );

    // The required managed output, staged (never in the checkout, so it
    // never appears in the boundary scan below) — satisfies the
    // required-output check so the Claim reaches the boundary check at
    // all, exactly `a_bare_claim_snapshots_both_declared_managed_outputs`'s
    // own precedent (`work_owned_outputs.rs`).
    let staging = output_dir(&estate, &submitted.work_id, &submitted.run_id);
    fs::write(staging.join("survey.md"), b"# survey\n").expect("write survey.md");

    // `survey`'s own declared `boundary` is empty: any checkout change
    // at all — even one nothing claims — is a genuine escape, refused
    // `OutOfBoundary` (`needs_input_set_on_out_of_boundary_refusal`'s
    // own precedent, `boundary_claim.rs`).
    fs::write(worktree.join("escape.txt"), b"outside the empty boundary\n")
        .expect("write escape.txt");
    let (code1, stdout1) = claim(&estate, &submitted.work_id, &submitted.run_id, &[]);
    assert_eq!(code1, Some(3), "{stdout1}");
    assert!(stdout1.starts_with("Refused: OutOfBoundary"), "{stdout1}");

    let held = status(&pointer.socket, &submitted.work_id);
    assert_eq!(held["state"].as_str(), Some("needs_input"), "{held}");

    let (retry_code, retry_stdout) = retry_run_cli(&estate, &submitted.work_id, &submitted.run_id);
    assert_eq!(retry_code, Some(0), "{retry_stdout}");
    let retried_run = retry_stdout
        .trim()
        .rsplit(' ')
        .next()
        .expect("retry prints the new run id")
        .to_string();
    assert_ne!(retried_run, submitted.run_id);

    let after_retry = status(&pointer.socket, &submitted.work_id);
    assert_eq!(
        after_retry["state"].as_str(),
        Some("active"),
        "{after_retry}"
    );
    assert_eq!(
        after_retry["current_waypoint"].as_str(),
        Some("outputs-two-stage/survey")
    );

    // The ORIGINAL, now-superseded Run of the SAME Waypoint files a
    // second, differently-wrong Claim.
    let (code2, stdout2) = claim(
        &estate,
        &submitted.work_id,
        &submitted.run_id,
        &["--output", "not-declared-either.md"],
    );
    assert_eq!(code2, Some(3), "{stdout2}");

    let after = status(&pointer.socket, &submitted.work_id);
    assert_eq!(
        after["state"].as_str(),
        Some("active"),
        "a same-Waypoint superseded Run's refusal must not re-latch needs_input over the \
         retried Run's own live progress: {after}"
    );
    assert_eq!(
        after["run_id"].as_str(),
        Some(retried_run.as_str()),
        "the retried Run must remain the one the Work is on: {after}"
    );

    stop_wirkd(&estate, wirkd_child);
}
