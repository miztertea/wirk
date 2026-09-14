//! Ruling 0292: an output-only **Deterministic** stage executes in the
//! Work's own owned directory, and what its Claim validated stays
//! readable afterwards.
//!
//! The defect these checks exist for was executed, not theorised
//! (`knowledge/work/p5-foundation-use/USE.md` finding 1, root's own
//! `ROOT-ACTUAL-USE.json`): the output-only Deterministic arm reserved
//! `state.estate_root` as its `cwd`, so every Work in the estate ran its
//! deterministic stage in the same directory. Two Works declaring
//! `prepared.md` overwrote each other, after which one of the two
//! already-validated Claims could only ever read
//! `ArtifactBytesChanged` — restoring either broke the other. The
//! residue had no storage class, and `work clean` refused such a Work
//! forever.
//!
//! What is checked here, each against a real `wirkd`, a real child
//! process and real bytes on disk (ruling 0040 — no fake daemon, no
//! constructed receipt):
//!
//! * two independently admitted Works declaring the *same* output name
//!   execute in their own directories, and both validated artifacts are
//!   still readable after the other Work runs, after a retry, and after
//!   one of them is cleaned;
//! * a Read-bound Deterministic→Actor handoff carries the actual
//!   collected document bytes to the Actor stage;
//! * a failed attempt retries into the same registered directory, with
//!   its identity recorded once and truthfully;
//! * cleanup of an owned directory succeeds without touching the
//!   sources it read or the other Work's outputs, while a foreign
//!   marker and a substituted directory are still refused;
//! * the estate root is not written to at all.
//!
//! `wirk` has no `lib.rs` (bin-only), so `wirkd` is compiled in via
//! `#[path]`, the move every other test binary in this crate makes.

#[path = "support/nested_harness.rs"]
mod harness;
#[path = "../src/wirkd/mod.rs"]
mod wirkd;

use std::fs;
use std::path::{Path, PathBuf};

use harness::{
    journal_events, raw_append, retry_cli, start_wirkd, state_of, status, stop_wirkd, wirk_cli,
};
use serde_json::Value;

use wirk_core::{ClaimKind, ClaimVerdict, EventKind, SourceBasis, World};

// ---- the verbs, exactly as an operator or an actor types them -------------

/// The ad hoc, Route-less deterministic shape. No `--source-basis`, so
/// the submission takes the output-only default: this is the shape
/// whose `cwd` was the estate root.
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

/// `wirk artifact read` with a Run's own injected triple — the way a
/// later stage reads what an earlier stage claimed. The verb writes the
/// claimed bytes themselves to stdout, having re-established custody
/// over them, so what comes back here is the evidence, not a rendering
/// of it.
fn artifact_read(
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
        .expect("wirk artifact read runs");
    (
        output.status.code(),
        output.stdout,
        String::from_utf8_lossy(&output.stderr).trim().to_string(),
    )
}

