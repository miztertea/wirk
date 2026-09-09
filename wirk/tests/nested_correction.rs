//! Real-daemon, real-Git proof of the W-A *correction* contract
//! (`knowledge/work/p3-world-loop/W-A-CORRECT.md`): monotonic container
//! activation identity that participates in fold/closure/child binding,
//! reopen-and-invalidate at recursive depth, content-identified artifact
//! evidence with an explicit unavailable answer after change, two-sided
//! child binding, and mechanism independence (Actor + Deterministic
//! leaves in one container).
//!
//! Same discipline as `nested_work.rs`, whose harness this shares
//! (`support/nested_harness.rs`): the real built `wirk` binary, a real
//! `wirkd` over its Unix socket, real `git` repositories, embedded Route
//! fixtures. No model is invoked anywhere in this file — the one Actor
//! leaf is materialized with real `git worktree add` through the
//! model-free `materialize_actor` helper.

#[path = "support/nested_harness.rs"]
mod harness;
#[path = "support/route_fixture.rs"]
mod route_fixture;
#[path = "../src/wirkd/mod.rs"]
mod wirkd;

use std::fs;
use std::path::Path;

use harness::*;

use wirk_core::{Event, EventKind, OutcomeReceipt, WorkId};

// ---- journal readers -------------------------------------------------

/// Every `ContainerActivated{waypoint, attempt}` in journal order.
fn activations(events: &[Event], waypoint: &str) -> Vec<u32> {
    events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::ContainerActivated {
                waypoint: w,
                attempt,
            } if w.0 == waypoint => Some(*attempt),
            _ => None,
        })
        .collect()
}

/// The receipts of the last `StageClosed` naming `waypoint`, if any.
fn last_closed(events: &[Event], waypoint: &str) -> Option<Vec<OutcomeReceipt>> {
    events.iter().rev().find_map(|e| match &e.kind {
        EventKind::StageClosed {
            waypoint: w,
            receipts,
            ..
        } if w.0 == waypoint => Some(receipts.clone()),
        _ => None,
    })
}

fn stage_events(events: &[Event], waypoint: &str) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::StageClosed { waypoint: w, .. } if w.0 == waypoint => {
                Some("closed".to_string())
            }
            EventKind::StageHeld {
                waypoint: w,
                missing,
                ..
            } if w.0 == waypoint => Some(format!("held:{}", missing.join("+"))),
            _ => None,
        })
        .collect()
}

/// The `Leaf` receipt for `waypoint` anywhere in a receipt tree.
fn find_leaf(receipts: &[OutcomeReceipt], waypoint: &str) -> Option<OutcomeReceipt> {
    for receipt in receipts {
        match receipt {
            OutcomeReceipt::Leaf { waypoint: w, .. } if w.0 == waypoint => {
                return Some(receipt.clone());
            }
            OutcomeReceipt::Container { receipts, .. } => {
                if let Some(found) = find_leaf(receipts, waypoint) {
                    return Some(found);
                }
            }
            _ => {}
        }
    }
    None
}

fn leaf_run(receipts: &[OutcomeReceipt], waypoint: &str) -> String {
    match find_leaf(receipts, waypoint) {
        Some(OutcomeReceipt::Leaf { run, .. }) => run.0,
        other => panic!("no Leaf receipt for {waypoint}: {other:?}"),
    }
}

fn open_runs(result: &serde_json::Value) -> Vec<String> {
    result["runs"]
        .as_array()
        .expect("runs array")
        .iter()
        .filter(|entry| entry["run"]["state"] == "Open")
        .map(|entry| entry["run"]["id"].as_str().unwrap_or_default().to_string())
        .collect()
}

fn current_run(socket: &Path, work_id: &str) -> String {
    status(socket, work_id)["run_id"]
        .as_str()
        .expect("status names a run_id")
        .to_string()
}

// ---- 1. F1/F2: reopening a closed nested stage --------------------------

