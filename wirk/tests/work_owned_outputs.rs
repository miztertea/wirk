//! Ruling 0145: a Work-owned declared output area, proved against a real
//! `wirkd`, the real `wirk` CLI and real `git` repositories (0040 D127).
//!
//! **The blocker this closes, restated so a reader need not chase it.**
//! A Waypoint may declare a required output while its Work's execution
//! binding is `Access::Read`. Both are legitimate — together they are an
//! independent read-only reviewer that must return a receipt — and until
//! this wave they were jointly unsatisfiable: the only place a Claim
//! could name an artifact was the repository checkout, and a Read
//! binding refuses *any* change to it (0050 D150). The observed case is
//! preserved as evidence: child `work-18d3914c7627cdd7-3` filed five
//! Claims for `independent-check.md` and every one was refused
//! `OutOfBoundary`, leaving parent `work-18d3911367cffe9f-0` held.
//!
//! `read_bound_declared_output_in_the_checkout_is_still_refused` is that
//! failure, reproduced here and *kept*: this wave does not relax it, and
//! a regression that admitted a checkout write under Read would fail
//! that test alongside `boundary_claim.rs`'s own four.
//!
//! Every other test drives the *new* route the same way an actor does:
//! `wirk output` to learn where to write, an ordinary file write, then
//! `wirk claim --output NAME`. No test here calls a library function for
//! anything the CLI exposes.

#[path = "support/nested_harness.rs"]
mod harness;
#[path = "support/route_fixture.rs"]
mod route_fixture;
#[path = "../src/wirkd/mod.rs"]
mod wirkd;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use harness::*;
use serde_json::Value;

