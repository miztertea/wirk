//! Ruling 0257: an automatic turn-end Claim must not complete a Run
//! whose own deliberate `wirk claim --question` is still standing, and
//! must not stop anything else from working.
//!
//! Reproduced against a **real** `wirkd` and a real `wirk` binary
//! (0040 D127, no fake service) on the actual path the defect lives on:
//! an `Actor` Waypoint with a required *managed* declared output,
//! materialized through `wirk_herdr::git::worktree_add` and addressed
//! through `wirk output dir`, which is where a hook-filed bare Claim
//! actually reaches (`wirk/src/main.rs`'s own contract fallback). The
//! substitution the prior verification stage had to make —
//! `Deterministic`+Git Worlds, to avoid launching a model — is not made
//! here: nothing in these tests needs an actor *model*, only an actor
//! *World*, and the harness materializes one for real.
//!
//! The sequence under test is the one observed live in
//! `knowledge/work/p4-recovery/question-completion-use/USE.md`: stage
//! the required output, file a question, then fire the claim the Stop
//! hook fires. Before this correction the last step validated `Done`
//! and completed the Work over the unanswered question.

#[path = "support/nested_harness.rs"]
mod harness;
#[path = "support/route_fixture.rs"]
mod route_fixture;
#[path = "../src/wirkd/mod.rs"]
mod wirkd;

use std::fs;
use std::path::{Path, PathBuf};

use harness::*;

const QUESTION: &str = "should REPORT.md cover the 2026 numbers as well?";