fn run_clean(estate: &Path, work: &str) -> (bool, String) {
    let output = wirk_cli()
        .args(["work", "clean", "--estate"])
        .arg(estate)
        .args(["--work", work, "--json"])
        .output()
        .expect("wirk work clean runs");
    (
        output.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

// ---- what the journal actually recorded -----------------------------------

/// The id of the validated `Done` Claim `run` filed, read from the
/// journal rather than from any rendering.
fn validated_claim(estate: &Path, work: &str, run: &str) -> String {
    journal_events(estate, work)
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ClaimRecorded {
                claim,
                claim_kind: ClaimKind::Done,
                verdict: ClaimVerdict::Validated,
                ..
            } if event.run.as_ref().is_some_and(|id| id.0 == run) => Some(claim.0.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no validated Done Claim journaled for run {run}"))
}

/// The receipt a validated Claim recorded for `name`: its store, its
/// path and its digest.
fn receipt(estate: &Path, work: &str, name: &str) -> (String, String, String) {
    journal_events(estate, work)
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ClaimRecorded {
                verdict: ClaimVerdict::Validated,
                artifacts,
                ..
            } => artifacts.iter().find(|a| a.name == name).map(|a| {
                (
                    a.store.label().to_string(),
                    a.path.clone(),
                    a.digest.clone(),
                )
            }),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no validated receipt for {name} on {work}"))
}

/// Every creation identity this Work's journal registers, in order.
fn creation_identities(estate: &Path, work: &str) -> Vec<(String, String)> {
    journal_events(estate, work)
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::WorktreeCreated {
                repo,
                base_sha,
                identity,
            } => Some((
                repo.clone(),
                format!(
                    "{base_sha}/{}",
                    match identity {
                        Some(identity) => format!("{}:{}", identity.dev, identity.ino),
                        None => "unregistered".to_string(),
                    }
                ),
            )),
            _ => None,
        })
        .collect()
}

fn reserved_world(socket: &Path, work: &str) -> World {
    serde_json::from_value(status(socket, work)["world"].clone())
        .expect("status carries a World for this Work")
}

fn owned_dir(estate: &Path, work: &str) -> PathBuf {
    estate.join("worktrees").join(work)
}

/// Every entry directly under the estate root, so "nothing was written
/// where every Work shares a directory" is asserted against the real
/// listing rather than against one expected name.
fn estate_root_entries(estate: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(estate)
        .expect("read estate root")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

// ---- the decisive control -------------------------------------------------

/// Two Works, admitted independently, whose deterministic commands both
/// declare `prepared.md`.
///
/// **Red before this correction**, and observed live rather than
/// predicted: both executed in the estate root, the second overwrote the
/// first's validated artifact, and `artifact read` on the first Claim
/// returned `ArtifactBytesChanged` (exit 3). Here each executes in its
/// own owned directory, both Claims read their own bytes, and they keep
/// reading them after the other Work runs and after one of them is
/// cleaned — because an output-only Claim's bytes are snapshotted
/// write-once into that Work's own `claims/` store.
#[test]
fn two_works_declaring_one_output_name_stay_isolated_and_both_stay_readable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    let (daemon, pointer) = start_wirkd(&estate);
    let before = estate_root_entries(&estate);

    let (work_a, run_a) = submit_adhoc(
        &estate,
        &["sh", "-c", "printf 'A collected\\n' > report.md"],
    );
    let (code, log) = run_deterministic(&estate, &work_a);
    assert_eq!(code, Some(0), "work A: {log}");

    let (work_b, run_b) = submit_adhoc(
        &estate,
        &["sh", "-c", "printf 'B collected\\n' > report.md"],
    );
    let (code, log) = run_deterministic(&estate, &work_b);
    assert_eq!(code, Some(0), "work B: {log}");

    // Separate execution areas, each this estate's own address for its
    // own Work — and neither of them the estate root.
    for (work, expected) in [(&work_a, "A collected\n"), (&work_b, "B collected\n")] {
        let world = reserved_world(&pointer.socket, work);
        let World::Deterministic(det) = world else {
            panic!("{work} must reserve a Deterministic World");
        };
        assert!(
            matches!(det.source_basis, SourceBasis::OutputOnly { .. }),
            "{work}: {:?}",
            det.source_basis
        );
        assert_eq!(det.cwd, owned_dir(&estate, work), "{work}'s execution area");
        assert_ne!(
            det.cwd, estate,
            "the estate root is never an execution area"
        );
        assert_eq!(
            fs::read_to_string(owned_dir(&estate, work).join("report.md"))
                .unwrap_or_else(|err| panic!("{work}'s own report.md: {err}")),
            expected,
            "{work} must hold its own bytes"
        );
        assert!(
            owned_dir(&estate, work).join(".wirk-owned").is_file(),
            "{work}'s directory carries the creation marker it was stamped with"
        );
    }

    // The estate root gained no undeclared output: it holds exactly the
    // directories the estate itself owns.
    let after = estate_root_entries(&estate);
    assert!(
        !after.contains(&"report.md".to_string()),
        "no deterministic output may land in the estate root: {after:?} (was {before:?})"
    );

    // Both Claims vouch for their own bytes, and the store they vouch
    // from is this Work's own write-once snapshot.
    let claim_a = validated_claim(&estate, &work_a, &run_a);
    let claim_b = validated_claim(&estate, &work_b, &run_b);
    let (store_a, path_a, digest_a) = receipt(&estate, &work_a, "report.md");
    let (store_b, _path_b, digest_b) = receipt(&estate, &work_b, "report.md");
    assert_eq!(
        store_a, "work_outputs",
        "an output-only Claim is snapshotted"
    );
    assert_eq!(store_b, "work_outputs");
    assert_eq!(path_a, format!("claims/{claim_a}/report.md"));
    assert_ne!(digest_a, digest_b, "the two Works claimed different bytes");

    let (code, read_a, err) = artifact_read(&estate, &work_a, &run_a, &claim_a, "report.md");
    assert_eq!(code, Some(0), "A's own Claim must read: {err}");
    assert_eq!(read_a, b"A collected\n", "A's Claim vouches for A's bytes");
    let (code, read_b, err) = artifact_read(&estate, &work_b, &run_b, &claim_b, "report.md");
    assert_eq!(code, Some(0), "B's own Claim must read: {err}");
    assert_eq!(read_b, b"B collected\n", "B's Claim vouches for B's bytes");

    // Cleaning A disposes of A's own directory and nothing else: B's
    // execution area and bytes are untouched, and A's Claim still reads
    // because its bytes are custodied outside the directory that went.
    let (ok, said) = run_clean(&estate, &work_a);
    assert!(ok, "an owned deterministic Work must be disposable: {said}");
    assert!(
        !owned_dir(&estate, &work_a).exists(),
        "A's owned directory was removed"
    );
    assert_eq!(
        fs::read_to_string(owned_dir(&estate, &work_b).join("report.md")).expect("B survives"),
        "B collected\n",
        "cleaning one Work must not reach another Work's outputs"
    );
    let (code, read_a, err) = artifact_read(&estate, &work_a, &run_a, &claim_a, "report.md");
    assert_eq!(
        code,
        Some(0),
        "a validated Claim's bytes survive the disposal of the directory they were written in: \
         {err}"
    );
    assert_eq!(read_a, b"A collected\n");
    let _ = (digest_a, digest_b);

    stop_wirkd(&estate, daemon);
}

/// A failed attempt and the retry that follows it: the same owned
/// directory, reattached by the identity this Work registered when it
/// created it, with that identity recorded exactly once.
///
/// The command is the proof of reuse rather than an assertion about it:
/// it fails on an attempt that finds no `attempt` file and succeeds on
/// one that does, so it can only succeed if the second attempt ran in
/// the directory the first one wrote in.
#[test]
fn a_failed_deterministic_attempt_retries_into_the_same_registered_directory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    let (daemon, pointer) = start_wirkd(&estate);

    let (work, first_run) = submit_adhoc(
        &estate,
        &[
            "sh",
            "-c",
            "if [ -f attempt ]; then printf 'FINAL\\n' > report.md; else : > attempt; exit 3; fi",
        ],
    );
    let (code, log) = run_deterministic(&estate, &work);
    assert_eq!(code, Some(5), "the first attempt must fail: {log}");
    assert_eq!(state_of(&pointer.socket, &work), "needs_input");
    assert!(
        owned_dir(&estate, &work).join("attempt").is_file(),
        "the failed attempt's own working state stays in its owned directory"
    );
    let registered = creation_identities(&estate, &work);
    assert_eq!(
        registered.len(),
        1,
        "one materialization, one registration: {registered:?}"
    );
    assert_eq!(
        registered[0].0, "",
        "an output-only World names no repository"
    );
    assert!(
        !registered[0].1.ends_with("unregistered"),
        "the creation identity must be real, not absent: {registered:?}"
    );

    let (code, said) = retry_cli(&estate, &work);
    assert_eq!(code, Some(0), "retry: {said}");
    let second_run = status(&pointer.socket, &work)["run_id"]
        .as_str()
        .expect("a retry opens a Run")
        .to_string();
    assert_ne!(second_run, first_run);

    let (code, log) = run_deterministic(&estate, &work);
    assert_eq!(code, Some(0), "the retry must complete: {log}");
    assert!(
        log.contains("reattached, not re-created"),
        "the retry reattaches to the directory this Work created: {log}"
    );
    assert_eq!(
        creation_identities(&estate, &work).len(),
        1,
        "a reattachment registers nothing new"
    );
    assert_eq!(
        fs::read_to_string(owned_dir(&estate, &work).join("report.md")).expect("report.md"),
        "FINAL\n",
        "the second attempt ran in the first attempt's own directory"
    );
    let claim = validated_claim(&estate, &work, &second_run);
    let (code, read, err) = artifact_read(&estate, &work, &second_run, &claim, "report.md");
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(read, b"FINAL\n");

    stop_wirkd(&estate, daemon);
}

