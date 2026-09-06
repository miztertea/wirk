//! Real-daemon, real-Git proof of W-A: recursive nested stages, child
//! Work identity/authority, exact closure receipts, usable holds/
//! retries, crash recovery, and recursive cancellation
//! (`knowledge/work/p3-world-loop/W-A-BUILD.md`, BUILD-BRIEF.md §3,
//! BUILD-AMENDMENTS.md). Drives the real built `wirk` binary against a
//! real `wirkd` and real `git` repositories, the same discipline
//! `wirkd_process.rs`/`route_files.rs`/`identity_binding.rs` already
//! use — never a library call for anything the CLI exposes. Route
//! fixtures are embedded at compile time (`route_fixture.rs`'s own
//! `include_str!` convention): a binary compiled in one worktree and
//! reused after that worktree is gone must still carry its fixtures.
//!
//! Every Waypoint here is `Deterministic` with a real `git`-verified
//! source basis (`--source-basis git --repo-path <repo>`, resolved with
//! `git rev-parse` against a real throwaway repository): this file
//! proves the journal/closure/authority mechanism, not the executor —
//! artifacts are written directly by the test (as a real actor or
//! `wirk run-deterministic` eventually would) and claimed through the
//! real `wirk claim` path, which performs the real containment/
//! boundary/Git-diff checks against the real checkout either way
//! (`deterministic_run.rs`/`child_executor.rs` cover the executor
//! itself). Each submitted Work gets its own throwaway repository:
//! `cwd` for a Git-basis Deterministic World is the checkout path
//! itself (no per-Work worktree), so two Works sharing one repository
//! would see each other's artifacts as unrelated out-of-boundary
//! changes.

#[path = "support/nested_harness.rs"]
mod harness;
#[path = "support/route_fixture.rs"]
mod route_fixture;
#[path = "../src/wirkd/mod.rs"]
mod wirkd;

use std::fs;

use harness::*;

use wirk_core::{EventKind, RunId, WorkId};

// ---- 1. container closes with leaf receipts, advances -----------------

#[test]
fn container_closes_with_leaf_receipts_and_advances_to_next_waypoint() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    route_fixture::install_route_fixture(&estate, "wa_container");
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let repo = dir.path().join("repo");
    init_repo(&repo);
    let work =
        submit(&estate, "wa_container", &repo, &["demo:write"], None).expect("submit wa_container");
    assert_eq!(work.waypoint, "outer/leaf-a");

    write_file(&repo, "a.md", "a\n");
    claim_ok(&estate, &work.work_id, &work.run_id, "a.md=a.md");
    // The container's own outcome contract is not yet satisfied
    // (leaf-b hasn't claimed): the Work must not complete, and no
    // `StageClosed`/`StageHeld` exists for "outer" yet.
    assert_eq!(state_of(&pointer.socket, &work.work_id), "active");
    assert!(
        !journal_events(&estate, &work.work_id)
            .iter()
            .any(|e| matches!(
                &e.kind,
                EventKind::StageClosed { .. } | EventKind::StageHeld { .. }
            )),
        "outer must not evaluate before its last direct leaf"
    );

    let leaf_b_run = status(&pointer.socket, &work.work_id)["run_id"]
        .as_str()
        .expect("run_id")
        .to_string();
    write_file(&repo, "b.md", "b\n");
    claim_ok(&estate, &work.work_id, &leaf_b_run, "b.md=b.md");

    let events = journal_events(&estate, &work.work_id);
    let closed = events.iter().find_map(|e| match &e.kind {
        EventKind::StageClosed {
            waypoint, receipts, ..
        } if waypoint.0 == "outer" => Some(receipts.clone()),
        _ => None,
    });
    assert!(
        closed.is_some(),
        "outer must close once both leaves validate"
    );
    assert_eq!(
        closed.unwrap().len(),
        2,
        "one Leaf receipt per direct child"
    );

    let after = status(&pointer.socket, &work.work_id);
    assert_eq!(after["state"].as_str().unwrap(), "active");
    assert_eq!(after["current_waypoint"].as_str().unwrap(), "after");

    let after_run = after["run_id"].as_str().unwrap().to_string();
    write_file(&repo, "c.md", "c\n");
    claim_ok(&estate, &work.work_id, &after_run, "c.md=c.md");
    assert_eq!(state_of(&pointer.socket, &work.work_id), "completed");

    stop_wirkd(&estate, wirkd_child);
}

