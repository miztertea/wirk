//! Ruling 0339: `wirk artifact read --estate <root> --work <id> (--admin
//! | --requesting-work <id>)` — the named-Work door onto a validated
//! artifact for a caller with no execution triple at all, or one naming
//! its own scope explicitly rather than presenting a borrowed one.
//!
//! Every case here drives the real built `wirk` binary against a real
//! `wirkd` and a real ad hoc output-only Deterministic Work (ruling
//! 0040: no fake daemon, no constructed receipt) — the same
//! `submit_adhoc`/`run_deterministic` shape `owned_deterministic_custody.rs`
//! already uses, model-free by construction. `wirk_cli()`
//! (`support/nested_harness.rs`) clears `WIRK_ESTATE_ROOT`/
//! `WIRK_WORK_ID`/`WIRK_RUN_ID` on every invocation, so the operator
//! cases here run with no ambient execution triple by default, not by a
//! test-only accommodation.

#[path = "support/nested_harness.rs"]
mod harness;
#[path = "support/route_fixture.rs"]
mod route_fixture;
use wirk::wirkd;

use std::path::{Path, PathBuf};
use std::process::Command;

use harness::{
    ParentRef, claim_ok, init_repo, journal_events, raw_append, start_wirkd, state_of, stop_wirkd,
    submit, wirk_cli, write_file,
};

use wirk_core::{ClaimKind, ClaimVerdict, EventKind};

// ---- fixture: one completed ad hoc output-only Deterministic Work --------