/// The reviewer's executed false-completion, corrected: `outer[
/// inner[leaf], lead ]` with a required child role. After `inner` has
/// closed and `outer` is held on the child, retrying the leaf *inside
/// the closed inner container* must reopen `inner` with a new journaled
/// activation, and the child's later completion must NOT close `outer`
/// on the superseded attempt's receipt. The Work completes only after
/// the reopened stage is actually re-executed, and its receipts name the
/// fresh Runs.
#[test]
fn retrying_a_leaf_inside_a_closed_nested_container_reopens_it_and_blocks_false_completion() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    route_fixture::install_route_fixture(&estate, "wa_reopen");
    route_fixture::install_route_fixture(&estate, "wa_simple_leaf");
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let repo = dir.path().join("repo");
    init_repo(&repo);
    let work = submit(
        &estate,
        "wa_reopen",
        &repo,
        &["demo:write", "child-output:write"],
        None,
    )
    .expect("submit wa_reopen");
    assert_eq!(work.waypoint, "outer/inner/leaf");

    // inner closes on its own leaf, outer advances to its lead leaf.
    write_file(&estate.join("worktrees").join(&work.work_id), "b.md", "b\n");
    claim_ok(&estate, &work.work_id, &work.run_id, "b.md=b.md");
    let lead_run = current_run(&pointer.socket, &work.work_id);
    write_file(&estate.join("worktrees").join(&work.work_id), "a.md", "a\n");
    claim_ok(&estate, &work.work_id, &lead_run, "a.md=a.md");
    assert_eq!(state_of(&pointer.socket, &work.work_id), "waiting");

    // Reopen: retry the leaf inside the ALREADY-CLOSED inner container.
    let (code, out) = retry_run_cli(&estate, &work.work_id, &work.run_id);
    assert_eq!(code, Some(0), "reopen retry refused: {out}");
    let events = journal_events(&estate, &work.work_id);
    assert_eq!(
        activations(&events, "outer/inner"),
        vec![1, 2],
        "reopening a closed nested container must mint a second journaled activation"
    );
    assert_eq!(
        activations(&events, "outer"),
        vec![1],
        "a held ancestor is not itself reopened by a descendant's retry"
    );

    // The required child completes while the reopened leaf is still open.
    let child_repo = dir.path().join("child-repo");
    init_repo(&child_repo);
    let child = submit(
        &estate,
        "wa_simple_leaf",
        &child_repo,
        &["child-output:write"],
        Some(ParentRef {
            work: &work.work_id,
            waypoint: "outer",
            run: &lead_run,
            role: "helper",
            attempt: None,
        }),
    )
    .expect("child submit");
    write_file(
        &estate.join("worktrees").join(&child.work_id),
        "helper.md",
        "helper\n",
    );
    claim_ok(
        &estate,
        &child.work_id,
        &child.run_id,
        "helper.md=helper.md",
    );

    // DECISIVE: no false completion on the superseded attempt.
    let result = status(&pointer.socket, &work.work_id);
    assert_eq!(
        result["state"].as_str(),
        Some("waiting"),
        "outer must not close while its reopened inner stage is unexecuted: {result}"
    );
    let events = journal_events(&estate, &work.work_id);
    assert!(
        last_closed(&events, "outer").is_none(),
        "outer must have no StageClosed yet: {:?}",
        stage_events(&events, "outer")
    );
    assert!(
        stage_events(&events, "outer")
            .last()
            .is_some_and(|last| last.starts_with("held:")),
        "outer's latest outcome must be a hold naming the unexecuted stage: {:?}",
        stage_events(&events, "outer")
    );

    // Re-execute the reopened stage: inner recloses, lead re-runs, outer closes.
    let reopened_leaf_run = current_run(&pointer.socket, &work.work_id);
    assert_ne!(reopened_leaf_run, work.run_id);
    write_file(
        &estate.join("worktrees").join(&work.work_id),
        "b.md",
        "b again\n",
    );
    claim_ok(&estate, &work.work_id, &reopened_leaf_run, "b.md=b.md");
    let second_lead_run = current_run(&pointer.socket, &work.work_id);
    write_file(
        &estate.join("worktrees").join(&work.work_id),
        "a.md",
        "a again\n",
    );
    claim_ok(&estate, &work.work_id, &second_lead_run, "a.md=a.md");

    // The re-execution superseded the Run that requested the child, so
    // the first child's receipt is superseded with it: the container
    // holds for a child outcome bound to the *current* requesting Run,
    // which is the same "no superseded receipt" rule one level up.
    assert_eq!(state_of(&pointer.socket, &work.work_id), "waiting");
    let child_repo2 = dir.path().join("child-repo-2");
    init_repo(&child_repo2);
    let child2 = submit(
        &estate,
        "wa_simple_leaf",
        &child_repo2,
        &["child-output:write"],
        Some(ParentRef {
            work: &work.work_id,
            waypoint: "outer",
            run: &second_lead_run,
            role: "helper",
            attempt: None,
        }),
    )
    .expect("second child submit");
    write_file(
        &estate.join("worktrees").join(&child2.work_id),
        "helper.md",
        "helper\n",
    );
    claim_ok(
        &estate,
        &child2.work_id,
        &child2.run_id,
        "helper.md=helper.md",
    );

    let result = status(&pointer.socket, &work.work_id);
    assert_eq!(result["state"].as_str(), Some("completed"), "{result}");
    assert!(
        open_runs(&result).is_empty(),
        "no Run may be stranded Open behind a completed Work: {result}"
    );
    let events = journal_events(&estate, &work.work_id);
    let receipts = last_closed(&events, "outer").expect("outer closes after re-execution");
    assert_eq!(
        leaf_run(&receipts, "outer/inner/leaf"),
        reopened_leaf_run,
        "the closure must credit the reopened attempt's Run, never the superseded one"
    );
    assert_eq!(leaf_run(&receipts, "outer/lead"), second_lead_run);

    stop_wirkd(&estate, wirkd_child);
}