/// `wirk output --json`, run exactly as an actor runs it: the injected
/// triple in the environment and no argument naming a Work or a path.
fn output_json(estate: &Path, work_id: &str, run_id: &str) -> Value {
    let out = Command::new(wirk_bin())
        .args(["output", "list", "--json"])
        .env("WIRK_ESTATE_ROOT", estate)
        .env("WIRK_WORK_ID", work_id)
        .env("WIRK_RUN_ID", run_id)
        .output()
        .expect("wirk output runs");
    assert_eq!(
        out.status.code(),
        Some(0),
        "wirk output: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("wirk output emits json")
}

/// `wirk output dir` — the one line a shell substitutes.
fn output_dir(estate: &Path, work_id: &str, run_id: &str) -> PathBuf {
    let out = Command::new(wirk_bin())
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

/// The artifact evidence `wirk work status` reports for `name`: the
/// receipt as recorded, re-checked against its digest right now.
fn evidence_for(socket: &Path, work_id: &str, name: &str) -> Value {
    let status = status(socket, work_id);
    for entry in status["evidence"].as_array().cloned().unwrap_or_default() {
        for artifact in entry["artifacts"].as_array().cloned().unwrap_or_default() {
            if artifact["name"].as_str() == Some(name) {
                return artifact;
            }
        }
    }
    panic!("no artifact evidence for {name}: {status}");
}

/// Every path git reports as changed in `worktree` since HEAD, tracked
/// or not — the same question `is_read_binding` asks.
fn worktree_changes(worktree: &Path) -> String {
    let out = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(worktree)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("git status runs");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// One Read-bound Actor Work on the reviewer Route, materialized.
struct Reviewer {
    work_id: String,
    run_id: String,
    worktree: PathBuf,
}

fn read_bound_reviewer(estate: &Path, repo: &Path, socket: &Path) -> Reviewer {
    route_fixture::install_route_fixture(estate, "outputs_read_reviewer");
    init_repo(repo);
    let submitted = submit_kind(
        estate,
        "outputs_read_reviewer",
        repo,
        &["demo:read"],
        None,
        Some("actor"),
    )
    .expect("submit the read-bound reviewer");
    let worktree = materialize_actor(socket, estate, &submitted.work_id, &submitted.run_id);
    Reviewer {
        work_id: submitted.work_id,
        run_id: submitted.run_id,
        worktree,
    }
}

// ---- 1. the preserved refusal ----------------------------------------

/// The observed blocker, reproduced and kept. Watched fail against the
/// pre-0145 binary (`de563fb5…`) as `Refused: OutOfBoundary report.md`,
/// which is the same verdict the preserved child received; it must go on
/// failing exactly that way, because ruling 0145 relaxes nothing here.
#[test]
fn read_bound_declared_output_in_the_checkout_is_still_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    let (wirkd_child, pointer) = start_wirkd(&estate);
    let reviewer = read_bound_reviewer(&estate, &dir.path().join("repo"), &pointer.socket);

    fs::write(reviewer.worktree.join("report.md"), b"# review\n").expect("write report.md");
    let (code, stdout) = claim(
        &estate,
        &reviewer.work_id,
        &reviewer.run_id,
        &["--artifact", "report.md=report.md"],
    );
    assert_eq!(code, Some(3), "expected a refusal, got: {stdout}");
    assert!(
        stdout.starts_with("Refused: OutOfBoundary") && stdout.contains("report.md"),
        "a Read binding must still refuse a checkout write, got: {stdout}"
    );

    stop_wirkd(&estate, wirkd_child);
}

// ---- 2. the capability, end to end -----------------------------------

/// The whole point: the same Work, the same Read binding, the same
/// declared required output — delivered. `wirk output` says where,
/// the actor writes there, `wirk claim --output` validates, and the
/// checkout is byte-for-byte what it was.
#[test]
fn read_bound_run_delivers_its_declared_output_through_the_managed_area() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    let (wirkd_child, pointer) = start_wirkd(&estate);
    let reviewer = read_bound_reviewer(&estate, &dir.path().join("repo"), &pointer.socket);

    // Discovery, before anything is produced.
    let listing = output_json(&estate, &reviewer.work_id, &reviewer.run_id);
    let declared = &listing["outputs"][0];
    assert_eq!(declared["name"].as_str(), Some("report.md"));
    assert_eq!(declared["required"].as_bool(), Some(true));
    assert_eq!(declared["addressable"].as_bool(), Some(true));
    assert_eq!(
        declared["staged"].as_bool(),
        Some(false),
        "nothing is staged yet"
    );
    let staging = output_dir(&estate, &reviewer.work_id, &reviewer.run_id);
    assert_eq!(
        staging.to_string_lossy(),
        listing["staging"].as_str().unwrap(),
        "`wirk output dir` and the listing must name one directory"
    );
    assert!(
        staging.starts_with(estate.join("works").join(&reviewer.work_id)),
        "the managed area belongs to this Work: {}",
        staging.display()
    );
    assert!(
        !staging.starts_with(&reviewer.worktree),
        "the managed area is outside every repository: {}",
        staging.display()
    );

    let body = b"# independent check\n\nThe source was not changed.\n";
    fs::write(staging.join("report.md"), body).expect("actor writes its output");
    assert_eq!(
        output_json(&estate, &reviewer.work_id, &reviewer.run_id)["outputs"][0]["staged"].as_bool(),
        Some(true),
    );

    let (code, stdout) = claim(
        &estate,
        &reviewer.work_id,
        &reviewer.run_id,
        &["--output", "report.md"],
    );
    assert_eq!(code, Some(0), "claim refused: {stdout}");
    assert_eq!(stdout, "Validated");
    assert_eq!(state_of(&pointer.socket, &reviewer.work_id), "completed");

    // The Read source is untouched: this is the rule that was never
    // relaxed, asserted on the actual checkout rather than assumed.
    assert_eq!(
        worktree_changes(&reviewer.worktree),
        "",
        "the checkout must be exactly as materialized"
    );

    // The receipt names the store explicitly, at the daemon's own
    // derived address, and re-checks as available now.
    let evidence = evidence_for(&pointer.socket, &reviewer.work_id, "report.md");
    assert_eq!(evidence["store"].as_str(), Some("work_outputs"));
    assert_eq!(evidence["available"].as_bool(), Some(true));
    let expected: String = <sha2::Sha256 as sha2::Digest>::digest(body)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert_eq!(evidence["digest"].as_str(), Some(expected.as_str()));
    let path = evidence["path"].as_str().expect("a recorded path");
    assert!(
        path.starts_with("claims/") && path.ends_with("/report.md"),
        "a managed receipt records its own derived relative address, got {path}"
    );

    stop_wirkd(&estate, wirkd_child);
}

// ---- 3. staging is not canonical -------------------------------------

/// The snapshot is the evidence. Rewriting the staged file after the
/// Claim — the ordinary case of an actor that keeps working — must not
/// move, invalidate or re-attribute what validated.
#[test]
fn rewriting_the_staged_file_after_the_claim_does_not_touch_the_evidence() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    let (wirkd_child, pointer) = start_wirkd(&estate);
    let reviewer = read_bound_reviewer(&estate, &dir.path().join("repo"), &pointer.socket);

    let staging = output_dir(&estate, &reviewer.work_id, &reviewer.run_id);
    fs::write(staging.join("report.md"), b"validated bytes\n").expect("write");
    let (code, _) = claim(
        &estate,
        &reviewer.work_id,
        &reviewer.run_id,
        &["--output", "report.md"],
    );
    assert_eq!(code, Some(0));
    let before = evidence_for(&pointer.socket, &reviewer.work_id, "report.md");

    fs::write(staging.join("report.md"), b"the actor kept typing\n").expect("rewrite");

    let after = evidence_for(&pointer.socket, &reviewer.work_id, "report.md");
    assert_eq!(
        before, after,
        "a staged rewrite is not a change of evidence"
    );
    assert_eq!(after["available"].as_bool(), Some(true));

    // And the canonical copy really is a separate file holding the
    // validated bytes, not a pointer at the mutable one.
    let stored = estate
        .join("works")
        .join(&reviewer.work_id)
        .join("outputs")
        .join(after["path"].as_str().unwrap());
    assert_eq!(
        fs::read(&stored).expect("stored bytes"),
        b"validated bytes\n"
    );

    stop_wirkd(&estate, wirkd_child);
}