// ---- 2. missing required container output holds --------------------

#[test]
fn last_leaf_done_holds_container_when_required_artifact_missing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    route_fixture::install_route_fixture(&estate, "wa_container_missing_output");
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let repo = dir.path().join("repo");
    init_repo(&repo);
    let work = submit(
        &estate,
        "wa_container_missing_output",
        &repo,
        &["demo:write"],
        None,
    )
    .expect("submit");

    write_file(&repo, "a.md", "a\n");
    claim_ok(&estate, &work.work_id, &work.run_id, "a.md=a.md");

    let after = status(&pointer.socket, &work.work_id);
    assert_eq!(after["state"].as_str().unwrap(), "waiting");
    assert_eq!(after["current_waypoint"].as_str().unwrap(), "outer");
    let held = &after["held"];
    assert_eq!(held["waypoint"].as_str().unwrap(), "outer");
    assert_eq!(
        held["missing"].as_array().unwrap(),
        &vec![serde_json::json!("phantom.md")]
    );
    // No retry can fix a Route-authored name no leaf will ever produce
    // (a genuine misconfiguration, not a leaf-fixable defect) — the
    // hold is real and inspectable, not silently papered over.
    assert!(journal_events(&estate, &work.work_id).iter().any(
        |e| matches!(&e.kind, EventKind::StageHeld { waypoint, .. } if waypoint.0 == "outer")
    ));

    stop_wirkd(&estate, wirkd_child);
}

// ---- 3. child binding narrowing ----------------------------------------

#[test]
fn child_submit_wider_than_parent_is_refused_and_narrower_accepted() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    route_fixture::install_route_fixture(&estate, "wa_container_child_role");
    route_fixture::install_route_fixture(&estate, "wa_simple_leaf");
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let parent_repo = dir.path().join("parent-repo");
    init_repo(&parent_repo);
    let parent = submit(
        &estate,
        "wa_container_child_role",
        &parent_repo,
        &["demo:write", "extra:read"],
        None,
    )
    .expect("submit parent");
    write_file(&parent_repo, "a.md", "a\n");
    claim_ok(&estate, &parent.work_id, &parent.run_id, "a.md=a.md");
    assert_eq!(state_of(&pointer.socket, &parent.work_id), "waiting");

    let wider_repo = dir.path().join("wider-repo");
    init_repo(&wider_repo);
    let wider = submit(
        &estate,
        "wa_simple_leaf",
        &wider_repo,
        &["extra:write"],
        Some(ParentRef {
            work: &parent.work_id,
            waypoint: "outer",
            run: &parent.run_id,
            role: "helper",
            attempt: None,
        }),
    );
    let err = wider
        .expect_err("a child asking for Write where the parent granted only Read must be refused");
    assert!(
        err.contains("ChildExceedsParentBinding"),
        "expected ChildExceedsParentBinding, got: {err}"
    );

    let narrower_repo = dir.path().join("narrower-repo");
    init_repo(&narrower_repo);
    let narrower = submit(
        &estate,
        "wa_simple_leaf",
        &narrower_repo,
        &["extra:read"],
        Some(ParentRef {
            work: &parent.work_id,
            waypoint: "outer",
            run: &parent.run_id,
            role: "helper",
            attempt: None,
        }),
    )
    .expect("a child asking for no more than the parent granted is accepted");
    assert!(!narrower.work_id.is_empty());

    stop_wirkd(&estate, wirkd_child);
}

// ---- 4. superseded parent Run cannot be named --------------------------