/// Ownership is proven, never inferred from the address. A directory
/// this estate did not create is neither executed in nor removed, and a
/// directory substituted *after* this Work created it is refused by the
/// identity the journal holds — which is the half a copied marker
/// satisfies for free.
#[test]
fn a_foreign_or_substituted_directory_is_never_executed_in_or_removed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    let (daemon, pointer) = start_wirkd(&estate);

    // -- foreign marker: a directory carrying another Work's record ----
    let (stranger, _run) = submit_adhoc(&estate, &["sh", "-c", "printf x > report.md"]);
    let address = owned_dir(&estate, &stranger);
    fs::create_dir_all(&address).expect("pre-create the address");
    fs::write(
        address.join(".wirk-owned"),
        "work=work-someone-else\nrun=run-someone-else\n",
    )
    .expect("write a foreign marker");
    fs::write(address.join("theirs.txt"), b"not ours\n").expect("their file");

    let (code, log) = run_deterministic(&estate, &stranger);
    assert_eq!(
        code,
        Some(2),
        "a foreign directory is not executed in: {log}"
    );
    assert!(
        log.contains("was created by work work-someone-else"),
        "the refusal names whose directory it is: {log}"
    );
    assert_eq!(
        fs::read_to_string(address.join("theirs.txt")).expect("their file survives"),
        "not ours\n",
        "nothing in a directory this estate did not create is touched"
    );
    assert!(!address.join("report.md").exists(), "no command ran in it");

    // -- substitution: this Work's own address, a different directory --
    //
    // On a Work that can legitimately run again, because that is where
    // a substitution actually gets to matter: the first attempt fails,
    // leaving the directory this Work created and registered, and the
    // recovery attempt must refuse to execute in whatever is standing
    // at the address afterwards.
    let (work, _run) = submit_adhoc(
        &estate,
        &["sh", "-c", "printf 'first\\n' > report.md; exit 4"],
    );
    let (code, log) = run_deterministic(&estate, &work);
    assert_eq!(code, Some(5), "the first attempt fails by design: {log}");
    let owned = owned_dir(&estate, &work);
    assert!(
        owned.join(".wirk-owned").is_file(),
        "it was created and stamped"
    );
    let saved = fs::read_to_string(owned.join(".wirk-owned")).expect("marker");
    // A *different object* at this Work's own address, built before the
    // original goes and renamed in. A rename cannot recycle the inode
    // it is moving, so this substitute is provably not the registered
    // directory — asserted below rather than assumed. The other case,
    // a directory genuinely *recreated* at the address (whose inode
    // number the kernel may hand straight back), is its own control
    // further down: ruling 0297 asks for both, separately.
    let registered_identity =
        wirk_core::directory_identity(&owned).expect("the directory this Work created");
    let substitute = estate.join("substitute");
    fs::create_dir_all(&substitute).expect("build the substitute first");
    fs::remove_dir_all(&owned).expect("remove the directory this Work created");
    fs::rename(&substitute, &owned).expect("stand a different directory at the address");
    let present = wirk_core::directory_identity(&owned).expect("the substituted directory");
    assert_ne!(
        present.ino, registered_identity.ino,
        "a renamed-in substitute must carry its own inode, or this control proves nothing"
    );
    // The marker is copied in: address, `is_dir` and a marker naming
    // this Work all agree, and the journal's creation identity does not.
    fs::write(owned.join(".wirk-owned"), saved).expect("copy the marker in");
    fs::write(owned.join("planted.txt"), b"substituted\n").expect("planted file");

    let (code, said) = retry_cli(&estate, &work);
    assert_eq!(code, Some(0), "retry: {said}");
    let (code, log) = run_deterministic(&estate, &work);
    assert_eq!(
        code,
        Some(2),
        "a substituted directory is not this Work's own: {log}"
    );
    assert!(
        log.contains("the journal registers")
            && log.contains("nothing was executed in it and it was not touched"),
        "the refusal must name the identity it checked against, not merely the address: {log}"
    );

    let (ok, said) = run_clean(&estate, &work);
    assert!(
        !ok,
        "cleanup must not remove a directory it cannot prove is this Work's: {said}"
    );
    assert_eq!(
        fs::read_to_string(owned.join("planted.txt")).expect("the planted file survives"),
        "substituted\n",
        "nothing was removed"
    );
    let _ = pointer;

    stop_wirkd(&estate, daemon);
}