// ---- 4. canonical bytes that move, or go, read as unavailable --------

#[test]
fn changed_or_absent_canonical_bytes_are_reported_unavailable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    let (wirkd_child, pointer) = start_wirkd(&estate);
    let reviewer = read_bound_reviewer(&estate, &dir.path().join("repo"), &pointer.socket);

    let staging = output_dir(&estate, &reviewer.work_id, &reviewer.run_id);
    fs::write(staging.join("report.md"), b"original\n").expect("write");
    assert_eq!(
        claim(
            &estate,
            &reviewer.work_id,
            &reviewer.run_id,
            &["--output", "report.md"]
        )
        .0,
        Some(0)
    );
    let recorded = evidence_for(&pointer.socket, &reviewer.work_id, "report.md");
    let stored = estate
        .join("works")
        .join(&reviewer.work_id)
        .join("outputs")
        .join(recorded["path"].as_str().unwrap());

    fs::write(&stored, b"rewritten behind the record\n").expect("tamper");
    let changed = evidence_for(&pointer.socket, &reviewer.work_id, "report.md");
    assert_eq!(changed["available"].as_bool(), Some(false));
    assert_eq!(changed["reason"].as_str(), Some("changed"));
    assert_eq!(
        changed["digest"].as_str(),
        recorded["digest"].as_str(),
        "the recorded digest is never re-hashed into a new current one"
    );

    fs::remove_file(&stored).expect("remove");
    let gone = evidence_for(&pointer.socket, &reviewer.work_id, "report.md");
    assert_eq!(gone["available"].as_bool(), Some(false));
    assert_eq!(gone["reason"].as_str(), Some("absent"));

    // A symlink where the snapshot was does not resolve back into the
    // area, and is reported as such rather than followed.
    std::os::unix::fs::symlink("/etc/hostname", &stored).expect("symlink");
    let escaped = evidence_for(&pointer.socket, &reviewer.work_id, "report.md");
    assert_eq!(escaped["available"].as_bool(), Some(false));
    assert_eq!(escaped["reason"].as_str(), Some("unresolved"));

    stop_wirkd(&estate, wirkd_child);
}

// ---- 5. addresses that are refused, precisely ------------------------