// ---- 2. F2: ancestor reopening at grandchild depth ---------------------

/// `top[ outer[ a[leaf], lead ], tail ]`: `a` and `outer` both close
/// before `top` holds on its child role. Retrying the leaf then has to
/// reopen *both* closed containers (descendant `a` and its ancestor
/// `outer`) while leaving the merely-held `top` on its own activation,
/// and `top` may not close until every reopened level is re-executed.
#[test]
fn reopen_invalidates_ancestor_container_closure_at_grandchild_depth() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    route_fixture::install_route_fixture(&estate, "wa_reopen_deep");
    route_fixture::install_route_fixture(&estate, "wa_simple_leaf");
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let repo = dir.path().join("repo");
    init_repo(&repo);
    let work = submit(
        &estate,
        "wa_reopen_deep",
        &repo,
        &["demo:write", "child-output:write"],
        None,
    )
    .expect("submit deep");
    assert_eq!(work.waypoint, "top/outer/a/leaf");

    write_file(&estate.join("worktrees").join(&work.work_id), "b.md", "b\n");
    claim_ok(&estate, &work.work_id, &work.run_id, "b.md=b.md");
    let lead_run = current_run(&pointer.socket, &work.work_id);
    write_file(&estate.join("worktrees").join(&work.work_id), "a.md", "a\n");
    claim_ok(&estate, &work.work_id, &lead_run, "a.md=a.md");
    let tail_run = current_run(&pointer.socket, &work.work_id);
    write_file(&estate.join("worktrees").join(&work.work_id), "t.md", "t\n");
    claim_ok(&estate, &work.work_id, &tail_run, "t.md=t.md");
    assert_eq!(state_of(&pointer.socket, &work.work_id), "waiting");

    let events = journal_events(&estate, &work.work_id);
    assert!(last_closed(&events, "top/outer/a").is_some());
    assert!(last_closed(&events, "top/outer").is_some());

    let (code, out) = retry_run_cli(&estate, &work.work_id, &work.run_id);
    assert_eq!(code, Some(0), "reopen retry refused: {out}");
    let events = journal_events(&estate, &work.work_id);
    assert_eq!(activations(&events, "top/outer/a"), vec![1, 2]);
    assert_eq!(
        activations(&events, "top/outer"),
        vec![1, 2],
        "a closed ancestor container must itself be reopened"
    );
    assert_eq!(
        activations(&events, "top"),
        vec![1],
        "a held ancestor keeps its own activation"
    );

    // The child completes; top still cannot close on the invalidated tree.
    let child_repo = dir.path().join("child-repo");
    init_repo(&child_repo);
    let child = submit(
        &estate,
        "wa_simple_leaf",
        &child_repo,
        &["child-output:write"],
        Some(ParentRef {
            work: &work.work_id,
            waypoint: "top",
            run: &tail_run,
            role: "helper",
            attempt: None,
        }),
    )
    .expect("child submit");
    write_file(
        &estate.join("worktrees").join(&child.work_id),
        "helper.md",
        "helper\n",
    );
    claim_ok(
        &estate,
        &child.work_id,
        &child.run_id,
        "helper.md=helper.md",
    );
    assert_eq!(
        state_of(&pointer.socket, &work.work_id),
        "waiting",
        "top must stay held while a reopened descendant is unexecuted"
    );

    // Re-execute every reopened level.
    let leaf2 = current_run(&pointer.socket, &work.work_id);
    write_file(
        &estate.join("worktrees").join(&work.work_id),
        "b.md",
        "b2\n",
    );
    claim_ok(&estate, &work.work_id, &leaf2, "b.md=b.md");
    let lead2 = current_run(&pointer.socket, &work.work_id);
    write_file(
        &estate.join("worktrees").join(&work.work_id),
        "a.md",
        "a2\n",
    );
    claim_ok(&estate, &work.work_id, &lead2, "a.md=a.md");
    let tail2 = current_run(&pointer.socket, &work.work_id);
    write_file(
        &estate.join("worktrees").join(&work.work_id),
        "t.md",
        "t2\n",
    );
    claim_ok(&estate, &work.work_id, &tail2, "t.md=t.md");

    // As in the one-level case: the Run that requested the child was
    // itself superseded by the re-execution, so a child outcome bound
    // to the current requesting Run is required before top can close.
    assert_eq!(state_of(&pointer.socket, &work.work_id), "waiting");
    let child_repo2 = dir.path().join("child-repo-2");
    init_repo(&child_repo2);
    let child2 = submit(
        &estate,
        "wa_simple_leaf",
        &child_repo2,
        &["child-output:write"],
        Some(ParentRef {
            work: &work.work_id,
            waypoint: "top",
            run: &tail2,
            role: "helper",
            attempt: None,
        }),
    )
    .expect("second child submit");
    write_file(
        &estate.join("worktrees").join(&child2.work_id),
        "helper.md",
        "helper\n",
    );
    claim_ok(
        &estate,
        &child2.work_id,
        &child2.run_id,
        "helper.md=helper.md",
    );

    let result = status(&pointer.socket, &work.work_id);
    assert_eq!(result["state"].as_str(), Some("completed"), "{result}");
    assert!(open_runs(&result).is_empty(), "{result}");
    let events = journal_events(&estate, &work.work_id);
    let receipts = last_closed(&events, "top").expect("top closes");
    assert_eq!(leaf_run(&receipts, "top/outer/a/leaf"), leaf2);
    assert_eq!(leaf_run(&receipts, "top/outer/lead"), lead2);
    assert_eq!(leaf_run(&receipts, "top/tail"), tail2);

    stop_wirkd(&estate, wirkd_child);
}