// ---- ruling 0300: a marker naming this Work is never enough alone --------

/// This Work's journal registers no creation identity at all for its own
/// address — the shape a Work materialized before creation-identity
/// tracking existed, or one whose registration was simply never
/// written — and a directory already stands there anyway, carrying a
/// marker that names this Work and a file that predates it. Before
/// ruling 0300, the marker alone reattached and the child that ran next
/// wrote into that directory; here it must refuse before the child is
/// ever launched.
#[test]
fn no_recorded_creation_identity_at_all_is_never_executed_in() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    let (daemon, pointer) = start_wirkd(&estate);

    let (work, run) = submit_adhoc(&estate, &["sh", "-c", "printf x > report.md"]);

    let owned = owned_dir(&estate, &work);
    fs::create_dir_all(&owned).expect("a directory already stands at this Work's own address");
    wirk_core::write_owned_marker(
        &owned,
        &wirk_core::WorkId(work.clone()),
        &wirk_core::RunId(run.clone()),
    )
    .expect("a marker naming this Work, exactly as a genuine materialization would write");
    fs::write(
        owned.join("already-here.txt"),
        b"not written by this estate\n",
    )
    .expect("a file that predates any run of this Work");

    let (code, log) = run_deterministic(&estate, &work);
    assert_eq!(
        code,
        Some(2),
        "no recorded creation identity must refuse execution, never reattach on the marker \
         alone: {log}"
    );
    assert!(
        log.contains("nothing was executed in it and it was not touched"),
        "the refusal must say nothing ran: {log}"
    );
    assert!(
        !owned.join("report.md").exists(),
        "the command must never have run: {log}"
    );
    assert_eq!(
        fs::read_to_string(owned.join("already-here.txt")).expect("the pre-existing file survives"),
        "not written by this estate\n",
        "nothing in an unproven directory is touched"
    );

    let (ok, said) = run_clean(&estate, &work);
    assert!(
        !ok,
        "cleanup must not remove a directory it cannot prove either: {said}"
    );

    let _ = pointer;
    stop_wirkd(&estate, daemon);
}

/// Where a registration predates the `created` field — the
/// `IdentityProof::Indistinguishable` shape ruling 0297 named — the
/// comparison cannot tell a directory apart from a substitute standing
/// at the same address. Real registrations on this host always carry a
/// creation time (`wirk-core/tests/directory_identity.rs`'s own note),
/// so the pre-0297 shape is injected directly, against a directory this
/// run never actually created: one built by this test, carrying a
/// marker copied from a genuine one and a file that predates it. Before
/// ruling 0300 this executed and the child wrote `two.md` beside the
/// foreign file — the exact case ruling 0283 named when it warned that
/// an external marker "would survive replacement by an unrelated
/// directory".
#[test]
fn an_indistinguishable_identity_is_never_executed_in_even_with_a_copied_marker() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    let (daemon, _pointer) = start_wirkd(&estate);

    let (work, run) = submit_adhoc(&estate, &["sh", "-c", "printf x > two.md"]);

    let owned = owned_dir(&estate, &work);
    fs::create_dir_all(&owned).expect("the foreign directory, standing at this Work's address");
    fs::write(owned.join("not-ours.txt"), b"already here\n").expect("their file");
    wirk_core::write_owned_marker(
        &owned,
        &wirk_core::WorkId(work.clone()),
        &wirk_core::RunId(run.clone()),
    )
    .expect("a marker copied in, naming this Work");
    let present =
        wirk_core::directory_identity(&owned).expect("the foreign directory's own identity");

    // Stopped before the raw append, exactly as `nested_work.rs` does it:
    // never write to a live wirkd's own journal out from under it.
    stop_wirkd(&estate, daemon);
    raw_append(
        &estate,
        &work,
        Some(&run),
        EventKind::WorktreeCreated {
            repo: String::new(),
            base_sha: "deadbeef".to_string(),
            identity: Some(wirk_core::DirectoryIdentity {
                dev: present.dev,
                ino: present.ino,
                created: None,
            }),
        },
    );
    let (daemon, pointer) = start_wirkd(&estate);

    let (code, log) = run_deterministic(&estate, &work);
    assert_eq!(
        code,
        Some(2),
        "an indistinguishable identity must refuse execution, never reattach on the marker \
         alone: {log}"
    );
    assert!(
        log.contains("nothing was executed in it and it was not touched"),
        "the refusal must say nothing ran: {log}"
    );
    assert!(
        !owned.join("two.md").exists(),
        "the marker must not have authorized a child write: {log}"
    );
    assert_eq!(
        fs::read_to_string(owned.join("not-ours.txt")).expect("their file survives"),
        "already here\n",
        "nothing in an unproven directory is touched"
    );

    let (ok, said) = run_clean(&estate, &work);
    assert!(!ok, "cleanup must not remove it either: {said}");

    let _ = pointer;
    stop_wirkd(&estate, daemon);
}

// ---- a real Read-bound handoff, and the two disclosure gaps --------------