/// Malformed and undeclared output addresses are refused by name, with
/// the rule they broke — ahead of the required-output check, so a
/// misspelled `--output` does not surface as the file it failed to
/// deliver. Nothing is stored for any of them.
#[test]
fn unaddressable_and_undeclared_output_names_are_refused_precisely() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    let (wirkd_child, pointer) = start_wirkd(&estate);
    let reviewer = read_bound_reviewer(&estate, &dir.path().join("repo"), &pointer.socket);

    for (name, expected) in [
        ("../../../etc/passwd", "may not begin with `.`"),
        ("/etc/passwd", "only ASCII letters"),
        ("a/b.md", "only ASCII letters"),
        (".", "may not begin with `.`"),
        ("..", "may not begin with `.`"),
        (".hidden", "may not begin with `.`"),
        ("rep ort.md", "only ASCII letters"),
        ("réport.md", "only ASCII letters"),
        ("", "may not be empty"),
        ("other.md", "is not a declared output of this Waypoint"),
    ] {
        let (code, stdout) = claim(
            &estate,
            &reviewer.work_id,
            &reviewer.run_id,
            &["--output", name],
        );
        assert_eq!(code, Some(3), "`{name}` should refuse, got: {stdout}");
        assert!(
            stdout.starts_with("Refused: OutOfBoundary"),
            "`{name}`: {stdout}"
        );
        assert!(
            stdout.contains(expected),
            "`{name}` must name the rule it broke ({expected}), got: {stdout}"
        );
    }

    let outputs = estate
        .join("works")
        .join(&reviewer.work_id)
        .join("outputs")
        .join("claims");
    assert!(
        !outputs.exists(),
        "a refused Claim stores nothing: {}",
        outputs.display()
    );
    assert_eq!(state_of(&pointer.socket, &reviewer.work_id), "needs_input");

    stop_wirkd(&estate, wirkd_child);
}

/// A declared output that is not there at all is `MissingArtifact` —
/// the same answer a missing checkout artifact gets, and the true one.
#[test]
fn an_unproduced_managed_output_is_missing_not_out_of_boundary() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    let (wirkd_child, pointer) = start_wirkd(&estate);
    let reviewer = read_bound_reviewer(&estate, &dir.path().join("repo"), &pointer.socket);

    let (code, stdout) = claim(
        &estate,
        &reviewer.work_id,
        &reviewer.run_id,
        &["--output", "report.md"],
    );
    assert_eq!(code, Some(3), "{stdout}");
    assert_eq!(stdout, "Refused: MissingArtifact report.md");

    stop_wirkd(&estate, wirkd_child);
}

// ---- 6. containment: a symlink out of the staging area ---------------

/// The escape that the area's own addressing cannot prevent: the actor
/// controls the staging directory's contents, so it can put a link there
/// pointing anywhere. Refused before it is ever read, so no byte from
/// outside the area is ever digested, stored or attributed.
#[test]
fn a_staged_symlink_out_of_the_area_is_refused_and_never_followed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    let (wirkd_child, pointer) = start_wirkd(&estate);
    let reviewer = read_bound_reviewer(&estate, &dir.path().join("repo"), &pointer.socket);

    let secret = dir.path().join("outside.txt");
    fs::write(&secret, b"bytes from outside the area\n").expect("write outside file");
    let staging = output_dir(&estate, &reviewer.work_id, &reviewer.run_id);

    for arrangement in ["file-symlink", "dangling-symlink", "directory"] {
        let staged = staging.join("report.md");
        let _ = fs::remove_file(&staged);
        let _ = fs::remove_dir_all(&staged);
        match arrangement {
            "file-symlink" => std::os::unix::fs::symlink(&secret, &staged).unwrap(),
            "dangling-symlink" => {
                std::os::unix::fs::symlink(dir.path().join("nothing"), &staged).unwrap()
            }
            _ => fs::create_dir(&staged).unwrap(),
        }
        let (code, stdout) = claim(
            &estate,
            &reviewer.work_id,
            &reviewer.run_id,
            &["--output", "report.md"],
        );
        assert_eq!(code, Some(3), "{arrangement}: {stdout}");
        assert!(
            stdout.starts_with("Refused: OutOfBoundary")
                && stdout.contains("not a regular file contained"),
            "{arrangement}: {stdout}"
        );
        assert!(
            !estate
                .join("works")
                .join(&reviewer.work_id)
                .join("outputs")
                .join("claims")
                .exists(),
            "{arrangement}: nothing may be stored"
        );
    }

    // A symlink *inside* the area is still not a regular file, and is
    // refused the same way: containment is proved on the entry itself,
    // not only on where it happens to point today.
    let inside = staging.join("real.txt");
    fs::write(&inside, b"inside\n").unwrap();
    let staged = staging.join("report.md");
    let _ = fs::remove_dir_all(&staged);
    let _ = fs::remove_file(&staged);
    std::os::unix::fs::symlink(&inside, &staged).unwrap();
    let (code, stdout) = claim(
        &estate,
        &reviewer.work_id,
        &reviewer.run_id,
        &["--output", "report.md"],
    );
    assert_eq!(code, Some(3), "inside symlink: {stdout}");
    assert!(stdout.contains("not a regular file contained"), "{stdout}");

    stop_wirkd(&estate, wirkd_child);
}