#[test]
fn child_submit_naming_superseded_parent_run_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    route_fixture::install_route_fixture(&estate, "wa_container_child_role");
    route_fixture::install_route_fixture(&estate, "wa_simple_leaf");
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let parent_repo = dir.path().join("parent-repo");
    init_repo(&parent_repo);
    let parent = submit(
        &estate,
        "wa_container_child_role",
        &parent_repo,
        &["demo:write"],
        None,
    )
    .expect("submit parent");
    write_file(&parent_repo, "a.md", "a\n");
    claim_ok(&estate, &parent.work_id, &parent.run_id, "a.md=a.md");
    assert_eq!(state_of(&pointer.socket, &parent.work_id), "waiting");

    // W-A (§3.2 amendment): a held container's own leaf has a usable
    // retry path even though the Work is `Waiting`, not `NeedsInput`.
    let (code, out) = retry_cli(&estate, &parent.work_id);
    assert_eq!(code, Some(0), "retry on a Waiting Work: {out}");
    assert_eq!(state_of(&pointer.socket, &parent.work_id), "active");

    let child_repo = dir.path().join("child-repo");
    init_repo(&child_repo);
    let refused = submit(
        &estate,
        "wa_simple_leaf",
        &child_repo,
        &["demo:read"],
        Some(ParentRef {
            work: &parent.work_id,
            waypoint: "outer",
            run: &parent.run_id, // the now-superseded original Run
            role: "helper",
            attempt: None,
        }),
    );
    let err = refused.expect_err("a superseded parent Run cannot be named by a child submit");
    assert!(
        err.contains("ChildParentMismatch"),
        "expected ChildParentMismatch, got: {err}"
    );

    stop_wirkd(&estate, wirkd_child);
}

// ---- 5. held container closes when the child completes ----------------

#[test]
fn held_container_closes_when_child_completes_and_receipt_binds_current_run() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    route_fixture::install_route_fixture(&estate, "wa_container_child_role");
    route_fixture::install_route_fixture(&estate, "wa_simple_leaf");
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let parent_repo = dir.path().join("parent-repo");
    init_repo(&parent_repo);
    let parent = submit(
        &estate,
        "wa_container_child_role",
        &parent_repo,
        &["demo:write"],
        None,
    )
    .expect("submit parent");
    write_file(&parent_repo, "a.md", "a\n");
    claim_ok(&estate, &parent.work_id, &parent.run_id, "a.md=a.md");
    assert_eq!(state_of(&pointer.socket, &parent.work_id), "waiting");

    let child_repo = dir.path().join("child-repo");
    init_repo(&child_repo);
    let child = submit(
        &estate,
        "wa_simple_leaf",
        &child_repo,
        &["demo:write"],
        Some(ParentRef {
            work: &parent.work_id,
            waypoint: "outer",
            run: &parent.run_id,
            role: "helper",
            attempt: None,
        }),
    )
    .expect("submit child");
    assert!(
        journal_events(&estate, &parent.work_id)
            .iter()
            .any(|e| matches!(&e.kind, EventKind::ChildWorkSpawned{ role, child: c, waypoint, run, .. }
                if role == "helper" && c.0 == child.work_id && waypoint.0 == "outer" && run.0 == parent.run_id)),
        "the parent's journal must name this exact child for this exact role and Run"
    );

    write_file(&child_repo, "helper.md", "helper\n");
    claim_ok(
        &estate,
        &child.work_id,
        &child.run_id,
        "helper.md=helper.md",
    );
    assert_eq!(state_of(&pointer.socket, &child.work_id), "completed");

    // The child's own completing Claim re-evaluates the parent's held
    // container automatically (no extra verb): the required role now
    // has a valid `Child` receipt, "outer" closes, and — being the
    // Route's only top-level element — the parent Work completes.
    let after = status(&pointer.socket, &parent.work_id);
    assert_eq!(after["state"].as_str().unwrap(), "completed");
    let closed_receipts = journal_events(&estate, &parent.work_id)
        .into_iter()
        .find_map(|e| match e.kind {
            EventKind::StageClosed {
                waypoint, receipts, ..
            } if waypoint.0 == "outer" => Some(receipts),
            _ => None,
        })
        .expect("outer closed");
    assert!(
        closed_receipts.iter().any(|r| matches!(r,
            wirk_core::OutcomeReceipt::Child { role, child: c, parent_run, .. }
                if role == "helper" && c.0 == child.work_id && parent_run.0 == parent.run_id
        )),
        "the closing receipt must bind the exact child and the exact parent Run: {closed_receipts:?}"
    );

    stop_wirkd(&estate, wirkd_child);
}

// ---- 6. a retried parent leaf cannot reuse an earlier child receipt ---