/// A real **document** collection — a plain directory of files, not a
/// Git repository — acquired with the estate's own `--kind
/// document-tree` policy and published.
///
/// Deliberately this and not a Git source. A document tree's
/// generations record each unit's identity and *not* its bytes (ruling
/// 0270), so an original that changes is genuinely no longer resolvable
/// at the generation a World captured — which is the availability
/// contract these checks are about, and the one a Git source, whose
/// object database keeps the old bytes, cannot exercise at all.
/// A directory genuinely **recreated** at this Work's own address, the
/// case `(dev, ino)` alone could not see (ruling 0297).
///
/// Observed in an actual estate through the frozen CLI, which is why
/// this control exists: the registered pair was `dev 64512, ino 30833`,
/// `rm -rf` plus `mkdir` at the same address returned `ino 30833`
/// again, and `wirk work clean` removed a directory this estate had not
/// created, exit 0.
///
/// This uses the sequence an operator actually types — remove, then
/// recreate — and never requires the inode number to come back. It is
/// the same control either way: whether the kernel recycles the number
/// or not, the recreated directory is not the registered object and
/// neither execution nor removal may treat it as one. Which of the two
/// actually happened on this run is reported in the assertion messages,
/// so a reader can see that the reused-inode case is the one being
/// exercised when it occurs.
#[test]
fn a_directory_recreated_at_this_address_is_refused_however_the_inode_falls() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    let (daemon, pointer) = start_wirkd(&estate);

    let (work, _run) = submit_adhoc(
        &estate,
        &["sh", "-c", "printf 'first\\n' > report.md; exit 4"],
    );
    let (code, log) = run_deterministic(&estate, &work);
    assert_eq!(code, Some(5), "the first attempt fails by design: {log}");
    let owned = owned_dir(&estate, &work);
    let registered =
        wirk_core::directory_identity(&owned).expect("the directory this Work created");
    let saved = fs::read_to_string(owned.join(".wirk-owned")).expect("marker");

    // Exactly what an operator would do, in the order they would do it.
    fs::remove_dir_all(&owned).expect("remove the directory this Work created");
    fs::create_dir(&owned).expect("recreate a directory at the same address");
    let present = wirk_core::directory_identity(&owned).expect("the recreated directory");
    // The marker is put back too: everything a reader of the directory
    // alone could check now agrees with the genuine article.
    fs::write(owned.join(".wirk-owned"), saved).expect("restore the marker");
    fs::write(owned.join("planted.txt"), b"recreated\n").expect("planted file");
    let inode_came_back = present.ino == registered.ino && present.dev == registered.dev;
    let situation = if inode_came_back {
        "the kernel reused the registered inode number: this run exercises the observed defect \
         directly"
    } else {
        "the kernel allocated a different inode number this time: the same refusal must hold"
    };

    // Execution refuses.
    let (code, said) = retry_cli(&estate, &work);
    assert_eq!(code, Some(0), "retry: {said}");
    let (code, log) = run_deterministic(&estate, &work);
    assert_eq!(
        code,
        Some(2),
        "a recreated directory is not the one this Work created ({situation}): {log}"
    );
    assert!(
        log.contains("nothing was executed in it and it was not touched"),
        "nothing may be run in it: {log}"
    );
    assert_eq!(
        fs::read_to_string(owned.join("planted.txt")).expect("the planted file survives"),
        "recreated\n",
        "the refused directory is left exactly as it is ({situation})"
    );

    // Removal refuses, which is the half that actually lost data.
    let (ok, said) = run_clean(&estate, &work);
    assert!(
        !ok,
        "cleanup must not remove a directory recreated at this address ({situation}): {said}"
    );
    assert!(
        owned.join("planted.txt").is_file(),
        "nothing under the recreated directory was removed ({situation})"
    );

    let _ = pointer;
    stop_wirkd(&estate, daemon);
}

/// The positive half, on the same real daemon: an owned directory that
/// really is this Work's own is still disposable. The correction above
/// must not buy its refusal by refusing everything (ruling 0297).
#[test]
fn an_untouched_owned_directory_is_still_cleanly_disposable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    let (daemon, pointer) = start_wirkd(&estate);

    let (work, run) = submit_adhoc(&estate, &["sh", "-c", "printf 'done\\n' > report.md"]);
    let (code, log) = run_deterministic(&estate, &work);
    assert_eq!(code, Some(0), "{log}");
    let owned = owned_dir(&estate, &work);
    assert!(owned.is_dir());

    let claim = validated_claim(&estate, &work, &run);
    let (ok, said) = run_clean(&estate, &work);
    assert!(
        ok,
        "an owned deterministic Work must stay disposable: {said}"
    );
    assert!(!owned.exists(), "its own directory went: {said}");
    let (code, read, err) = artifact_read(&estate, &work, &run, &claim, "report.md");
    assert_eq!(code, Some(0), "and its Claim still reads: {err}");
    assert_eq!(read, b"done\n");

    let _ = pointer;
    stop_wirkd(&estate, daemon);
}

fn publish_source(estate: &Path, alias: &str, docs: &[(&str, &str)]) -> PathBuf {
    let collection = estate.join("sources").join(alias);
    fs::create_dir_all(&collection).expect("source dir");
    for (name, body) in docs {
        fs::write(collection.join(name), body).expect("write document");
    }
    let acquired = atlas_json(
        estate,
        &[
            "acquire",
            "--source",
            alias,
            "--kind",
            "document-tree",
            "--repository",
            collection.to_str().expect("utf-8 source path"),
        ],
    );
    publish_admitted(estate, alias, generation_of(&acquired));
    collection
}

/// Re-observes a published document collection and publishes what it
/// finds: a new generation of the same alias, which is how the estate's
/// publication advances past a World that was already captured.
fn refresh_source(estate: &Path, alias: &str) {
    let refreshed = atlas_json(estate, &["refresh", "--source", alias]);
    publish_admitted(estate, alias, generation_of(&refreshed));
}

fn generation_of(reply: &Value) -> String {
    reply["generation"]["generation"]
        .as_str()
        .unwrap_or_else(|| panic!("no generation id in {reply:#}"))
        .to_string()
}