// ---- 7. one Work's outputs are never another's -----------------------

/// Addressing is derived from the *bound* Work and Run, so a second Work
/// staging the identical name is not reachable — there is no argument
/// through which one Work could name another's area, and the derivation
/// resolves inside its own.
#[test]
fn one_works_managed_output_is_never_another_works() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    let (wirkd_child, pointer) = start_wirkd(&estate);
    let first = read_bound_reviewer(&estate, &dir.path().join("repo-a"), &pointer.socket);
    let second = read_bound_reviewer(&estate, &dir.path().join("repo-b"), &pointer.socket);
    assert_ne!(first.work_id, second.work_id);

    let second_staging = output_dir(&estate, &second.work_id, &second.run_id);
    fs::write(
        second_staging.join("report.md"),
        b"the other Work's report\n",
    )
    .unwrap();
    assert_ne!(
        output_dir(&estate, &first.work_id, &first.run_id),
        second_staging,
        "two Works must not share a staging directory"
    );

    // The first Work claims the same declared name. Its own area is
    // empty, so this is `MissingArtifact` — never the neighbour's bytes.
    let (code, stdout) = claim(
        &estate,
        &first.work_id,
        &first.run_id,
        &["--output", "report.md"],
    );
    assert_eq!(code, Some(3), "{stdout}");
    assert_eq!(stdout, "Refused: MissingArtifact report.md");

    // And when it does produce its own, the receipt is bound to its own
    // bytes.
    fs::write(
        output_dir(&estate, &first.work_id, &first.run_id).join("report.md"),
        b"my own report\n",
    )
    .unwrap();
    assert_eq!(
        claim(
            &estate,
            &first.work_id,
            &first.run_id,
            &["--output", "report.md"]
        )
        .0,
        Some(0)
    );
    let evidence = evidence_for(&pointer.socket, &first.work_id, "report.md");
    let stored = estate
        .join("works")
        .join(&first.work_id)
        .join("outputs")
        .join(evidence["path"].as_str().unwrap());
    assert_eq!(fs::read(stored).unwrap(), b"my own report\n");

    stop_wirkd(&estate, wirkd_child);
}

// ---- 8. historical worktree receipts are unchanged --------------------

/// The compatibility half: a Write-bound Work claiming an ordinary
/// checkout artifact records exactly what it always did — a
/// worktree-relative path, resolved against the worktree — and now says
/// so. Nothing about that path changes because a second store exists.
#[test]
fn a_checkout_artifact_still_records_and_resolves_as_a_worktree_receipt() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    route_fixture::install_route_fixture(&estate, "outputs_read_reviewer");
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let repo = dir.path().join("repo");
    init_repo(&repo);
    let submitted = submit_kind(
        &estate,
        "outputs_read_reviewer",
        &repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .expect("submit a Write-bound Work");
    let worktree = materialize_actor(
        &pointer.socket,
        &estate,
        &submitted.work_id,
        &submitted.run_id,
    );
    fs::write(worktree.join("report.md"), b"# checkout report\n").unwrap();

    let (code, stdout) = claim(
        &estate,
        &submitted.work_id,
        &submitted.run_id,
        &["--artifact", "report.md=report.md"],
    );
    assert_eq!(code, Some(0), "{stdout}");

    let evidence = evidence_for(&pointer.socket, &submitted.work_id, "report.md");
    assert_eq!(evidence["store"].as_str(), Some("worktree"));
    assert_eq!(evidence["path"].as_str(), Some("report.md"));
    assert_eq!(evidence["available"].as_bool(), Some(true));

    // Resolved against the worktree, as it always was: editing the file
    // there is what makes it read `changed`.
    fs::write(worktree.join("report.md"), b"edited after the claim\n").unwrap();
    let after = evidence_for(&pointer.socket, &submitted.work_id, "report.md");
    assert_eq!(after["available"].as_bool(), Some(false));
    assert_eq!(after["reason"].as_str(), Some("changed"));

    stop_wirkd(&estate, wirkd_child);
}