#[test]
fn retried_parent_leaf_cannot_reuse_earlier_attempts_child_receipt() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    route_fixture::install_route_fixture(&estate, "wa_container_child_role");
    route_fixture::install_route_fixture(&estate, "wa_simple_leaf");
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let parent_repo = dir.path().join("parent-repo");
    init_repo(&parent_repo);
    let parent = submit(
        &estate,
        "wa_container_child_role",
        &parent_repo,
        &["demo:write"],
        None,
    )
    .expect("submit parent");
    let first_run = parent.run_id.clone();
    write_file(&parent_repo, "a.md", "a\n");
    claim_ok(&estate, &parent.work_id, &first_run, "a.md=a.md");
    assert_eq!(state_of(&pointer.socket, &parent.work_id), "waiting");

    let child_repo = dir.path().join("child-repo");
    init_repo(&child_repo);
    let child = submit(
        &estate,
        "wa_simple_leaf",
        &child_repo,
        &["demo:write"],
        Some(ParentRef {
            work: &parent.work_id,
            waypoint: "outer",
            run: &first_run,
            role: "helper",
            attempt: None,
        }),
    )
    .expect("submit child against the first attempt");

    // Retry the parent leaf before the child completes: its Run is
    // superseded, and re-claiming re-establishes the leaf's own
    // declared output on the fresh attempt.
    let (code, out) = retry_cli(&estate, &parent.work_id);
    assert_eq!(code, Some(0), "retry: {out}");
    let second_run = status(&pointer.socket, &parent.work_id)["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(second_run, first_run);
    write_file(&parent_repo, "a.md", "a again\n");
    claim_ok(&estate, &parent.work_id, &second_run, "a.md=a.md");
    assert_eq!(state_of(&pointer.socket, &parent.work_id), "waiting");

    // Now complete the *first* attempt's child.
    write_file(&child_repo, "helper.md", "helper\n");
    claim_ok(
        &estate,
        &child.work_id,
        &child.run_id,
        "helper.md=helper.md",
    );
    assert_eq!(state_of(&pointer.socket, &child.work_id), "completed");

    // The parent must stay held: the completed child's receipt names
    // `first_run`, which is no longer the current Run of its leaf.
    let after = status(&pointer.socket, &parent.work_id);
    assert_eq!(
        after["state"].as_str().unwrap(),
        "waiting",
        "a superseded attempt's child receipt must never close the container"
    );
    assert!(
        after["held"]["missing"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str().unwrap().contains("helper")),
        "{after}"
    );

    stop_wirkd(&estate, wirkd_child);
}

// ---- 7. a dangling spawn with no child journal is missing, not credited

#[test]
fn dangling_spawn_without_child_journal_is_missing_not_credited() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    route_fixture::install_route_fixture(&estate, "wa_container_child_role");
    let (wirkd_child, _pointer) = start_wirkd(&estate);

    let repo = dir.path().join("repo");
    init_repo(&repo);
    let parent = submit(
        &estate,
        "wa_container_child_role",
        &repo,
        &["demo:write"],
        None,
    )
    .expect("submit parent");

    // Simulate a crash between minting a spawn record and creating the
    // child's own journal (§3.3: "a crash after the parent line leaves
    // a spawn naming a child with no journal") — raw-appended while
    // wirkd is stopped, the same discipline `identity_binding.rs` uses,
    // never while a live wirkd holds this Work's journal open.
    stop_wirkd(&estate, wirkd_child);
    raw_append(
        &estate,
        &parent.work_id,
        Some(&parent.run_id),
        EventKind::ChildWorkSpawned {
            role: "helper".to_string(),
            attempt: 1,
            child: WorkId("work-dangling-no-journal".to_string()),
            waypoint: wirk_core::WaypointId("outer".to_string()),
            run: RunId(parent.run_id.clone()),
        },
    );
    let (wirkd_child, pointer) = start_wirkd(&estate);

    write_file(&repo, "a.md", "a\n");
    claim_ok(&estate, &parent.work_id, &parent.run_id, "a.md=a.md");

    let after = status(&pointer.socket, &parent.work_id);
    assert_eq!(
        after["state"].as_str().unwrap(),
        "waiting",
        "a spawn naming a child with no journal must never be credited"
    );
    assert!(
        after["held"]["missing"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str().unwrap().contains("helper")),
        "{after}"
    );

    stop_wirkd(&estate, wirkd_child);
}

// ---- 8. restart reconstructs a crash between the two journals ---------