/// `atlas publish` of a document generation is *admitted* work, not a
/// catalog edit: it can be refused while the estate is running
/// something else expensive, and a control that assumed success would
/// fail for a reason that has nothing to do with its subject.
fn publish_admitted(estate: &Path, alias: &str, generation: String) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let (ok, _value, err) = atlas(
            estate,
            &["publish", "--source", alias, "--generation", &generation],
        );
        if ok {
            return;
        }
        let busy = ["Busy", "AtCapacity", "Admission", "admitted", "capacity"]
            .iter()
            .any(|marker| err.contains(marker));
        assert!(busy, "atlas publish {alias}: {err}");
        assert!(
            std::time::Instant::now() < deadline,
            "atlas publish of {alias} stayed capacity-refused for 60s: {err}"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

fn atlas(estate: &Path, args: &[&str]) -> (bool, Value, String) {
    let mut full = vec!["atlas"];
    full.extend_from_slice(args);
    let output = wirk_cli()
        .args(&full)
        .args(["--estate"])
        .arg(estate)
        .arg("--json")
        .output()
        .expect("wirk atlas runs");
    (
        output.status.success(),
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).unwrap_or(Value::Null),
        String::from_utf8_lossy(&output.stderr).trim().to_string(),
    )
}

fn atlas_json(estate: &Path, args: &[&str]) -> Value {
    let (ok, value, err) = atlas(estate, args);
    assert!(ok, "wirk atlas {args:?}: {err}");
    value
}

/// A two-stage Route: a Deterministic stage that really collects the
/// bytes of an admitted document, then an Actor stage that reads what
/// it claimed.
fn collect_then_actor_route(estate: &Path, source_file: &Path) -> PathBuf {
    let dir = estate.join("routes");
    fs::create_dir_all(&dir).expect("routes dir");
    let path = dir.join("collect_then_actor.json");
    let command = format!("cat {} > prepared.md", source_file.display());
    fs::write(
        &path,
        serde_json::to_string_pretty(&serde_json::json!({
            "id": "collect-then-actor",
            "waypoints": [
                {
                    "id": "collect-then-actor/collect",
                    "kind": "Deterministic",
                    "command": ["sh", "-c", command],
                    "declared_outputs": [{"name": "prepared.md", "required": true}],
                    "boundary": []
                },
                {
                    "id": "collect-then-actor/draft",
                    "kind": "Actor",
                    "intent": "read the prepared evidence and claim a cited draft",
                    "declared_outputs": [{"name": "draft.md", "required": true}],
                    "boundary": []
                }
            ]
        }))
        .expect("route serializes"),
    )
    .expect("write route");
    path
}

/// A single oriented Actor stage on an output-only basis: the shape
/// that is actually delivered a World, and therefore the shape a
/// current search has a captured basis to diverge from.
fn oriented_actor_route(estate: &Path) -> PathBuf {
    let dir = estate.join("routes");
    fs::create_dir_all(&dir).expect("routes dir");
    let path = dir.join("oriented_actor.json");
    fs::write(
        &path,
        serde_json::to_string_pretty(&serde_json::json!({
            "id": "oriented-actor",
            "waypoints": [
                {
                    "id": "oriented-actor/draft",
                    "kind": "Actor",
                    "intent": "read the admitted handbook and claim a cited draft",
                    "declared_outputs": [{"name": "draft.md", "required": true}],
                    "boundary": [],
                    "orient": {
                        "question": "What does the handbook say about retention?",
                        "sources": ["handbook"]
                    }
                }
            ]
        }))
        .expect("route serializes"),
    )
    .expect("write route");
    path
}

fn submit_route_output_only(
    estate: &Path,
    route: &Path,
    reference: &str,
    bindings: &[&str],
) -> (String, String, String) {
    submit_route_output_only_kind(estate, route, reference, bindings, None)
}

