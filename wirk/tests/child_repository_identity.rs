//! P3 W3, ruling 0090 (corrected per 0091/0092): the real positive and
//! negative required-child-role scenario `loop-a-native-verify/
//! VERDICT.md` §4 found missing — a child submitted under the same
//! `--repo` alias/access the parent itself uses for its own execution
//! checkout must actually resolve to the *same* repository, not merely
//! share the name. Real `wirk`/`wirkd`/`git`, the same harness
//! `nested_work.rs`/`nested_correction.rs` already use (R2).

#[path = "support/nested_harness.rs"]
mod harness;
#[path = "support/route_fixture.rs"]
mod route_fixture;
#[path = "../src/wirkd/mod.rs"]
mod wirkd;

use std::fs;
use std::path::Path;
use std::process::Command;

use harness::*;

fn git_worktree_add(source_repo: &Path, worktree_path: &Path) {
    let status = Command::new("git")
        .arg("-C")
        .arg(source_repo)
        .args(["worktree", "add", "--detach"])
        .arg(worktree_path)
        .arg("HEAD")
        .status()
        .expect("git worktree add runs");
    assert!(
        status.success(),
        "git worktree add {} from {} failed",
        worktree_path.display(),
        source_repo.display()
    );
}

/// Positive: the child's own checkout is a *real* `git worktree` of the
/// exact repository the parent itself executes in — same canonical
/// `git rev-parse --git-common-dir` identity, different working
/// directory. This is the "useful native required child against
/// explicitly admitted source/output identity" the corrected
/// admission must actually allow, not merely the negative refusal.
#[test]
fn required_child_sharing_the_parents_own_repository_via_a_real_worktree_is_admitted() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    route_fixture::install_route_fixture(&estate, "wa_container_child_role");
    route_fixture::install_route_fixture(&estate, "wa_simple_leaf");
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let repo = dir.path().join("repo");
    init_repo(&repo);

    let parent = submit(
        &estate,
        "wa_container_child_role",
        &repo,
        &["wirk:write"],
        None,
    )
    .expect("submit parent");
    write_file(&repo, "a.md", "a\n");
    claim_ok(&estate, &parent.work_id, &parent.run_id, "a.md=a.md");
    assert_eq!(state_of(&pointer.socket, &parent.work_id), "waiting");

    let child_worktree = dir.path().join("child-worktree");
    git_worktree_add(&repo, &child_worktree);

    let child = submit(
        &estate,
        "wa_simple_leaf",
        &child_worktree,
        &["wirk:write"],
        Some(ParentRef {
            work: &parent.work_id,
            waypoint: "outer",
            run: &parent.run_id,
            role: "helper",
            attempt: None,
        }),
    )
    .expect(
        "a genuine worktree of the parent's own repository must be admitted under the parent's own execution alias",
    );

    write_file(&child_worktree, "helper.md", "helper\n");
    claim_ok(
        &estate,
        &child.work_id,
        &child.run_id,
        "helper.md=helper.md",
    );
    assert_eq!(
        state_of(&pointer.socket, &parent.work_id),
        "completed",
        "the parent's own container must close on the genuinely admitted child's receipt"
    );

    stop_wirkd(&estate, wirkd_child);
}

/// Negative: `loop-a-native-verify/VERDICT.md` §4's exact exploit — a
/// same-named `--repo wirk:write` binding, but a genuinely different,
/// disconnected repository the child itself created. Name/access alone
/// admitted this before the correction; it must be refused now.
#[test]
fn required_child_under_a_disconnected_repository_with_the_parents_own_alias_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    route_fixture::install_route_fixture(&estate, "wa_container_child_role");
    route_fixture::install_route_fixture(&estate, "wa_simple_leaf");
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let repo = dir.path().join("repo");
    init_repo(&repo);
    let parent = submit(
        &estate,
        "wa_container_child_role",
        &repo,
        &["wirk:write"],
        None,
    )
    .expect("submit parent");
    write_file(&repo, "a.md", "a\n");
    claim_ok(&estate, &parent.work_id, &parent.run_id, "a.md=a.md");
    assert_eq!(state_of(&pointer.socket, &parent.work_id), "waiting");

    let disconnected = dir.path().join("disconnected-repo");
    init_repo(&disconnected);

    let refused = submit(
        &estate,
        "wa_simple_leaf",
        &disconnected,
        &["wirk:write"],
        Some(ParentRef {
            work: &parent.work_id,
            waypoint: "outer",
            run: &parent.run_id,
            role: "helper",
            attempt: None,
        }),
    );
    let message = refused.expect_err(
        "a same-named, disconnected repository must never be admitted as the parent's own execution repository",
    );
    assert!(
        message.contains("ChildExceedsParentBinding"),
        "expected ChildExceedsParentBinding, got: {message}"
    );
    assert!(
        message.contains("does not resolve to the parent's own repository"),
        "the refusal must name the real reason, not a generic denial: {message}"
    );

    assert_eq!(
        state_of(&pointer.socket, &parent.work_id),
        "waiting",
        "the refused child must never be credited toward the parent's own container"
    );

    stop_wirkd(&estate, wirkd_child);
}