#[test]
fn restart_reevaluates_held_container_after_crash_between_journals() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    route_fixture::install_route_fixture(&estate, "wa_container_child_role");
    route_fixture::install_route_fixture(&estate, "wa_simple_leaf");
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let parent_repo = dir.path().join("parent-repo");
    init_repo(&parent_repo);
    let parent = submit(
        &estate,
        "wa_container_child_role",
        &parent_repo,
        &["demo:write"],
        None,
    )
    .expect("submit parent");
    write_file(&parent_repo, "a.md", "a\n");
    claim_ok(&estate, &parent.work_id, &parent.run_id, "a.md=a.md");
    assert_eq!(state_of(&pointer.socket, &parent.work_id), "waiting");

    let child_repo = dir.path().join("child-repo");
    init_repo(&child_repo);
    let child = submit(
        &estate,
        "wa_simple_leaf",
        &child_repo,
        &["demo:read"],
        Some(ParentRef {
            work: &parent.work_id,
            waypoint: "outer",
            run: &parent.run_id,
            role: "helper",
            attempt: None,
        }),
    )
    .expect("submit child");

    // Stop wirkd, then complete the child entirely by raw-appending
    // its own final Claim events directly to its journal — this is
    // exactly "a crash between the child's own completing Claim and
    // the parent's `StageClosed`": the live `handle_claim` path that
    // would normally re-evaluate the parent never runs at all.
    stop_wirkd(&estate, wirkd_child);
    let child_run = child.run_id.clone();
    raw_append(
        &estate,
        &child.work_id,
        Some(&child_run),
        EventKind::ClaimFiled {
            claim: wirk_core::ClaimId("claim-crash".to_string()),
        },
    );
    raw_append(
        &estate,
        &child.work_id,
        Some(&child_run),
        EventKind::ClaimRecorded {
            artifacts: Vec::new(),
            claim: wirk_core::ClaimId("claim-crash".to_string()),
            claim_kind: wirk_core::ClaimKind::Done,
            verdict: wirk_core::ClaimVerdict::Validated,
        },
    );
    let child_events = journal_events(&estate, &child.work_id);
    assert!(
        matches!(
            wirk_core::fold(&child_events).state,
            wirk_core::WorkState::Completed
        ),
        "the raw-appended events must fold the child Completed before restart"
    );
    assert!(
        matches!(
            wirk_core::fold(&journal_events(&estate, &parent.work_id)).state,
            wirk_core::WorkState::Waiting
        ),
        "the parent must still read Waiting before restart — nothing re-evaluated it"
    );

    // Restart: the startup sweep re-evaluates every `Waiting` Work.
    let (wirkd_child, pointer) = start_wirkd(&estate);
    let after = status(&pointer.socket, &parent.work_id);
    assert_eq!(
        after["state"].as_str().unwrap(),
        "completed",
        "the startup sweep must repair the crash and close+complete the parent"
    );

    stop_wirkd(&estate, wirkd_child);
}

// ---- 9. cancel refuses an open child without cascade, cascades with ---
//         attribution

#[test]
fn cancel_refuses_open_child_without_cascade_and_cascades_with_attribution() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    route_fixture::install_route_fixture(&estate, "wa_container_child_role");
    route_fixture::install_route_fixture(&estate, "wa_simple_leaf");
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let parent_repo = dir.path().join("parent-repo");
    init_repo(&parent_repo);
    let parent = submit(
        &estate,
        "wa_container_child_role",
        &parent_repo,
        &["demo:write"],
        None,
    )
    .expect("submit parent");
    write_file(&parent_repo, "a.md", "a\n");
    claim_ok(&estate, &parent.work_id, &parent.run_id, "a.md=a.md");

    let child_repo = dir.path().join("child-repo");
    init_repo(&child_repo);
    let child = submit(
        &estate,
        "wa_simple_leaf",
        &child_repo,
        &["demo:read"],
        Some(ParentRef {
            work: &parent.work_id,
            waypoint: "outer",
            run: &parent.run_id,
            role: "helper",
            attempt: None,
        }),
    )
    .expect("submit child");
    // The child is left open (never claimed).

    let (code, out) = cancel_cli(&estate, &parent.work_id, false);
    assert_ne!(code, Some(0), "cancel without --cascade must refuse: {out}");
    assert!(out.contains("OpenChild"), "{out}");
    assert_eq!(state_of(&pointer.socket, &parent.work_id), "waiting");
    assert_eq!(state_of(&pointer.socket, &child.work_id), "active");

    let (code, out) = cancel_cli(&estate, &parent.work_id, true);
    assert_eq!(code, Some(0), "cascade cancel: {out}");
    assert_eq!(state_of(&pointer.socket, &parent.work_id), "canceled");
    assert_eq!(state_of(&pointer.socket, &child.work_id), "canceled");

    let child_canceled = journal_events(&estate, &child.work_id)
        .into_iter()
        .find_map(|e| match e.kind {
            EventKind::WorkCanceled { caused_by, .. } => Some(caused_by),
            _ => None,
        })
        .flatten();
    assert_eq!(
        child_canceled.map(|w| w.0),
        Some(parent.work_id.clone()),
        "the cascade step must attribute the child's cancellation to the parent"
    );
    let parent_canceled = journal_events(&estate, &parent.work_id)
        .into_iter()
        .find_map(|e| match e.kind {
            EventKind::WorkCanceled { caused_by, .. } => Some(caused_by),
            _ => None,
        })
        .flatten();
    assert!(
        parent_canceled.is_none(),
        "the explicitly named target of the verb itself carries no caused_by"
    );

    stop_wirkd(&estate, wirkd_child);
}