// ---- 3. F1/F2: the reopened identity survives a restart ----------------

#[test]
fn a_reopened_activation_survives_a_daemon_restart_without_fabricating_closure() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    route_fixture::install_route_fixture(&estate, "wa_reopen");
    route_fixture::install_route_fixture(&estate, "wa_simple_leaf");
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let repo = dir.path().join("repo");
    init_repo(&repo);
    let work = submit(
        &estate,
        "wa_reopen",
        &repo,
        &["demo:write", "child-output:write"],
        None,
    )
    .expect("submit wa_reopen");
    write_file(&estate.join("worktrees").join(&work.work_id), "b.md", "b\n");
    claim_ok(&estate, &work.work_id, &work.run_id, "b.md=b.md");
    let lead_run = current_run(&pointer.socket, &work.work_id);
    write_file(&estate.join("worktrees").join(&work.work_id), "a.md", "a\n");
    claim_ok(&estate, &work.work_id, &lead_run, "a.md=a.md");

    let (code, out) = retry_run_cli(&estate, &work.work_id, &work.run_id);
    assert_eq!(code, Some(0), "reopen retry refused: {out}");
    let reopened_run = current_run(&pointer.socket, &work.work_id);
    stop_wirkd(&estate, wirkd_child);

    // Restart: the startup sweep must not self-heal a reopened stage.
    let (wirkd_child, pointer) = start_wirkd(&estate);
    let events = journal_events(&estate, &work.work_id);
    assert_eq!(
        activations(&events, "outer/inner"),
        vec![1, 2],
        "the reopened activation is replayed exactly, not re-minted"
    );
    assert!(
        last_closed(&events, "outer").is_none(),
        "the startup sweep must not close outer on superseded evidence: {:?}",
        stage_events(&events, "outer")
    );
    assert_eq!(
        current_run(&pointer.socket, &work.work_id),
        reopened_run,
        "the exact reopened Run identity survives the restart"
    );

    // And the correction path still finishes after the restart.
    write_file(
        &estate.join("worktrees").join(&work.work_id),
        "b.md",
        "b after restart\n",
    );
    claim_ok(&estate, &work.work_id, &reopened_run, "b.md=b.md");
    let lead2 = current_run(&pointer.socket, &work.work_id);
    write_file(
        &estate.join("worktrees").join(&work.work_id),
        "a.md",
        "a after restart\n",
    );
    claim_ok(&estate, &work.work_id, &lead2, "a.md=a.md");
    assert_eq!(state_of(&pointer.socket, &work.work_id), "waiting");

    let child_repo = dir.path().join("child-repo");
    init_repo(&child_repo);
    let child = submit(
        &estate,
        "wa_simple_leaf",
        &child_repo,
        &["child-output:write"],
        Some(ParentRef {
            work: &work.work_id,
            waypoint: "outer",
            run: &lead2,
            role: "helper",
            attempt: None,
        }),
    )
    .expect("child submit");
    write_file(
        &estate.join("worktrees").join(&child.work_id),
        "helper.md",
        "helper\n",
    );
    claim_ok(
        &estate,
        &child.work_id,
        &child.run_id,
        "helper.md=helper.md",
    );
    assert_eq!(state_of(&pointer.socket, &work.work_id), "completed");

    stop_wirkd(&estate, wirkd_child);
}

// ---- 4. F3: artifact content identity ---------------------------------