// ---- 9. the held parent, released ------------------------------------

/// The whole observed scenario, end to end: a container that requires an
/// independent reviewer as a child role, a Read-bound child Work that
/// cannot write to the source it is reviewing, and the child's own Claim
/// — filed by the child, from its own managed output — closing the
/// container and completing the held parent.
#[test]
fn a_read_bound_child_reviewer_closes_its_parents_held_container() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    route_fixture::install_route_fixture(&estate, "outputs_container_reviewer_role");
    route_fixture::install_route_fixture(&estate, "outputs_read_reviewer");
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let parent_repo = dir.path().join("parent-repo");
    init_repo(&parent_repo);
    let parent = submit(
        &estate,
        "outputs_container_reviewer_role",
        &parent_repo,
        &["demo:write", "reviewed:read"],
        None,
    )
    .expect("submit parent");
    write_file(&parent_repo, "a.md", "a\n");
    claim_ok(&estate, &parent.work_id, &parent.run_id, "a.md=a.md");
    assert_eq!(
        state_of(&pointer.socket, &parent.work_id),
        "waiting",
        "the container is held on its required reviewer role"
    );

    // The child: bound Read on the very repository it reviews, with a
    // required declared output. Before ruling 0145 this Work could file
    // no Validated Claim at all, and the parent above stayed held.
    let child_repo = dir.path().join("child-repo");
    init_repo(&child_repo);
    let child = submit_kind(
        &estate,
        "outputs_read_reviewer",
        &child_repo,
        &["reviewed:read"],
        Some(ParentRef {
            work: &parent.work_id,
            waypoint: "outer",
            run: &parent.run_id,
            role: "reviewer",
            attempt: None,
        }),
        Some("actor"),
    )
    .expect("submit the read-bound child reviewer");
    let child_worktree = materialize_actor(&pointer.socket, &estate, &child.work_id, &child.run_id);

    let staging = output_dir(&estate, &child.work_id, &child.run_id);
    fs::write(
        staging.join("report.md"),
        b"# independent check\n\nNo change required.\n",
    )
    .unwrap();
    let (code, stdout) = claim(
        &estate,
        &child.work_id,
        &child.run_id,
        &["--output", "report.md"],
    );
    assert_eq!(code, Some(0), "the child's own Claim: {stdout}");
    assert_eq!(state_of(&pointer.socket, &child.work_id), "completed");
    assert_eq!(
        worktree_changes(&child_worktree),
        "",
        "the reviewed source is untouched"
    );

    // The child's Claim re-evaluates the parent's held container: the
    // required role now has a valid receipt, "outer" closes, and the
    // parent completes.
    assert_eq!(
        state_of(&pointer.socket, &parent.work_id),
        "completed",
        "the held parent must be released by the child's own receipt"
    );
    let closed = journal_events(&estate, &parent.work_id)
        .into_iter()
        .find_map(|event| match event.kind {
            wirk_core::EventKind::StageClosed {
                waypoint, receipts, ..
            } if waypoint.0 == "outer" => Some(receipts),
            _ => None,
        })
        .expect("outer closed");
    assert!(
        closed.iter().any(|receipt| matches!(receipt,
            wirk_core::OutcomeReceipt::Child { role, child: c, .. }
                if role == "reviewer" && c.0 == child.work_id)),
        "the closing receipt must bind this child in this role: {closed:?}"
    );

    stop_wirkd(&estate, wirkd_child);
}