/// `wirk output dir` for an already-known triple — the managed staging
/// area an Actor Waypoint's declared output is written into. Duplicated
/// per test binary rather than shared across binaries (R6, the same
/// reason `run_claim_binding.rs` duplicates it).
fn output_dir(estate: &Path, work_id: &str, run_id: &str) -> PathBuf {
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
    PathBuf::from(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

struct Fixture {
    estate: PathBuf,
    _wirkd: KillOnDrop,
    socket: PathBuf,
    work_id: String,
    run_id: String,
    staging: PathBuf,
}

/// A submitted, materialized single-Waypoint Actor Work whose one
/// required declared output (`REPORT.md`) is managed. Nothing is staged
/// yet — each test decides that, because staged-versus-absent is the
/// axis the whole defect turned on.
fn materialized_actor_work(dir: &tempfile::TempDir) -> Fixture {
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    route_fixture::install_route_fixture(&estate, "actor_question_hold");
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let repo = dir.path().join("repo");
    init_repo(&repo);
    let submitted = submit_kind(
        &estate,
        "actor_question_hold",
        &repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .expect("submit actor_question_hold");
    materialize_actor(
        &pointer.socket,
        &estate,
        &submitted.work_id,
        &submitted.run_id,
    );
    let staging = output_dir(&estate, &submitted.work_id, &submitted.run_id);
    Fixture {
        estate,
        _wirkd: wirkd_child,
        socket: pointer.socket,
        work_id: submitted.work_id,
        run_id: submitted.run_id,
        staging,
    }
}

fn stage_report(fixture: &Fixture) {
    fs::write(fixture.staging.join("REPORT.md"), b"# report (draft)\n").expect("stage REPORT.md");
}

fn ask_question(fixture: &Fixture) {
    let (code, stdout) = claim(
        &fixture.estate,
        &fixture.work_id,
        &fixture.run_id,
        &["--question", QUESTION],
    );
    assert_eq!(code, Some(0), "the question itself must validate: {stdout}");
    assert_eq!(stdout, "Validated");
    assert_eq!(state_of(&fixture.socket, &fixture.work_id), "needs_input");
}

/// The needs-input cause as `work status --admin` reports it.
fn needs_input_cause(fixture: &Fixture) -> serde_json::Value {
    status(&fixture.socket, &fixture.work_id)["needs_input"].clone()
}

fn run_state(fixture: &Fixture) -> serde_json::Value {
    let result = status(&fixture.socket, &fixture.work_id);
    let runs = result["runs"].as_array().expect("status carries runs");
    runs.iter()
        .find(|entry| entry["run"]["id"].as_str() == Some(fixture.run_id.as_str()))
        .expect("this Run appears in status")["run"]["state"]
        .clone()
}

/// The decisive one. Everything this Run needs to complete is true —
/// the required managed output exists by name, the Run is `Open`, the
/// boundary is clean — except that the actor said it needed an answer
/// first. The automatic attempt must refuse and leave the hold visible.
#[test]
fn an_automatic_claim_does_not_complete_over_this_runs_standing_question() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fixture = materialized_actor_work(&dir);
    stage_report(&fixture);
    ask_question(&fixture);

    let (code, stdout) = claim(
        &fixture.estate,
        &fixture.work_id,
        &fixture.run_id,
        &["--automatic"],
    );
    assert_eq!(
        code,
        Some(3),
        "the automatic turn-end claim must be refused, not validated: {stdout}"
    );
    assert!(
        stdout.starts_with("Refused: QuestionOutstanding"),
        "refused for the actual reason, by name: {stdout}"
    );
    assert!(
        stdout.contains(QUESTION),
        "the refusal names the question it preserved: {stdout}"
    );

    // The hold is still exactly the hold the actor filed: same reason,
    // same Run, same text. Not merely "not completed".
    assert_eq!(
        state_of(&fixture.socket, &fixture.work_id),
        "needs_input",
        "the Work must still be waiting on the answer"
    );
    let cause = needs_input_cause(&fixture);
    assert_eq!(cause["reason"].as_str(), Some("question"));
    assert_eq!(cause["detail"].as_str(), Some(QUESTION));
    assert_eq!(cause["run"].as_str(), Some(fixture.run_id.as_str()));
    assert!(
        run_state(&fixture).get("Claimed").is_none(),
        "the Run must still be open to its own deliberate completion: {:?}",
        run_state(&fixture)
    );

    // A refused Claim promotes nothing: the staged bytes stay staged
    // and no claim snapshot was written for them (`store_claimed_bytes`
    // is gated on a still-Validated verdict).
    let claims_dir = fixture
        .estate
        .join("works")
        .join(&fixture.work_id)
        .join("outputs")
        .join("claims");
    let promoted: Vec<PathBuf> = fs::read_dir(&claims_dir)
        .map(|entries| entries.filter_map(Result::ok).map(|e| e.path()).collect())
        .unwrap_or_default();
    assert!(
        promoted.is_empty(),
        "a refused automatic claim must promote no bytes: {promoted:?}"
    );

    // And the refusal is *recorded*, not swallowed — the hook's failure
    // is evidence a reader can find.
    let events = journal_events(&fixture.estate, &fixture.work_id);
    let refusals = events
        .iter()
        .filter(|event| {
            matches!(
                &event.kind,
                wirk_core::EventKind::ClaimRecorded { verdict, origin, .. }
                    if matches!(
                        verdict,
                        wirk_core::ClaimVerdict::Refused(
                            wirk_core::ClaimRefusal::QuestionOutstanding(_)
                        )
                    ) && *origin == Some(wirk_core::ClaimOrigin::Automatic)
            )
        })
        .count();
    assert_eq!(
        refusals, 1,
        "exactly one journaled automatic-over-question refusal"
    );
}

/// The required positive beside it: the actor's own completion command
/// still finishes **this same Run**, question or no question. If this
/// ever fails, the correction above has turned a standing question into
/// a dead end, which is the one thing it must not do.
#[test]
fn the_actors_own_claim_still_finishes_the_same_run_after_its_question() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fixture = materialized_actor_work(&dir);
    stage_report(&fixture);
    ask_question(&fixture);

    // Exactly what the delivered prompt tells the actor to run, with no
    // extra flag of any kind.
    let (code, stdout) = claim(&fixture.estate, &fixture.work_id, &fixture.run_id, &[]);
    assert_eq!(
        code,
        Some(0),
        "the actor's own claim must still complete this Run: {stdout}"
    );
    assert_eq!(stdout, "Validated");
    assert_eq!(
        state_of(&fixture.socket, &fixture.work_id),
        "completed",
        "the same Run finished the Work deliberately"
    );
    assert!(
        run_state(&fixture).get("Claimed").is_some(),
        "the Run is Claimed by its own deliberate Claim: {:?}",
        run_state(&fixture)
    );
}

/// The ordinary case, unchanged: no question was ever asked, the
/// required output exists, the turn ended. This is the accepted
/// automatic-completion contract (0212) and it must still work exactly
/// as it did — the correction is narrow or it is not worth having.
#[test]
fn an_automatic_claim_with_no_question_completes_normally() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fixture = materialized_actor_work(&dir);
    stage_report(&fixture);

    let (code, stdout) = claim(
        &fixture.estate,
        &fixture.work_id,
        &fixture.run_id,
        &["--automatic"],
    );
    assert_eq!(code, Some(0), "ordinary automatic completion: {stdout}");
    assert_eq!(stdout, "Validated");
    assert_eq!(state_of(&fixture.socket, &fixture.work_id), "completed");
}