/// A leaf receipt must carry the content identity of the bytes that
/// actually validated, and a later rewrite of that file must read back
/// as explicitly unavailable rather than being silently credited to the
/// earlier Claim.
#[test]
fn closure_records_validated_artifact_digest_and_reports_changed_bytes_unavailable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    route_fixture::install_route_fixture(&estate, "wa_container_child_role");
    route_fixture::install_route_fixture(&estate, "wa_simple_leaf");
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let repo = dir.path().join("repo");
    init_repo(&repo);
    let work = submit(
        &estate,
        "wa_container_child_role",
        &repo,
        &["demo:write", "child-output:write"],
        None,
    )
    .expect("submit");
    write_file(
        &estate.join("worktrees").join(&work.work_id),
        "a.md",
        "ORIGINAL-CLAIMED-BYTES\n",
    );
    claim_ok(&estate, &work.work_id, &work.run_id, "a.md=a.md");
    assert_eq!(state_of(&pointer.socket, &work.work_id), "waiting");

    // The digest recorded at validation, read back over the public status verb.
    let before = status(&pointer.socket, &work.work_id);
    let entry = &before["evidence"][0]["artifacts"][0];
    let recorded = entry["digest"]
        .as_str()
        .expect("the validated artifact's content digest is recorded")
        .to_string();
    assert_eq!(entry["name"].as_str(), Some("a.md"), "{before}");
    assert_eq!(entry["available"].as_bool(), Some(true), "{before}");

    // Rewrite the file while the container is held.
    write_file(
        &estate.join("worktrees").join(&work.work_id),
        "a.md",
        "TAMPERED-AFTER-THE-CLAIM\n",
    );

    let child_repo = dir.path().join("child-repo");
    init_repo(&child_repo);
    let child = submit(
        &estate,
        "wa_simple_leaf",
        &child_repo,
        &["child-output:write"],
        Some(ParentRef {
            work: &work.work_id,
            waypoint: "outer",
            run: &work.run_id,
            role: "helper",
            attempt: None,
        }),
    )
    .expect("child submit");
    write_file(
        &estate.join("worktrees").join(&child.work_id),
        "helper.md",
        "helper\n",
    );
    claim_ok(
        &estate,
        &child.work_id,
        &child.run_id,
        "helper.md=helper.md",
    );
    assert_eq!(state_of(&pointer.socket, &work.work_id), "completed");

    // The receipt names the bytes that validated, not the later ones.
    let events = journal_events(&estate, &work.work_id);
    let receipts = last_closed(&events, "outer").expect("outer closes");
    let Some(OutcomeReceipt::Leaf { artifacts, .. }) = find_leaf(&receipts, "outer/leaf-a") else {
        panic!("no leaf receipt: {receipts:?}");
    };
    assert_eq!(artifacts.len(), 1, "{artifacts:?}");
    assert_eq!(artifacts[0].name, "a.md");
    assert_eq!(
        artifacts[0].digest, recorded,
        "the receipt must carry the digest of the validated bytes"
    );

    // And inspection now answers explicitly unavailable.
    let after = status(&pointer.socket, &work.work_id);
    let entry = &after["evidence"][0]["artifacts"][0];
    assert_eq!(entry["available"].as_bool(), Some(false), "{after}");
    assert_eq!(entry["reason"].as_str(), Some("changed"), "{after}");
    assert_eq!(entry["digest"].as_str(), Some(recorded.as_str()), "{after}");

    stop_wirkd(&estate, wirkd_child);
}

/// The absent case, and historical inspection after a real restart.
#[test]
fn a_removed_artifact_reads_unavailable_and_history_survives_restart() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    route_fixture::install_route_fixture(&estate, "wa_container_child_role");
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let repo = dir.path().join("repo");
    init_repo(&repo);
    let work = submit(
        &estate,
        "wa_container_child_role",
        &repo,
        &["demo:write"],
        None,
    )
    .expect("submit");
    write_file(
        &estate.join("worktrees").join(&work.work_id),
        "a.md",
        "kept bytes\n",
    );
    claim_ok(&estate, &work.work_id, &work.run_id, "a.md=a.md");
    let recorded = status(&pointer.socket, &work.work_id)["evidence"][0]["artifacts"][0]["digest"]
        .as_str()
        .expect("digest recorded")
        .to_string();

    fs::remove_file(estate.join("worktrees").join(&work.work_id).join("a.md"))
        .expect("remove the claimed artifact");
    stop_wirkd(&estate, wirkd_child);
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let after = status(&pointer.socket, &work.work_id);
    let entry = &after["evidence"][0]["artifacts"][0];
    assert_eq!(entry["digest"].as_str(), Some(recorded.as_str()), "{after}");
    assert_eq!(entry["available"].as_bool(), Some(false), "{after}");
    assert_eq!(entry["reason"].as_str(), Some("absent"), "{after}");

    stop_wirkd(&estate, wirkd_child);
}