// ---- 10. the next stage's World -------------------------------------

/// The consumer the exercise actually needs: a later stage of the same
/// Work receives the prior stage's declared output *as bound evidence at
/// its verified digest*. A receipt that validated and then was invisible
/// here would be the defect ruling 0145 names — "a string field being
/// syntactically permissive does not prove its consumers support another
/// namespace" — so this reads the delivered context, not the receipt.
#[test]
fn a_later_stage_binds_a_prior_stages_managed_output_at_its_digest() {
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
        &["demo:read"],
        None,
        Some("actor"),
    )
    .expect("submit");
    let worktree = materialize_actor(
        &pointer.socket,
        &estate,
        &submitted.work_id,
        &submitted.run_id,
    );

    let body = b"# survey\n\nsrc/lib.rs holds the boundary decision.\n";
    fs::write(
        output_dir(&estate, &submitted.work_id, &submitted.run_id).join("survey.md"),
        body,
    )
    .unwrap();
    let (code, stdout) = claim(
        &estate,
        &submitted.work_id,
        &submitted.run_id,
        &["--output", "survey.md"],
    );
    assert_eq!(code, Some(0), "{stdout}");
    assert_eq!(worktree_changes(&worktree), "", "the Read source is intact");

    // The Route advanced to the orienting second stage, which assembled
    // its own projection at reservation time.
    let after = status(&pointer.socket, &submitted.work_id);
    let second_run = after["run_id"]
        .as_str()
        .expect("the next stage opened a Run")
        .to_string();
    assert_eq!(
        after["current_waypoint"].as_str(),
        Some("outputs-two-stage/change")
    );

    let world = Command::new(wirk_bin())
        .args(["world", "show", "--json"])
        .env("WIRK_ESTATE_ROOT", &estate)
        .env("WIRK_WORK_ID", &submitted.work_id)
        .env("WIRK_RUN_ID", &second_run)
        .output()
        .expect("wirk world show runs");
    assert_eq!(
        world.status.code(),
        Some(0),
        "world show: {}",
        String::from_utf8_lossy(&world.stderr)
    );
    let world: Value = serde_json::from_slice(&world.stdout).expect("world show emits json");
    let bound = world["projection"]["bound"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let survey = bound
        .iter()
        .find(|item| item["identity"]["kind"].as_str() == Some("artifact_digest"))
        .unwrap_or_else(|| panic!("no prior-stage artifact was bound: {world}"));
    let digest = evidence_for(&pointer.socket, &submitted.work_id, "survey.md")["digest"]
        .as_str()
        .expect("a recorded digest")
        .to_string();
    assert_eq!(
        survey["identity"]["digest"].as_str(),
        Some(digest.as_str()),
        "the bound item must carry the digest the Claim validated: {survey}"
    );
    assert!(
        survey["coordinate"]
            .as_str()
            .is_some_and(|coordinate| coordinate.ends_with("/artifact/survey.md")),
        "bound at the artifact's own coordinate: {survey}"
    );
    // The bytes really were read back and verified, not merely named:
    // the summary is derived from them.
    assert!(
        survey["summary"]
            .as_str()
            .is_some_and(|summary| summary.contains("boundary decision")),
        "the delivered summary must come from the stored bytes: {survey}"
    );

    stop_wirkd(&estate, wirkd_child);
}

// ---- 6b. containment: a symlink at an *ancestor* of the staged file --