/// `--kind actor` is required when the Route's own first Waypoint is an
/// Actor (it selects the output-only Actor arm); a Route whose first
/// Waypoint is Deterministic must *not* carry it, since `--kind
/// deterministic` names the ad hoc Route-less shape and `--kind actor`
/// would contradict the file.
fn submit_route_output_only_kind(
    estate: &Path,
    route: &Path,
    reference: &str,
    bindings: &[&str],
    kind: Option<&str>,
) -> (String, String, String) {
    let mut command = wirk_cli();
    command
        .args(["work", "submit", "--estate"])
        .arg(estate)
        .arg("--route")
        .arg(route)
        .args(["--source-basis", "output-only"])
        .args(["--base", reference]);
    if let Some(kind) = kind {
        command.args(["--kind", kind]);
    }
    for binding in bindings {
        command.args(["--repo", binding]);
    }
    let output = command.output().expect("work submit runs");
    assert!(
        output.status.success(),
        "submit: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let (mut work, mut run, mut waypoint) = (String::new(), String::new(), String::new());
    for pair in stdout.split_whitespace().collect::<Vec<_>>().chunks(2) {
        if let [key, value] = pair {
            match *key {
                "work_id" => work = (*value).to_string(),
                "run_id" => run = (*value).to_string(),
                "waypoint" => waypoint = (*value).to_string(),
                _ => {}
            }
        }
    }
    (work, run, waypoint)
}

/// The handoff the whole owned-workflow outcome exists for: a
/// Read-bound Deterministic stage collects the real bytes of an
/// admitted document, and the Actor stage that follows reads exactly
/// those bytes back — before the Work is cleaned and after it.
///
/// The Actor stage inherits the directory the Deterministic stage
/// created and registered (rather than being reserved unmaterialized,
/// which is what it had to be while the Deterministic stage's own area
/// was the estate root), and the source original is never written to.
#[test]
fn a_read_bound_deterministic_stage_hands_its_collected_bytes_to_the_actor_stage() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    let (daemon, pointer) = start_wirkd(&estate);

    const HANDBOOK: &str = "# Handbook\n\nClaims are checked, never asserted.\n";
    let source = publish_source(&estate, "handbook", &[("claims.md", HANDBOOK)]);
    let source_file = source.join("claims.md");
    let route = collect_then_actor_route(&estate, &source_file);
    let (work, collect_run, waypoint) =
        submit_route_output_only(&estate, &route, "doc-set-1", &["handbook:read"]);
    assert_eq!(waypoint, "collect-then-actor/collect");

    let (code, log) = run_deterministic(&estate, &work);
    assert_eq!(code, Some(0), "the collect stage: {log}");

    // The Actor stage is reserved on the directory the Deterministic
    // stage already created — one execution area per Work, as one Git
    // worktree is reused across a Work's Waypoints.
    let result = status(&pointer.socket, &work);
    assert_eq!(
        result["current_waypoint"].as_str(),
        Some("collect-then-actor/draft"),
        "the mixed Route must advance: {result:#}"
    );
    let World::Actor(actor) = reserved_world(&pointer.socket, &work) else {
        panic!("the draft stage must reserve an Actor World");
    };
    assert_eq!(
        actor.worktree_path,
        owned_dir(&estate, &work),
        "the Actor stage inherits this Work's own owned directory"
    );
    assert_ne!(actor.worktree_path, estate, "never the estate root");
    let draft_run = result["run_id"]
        .as_str()
        .expect("the draft stage has a Run")
        .to_string();

    // The handoff itself: the later stage reads the earlier stage's
    // validated bytes, and they are the document's bytes.
    let collect_claim = validated_claim(&estate, &work, &collect_run);
    let (code, read, err) =
        artifact_read(&estate, &work, &draft_run, &collect_claim, "prepared.md");
    assert_eq!(
        code,
        Some(0),
        "the Actor stage must read the prepared bytes: {err}"
    );
    assert_eq!(
        read,
        HANDBOOK.as_bytes(),
        "the bytes handed on are the document's own"
    );
    let (store, path, _digest) = receipt(&estate, &work, "prepared.md");
    assert_eq!(store, "work_outputs", "an output-only Claim is snapshotted");
    assert_eq!(path, format!("claims/{collect_claim}/prepared.md"));
    assert_eq!(
        fs::read_to_string(&source_file).expect("the source original"),
        HANDBOOK,
        "collecting a document must not write to the original"
    );

    // The Actor stage claims its own managed output, and the Work
    // completes; then the owned directory is disposed of, and the
    // collected bytes are still readable from the Claim's own store.
    let outputs = wirk_cli()
        .args(["output", "dir"])
        .env("WIRK_ESTATE_ROOT", &estate)
        .env("WIRK_WORK_ID", &work)
        .env("WIRK_RUN_ID", &draft_run)
        .output()
        .expect("wirk output dir runs");
    let staging = PathBuf::from(
        String::from_utf8_lossy(&outputs.stdout)
            .trim()
            .lines()
            .last()
            .expect("wirk output dir prints a path")
            .trim(),
    );
    fs::create_dir_all(&staging).expect("staging dir");
    fs::write(
        staging.join("draft.md"),
        b"# Draft\n\nCites the handbook.\n",
    )
    .expect("stage draft");
    let claimed = wirk_cli()
        .arg("claim")
        .env("WIRK_ESTATE_ROOT", &estate)
        .env("WIRK_WORK_ID", &work)
        .env("WIRK_RUN_ID", &draft_run)
        .args(["--output", "draft.md"])
        .output()
        .expect("wirk claim runs");
    assert!(
        claimed.status.success(),
        "the Actor stage's Claim: {} {}",
        String::from_utf8_lossy(&claimed.stdout),
        String::from_utf8_lossy(&claimed.stderr)
    );
    assert_eq!(state_of(&pointer.socket, &work), "completed");

    let (ok, said) = run_clean(&estate, &work);
    assert!(ok, "a completed mixed Work must be disposable: {said}");
    assert!(!owned_dir(&estate, &work).exists());
    assert_eq!(
        fs::read_to_string(&source_file).expect("the source original"),
        HANDBOOK,
        "cleanup never reaches the sources a Work read"
    );
    let (code, read, err) =
        artifact_read(&estate, &work, &draft_run, &collect_claim, "prepared.md");
    assert_eq!(
        code,
        Some(0),
        "the collected bytes a Claim validated survive cleanup: {err}"
    );
    assert_eq!(read, HANDBOOK.as_bytes());

    stop_wirkd(&estate, daemon);
}

// ---- disclosure: what a World retains, and what a current search read ----

/// `wirk atlas search` as an actor types it, in both surfaces.
fn search(estate: &Path, work: &str, run: &str, query: &str, json: bool) -> (String, Value) {
    let mut command = wirk_cli();
    command
        .args(["atlas", "search", "--query", query])
        .env("WIRK_ESTATE_ROOT", estate)
        .env("WIRK_WORK_ID", work)
        .env("WIRK_RUN_ID", run);
    if json {
        command.arg("--json");
    }
    let output = command.output().expect("wirk atlas search runs");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let parsed = serde_json::from_str(stdout.trim()).unwrap_or(Value::Null);
    (stdout, parsed)
}