// ---- 5. F4: two-sided child identity ----------------------------------

/// A `ChildWorkSpawned` naming an independently submitted, completed
/// Work — one whose own journal records no parent binding at all —
/// cannot become a required child receipt. The positive control in the
/// same estate proves the crediting path itself runs.
#[test]
fn an_unrelated_completed_work_cannot_be_credited_as_a_child_receipt() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    route_fixture::install_route_fixture(&estate, "wa_container_child_role");
    route_fixture::install_route_fixture(&estate, "wa_simple_leaf");
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let repo = dir.path().join("repo");
    init_repo(&repo);
    let work = submit(
        &estate,
        "wa_container_child_role",
        &repo,
        &["demo:write", "child-output:write"],
        None,
    )
    .expect("submit");
    write_file(&estate.join("worktrees").join(&work.work_id), "a.md", "a\n");
    claim_ok(&estate, &work.work_id, &work.run_id, "a.md=a.md");
    assert_eq!(state_of(&pointer.socket, &work.work_id), "waiting");

    // An entirely independent Work: no --parent-* flags at all.
    let loose_repo = dir.path().join("loose-repo");
    init_repo(&loose_repo);
    let loose = submit(
        &estate,
        "wa_simple_leaf",
        &loose_repo,
        &["demo:write"],
        None,
    )
    .expect("independent submit");
    write_file(
        &estate.join("worktrees").join(&loose.work_id),
        "helper.md",
        "helper\n",
    );
    claim_ok(
        &estate,
        &loose.work_id,
        &loose.run_id,
        "helper.md=helper.md",
    );
    assert_eq!(state_of(&pointer.socket, &loose.work_id), "completed");

    // Cross-record inconsistency: the parent claims a child that does
    // not claim the parent.
    stop_wirkd(&estate, wirkd_child);
    raw_append(
        &estate,
        &work.work_id,
        None,
        EventKind::ChildWorkSpawned {
            role: "helper".to_string(),
            child: WorkId(loose.work_id.clone()),
            waypoint: wirk_core::WaypointId("outer".to_string()),
            run: wirk_core::RunId(work.run_id.clone()),
            attempt: 1,
        },
    );
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let result = status(&pointer.socket, &work.work_id);
    assert_eq!(
        result["state"].as_str(),
        Some("waiting"),
        "a child that does not itself name this parent is not a receipt: {result}"
    );
    let events = journal_events(&estate, &work.work_id);
    assert!(
        last_closed(&events, "outer").is_none(),
        "{:?}",
        stage_events(&events, "outer")
    );

    // Positive control: a real child, bound both ways, does close it.
    let child_repo = dir.path().join("child-repo");
    init_repo(&child_repo);
    let child = submit(
        &estate,
        "wa_simple_leaf",
        &child_repo,
        &["child-output:write"],
        Some(ParentRef {
            work: &work.work_id,
            waypoint: "outer",
            run: &work.run_id,
            role: "helper",
            attempt: None,
        }),
    )
    .expect("child submit");
    write_file(
        &estate.join("worktrees").join(&child.work_id),
        "helper.md",
        "helper\n",
    );
    claim_ok(
        &estate,
        &child.work_id,
        &child.run_id,
        "helper.md=helper.md",
    );
    assert_eq!(state_of(&pointer.socket, &work.work_id), "completed");

    stop_wirkd(&estate, wirkd_child);
}