fn submit_adhoc(estate: &Path, command: &[&str]) -> (String, String) {
    let output = wirk_cli()
        .args(["work", "submit", "--estate"])
        .arg(estate)
        .args([
            "--repo",
            "demo:write",
            "--base",
            "deadbeef",
            "--kind",
            "deterministic",
            "--command",
        ])
        .args(command)
        .output()
        .expect("work submit runs");
    assert!(
        output.status.success(),
        "work submit failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let mut work = String::new();
    let mut run = String::new();
    for pair in stdout.split_whitespace().collect::<Vec<_>>().chunks(2) {
        if let [key, value] = pair {
            match *key {
                "work_id" => work = (*value).to_string(),
                "run_id" => run = (*value).to_string(),
                _ => {}
            }
        }
    }
    assert!(!work.is_empty() && !run.is_empty(), "submit said: {stdout}");
    (work, run)
}

fn run_deterministic(estate: &Path, work: &str) -> (Option<i32>, String) {
    let output = wirk_cli()
        .args(["run-deterministic", "--estate"])
        .arg(estate)
        .args(["--work", work, "--executor", "child"])
        .output()
        .expect("run-deterministic runs");
    (
        output.status.code(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

fn validated_claim(estate: &Path, work: &str) -> String {
    journal_events(estate, work)
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ClaimRecorded {
                claim,
                claim_kind: ClaimKind::Done,
                verdict: ClaimVerdict::Validated,
                ..
            } => Some(claim.0.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no validated Done Claim journaled for {work}"))
}

/// A completed ad hoc Work: one `report.md` output, one validated `Done`
/// Claim, filed by `run` on `waypoint`. `(estate dir, work, run, claim)`.
fn completed_work(command: &[&str]) -> (tempfile::TempDir, PathBuf, String, String, String) {
    let dir = tempfile::tempdir().expect("estate tempdir");
    let estate = dir.path().to_path_buf();
    let (daemon, _pointer) = start_wirkd(&estate);
    let (work, run) = submit_adhoc(&estate, command);
    let (code, log) = run_deterministic(&estate, &work);
    assert_eq!(code, Some(0), "run-deterministic: {log}");
    let claim = validated_claim(&estate, &work);
    stop_wirkd(&estate, daemon);
    (dir, estate, work, run, claim)
}

fn artifact_admin(
    estate: &Path,
    work: &str,
    claim: &str,
    name: &str,
) -> (Option<i32>, Vec<u8>, String) {
    let output = wirk_cli()
        .args(["artifact", "read", "--estate"])
        .arg(estate)
        .args(["--work", work, "--admin", "--claim", claim, "--name", name])
        .output()
        .expect("wirk artifact read --admin runs");
    (
        output.status.code(),
        output.stdout,
        String::from_utf8_lossy(&output.stderr).trim().to_string(),
    )
}

fn artifact_scoped(
    estate: &Path,
    work: &str,
    requester: &str,
    claim: &str,
    name: &str,
) -> (Option<i32>, Vec<u8>, String) {
    let output = wirk_cli()
        .args(["artifact", "read", "--estate"])
        .arg(estate)
        .args([
            "--work",
            work,
            "--requesting-work",
            requester,
            "--claim",
            claim,
            "--name",
            name,
        ])
        .output()
        .expect("wirk artifact read --requesting-work runs");
    (
        output.status.code(),
        output.stdout,
        String::from_utf8_lossy(&output.stderr).trim().to_string(),
    )
}

fn artifact_triple(
    estate: &Path,
    work: &str,
    run: &str,
    claim: &str,
    name: &str,
) -> (Option<i32>, Vec<u8>, String) {
    let output = wirk_cli()
        .args(["artifact", "read", "--claim", claim, "--name", name])
        .env("WIRK_ESTATE_ROOT", estate)
        .env("WIRK_WORK_ID", work)
        .env("WIRK_RUN_ID", run)
        .output()
        .expect("wirk artifact read (triple) runs");
    (
        output.status.code(),
        output.stdout,
        String::from_utf8_lossy(&output.stderr).trim().to_string(),
    )
}

// ---- 1. an administrative shell has no Run, and needs none ----------------

/// The concrete case ruling 0339 opens with: an operator's ordinary
/// shell, no `WIRK_*` in its environment at all (`wirk_cli()` clears
/// every one of them), reading an ended Work's validated result by
/// naming the Work, not by presenting a triple it does not have.
#[test]
fn operator_reads_an_ended_works_artifact_with_no_execution_triple_at_all() {
    let (_dir, estate, work, _run, claim) =
        completed_work(&["sh", "-c", "printf 'operator read\\n' > report.md"]);
    let (daemon, _pointer) = start_wirkd(&estate);

    let (code, out, err) = artifact_admin(&estate, &work, &claim, "report.md");
    assert_eq!(code, Some(0), "administrative read refused: {err}");
    assert_eq!(out, b"operator read\n");

    stop_wirkd(&estate, daemon);
}

// ---- 2. a historical validated artifact after its producing Run is
//         superseded on its own Waypoint --------------------------------

/// Reproduces ruling 0339's named defect concretely: the *old* guidance
/// shape named the Claim's producing Run in the printed triple, and
/// `handle_run_artifact` refuses any caller that is not the current
/// unsuperseded Run of its own Waypoint — so a real retry that reopens
/// the very Waypoint a validated Claim was filed on leaves that Claim's
/// own producing Run refused, even though the Claim itself is still
/// `Validated` and its bytes are untouched. The new administrative door
/// does not name a Run at all, and is unaffected: it reads the same
/// bytes before and after.
///
/// The supersession is journaled directly (`raw_append`, the same
/// pattern `nested_correction.rs`/`nested_work.rs` already use to reach
/// states no single CLI call drives on its own): a second `RunOpened`
/// for the *same* Waypoint the completed Run already validated a Claim
/// on. This is exactly the shape W-A's held-container correction path
/// produces when a leaf's Claim validated cleanly but the leaf is
/// reopened anyway (`server.rs`'s own `WorkState::Waiting` retry arm) —
/// reached here directly, on real `RunOpened`/`ClaimRecorded` journal
/// semantics and a real `handle_run_artifact` call, rather than by
/// building the full nested-container fixture that path requires.
#[test]
fn historical_artifact_survives_a_real_retry_that_supersedes_its_producing_run() {
    let (_dir, estate, work, run, claim) =
        completed_work(&["sh", "-c", "printf 'before retry\\n' > report.md"]);
    let (daemon, _pointer) = start_wirkd(&estate);

    // Before the retry: the producing Run's own triple still reads.
    let (code, out, err) = artifact_triple(&estate, &work, &run, &claim, "report.md");
    assert_eq!(
        code,
        Some(0),
        "the producing Run must read its own Claim: {err}"
    );
    assert_eq!(out, b"before retry\n");

    let (opened_waypoint, opened_hash) = journal_events(&estate, &work)
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::RunOpened {
                run: opened_run,
                waypoint,
                world_hash,
                ..
            } if opened_run.0 == run => Some((waypoint.clone(), world_hash.clone())),
            _ => None,
        })
        .expect("the completed run journaled its own RunOpened");
    let superseding_run = format!("{run}-retry");
    raw_append(
        &estate,
        &work,
        Some(&superseding_run),
        EventKind::RunOpened {
            run: wirk_core::RunId(superseding_run.clone()),
            waypoint: opened_waypoint,
            attempt: 2,
            world_hash: opened_hash,
        },
    );

    // RED, watched: the Claim is exactly as validated as it was, but the
    // producing Run is no longer current for its own Waypoint.
    let (code, _out, err) = artifact_triple(&estate, &work, &run, &claim, "report.md");
    assert_ne!(
        code,
        Some(0),
        "a superseded producing Run must not go on reading this Claim"
    );
    assert!(
        err.contains("superseded"),
        "expected the refusal to say why, got: {err}"
    );

    // GREEN: the administrative door names no Run at all, so the
    // supersession is not a question it has to ask, and the historical
    // validated artifact remains available exactly as ruling 0339 says
    // it must.
    let (code, out, err) = artifact_admin(&estate, &work, &claim, "report.md");
    assert_eq!(
        code,
        Some(0),
        "a historical validated artifact must remain administratively readable: {err}"
    );
    assert_eq!(out, b"before retry\n");

    stop_wirkd(&estate, daemon);
}

// ---- 3. a named requester is admitted to its own Work, withheld from
//         an unrelated one ------------------------------------------------

/// The scoped door is admitted by the identical lineage `wirk wirkd
/// status --requesting-work` already checks (`handle_status`'s own
/// `lineage_of`), never by a borrowed Run identity: a Work reading its
/// own validated artifact is trivially in its own lineage, and an
/// unrelated Work naming it is refused by name, the same
/// `InadmissibleEvidence` a scoped status read already answers with.
#[test]
fn a_requesting_work_is_admitted_to_its_own_artifact_and_withheld_from_an_unrelated_one() {
    let (_dir, estate, work, _run, claim) =
        completed_work(&["sh", "-c", "printf 'scoped read\\n' > report.md"]);
    let (daemon, _pointer) = start_wirkd(&estate);
    let (other_work, _other_run) =
        submit_adhoc(&estate, &["sh", "-c", "printf 'unrelated\\n' > report.md"]);

    let (code, out, err) = artifact_scoped(&estate, &work, &work, &claim, "report.md");
    assert_eq!(code, Some(0), "a Work must read its own artifact: {err}");
    assert_eq!(out, b"scoped read\n");

    let (code, _out, err) = artifact_scoped(&estate, &work, &other_work, &claim, "report.md");
    assert_ne!(
        code,
        Some(0),
        "an unrelated requester must be withheld, not silently answered"
    );
    assert!(
        err.contains("InadmissibleEvidence"),
        "expected the named scoped refusal, got: {err}"
    );

    stop_wirkd(&estate, daemon);
}

// ---- 4. the printed guidance itself: operator and actor shapes --------

/// `wirk work status --admin` names the operator's own retrieval
/// command, and — copied verbatim into a shell with no execution triple
/// — it actually runs. No env var, no producing Run, ruling 0339.
#[test]
fn status_prints_operator_guidance_that_runs_with_no_ambient_triple() {
    let (_dir, estate, work, _run, claim) =
        completed_work(&["sh", "-c", "printf 'guided\\n' > report.md"]);
    let (daemon, pointer) = start_wirkd(&estate);
    let _ = &pointer;

    let output = wirk_cli()
        .args(["work", "status", "--estate"])
        .arg(&estate)
        .args(["--work", &work, "--admin"])
        .output()
        .expect("wirk work status --admin runs");
    let printed = String::from_utf8_lossy(&output.stdout).into_owned();
    let line = printed
        .lines()
        .find(|line| line.trim_start().starts_with("read with:"))
        .unwrap_or_else(|| panic!("no retrieval guidance printed:\n{printed}"));
    let command_text = line
        .trim_start()
        .strip_prefix("read with: ")
        .expect("read with: prefix");
    assert!(
        !command_text.contains("WIRK_RUN_ID"),
        "the operator's own guidance must name no Run at all: {command_text}"
    );
    assert!(
        command_text.contains("--admin"),
        "the operator's guidance must be the administrative form: {command_text}"
    );

    let rest = command_text
        .strip_prefix("wirk ")
        .expect("guidance names the wirk binary first");
    let run = std::process::Command::new("env")
        .args([
            "-u",
            "WIRK_ESTATE_ROOT",
            "-u",
            "WIRK_WORK_ID",
            "-u",
            "WIRK_RUN_ID",
            "sh",
            "-c",
            &format!("'{}' {rest}", env!("CARGO_BIN_EXE_wirk")),
        ])
        .output()
        .expect("the printed command runs as printed, with no ambient triple");
    assert!(
        run.status.success(),
        "printed guidance failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(run.stdout, b"guided\n");
    let _ = claim;

    stop_wirkd(&estate, daemon);
}

// ---- 5. a related Work's own bindings, not its lineage alone ------------

/// The family the scoped artifact door has to answer correctly: one
/// held container Work whose leaf has already filed a validated `Done`
/// Claim over `a.md`, and two real required children of that same
/// container — one **narrowed** (bound to `extra` only) and one
/// **covered** (bound to the parent's whole set, its `wirk` checkout a
/// genuine `git worktree` of the parent's own repository, which is what
/// `spawn_child_on_parent`'s execution-identity check admits).
///
/// Both children are in the parent's lineage. Only the covered one's
/// own bindings cover the parent's, which is the distinction
/// `DisclosureView::admits_work_checkout` draws and `handle_status`
/// already applies before releasing this same Work's checkout-derived
/// content.
struct ReaderScopeFamily {
    _dir: tempfile::TempDir,
    estate: PathBuf,
    parent: String,
    parent_run: String,
    claim: String,
    narrowed_child: String,
    covered_child: String,
    covered_child_run: String,
}

fn git_worktree_add(source_repo: &Path, worktree_path: &Path) {
    let status = Command::new("git")
        .arg("-C")
        .arg(source_repo)
        .args(["worktree", "add", "--detach"])
        .arg(worktree_path)
        .arg("HEAD")
        .status()
        .expect("git worktree add runs");
    assert!(status.success(), "git worktree add failed");
}

fn reader_scope_family() -> (ReaderScopeFamily, harness::KillOnDrop) {
    let dir = tempfile::tempdir().expect("estate tempdir");
    let estate = dir.path().join("estate");
    std::fs::create_dir_all(&estate).expect("estate dir");
    // Two required child roles, so neither child's own completion can
    // close the container out from under the other: the parent stays
    // `waiting` — non-terminal, which is what `spawn_child_on_parent`
    // requires — for the whole of this fixture.
    route_fixture::write_route(
        &estate,
        "reader_scope_family",
        r#"{"id":"reader-scope-family","waypoints":[
            {"id":"outer","kind":"Container",
             "declared_outputs":[{"name":"a.md","required":true}],
             "required_child_outcomes":[
               {"role":"narrow","required":true},
               {"role":"covered","required":true}],
             "leaves":[
               {"id":"outer/leaf-a","kind":"Deterministic",
                "command":["sh","-c","echo a > a.md"],
                "declared_outputs":[{"name":"a.md","required":true}]}
             ]}
        ]}"#,
    );
    route_fixture::install_route_fixture(&estate, "wa_simple_leaf");
    let (daemon, pointer) = start_wirkd(&estate);

    let parent_repo = dir.path().join("parent-repo");
    init_repo(&parent_repo);
    let parent = submit(
        &estate,
        "reader_scope_family",
        &parent_repo,
        &["wirk:write", "extra:write"],
        None,
    )
    .expect("submit the container parent");

    // The parent's own Deterministic leaf executes in this Work's own
    // worktree; its claimed artifact is a real file there.
    let parent_worktree = estate.join("worktrees").join(&parent.work_id);
    write_file(&parent_worktree, "a.md", "parent claimed bytes\n");
    claim_ok(&estate, &parent.work_id, &parent.run_id, "a.md=a.md");
    assert_eq!(
        state_of(&pointer.socket, &parent.work_id),
        "waiting",
        "the container must still be holding for its required children"
    );
    let claim = validated_claim(&estate, &parent.work_id);

    // The narrowed child: bound to `extra` only, so it does not hold
    // the parent's `wirk` binding at all. `extra` is not the parent's
    // own execution repository, so this is an ordinary admitted child
    // under a name the parent explicitly declared for the purpose.
    let extra_repo = dir.path().join("extra-repo");
    init_repo(&extra_repo);
    let narrowed_child = submit(
        &estate,
        "wa_simple_leaf",
        &extra_repo,
        &["extra:write"],
        Some(ParentRef {
            work: &parent.work_id,
            waypoint: "outer",
            run: &parent.run_id,
            role: "narrow",
            attempt: None,
        }),
    )
    .expect("a narrowed required child is an ordinary admitted spawn");

    // The covered child: the parent's whole binding set, its `wirk`
    // checkout a real worktree of the parent's own repository.
    let covered_checkout = dir.path().join("covered-checkout");
    git_worktree_add(&parent_repo, &covered_checkout);
    let covered_child = submit(
        &estate,
        "wa_simple_leaf",
        &covered_checkout,
        &["wirk:write", "extra:write"],
        Some(ParentRef {
            work: &parent.work_id,
            waypoint: "outer",
            run: &parent.run_id,
            role: "covered",
            attempt: None,
        }),
    )
    .expect("a child covering the parent's whole binding set is admitted");

    (
        ReaderScopeFamily {
            _dir: dir,
            estate,
            parent: parent.work_id,
            parent_run: parent.run_id,
            claim,
            narrowed_child: narrowed_child.work_id,
            covered_child: covered_child.work_id,
            covered_child_run: covered_child.run_id,
        },
        daemon,
    )
}

/// **RED on the pre-correction tree.** `handle_work_artifact` admitted a
/// scoped reader on `lineage_of` alone and went straight to
/// `resolve_validated_artifact` — while `handle_status`, asked about
/// that same target Work by that same narrowed requester, withholds
/// every checkout-derived part of its answer first
/// (`DisclosureView::admits_work_checkout`). Lineage grants permission
/// to *reference* a related Work's journal; it has never granted
/// disclosure of what is in that Work's checkout, and an artifact's
/// resolved path, digest and bytes are exactly that.
///
/// So the narrowed child could fetch, in full, the very content the
/// sibling verb hands it back withheld. The covered child in the same
/// family is the paired control: its own bindings do cover the
/// parent's, so it reads the claimed bytes and the correction takes
/// nothing legitimate away.
#[test]
fn a_narrowed_related_work_is_refused_the_artifact_content_status_withholds() {
    let (family, daemon) = reader_scope_family();

    // The sibling verb, on the identical pair: admitted to the journal
    // identities, withheld the checkout-derived content. This is the
    // boundary the artifact door has to agree with, read from the real
    // service rather than asserted.
    let status = wirk_cli()
        .args(["work", "status", "--estate"])
        .arg(&family.estate)
        .args([
            "--work",
            &family.parent,
            "--requesting-work",
            &family.narrowed_child,
            "--json",
        ])
        .output()
        .expect("wirk work status --requesting-work runs");
    let status: serde_json::Value =
        serde_json::from_slice(&status.stdout).expect("scoped status is JSON");
    assert_eq!(status["scope"], "requester", "{status}");
    assert!(
        status["disclosure"]["withheld"].as_u64().unwrap_or(0) > 0,
        "the fixture must actually be a withholding pair, or this test proves nothing: {status}"
    );

    // RED, watched: the same narrowed requester reading the same Work's
    // claimed artifact.
    let (code, out, err) = artifact_scoped(
        &family.estate,
        &family.parent,
        &family.narrowed_child,
        &family.claim,
        "a.md",
    );
    assert_ne!(
        code,
        Some(0),
        "a requester whose bindings do not cover the named Work must not fetch its claimed \
         artifact: status withholds this same content, and it came back in full: {out:?}"
    );
    assert!(
        err.contains("InadmissibleEvidence"),
        "the refusal must be the named scoped one, got: {err}"
    );
    assert!(
        !String::from_utf8_lossy(&out).contains("parent claimed bytes"),
        "no part of the withheld content may travel in the refusal"
    );

    // GREEN control, in the same family and against the same Claim: the
    // covered child's own bindings do cover the parent's.
    let (code, out, err) = artifact_scoped(
        &family.estate,
        &family.parent,
        &family.covered_child,
        &family.claim,
        "a.md",
    );
    assert_eq!(
        code,
        Some(0),
        "a covered related Work must still read the artifact it is admitted to: {err}"
    );
    assert_eq!(out, b"parent claimed bytes\n");

    // And the administrative door is unchanged by the correction.
    let (code, out, err) = artifact_admin(&family.estate, &family.parent, &family.claim, "a.md");
    assert_eq!(code, Some(0), "administrative read refused: {err}");
    assert_eq!(out, b"parent claimed bytes\n");

    stop_wirkd(&family.estate, daemon);
}

// ---- 6. the printed guidance inside a real actor environment -----------

/// **RED on the pre-correction tree.** Ruling 0339's rule for this line
/// is that the printed guidance names *the scope that generated the
/// reply*. `is_bound_actor` was read from the actor context alone —
/// whether this process holds an injected triple — and not from whether
/// the line being printed is about that actor's **own** Work.
///
/// A bound actor may legitimately ask about a related Work it covers
/// (`wirk work status --work <other>`, answered scoped as itself). Every
/// evidence line of that other Work's reply then got the bare
/// triple-form `wirk artifact read --claim … --name …`, whose door is
/// `handle_run_artifact` against the *actor's own* Work — so the command
/// printed beside another Work's claimed artifact silently reads
/// somewhere else, and cannot return the bytes on the line above it.
/// That is the same "do not silently change the identity or the scope
/// the reader is in" defect 0339 named on the operator's side, on the
/// actor's side of the same block.
///
/// The actor's own-Work guidance is unchanged and checked below beside
/// it: that line must stay the bare form, naming no triple, because the
/// actor's already-injected environment is exactly the current Run
/// `handle_run_artifact` checks.
#[test]
fn an_actors_guidance_for_a_related_work_reads_that_work_and_not_its_own() {
    let (family, daemon) = reader_scope_family();

    let actor_status = |target: &str| -> String {
        let output = wirk_cli()
            .args(["work", "status", "--estate"])
            .arg(&family.estate)
            .args(["--work", target])
            .env("WIRK_ESTATE_ROOT", &family.estate)
            .env("WIRK_WORK_ID", &family.covered_child)
            .env("WIRK_RUN_ID", &family.covered_child_run)
            .output()
            .expect("wirk work status runs inside an actor environment");
        String::from_utf8_lossy(&output.stdout).into_owned()
    };

    // The covered child, asking about the parent it is admitted to.
    let printed = actor_status(&family.parent);
    let line = printed
        .lines()
        .find(|line| line.trim_start().starts_with("read with:"))
        .unwrap_or_else(|| panic!("no retrieval guidance printed for the related Work:\n{printed}"))
        .trim_start()
        .strip_prefix("read with: ")
        .expect("read with: prefix")
        .to_string();

    // Run it exactly as printed, in the very environment it was printed
    // into — the actor's own. It must return the bytes of the line it
    // was printed beside.
    let rest = line
        .strip_prefix("wirk ")
        .expect("guidance names the wirk binary first");
    let run = Command::new("sh")
        .args(["-c", &format!("'{}' {rest}", env!("CARGO_BIN_EXE_wirk"))])
        .env("WIRK_ESTATE_ROOT", &family.estate)
        .env("WIRK_WORK_ID", &family.covered_child)
        .env("WIRK_RUN_ID", &family.covered_child_run)
        .output()
        .expect("the printed command runs as printed");
    assert!(
        run.status.success(),
        "the guidance printed beside another Work's artifact must read that Work: {line}\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        run.stdout, b"parent claimed bytes\n",
        "the printed command must return the bytes of the evidence line it was printed beside"
    );

    // The actor's own Work: still the bare form, naming no triple at
    // all, because the environment it is printed into already carries
    // the current Run that door checks.
    let own = actor_status(&family.covered_child);
    if let Some(own_line) = own
        .lines()
        .find(|line| line.trim_start().starts_with("read with:"))
    {
        assert!(
            !own_line.contains("--work"),
            "an actor's guidance for its own Work must name no scope at all: {own_line}"
        );
    }

    stop_wirkd(&family.estate, daemon);
}

// ---- 7. the same historical-artifact promise, through a public retry ----

/// Ruling 0341 asks for the historical-artifact-after-retry promise on
/// an **actually reachable** transition rather than a journaled
/// `RunOpened`. This is that transition, driven entirely through the
/// public CLI: `wirk work retry` on a leaf of a *held container*
/// (`handle_retry`'s own `WorkState::Waiting` arm — "the leaf need not
/// itself have failed; its own Claim may have validated cleanly while
/// the container's own requirement went unmet"), which is the exact
/// real-use shape the directly-journaled fixture in
/// `historical_artifact_survives_a_real_retry_that_supersedes_its_producing_run`
/// stands in for.
///
/// The container in `reader_scope_family` is held on two required child
/// roles while its leaf's `Done` Claim over `a.md` is already
/// `Validated`, so it is that shape with nothing added for the test.
///
/// After the retry the producing Run is superseded on its own Waypoint
/// and its triple-form read is refused — while the Claim is untouched
/// and the named-Work door still returns the very same bytes.
#[test]
fn a_public_retry_supersedes_the_producing_run_and_the_named_door_still_reads_it() {
    let (family, daemon) = reader_scope_family();

    // Before: the producing Run reads its own validated Claim.
    let (code, out, err) = artifact_triple(
        &family.estate,
        &family.parent,
        &family.parent_run,
        &family.claim,
        "a.md",
    );
    assert_eq!(
        code,
        Some(0),
        "the producing Run reads its own Claim: {err}"
    );
    assert_eq!(out, b"parent claimed bytes\n");

    // The public transition: no journal append, no constructed event.
    let (code, log) = harness::retry_run_cli(&family.estate, &family.parent, &family.parent_run);
    assert_eq!(
        code,
        Some(0),
        "a held container's leaf must be publicly retryable: {log}"
    );

    // RED for the old execution-scoped door, exactly as ruling 0339
    // described and now reached without touching the journal directly.
    let (code, _out, err) = artifact_triple(
        &family.estate,
        &family.parent,
        &family.parent_run,
        &family.claim,
        "a.md",
    );
    assert_ne!(
        code,
        Some(0),
        "a superseded producing Run must not go on reading this Claim"
    );
    assert!(
        err.contains("superseded"),
        "the refusal must say why, got: {err}"
    );

    // GREEN: the historical validated artifact is still there, and the
    // named-Work door — which asks about no Run at all — still reads it.
    let (code, out, err) = artifact_admin(&family.estate, &family.parent, &family.claim, "a.md");
    assert_eq!(
        code,
        Some(0),
        "a historical validated artifact must survive a real retry: {err}"
    );
    assert_eq!(out, b"parent claimed bytes\n");

    stop_wirkd(&family.estate, daemon);
}