/// The control that says which refusal belongs to which cause. With the
/// question standing but the required output **absent**, the honest
/// answer is still `MissingArtifact` — the actor has not produced it —
/// and that has always preserved the question. The new refusal adds a
/// state; it does not take this one over.
#[test]
fn an_automatic_claim_without_the_output_still_refuses_missing_artifact() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fixture = materialized_actor_work(&dir);
    ask_question(&fixture);

    let (code, stdout) = claim(
        &fixture.estate,
        &fixture.work_id,
        &fixture.run_id,
        &["--automatic"],
    );
    assert_eq!(code, Some(3), "still refused: {stdout}");
    assert_eq!(
        stdout, "Refused: MissingArtifact REPORT.md",
        "the absent required output keeps its own refusal"
    );
    assert_eq!(state_of(&fixture.socket, &fixture.work_id), "needs_input");
    assert_eq!(
        needs_input_cause(&fixture)["reason"].as_str(),
        Some("question")
    );
}

/// A legitimate zero-output check: a Waypoint declaring no *required*
/// output completes automatically with no artifacts at all, and the
/// correction must not have turned "produced nothing" into a defect.
/// Same Route shape, one optional declared output, nothing staged.
#[test]
fn an_automatic_claim_still_completes_a_waypoint_with_no_required_output() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    route_fixture::write_route(
        &estate,
        "actor_optional_only",
        r#"{
  "id": "actor-optional-only",
  "waypoints": [
    {
      "id": "actor-optional-only/check",
      "kind": "Actor",
      "intent": "check something that legitimately leaves no artifact",
      "declared_outputs": [{"name": "NOTES.md", "required": false}],
      "boundary": []
    }
  ]
}"#,
    );
    let (_wirkd, pointer) = start_wirkd(&estate);
    let repo = dir.path().join("repo");
    init_repo(&repo);
    let submitted = submit_kind(
        &estate,
        "actor_optional_only",
        &repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .expect("submit actor_optional_only");
    materialize_actor(
        &pointer.socket,
        &estate,
        &submitted.work_id,
        &submitted.run_id,
    );

    let (code, stdout) = claim(
        &estate,
        &submitted.work_id,
        &submitted.run_id,
        &["--automatic"],
    );
    assert_eq!(
        code,
        Some(0),
        "a legitimate output-free check still completes: {stdout}"
    );
    assert_eq!(stdout, "Validated");
    assert_eq!(state_of(&pointer.socket, &submitted.work_id), "completed");
}