/// The activation a child serves is part of its binding: naming a
/// generation that is not the container's current one is refused at
/// submit, and a child that served a superseded activation cannot
/// credit the new one.
#[test]
fn a_child_bound_to_a_superseded_container_activation_is_refused_and_never_credits() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    route_fixture::install_route_fixture(&estate, "wa_reopen_roles");
    route_fixture::install_route_fixture(&estate, "wa_simple_leaf");
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let repo = dir.path().join("repo");
    init_repo(&repo);
    let work = submit(
        &estate,
        "wa_reopen_roles",
        &repo,
        &["demo:write", "child-output:write"],
        None,
    )
    .expect("submit roles");
    write_file(&estate.join("worktrees").join(&work.work_id), "b.md", "b\n");
    claim_ok(&estate, &work.work_id, &work.run_id, "b.md=b.md");
    let lead_run = current_run(&pointer.socket, &work.work_id);
    write_file(&estate.join("worktrees").join(&work.work_id), "a.md", "a\n");
    claim_ok(&estate, &work.work_id, &lead_run, "a.md=a.md");
    assert_eq!(state_of(&pointer.socket, &work.work_id), "waiting");

    // A stale generation is refused outright.
    let helper_repo = dir.path().join("helper-repo");
    init_repo(&helper_repo);
    let refused = submit(
        &estate,
        "wa_simple_leaf",
        &helper_repo,
        &["child-output:write"],
        Some(ParentRef {
            work: &work.work_id,
            waypoint: "top/outer",
            run: &lead_run,
            role: "helper",
            attempt: Some(7),
        }),
    );
    let message = refused.expect_err("a non-current activation must be refused");
    assert!(
        message.contains("activation"),
        "refusal should name the activation: {message}"
    );

    // The current generation is accepted and closes top/outer.
    let helper = submit(
        &estate,
        "wa_simple_leaf",
        &helper_repo,
        &["child-output:write"],
        Some(ParentRef {
            work: &work.work_id,
            waypoint: "top/outer",
            run: &lead_run,
            role: "helper",
            attempt: Some(1),
        }),
    )
    .expect("child submit at the current activation");
    write_file(
        &estate.join("worktrees").join(&helper.work_id),
        "helper.md",
        "helper\n",
    );
    claim_ok(
        &estate,
        &helper.work_id,
        &helper.run_id,
        "helper.md=helper.md",
    );
    let tail_run = current_run(&pointer.socket, &work.work_id);
    write_file(&estate.join("worktrees").join(&work.work_id), "t.md", "t\n");
    claim_ok(&estate, &work.work_id, &tail_run, "t.md=t.md");
    assert_eq!(
        state_of(&pointer.socket, &work.work_id),
        "waiting",
        "top now holds on its own reviewer role"
    );

    // Reopen top/outer: its old helper receipt served activation 1 and
    // must not credit activation 2.
    let (code, out) = retry_run_cli(&estate, &work.work_id, &work.run_id);
    assert_eq!(code, Some(0), "reopen retry refused: {out}");
    let events = journal_events(&estate, &work.work_id);
    assert_eq!(activations(&events, "top/outer"), vec![1, 2]);

    let leaf2 = current_run(&pointer.socket, &work.work_id);
    write_file(
        &estate.join("worktrees").join(&work.work_id),
        "b.md",
        "b2\n",
    );
    claim_ok(&estate, &work.work_id, &leaf2, "b.md=b.md");
    let lead2 = current_run(&pointer.socket, &work.work_id);
    write_file(
        &estate.join("worktrees").join(&work.work_id),
        "a.md",
        "a2\n",
    );
    claim_ok(&estate, &work.work_id, &lead2, "a.md=a.md");

    let events = journal_events(&estate, &work.work_id);
    let held = stage_events(&events, "top/outer");
    assert!(
        held.last().is_some_and(|last| last.contains("helper")),
        "the reopened container must hold for a fresh child outcome: {held:?}"
    );
    assert_eq!(state_of(&pointer.socket, &work.work_id), "waiting");

    // A fresh child for the new activation closes it.
    let helper2_repo = dir.path().join("helper2-repo");
    init_repo(&helper2_repo);
    let helper2 = submit(
        &estate,
        "wa_simple_leaf",
        &helper2_repo,
        &["child-output:write"],
        Some(ParentRef {
            work: &work.work_id,
            waypoint: "top/outer",
            run: &lead2,
            role: "helper",
            attempt: Some(2),
        }),
    )
    .expect("child submit at the reopened activation");
    write_file(
        &estate.join("worktrees").join(&helper2.work_id),
        "helper.md",
        "helper\n",
    );
    claim_ok(
        &estate,
        &helper2.work_id,
        &helper2.run_id,
        "helper.md=helper.md",
    );
    let events = journal_events(&estate, &work.work_id);
    assert!(
        last_closed(&events, "top/outer").is_some(),
        "the fresh child closes the reopened container"
    );

    stop_wirkd(&estate, wirkd_child);
}

// ---- 6. F5: mechanism independence ------------------------------------