/// The escape a final-component check cannot see, and the one the
/// previous shape actually admitted.
///
/// Validation was `contained_regular_file(&staging, &staged)` — `lstat`
/// the entry, then `canonicalize` both and compare prefixes — followed
/// by `std::fs::read(&real)`, a second, fresh path lookup. The `lstat`
/// proves only that the *final* component is not a symlink. If an
/// ancestor is one, `canonicalize(staging)` follows it too, so the
/// prefix comparison is made *inside the attacker's own directory* and
/// succeeds: bytes from outside the area were digested, stored and
/// attributed to the Claim. The actor owns its staging area, so this is
/// an arrangement it can make.
///
/// Watched red against the pre-correction implementation, at both
/// ancestors below: exit 0, a Validated Claim, and a snapshot holding
/// `bytes from outside the area`. The correction walks to the staged
/// file one component at a time with `openat` and `O_NOFOLLOW` from the
/// estate root, so an ancestor symlink is refused at the component it
/// sits on and there is no lookup left to race.
///
/// This is the deterministic control. The narrower "replace the entry
/// between the check and the read" window is closed by the same change
/// — check and read are now one open file object — but that one is a
/// timing race and is not asserted here by timing.
#[test]
fn a_staged_ancestor_symlink_out_of_the_area_is_refused_and_never_followed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    let (wirkd_child, pointer) = start_wirkd(&estate);
    let reviewer = read_bound_reviewer(&estate, &dir.path().join("repo"), &pointer.socket);
    // Creates the real staging directory, and is what the actor is told
    // to write into.
    let staging = output_dir(&estate, &reviewer.work_id, &reviewer.run_id);
    let outputs = estate.join("works").join(&reviewer.work_id).join("outputs");

    const OUTSIDE: &[u8] = b"bytes from outside the area\n";

    for ancestor in ["run", "staging"] {
        // A directory outside the area, holding the file the escape is
        // meant to deliver, laid out so the *same* relative path
        // resolves inside it.
        let elsewhere = dir.path().join(format!("elsewhere-{ancestor}"));
        let _ = fs::remove_dir_all(&elsewhere);
        let (link, target) = match ancestor {
            // `.../outputs/staging/<run>` is a symlink to a directory
            // holding `report.md`.
            "run" => {
                fs::create_dir_all(&elsewhere).unwrap();
                fs::write(elsewhere.join("report.md"), OUTSIDE).unwrap();
                (staging.clone(), elsewhere.clone())
            }
            // `.../outputs/staging` is a symlink to a directory holding
            // `<run>/report.md`.
            _ => {
                let run_dir = elsewhere.join(
                    staging
                        .file_name()
                        .expect("the staging directory is named for the Run"),
                );
                fs::create_dir_all(&run_dir).unwrap();
                fs::write(run_dir.join("report.md"), OUTSIDE).unwrap();
                (outputs.join("staging"), elsewhere.clone())
            }
        };
        let _ = fs::remove_dir_all(&link);
        let _ = fs::remove_file(&link);
        std::os::unix::fs::symlink(&target, &link).unwrap();
        // The arrangement really is live: the ordinary path resolution
        // the previous implementation used does reach the outside bytes.
        assert_eq!(
            fs::read(staging.join("report.md")).expect("the escape resolves by path"),
            OUTSIDE,
            "{ancestor}: the test's own arrangement must be a real escape"
        );

        let (code, stdout) = claim(
            &estate,
            &reviewer.work_id,
            &reviewer.run_id,
            &["--output", "report.md"],
        );
        assert_eq!(
            code,
            Some(3),
            "{ancestor}: expected a refusal, got: {stdout}"
        );
        assert!(
            stdout.starts_with("Refused: OutOfBoundary")
                && stdout.contains("not a regular file contained"),
            "{ancestor}: {stdout}"
        );
        // Nothing outside the area was digested, stored or attributed.
        assert!(
            !outputs.join("claims").exists(),
            "{ancestor}: nothing may be stored"
        );

        // Put the area back the way wirkd made it for the next round.
        fs::remove_file(&link).unwrap();
        fs::create_dir_all(&staging).unwrap();
    }

    // Positive control on the same Run, after both escapes: the
    // ordinary staged file in the real area still validates, so the
    // refusals above are the boundary and not a broken read path.
    fs::write(staging.join("report.md"), b"# the actor's own report\n").unwrap();
    let (code, stdout) = claim(
        &estate,
        &reviewer.work_id,
        &reviewer.run_id,
        &["--output", "report.md"],
    );
    assert_eq!(code, Some(0), "positive control: {stdout}");
    let evidence = evidence_for(&pointer.socket, &reviewer.work_id, "report.md");
    assert_eq!(evidence["store"], "work_outputs");
    assert_eq!(evidence["available"], true);

    stop_wirkd(&estate, wirkd_child);
}