/// Reason-scoping, and the behaviour ruling 0243 asked to be preserved:
/// a Work held `NeedsInput` for `no_observable_progress` is **not**
/// holding a question. A late Claim arriving afterwards is exactly the
/// evidence that the actor was working all along, and an automatic one
/// still completes it. A live background service is not an
/// incomplete-work signal and this correction does not make it one.
#[test]
fn an_automatic_claim_still_completes_after_no_observable_progress() {
    use crate::wirkd::RecordPayload;
    use wirk_core::{EventKind, RunId, WorkId};

    let dir = tempfile::tempdir().expect("tempdir");
    let fixture = materialized_actor_work(&dir);
    stage_report(&fixture);

    // The same two events `wirk run` journals on that path, through the
    // daemon's own `record` verb — a launch request, then the
    // observation itself (`LifecycleObserved` is refused before a
    // launch is on record, `server.rs`'s own guard).
    for kind in [
        EventKind::RunLaunchRequested {
            run: RunId(fixture.run_id.clone()),
            actor_kind: Default::default(),
            selection: Default::default(),
        },
        EventKind::LifecycleObserved {
            status: "NoObservableProgress".to_string(),
            detail: Some("no tool call or message in the observation window".to_string()),
        },
    ] {
        let reply = wirkd::client::call(
            &fixture.socket,
            &wirkd::Request::record(RecordPayload {
                work_id: WorkId(fixture.work_id.clone()),
                run: Some(RunId(fixture.run_id.clone())),
                kind,
            }),
        )
        .expect("record call succeeds");
        assert!(matches!(reply, wirkd::Reply::Ok { .. }), "{reply:?}");
    }
    assert_eq!(state_of(&fixture.socket, &fixture.work_id), "needs_input");
    assert_eq!(
        needs_input_cause(&fixture)["reason"].as_str(),
        Some("no_observable_progress"),
        "the hold under test is a progress observation, not a question"
    );

    let (code, stdout) = claim(
        &fixture.estate,
        &fixture.work_id,
        &fixture.run_id,
        &["--automatic"],
    );
    assert_eq!(
        code,
        Some(0),
        "a late legitimate automatic Claim after NoObservableProgress must still complete: \
         {stdout}"
    );
    assert_eq!(stdout, "Validated");
    assert_eq!(state_of(&fixture.socket, &fixture.work_id), "completed");
}

/// Run-scoping: the question belongs to a Run, not to the Work. A
/// question standing against *another* Run must never block this Run's
/// own automatic completion. The two-stage Route gives two real Runs of
/// one Work; the first asks, the second finishes.
#[test]
fn another_runs_question_does_not_hold_this_runs_automatic_claim() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    route_fixture::install_route_fixture(&estate, "outputs_two_stage");
    let (_wirkd, pointer) = start_wirkd(&estate);
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
    materialize_actor(
        &pointer.socket,
        &estate,
        &submitted.work_id,
        &submitted.run_id,
    );

    // Run 1 (`survey`) asks a question, then finishes deliberately.
    let staging = output_dir(&estate, &submitted.work_id, &submitted.run_id);
    fs::write(staging.join("survey.md"), b"# survey\n").expect("stage survey.md");
    let (code, stdout) = claim(
        &estate,
        &submitted.work_id,
        &submitted.run_id,
        &["--question", "is src/lib.rs in scope?"],
    );
    assert_eq!(code, Some(0), "{stdout}");
    let (code, stdout) = claim(&estate, &submitted.work_id, &submitted.run_id, &[]);
    assert_eq!(code, Some(0), "survey's own deliberate claim: {stdout}");

    // The Work advanced to `change`, which opened its own Run. A stale
    // question from `survey` must not reach into it.
    let result = status(&pointer.socket, &submitted.work_id);
    let run2 = result["run_id"].as_str().expect("a second Run").to_string();
    assert_ne!(run2, submitted.run_id);
    let staging2 = output_dir(&estate, &submitted.work_id, &run2);
    fs::write(staging2.join("change.md"), b"# change\n").expect("stage change.md");

    let (code, stdout) = claim(&estate, &submitted.work_id, &run2, &["--automatic"]);
    assert_eq!(
        code,
        Some(0),
        "this Run has no question of its own and must complete: {stdout}"
    );
    assert_eq!(stdout, "Validated");
}