// ---- 10. a retried child leaf's own history does not affect the receipt

#[test]
fn retried_child_leaf_mints_fresh_world_and_receipt_is_unaffected() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    route_fixture::install_route_fixture(&estate, "wa_container_child_role");
    route_fixture::install_route_fixture(&estate, "wa_simple_leaf");
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let parent_repo = dir.path().join("parent-repo");
    init_repo(&parent_repo);
    let parent = submit(
        &estate,
        "wa_container_child_role",
        &parent_repo,
        &["demo:write"],
        None,
    )
    .expect("submit parent");
    write_file(&parent_repo, "a.md", "a\n");
    claim_ok(&estate, &parent.work_id, &parent.run_id, "a.md=a.md");

    let child_repo = dir.path().join("child-repo");
    init_repo(&child_repo);
    let child = submit(
        &estate,
        "wa_simple_leaf",
        &child_repo,
        &["demo:write"],
        Some(ParentRef {
            work: &parent.work_id,
            waypoint: "outer",
            run: &parent.run_id,
            role: "helper",
            attempt: None,
        }),
    )
    .expect("submit child");

    // The child's own first attempt fails (a local executor failure, in
    // real use); the operator retries it through the ordinary
    // `NeedsInput` path, unrelated to any container mechanics.
    fail_via_socket(&pointer.socket, &estate, &child.work_id, &child.run_id);
    assert_eq!(state_of(&pointer.socket, &child.work_id), "needs_input");
    let (code, out) = retry_cli(&estate, &child.work_id);
    assert_eq!(code, Some(0), "retry the failed child leaf: {out}");
    let second_run = status(&pointer.socket, &child.work_id)["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(second_run, child.run_id);

    write_file(&child_repo, "helper.md", "helper\n");
    claim_ok(&estate, &child.work_id, &second_run, "helper.md=helper.md");
    assert_eq!(state_of(&pointer.socket, &child.work_id), "completed");

    let after = status(&pointer.socket, &parent.work_id);
    assert_eq!(
        after["state"].as_str().unwrap(),
        "completed",
        "the parent's receipt reads the child's current, completed state \
         regardless of the child's own internal retry history"
    );

    stop_wirkd(&estate, wirkd_child);
}

// ---- 11. a grandchild container cascades closure to its outer container

#[test]
fn grandchild_container_closes_and_cascades_closure_to_outer_container() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    route_fixture::install_route_fixture(&estate, "wa_grandchild");
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let repo = dir.path().join("repo");
    init_repo(&repo);
    let work = submit(&estate, "wa_grandchild", &repo, &["demo:write"], None)
        .expect("submit wa_grandchild");
    assert_eq!(work.waypoint, "outer/lead");

    write_file(&repo, "a.md", "a\n");
    claim_ok(&estate, &work.work_id, &work.run_id, "a.md=a.md");
    assert_eq!(state_of(&pointer.socket, &work.work_id), "active");

    let leaf_run = status(&pointer.socket, &work.work_id)["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        status(&pointer.socket, &work.work_id)["current_waypoint"]
            .as_str()
            .unwrap(),
        "outer/inner/leaf"
    );
    write_file(&repo, "b.md", "b\n");
    claim_ok(&estate, &work.work_id, &leaf_run, "b.md=b.md");

    let events = journal_events(&estate, &work.work_id);
    assert!(
        events.iter().any(
            |e| matches!(&e.kind, EventKind::StageClosed { waypoint, .. } if waypoint.0 == "outer/inner")
        ),
        "the inner container must close first"
    );
    let outer_receipts = events.into_iter().find_map(|e| match e.kind {
        EventKind::StageClosed {
            waypoint, receipts, ..
        } if waypoint.0 == "outer" => Some(receipts),
        _ => None,
    });
    let outer_receipts = outer_receipts.expect("outer must cascade-close in the same call");
    assert!(
        outer_receipts.iter().any(|r| matches!(r,
            wirk_core::OutcomeReceipt::Container { waypoint, .. } if waypoint.0 == "outer/inner"
        )),
        "outer's own receipt must aggregate the inner container's receipts, not re-derive them: {outer_receipts:?}"
    );

    assert_eq!(state_of(&pointer.socket, &work.work_id), "completed");

    stop_wirkd(&estate, wirkd_child);
}

