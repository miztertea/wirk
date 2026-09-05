//! `worktree_add` (W6b, `p2-concurrency/w6b/BUILD.md`): a real scratch
//! git repo (0040 D127 — no fake for a `git` behaviour), created and
//! removed by the test. Three states, one test each, matching the
//! order `worktree_add` itself checks them in:
//!
//! 1. path present -> reused (already covered live by W6's own retry
//!    test; pinned here as a unit test too since it is the first branch
//!    checked).
//! 2. path absent, branch absent -> created fresh from `base_sha`.
//! 3. path absent, branch present -> checked out onto that branch, no
//!    `-b` (W6b's own fix; **red today**: the pre-W6b helper always ran
//!    `git worktree add -b`, which fails with "branch already exists").

use std::path::Path;
use std::process::Command;

use wirk_herdr::git::worktree_add;

fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// One commit on `main`, returning the repo dir (kept alive by the
/// caller) and the base SHA.
fn init_repo() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = dir.path();
    git(repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("f.txt"), b"one\n").expect("write f.txt");
    git(repo, &["add", "-A"]);
    git(
        repo,
        &[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-q",
            "-m",
            "base",
        ],
    );
    let base_sha = git(repo, &["rev-parse", "HEAD"]);
    (dir, base_sha)
}

#[test]
fn worktree_add_creates_branch_and_worktree_when_neither_exists() {
    let (dir, base_sha) = init_repo();
    let repo = dir.path();
    let path = repo.parent().unwrap().join("fresh-worktree");
    let branch = "wirk/fresh";

    let head = worktree_add(repo, &path, branch, &base_sha).expect("worktree_add succeeds");
    assert_eq!(head, base_sha, "the new worktree's HEAD is base_sha");
    assert!(
        path.join("f.txt").exists(),
        "the worktree checked out base_sha's tree"
    );
    let branches = git(repo, &["branch", "--list", branch]);
    assert!(
        branches.contains(branch.trim_start_matches("wirk/")),
        "the branch was created: {branches:?}"
    );

    let _ = std::fs::remove_dir_all(&path);
}

#[test]
fn worktree_add_reuses_the_path_when_it_already_exists() {
    let (dir, base_sha) = init_repo();
    let repo = dir.path();
    let path = repo.parent().unwrap().join("reused-worktree");
    let branch = "wirk/reused";

    let head1 = worktree_add(repo, &path, branch, &base_sha).expect("first add succeeds");
    // A second call with the same path must reuse it (no `git worktree
    // add` run at all — reached this branch, not case 2 or 3), even
    // though nothing about the branch or base_sha changed.
    let head2 = worktree_add(repo, &path, branch, &base_sha).expect("reuse succeeds");
    assert_eq!(
        head1, head2,
        "the second call reused the same worktree, same HEAD"
    );

    let list = git(repo, &["worktree", "list"]);
    assert_eq!(
        list.lines()
            .filter(|l| l.contains("reused-worktree"))
            .count(),
        1,
        "exactly one worktree entry for this path: {list:?}"
    );

    let _ = std::fs::remove_dir_all(&path);
}

/// **Red before W6b**: the branch exists (a prior `worktree_add` call
/// created it) but `path` does not (removed out from under git, the
/// land's own traced shape, `p2-concurrency/w6b/BUILD.md`). The
/// pre-W6b helper always ran `git worktree add -b <branch> ...` when
/// `path` was absent, which fails with "fatal: a branch named
/// '<branch>' already exists". W6b's fix checks the branch first and
/// runs plain `git worktree add <path> <branch>` (no `-b`) instead.
#[test]
fn worktree_add_checks_out_the_existing_branch_when_only_the_path_is_gone() {
    let (dir, base_sha) = init_repo();
    let repo = dir.path();
    let path = repo.parent().unwrap().join("gone-worktree");
    let branch = "wirk/gone";

    let head1 = worktree_add(repo, &path, branch, &base_sha).expect("first add succeeds");
    // Remove only the worktree directory (`git worktree remove` also
    // unregisters it from git's own admin state) -- the branch itself
    // survives (0017 D54: "the branch survives `worktree remove`"),
    // exactly the shape this test pins.
    git(repo, &["worktree", "remove", &path.to_string_lossy()]);
    assert!(!path.exists(), "the worktree directory is gone");
    let branches = git(repo, &["branch", "--list", branch]);
    assert!(
        branches.contains("gone"),
        "the branch must still exist after `worktree remove`: {branches:?}"
    );

    let head2 = worktree_add(repo, &path, branch, &base_sha)
        .expect("worktree_add must check the existing branch out, not fail on -b");
    assert_eq!(
        head1, head2,
        "checked out at the branch's own tip, unchanged since the first add"
    );
    assert!(path.join("f.txt").exists(), "the worktree is live again");

    let _ = std::fs::remove_dir_all(&path);
}