/// One container, an `Actor` leaf and a `Deterministic` leaf, closing on
/// the same outcome contract: activation, hold on the child role, exact
/// artifact receipts and completion are all independent of the leaf's
/// mechanism. The Actor checkout is materialized with real `git`, never
/// a model.
#[test]
fn a_container_mixing_actor_and_deterministic_leaves_closes_with_exact_receipts() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    route_fixture::install_route_fixture(&estate, "wa_mixed_mechanism");
    route_fixture::install_route_fixture(&estate, "wa_simple_leaf");
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let repo = dir.path().join("repo");
    init_repo(&repo);
    let work = submit_kind(
        &estate,
        "wa_mixed_mechanism",
        &repo,
        &["demo:write", "child-output:write"],
        None,
        Some("actor"),
    )
    .expect("submit mixed");
    assert_eq!(work.waypoint, "outer/act");

    // The Actor leaf: real worktree, real bytes, real Claim.
    let worktree = materialize_actor(&pointer.socket, &estate, &work.work_id, &work.run_id);
    fs::write(worktree.join("a.md"), b"written by hand\n").expect("write a.md");
    claim_ok(&estate, &work.work_id, &work.run_id, "a.md=a.md");

    // The Deterministic leaf, in the same container.
    let det_run = current_run(&pointer.socket, &work.work_id);
    fs::write(worktree.join("b.md"), b"b\n").expect("write b.md");
    claim_ok(&estate, &work.work_id, &det_run, "b.md=b.md");
    assert_eq!(
        state_of(&pointer.socket, &work.work_id),
        "waiting",
        "the container holds for its child role whatever the leaf mechanism was"
    );

    let child_repo = dir.path().join("child-repo");
    init_repo(&child_repo);
    let child = submit(
        &estate,
        "wa_simple_leaf",
        &child_repo,
        &["child-output:write"],
        Some(ParentRef {
            work: &work.work_id,
            waypoint: "outer",
            run: &det_run,
            role: "helper",
            attempt: None,
        }),
    )
    .expect("child submit");
    write_file(
        &estate.join("worktrees").join(&child.work_id),
        "helper.md",
        "helper\n",
    );
    claim_ok(
        &estate,
        &child.work_id,
        &child.run_id,
        "helper.md=helper.md",
    );

    assert_eq!(state_of(&pointer.socket, &work.work_id), "completed");
    let events = journal_events(&estate, &work.work_id);
    let receipts = last_closed(&events, "outer").expect("outer closes");
    let Some(OutcomeReceipt::Leaf { artifacts, .. }) = find_leaf(&receipts, "outer/act") else {
        panic!("no Actor leaf receipt: {receipts:?}");
    };
    assert_eq!(artifacts.len(), 1);
    assert_eq!(artifacts[0].name, "a.md");
    assert!(
        !artifacts[0].digest.is_empty(),
        "an Actor leaf's receipt carries content identity too"
    );
    assert_eq!(leaf_run(&receipts, "outer/act"), work.run_id);
    assert_eq!(leaf_run(&receipts, "outer/det"), det_run);

    stop_wirkd(&estate, wirkd_child);
}

// ---- 7. minor corrections ---------------------------------------------

/// The startup sweep re-evaluates a held container idempotently: an
/// unchanged hold appends nothing.
#[test]
fn restarting_does_not_duplicate_an_unchanged_stage_held() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    route_fixture::install_route_fixture(&estate, "wa_container_child_role");
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let repo = dir.path().join("repo");
    init_repo(&repo);
    let work = submit(
        &estate,
        "wa_container_child_role",
        &repo,
        &["demo:write"],
        None,
    )
    .expect("submit");
    write_file(&estate.join("worktrees").join(&work.work_id), "a.md", "a\n");
    claim_ok(&estate, &work.work_id, &work.run_id, "a.md=a.md");
    assert_eq!(state_of(&pointer.socket, &work.work_id), "waiting");
    let before = stage_events(&journal_events(&estate, &work.work_id), "outer");
    assert_eq!(before.len(), 1, "{before:?}");
    stop_wirkd(&estate, wirkd_child);

    for _ in 0..2 {
        let (wirkd_child, _pointer) = start_wirkd(&estate);
        stop_wirkd(&estate, wirkd_child);
    }
    let after = stage_events(&journal_events(&estate, &work.work_id), "outer");
    assert_eq!(
        after, before,
        "an unchanged hold must not be re-journaled on every restart"
    );
}

/// A Claim against an already-terminal Work is refused before anything
/// is appended: no `ClaimFiled`/`ClaimRecorded` false progress.
#[test]
fn a_claim_against_a_terminal_work_is_refused_and_journals_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    route_fixture::install_route_fixture(&estate, "wa_simple_leaf");
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let repo = dir.path().join("repo");
    init_repo(&repo);
    let work = submit(&estate, "wa_simple_leaf", &repo, &["demo:write"], None).expect("submit");
    write_file(
        &estate.join("worktrees").join(&work.work_id),
        "helper.md",
        "helper\n",
    );
    claim_ok(&estate, &work.work_id, &work.run_id, "helper.md=helper.md");
    assert_eq!(state_of(&pointer.socket, &work.work_id), "completed");
    let before = journal_events(&estate, &work.work_id).len();

    let (code, stdout) = claim(
        &estate,
        &work.work_id,
        &work.run_id,
        &["--artifact", "helper.md=helper.md"],
    );
    assert_ne!(code, Some(0), "a late claim must not succeed: {stdout}");
    assert_eq!(
        journal_events(&estate, &work.work_id).len(),
        before,
        "a refused late claim must append nothing: {stdout}"
    );

    stop_wirkd(&estate, wirkd_child);
}