/// `wirk atlas resolve` in both surfaces: the JSON identity and the
/// plain text an actor actually reads.
fn resolve(estate: &Path, work: &str, run: &str, coordinate: &str) -> (Value, String) {
    let json = wirk_cli()
        .args(["atlas", "resolve", "--coordinate", coordinate, "--json"])
        .env("WIRK_ESTATE_ROOT", estate)
        .env("WIRK_WORK_ID", work)
        .env("WIRK_RUN_ID", run)
        .output()
        .expect("wirk atlas resolve runs");
    let text = wirk_cli()
        .args(["atlas", "resolve", "--coordinate", coordinate])
        .env("WIRK_ESTATE_ROOT", estate)
        .env("WIRK_WORK_ID", work)
        .env("WIRK_RUN_ID", run)
        .output()
        .expect("wirk atlas resolve runs");
    (
        serde_json::from_str(String::from_utf8_lossy(&json.stdout).trim()).unwrap_or(Value::Null),
        String::from_utf8_lossy(&text.stdout).into_owned(),
    )
}

/// One coordinate this Work may resolve, for the document whose path
/// ends in `needle` — taken from a real search rather than constructed.
fn coordinate_for(estate: &Path, work: &str, run: &str, query: &str, needle: &str) -> String {
    let (_text, answer) = search(estate, work, run, query, true);
    answer["hits"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .find(|hit| {
            hit["path"]
                .as_str()
                .is_some_and(|path| path.ends_with(needle))
        })
        .and_then(|hit| hit["coordinate"].as_str())
        .unwrap_or_else(|| panic!("no hit for {needle} in {answer:#}"))
        .to_string()
}

/// Ruling 0292, findings 3 and 4, in one Run because they are two halves
/// of the same honesty: what a delivered World retains, and what a
/// current search actually read.
///
/// * An unavailable `resolve` keeps the reason it already had in
///   `--json`. The plain surface used to print `outcome unavailable
///   ?:0-0` and nothing else, which is the surface an actor reads.
/// * A source that did **not** change still resolves — the refusal is
///   about the bytes that moved, never a blanket unavailability.
/// * A current `atlas search` whose publication basis differs from the
///   Run's own captured World says so. Neither basis is changed: the
///   World is not refreshed and the search is not pinned back.
///
/// Nothing here adds retention. Ruling 0270 permits changed historical
/// document bytes to be unavailable, and this asserts that they are —
/// with the reason said out loud.
#[test]
fn an_unavailable_resolve_keeps_its_reason_and_a_current_search_discloses_its_basis() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    let (daemon, pointer) = start_wirkd(&estate);

    const RETENTION: &str = "# Retention\n\nHistorical document availability is advisory.\n";
    const ONBOARDING: &str = "# Onboarding\n\nThe estate is read before it is written.\n";
    let source = publish_source(
        &estate,
        "handbook",
        &[("retention.md", RETENTION), ("onboarding.md", ONBOARDING)],
    );

    // An oriented Actor stage, so this Run really is delivered a World
    // with a captured generation vector to compare a current search
    // against — the shape the observed divergence was seen in.
    let route = oriented_actor_route(&estate);
    let (work, run, _waypoint) = submit_route_output_only_kind(
        &estate,
        &route,
        "doc-set-1",
        &["handbook:read"],
        Some("actor"),
    );

    let retention = coordinate_for(&estate, &work, &run, "retention", "retention.md");
    let onboarding = coordinate_for(&estate, &work, &run, "estate", "onboarding.md");

    // Both resolve while both originals stand.
    let (json, text) = resolve(&estate, &work, &run, &retention);
    assert_eq!(json["outcome"].as_str(), Some("resolved"), "{json:#}");
    assert!(text.contains("outcome resolved"), "{text}");

    // One original changes; the other does not.
    fs::write(
        source.join("retention.md"),
        "# Retention\n\nHistorical document availability is enforced.\n",
    )
    .expect("edit the original");

    let (json, text) = resolve(&estate, &work, &run, &retention);
    assert_eq!(
        json["outcome"].as_str(),
        Some("unavailable"),
        "a changed original is honestly unavailable (ruling 0270): {json:#}"
    );
    let detail = json["detail"]
        .as_str()
        .expect("the JSON reply carries the reason it always carried")
        .to_string();
    assert!(!detail.is_empty(), "{json:#}");
    assert!(
        text.contains(&detail),
        "the plain surface must carry the same reason, not drop it: {text} / {detail}"
    );
    assert!(
        text.contains("retention"),
        "and must say what a World actually retains: {text}"
    );

    let (json, text) = resolve(&estate, &work, &run, &onboarding);
    assert_eq!(
        json["outcome"].as_str(),
        Some("resolved"),
        "an unchanged source is unaffected: {json:#}"
    );
    assert!(text.contains("outcome resolved"), "{text}");

    // -- the current search's own basis --------------------------------
    // Before anything is republished, the current publication is the one
    // this Run's World was captured at.
    let (text, answer) = search(&estate, &work, &run, "retention", true);
    let _ = text;
    if answer["captured_basis"].is_object() {
        assert_eq!(
            answer["captured_basis"]["state"].as_str(),
            Some("matches"),
            "nothing has moved yet: {answer:#}"
        );
    }

    // Re-observe the changed collection and publish it: the estate's
    // publication advances, and this Run's delivered World does not.
    refresh_source(&estate, "handbook");

    let (text, answer) = search(&estate, &work, &run, "retention", true);
    let _ = text;
    let basis = &answer["captured_basis"];
    assert!(
        basis.is_object(),
        "a Work with a delivered World compares its basis: {answer:#}"
    );
    assert_eq!(
        basis["state"].as_str(),
        Some("diverges"),
        "the current search read a publication this Run's World was not captured at: {answer:#}"
    );
    assert!(
        basis["current_publication_revision"].as_u64()
            > basis["captured_publication_revision"].as_u64(),
        "{basis:#}"
    );
    let (text, _) = search(&estate, &work, &run, "retention", false);
    assert!(
        text.contains("captured basis diverges"),
        "the plain surface an actor reads must say it: {text}"
    );

    let _ = pointer;
    stop_wirkd(&estate, daemon);
}
