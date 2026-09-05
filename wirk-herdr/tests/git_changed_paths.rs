//! `changed_paths` (P2.4 W1, `orient/check.md` §2's own worked example):
//! a real scratch git repo (0040 D127 — no fake for a `git` behaviour),
//! not `wirk-workspace` itself, created and removed by the test.

use std::fs;
use std::path::Path;
use std::process::Command;

use wirk_herdr::git::changed_paths;

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

/// check.md §2's own worked example, reproduced as a test: a tracked
/// file modified unstaged, a tracked file renamed (staged, `git mv`),
/// and a new untracked file — all three land once each, the rename
/// reported only by its new path.
#[test]
fn changed_paths_reports_tracked_untracked_and_renamed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = dir.path();
    git(repo, &["init", "-q"]);
    fs::create_dir_all(repo.join("src")).expect("mkdir src");
    fs::create_dir_all(repo.join("docs")).expect("mkdir docs");
    fs::write(repo.join("src/keep.txt"), b"one\n").expect("write keep.txt");
    fs::write(repo.join("docs/rename_me.md"), b"rename me\n").expect("write rename_me.md");
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

    // Unstaged modification.
    fs::write(repo.join("src/keep.txt"), b"one\ntwo\n").expect("modify keep.txt");
    // Staged rename.
    git(repo, &["mv", "docs/rename_me.md", "docs/renamed.md"]);
    // Untracked new file.
    fs::write(repo.join("docs/new_file.md"), b"new\n").expect("write new_file.md");

    let mut paths = changed_paths(repo, &base_sha).expect("changed_paths succeeds");
    paths.sort();
    assert_eq!(
        paths,
        vec![
            "docs/new_file.md".to_string(),
            "docs/renamed.md".to_string(),
            "src/keep.txt".to_string(),
        ],
        "each category (tracked-unstaged, staged-rename, untracked) reported once, rename by its new path only"
    );
}

/// A committed change since `base_sha` (no working-tree dirt at all) is
/// reported too — `git diff --name-only` compares against the pinned
/// base, not merely the index.
#[test]
fn changed_paths_reports_a_committed_change_since_base_sha() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = dir.path();
    git(repo, &["init", "-q"]);
    fs::write(repo.join("a.txt"), b"a\n").expect("write a.txt");
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

    fs::write(repo.join("b.txt"), b"b\n").expect("write b.txt");
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
            "add b",
        ],
    );

    let paths = changed_paths(repo, &base_sha).expect("changed_paths succeeds");
    assert_eq!(paths, vec!["b.txt".to_string()]);
}

/// No changes since `base_sha` reports an empty set.
#[test]
fn changed_paths_is_empty_when_nothing_changed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = dir.path();
    git(repo, &["init", "-q"]);
    fs::write(repo.join("a.txt"), b"a\n").expect("write a.txt");
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

    let paths = changed_paths(repo, &base_sha).expect("changed_paths succeeds");
    assert!(paths.is_empty(), "expected no changed paths, got {paths:?}");
}