// ---- 12. a child Work can itself spawn a child -------------------------

#[test]
fn child_work_can_itself_spawn_a_child_and_the_full_chain_completes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    route_fixture::install_route_fixture(&estate, "wa_container_child_role");
    route_fixture::install_route_fixture(&estate, "wa_simple_leaf");
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let g_repo = dir.path().join("g-repo");
    init_repo(&g_repo);
    let grandparent = submit(
        &estate,
        "wa_container_child_role",
        &g_repo,
        &["demo:write"],
        None,
    )
    .expect("submit grandparent G");
    write_file(&g_repo, "a.md", "a\n");
    claim_ok(
        &estate,
        &grandparent.work_id,
        &grandparent.run_id,
        "a.md=a.md",
    );
    assert_eq!(state_of(&pointer.socket, &grandparent.work_id), "waiting");

    let p_repo = dir.path().join("p-repo");
    init_repo(&p_repo);
    let parent = submit(
        &estate,
        "wa_container_child_role", // P's own route also needs a child for its "helper" role
        &p_repo,
        &["demo:write"],
        Some(ParentRef {
            work: &grandparent.work_id,
            waypoint: "outer",
            run: &grandparent.run_id,
            role: "helper",
            attempt: None,
        }),
    )
    .expect("submit middle child P, itself asking G for a child role");
    write_file(&p_repo, "a.md", "a\n");
    claim_ok(&estate, &parent.work_id, &parent.run_id, "a.md=a.md");
    assert_eq!(
        state_of(&pointer.socket, &parent.work_id),
        "waiting",
        "P needs its own grandchild before it can close its own container"
    );
    // G is unaffected so far: P has not completed yet.
    assert_eq!(state_of(&pointer.socket, &grandparent.work_id), "waiting");

    let gc_repo = dir.path().join("gc-repo");
    init_repo(&gc_repo);
    let grandchild = submit(
        &estate,
        "wa_simple_leaf",
        &gc_repo,
        &["demo:write"],
        Some(ParentRef {
            work: &parent.work_id,
            waypoint: "outer",
            run: &parent.run_id,
            role: "helper",
            attempt: None,
        }),
    )
    .expect("submit grandchild GC under P");

    write_file(&gc_repo, "helper.md", "helper\n");
    claim_ok(
        &estate,
        &grandchild.work_id,
        &grandchild.run_id,
        "helper.md=helper.md",
    );

    // One Claim on the great-grandchild leaf cascades: GC completes,
    // closes P's own container (completing P), which in turn re-
    // evaluates and closes G's container (completing G) — recursive
    // stage activation across three real, distinct Works, no bare
    // WorkState inference anywhere in the chain.
    assert_eq!(state_of(&pointer.socket, &grandchild.work_id), "completed");
    assert_eq!(state_of(&pointer.socket, &parent.work_id), "completed");
    assert_eq!(state_of(&pointer.socket, &grandparent.work_id), "completed");

    let g_receipt = journal_events(&estate, &grandparent.work_id)
        .into_iter()
        .find_map(|e| match e.kind {
            EventKind::StageClosed {
                waypoint, receipts, ..
            } if waypoint.0 == "outer" => Some(receipts),
            _ => None,
        })
        .expect("G's outer closed");
    assert!(
        g_receipt.iter().any(|r| matches!(r,
            wirk_core::OutcomeReceipt::Child { child, .. } if child.0 == parent.work_id
        )),
        "G's own receipt names P directly, not GC — each level's authority is its own"
    );

    stop_wirkd(&estate, wirkd_child);
}
