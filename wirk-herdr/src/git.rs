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
}

/// `git worktree add -b <branch> <path> <base_sha>` run inside `repo`,
/// then `git -C <path> rev-parse HEAD` to read back the new worktree's
/// exact HEAD SHA — the pinned value D9#6 asserts against the SHA
/// `base_sha` named (a worktree's `HEAD` after `add` at an exact commit
/// *is* that commit; the round-trip through `rev-parse` is the same
/// check the tried step's checkpoint list uses, `session.md` §7).
pub fn worktree_add(
    repo: &Path,
    path: &Path,
    branch: &str,
    base_sha: &str,
) -> Result<String, GitError> {
    if base_sha.trim().is_empty() {
        return Err(GitError::EmptyBaseSha);
    }
    let path_str = path.to_string_lossy().into_owned();
    run_git(
        repo,
        &["worktree", "add", "-b", branch, &path_str, base_sha],
    )?;
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
