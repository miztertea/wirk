//! git.rs: the executor's own wirk-side `git worktree` helper (item 4,
//! W2; 0018 D60, 0022 D77). Not a Herdr request — `worktree.open`
//! (`OpenWorktree` in `lib.rs`) only *binds* Herdr to a worktree that
//! already exists; creating and removing the worktree itself is plain
//! git, run the same way every other git call in this estate is run:
//! `std::process::Command` over the box's installed `git` (R4 — native
//! platform CLI, nothing to wrap).

use std::path::Path;
use std::process::Command;

use thiserror::Error;

/// Everything `worktree_add`/`worktree_remove` can fail with. Kept flat
/// (R6): one crate-internal caller (`RunLoop`), no need for per-git-verb
/// variants beyond what the caller already has to report (`RunFailed`'s
/// `detail`, issue 275).
#[derive(Debug, Error)]
pub enum GitError {
    /// Issue 285: an empty `base_sha` would let `git worktree add`
    /// resolve the base to whatever ref/HEAD it feels like, silently
    /// unpinning the worktree D9#6 exists to pin. Refused before git is
    /// ever spawned, not left to git's own (permissive) argument
    /// handling.
    #[error("git worktree add refused: base_sha is empty (issue 285)")]
    EmptyBaseSha,
    #[error("failed to spawn git: {0}")]
    Spawn(#[from] std::io::Error),
    #[error("git {args} failed (status {status}): {stderr}")]
    Command {
        args: String,
        status: String,
        stderr: String,
    },
    #[error("could not canonicalize resolved Git common directory {path}: {source}")]
    Canonicalize {
        path: String,
        source: std::io::Error,
    },
}

/// `git worktree add -b <branch> <path> <base_sha>` run inside `repo`,
/// then `git -C <path> rev-parse HEAD` to read back the new worktree's
/// exact HEAD SHA — the pinned value D9#6 asserts against the SHA
/// `base_sha` named (a worktree's `HEAD` after `add` at an exact commit
/// *is* that commit; the round-trip through `rev-parse` is the same
/// check the tried step's checkpoint list uses, `session.md` §7).
///
/// W6 (`p2-concurrency/tried/RESULT.md` stage 04's own finding: a
/// retry's second launch names the same `path`/`branch` as the first —
/// `wirk/<work_id>`, `run_command`'s own naming, unchanged by a retry).
/// W6b (`p2-concurrency/land/LAND-w6.md`: W6's own path-only reuse
/// check reached `git worktree add -b` — and its "fatal: branch already
/// exists" — the one time `path` did not exist on disk but `branch`
/// still did; see `p2-concurrency/w6b/BUILD.md` for the traced cause).
/// Three states, checked in this order, each naming the git call it
/// takes:
///
/// 1. **`path` exists on disk** — this Work's worktree is already live
///    (the common retry case, W6): reused as-is, `git -C <path>
///    rev-parse HEAD` reads its HEAD, no `worktree add` run at all.
/// 2. **`path` absent, `branch` exists** (`git -C <repo> show-ref
///    --verify --quiet refs/heads/<branch>`, R3 — plumbing built for
///    exactly this test, no `git branch --list` output to parse) — the
///    branch survived something that removed only the worktree
///    directory (W6b's own traced cause). `git worktree add <path>
///    <branch>` (no `-b`) checks that branch out fresh at `path` rather
///    than trying to recreate it.
/// 3. **Neither exists** — a fresh Work, or one whose worktree and
///    branch were both genuinely removed: `git worktree add -b <branch>
///    <path> <base_sha>` creates both, exactly as before.
pub fn worktree_add(
    repo: &Path,
    path: &Path,
    branch: &str,
    base_sha: &str,
) -> Result<String, GitError> {
    if base_sha.trim().is_empty() {
        return Err(GitError::EmptyBaseSha);
    }
    if path.exists() {
        let head = run_git(path, &["rev-parse", "HEAD"])?;
        return Ok(head.trim().to_string());
    }
    let path_str = path.to_string_lossy().into_owned();
    let branch_ref = format!("refs/heads/{branch}");
    if run_git(repo, &["show-ref", "--verify", "--quiet", &branch_ref]).is_ok() {
        run_git(repo, &["worktree", "add", &path_str, branch])?;
    } else {
        run_git(
            repo,
            &["worktree", "add", "-b", branch, &path_str, base_sha],
        )?;
    }
    let head = run_git(path, &["rev-parse", "HEAD"])?;
    Ok(head.trim().to_string())
}

/// `git worktree remove <path>` run inside `repo`. The branch is never
/// deleted here (0017 D54: "the branch survives `worktree remove`") —
/// only `worktree remove` is called, never `branch -D`.
pub fn worktree_remove(repo: &Path, path: &Path) -> Result<(), GitError> {
    let path_str = path.to_string_lossy().into_owned();
    run_git(repo, &["worktree", "remove", &path_str])?;
    Ok(())
}

/// `git rev-parse --abbrev-ref HEAD` run inside `worktree` — the branch
/// currently checked out there, or a detached-HEAD marker
/// (`"HEAD"`) git itself prints when none is. P3 execution-recovery
/// item 2: `wirk run`'s reattachment check (`executor.rs`) needs this to
/// tell a Run's own worktree, now ahead of its reserved base by the
/// actor's own commits, from one whose checkout was pointed somewhere
/// foreign — the two states `git rev-parse HEAD` alone cannot
/// distinguish.
pub fn current_branch(worktree: &Path) -> Result<String, GitError> {
    let branch = run_git(worktree, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    Ok(branch.trim().to_string())
}

/// `git merge-base --is-ancestor <ancestor> <descendant>` run inside
/// `repo`, true on exit 0, false on exit 1 (git's own documented
/// contract for this verb — the two outcomes plumbing exists to test
/// without parsing output) and a hard error on any other exit or a
/// failure to spawn. P3 execution-recovery item 2: this is the
/// distinction between the actor's own forward commits on its own
/// branch (an ancestor of the current HEAD — legitimate in-Work
/// progress a recovering `wirk run` may reattach to) and a genuinely
/// foreign or rewritten base (no ancestor relation — refused exactly as
/// before).
/// `git -C <path> rev-parse --path-format=absolute --git-common-dir`,
/// canonicalized (R2: the exact same call and shape
/// `wirkd::server::canonical_repository_identity` already makes at
/// submit time — reused here, not reinvented, so the executor's own
/// reattachment check answers "is this genuinely the same repository"
/// the identical way admission already does). Two worktrees of one
/// repository share one common Git directory and resolve identically;
/// an unrelated repository placed at a coincidentally-matching path
/// does not. P3 execution-recovery item 2 (root's correction): path
/// equality and branch-name equality alone do not establish this —
/// this is the actual identity check, checked on every reattachment,
/// not only a recovering one.
pub fn repository_identity(path: &Path) -> Result<String, GitError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output()?;
    if !output.status.success() {
        return Err(GitError::Command {
            args: "rev-parse --path-format=absolute --git-common-dir".to_string(),
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
    std::fs::canonicalize(&raw)
        .map(|p| p.display().to_string())
        .map_err(|source| GitError::Canonicalize { path: raw, source })
}

/// `git merge-base --is-ancestor <ancestor> <descendant>` run inside
/// `repo`, true on exit 0, false on exit 1 (git's own documented
/// contract for this verb — the two outcomes plumbing exists to test
/// without parsing output) and a hard error on any other exit or a
/// failure to spawn. P3 execution-recovery item 2: this is the
/// distinction between the actor's own forward commits on its own
/// branch (an ancestor of the current HEAD — legitimate in-Work
/// progress a recovering `wirk run` may reattach to) and a genuinely
/// foreign or rewritten base (no ancestor relation — refused exactly as
/// before).
pub fn is_ancestor(repo: &Path, ancestor: &str, descendant: &str) -> Result<bool, GitError> {
    let output = Command::new("git")
        .current_dir(repo)
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .output()?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(GitError::Command {
            args: format!("merge-base --is-ancestor {ancestor} {descendant}"),
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }),
    }
}

/// The worktree's own no-progress signal (ruling 0044/D133's "no
/// progress" check, item C): `git status --porcelain` (uncommitted
/// changes) plus `git rev-parse HEAD` (a new commit), run with `cwd` as
/// the worktree — exactly the two `git` calls named in the brief,
/// nothing timed. Unreadable (not a git worktree, git missing) folds to
/// an empty string rather than erroring: the caller compares two
/// fingerprints for equality, and a worktree that cannot be read is
/// "no progress observable" either way, not a hard failure of the
/// stuck-actor check.
pub fn fingerprint(cwd: &Path) -> String {
    let status = run_git(cwd, &["status", "--porcelain"]).unwrap_or_default();
    let head = run_git(cwd, &["rev-parse", "HEAD"]).unwrap_or_default();
    format!("{}\n{}", status.trim(), head.trim())
}

/// Worktree-relative paths that differ from `base_sha` (P2.4 W1,
/// `orient/check.md` §2): the union of `git diff --name-only -M
/// <base_sha>` (committed, staged, and unstaged changes to tracked
/// files — `-M` forces rename detection explicitly since `diff.renames`
/// is unset on this box, and with `--name-only` a detected rename
/// prints only its new path) and `git status --porcelain
/// --untracked-files=all` parsed for its own path column (adds
/// untracked files `diff` never reports; a rename line there also
/// resolves to its new path via `" -> "`, redundant with `diff -M` but
/// harmless — the result is deduplicated). Same `run_git` shape as
/// every other call in this file (R2/R3); a new function beside
/// `worktree_add`/`fingerprint`, not a reuse of `fingerprint` — its
/// return is an opaque comparison string, not a path list.
///
/// Deterministic in the paths it reports, not merely in whether it
/// errs: returned in sorted order (`BTreeSet`) so a caller comparing
/// the set, or joining it into a message, never depends on git's own
/// listing order.
pub fn changed_paths(worktree: &Path, base_sha: &str) -> Result<Vec<String>, GitError> {
    let diff = run_git(worktree, &["diff", "--name-only", "-M", base_sha])?;
    let status = run_git(
        worktree,
        &["status", "--porcelain", "--untracked-files=all"],
    )?;

    let mut paths = std::collections::BTreeSet::new();
    for line in diff.lines() {
        let path = line.trim();
        if !path.is_empty() {
            paths.insert(path.to_string());
        }
    }
    for line in status.lines() {
        // Porcelain v1 format: two status-code columns then a space,
        // then the path (`"XY path"`, e.g. `" M src/keep.txt"`, `"??
        // new.md"`, `"R  old -> new"`). A rename's path column reads
        // `"old -> new"`; keep only the new path.
        if line.len() <= 3 {
            continue;
        }
        let rest = &line[3..];
        let path = rest.rsplit(" -> ").next().unwrap_or(rest).trim();
        if !path.is_empty() {
            paths.insert(path.to_string());
        }
    }
    Ok(paths.into_iter().collect())
}

fn run_git(cwd: &Path, args: &[&str]) -> Result<String, GitError> {
    let output = Command::new("git").current_dir(cwd).args(args).output()?;
    if !output.status.success() {
        return Err(GitError::Command {
            args: args.join(" "),
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
