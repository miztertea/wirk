//! P6.7 (ruling 0393): an estate owner explicitly selects scoped
//! doctrine documents, and a reservation binds them.
//!
//! Real daemon, real `git`, real `wirk work submit`, real CLI — never a
//! library call for anything the CLI exposes, the discipline
//! `estate_storage.rs` and `work_clean.rs` already keep.
//!
//! **What these pin, and what they do not.** They pin the contract: an
//! owner's declaration reaches a reservation, the reservation fixes an
//! identity a bound Run reads back, a later change of declaration moves
//! the *next* reservation and not a bound one, a broken document refuses
//! by name and recovers, an estate that declared nothing is unchanged,
//! and a Work-scoped caller can neither change the selection nor be told
//! about documents it has no binding for. What they are not is the
//! actual-use qualification — whether a real model actually operates
//! under the delivered rules is a separate stage, and ruling 0040 is why
//! both exist rather than either alone.

use wirk::wirkd;

#[path = "support/nested_harness.rs"]
mod harness;
#[path = "support/route_fixture.rs"]
mod route_fixture;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use harness::*;

/// Every CLI call here is an **operator's**, with this test process's
/// own injected execution triple removed — a test binary run inside a
/// Wirk pane inherits one, and under ruling 0117 it would decide the
/// default scope of the very verbs under test.
fn cli() -> Command {
    let mut command = wirk_cli();
    command
        .env_remove("WIRK_ESTATE_ROOT")
        .env_remove("WIRK_WORK_ID")
        .env_remove("WIRK_RUN_ID");
    command
}

fn run(args: &[&str]) -> (bool, String, String) {
    let output = cli().args(args).output().expect("wirk runs");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

fn json(args: &[&str]) -> serde_json::Value {
    let (ok, stdout, stderr) = run(args);
    assert!(ok, "{args:?} failed: {stdout}{stderr}");
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|err| panic!("{args:?} did not print JSON ({err}): {stdout:?}"))
}

struct Fixture {
    _dir: tempfile::TempDir,
    estate: PathBuf,
    repo: PathBuf,
    /// The owner's own documents, deliberately **outside** the estate
    /// and outside every worktree: nothing here is an ancestor of
    /// anything wirk creates, which is exactly the arrangement no
    /// ancestor-file convention can serve.
    documents: PathBuf,
}

impl Fixture {
    fn estate_arg(&self) -> &str {
        self.estate.to_str().expect("estate path is utf-8")
    }

    fn document(&self, name: &str) -> String {
        self.documents
            .join(name)
            .to_str()
            .expect("utf-8")
            .to_string()
    }

    /// `wirk world show`, asked as the actor of this Run would ask it:
    /// the injected triple and nothing else, which is the only door that
    /// verb has.
    fn world_show(&self, work: &str, run_id: &str) -> serde_json::Value {
        let output = wirk_cli()
            .args(["world", "show", "--json"])
            .env("WIRK_ESTATE_ROOT", &self.estate)
            .env("WIRK_WORK_ID", work)
            .env("WIRK_RUN_ID", run_id)
            .output()
            .expect("wirk world show runs");
        assert!(
            output.status.success(),
            "world show failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).expect("world show prints JSON")
    }
}

fn fixture() -> (Fixture, KillOnDrop) {
    let dir = tempfile::tempdir().expect("temp dir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(estate.join(".wirk")).expect("create estate");
    route_fixture::install_route_fixture(&estate, "smoke");

    let documents = dir.path().join("owner-documents");
    fs::create_dir_all(&documents).expect("documents dir");
    fs::write(
        documents.join("house-rules.md"),
        "# House rules\n\nCite the rung that resolved each decision.\n",
    )
    .expect("house rules");
    fs::write(
        documents.join("alpha-rules.md"),
        "# The alpha repository\n\nNever land on main without a green gate.\n",
    )
    .expect("alpha rules");

    let (wirkd, _pointer) = start_wirkd(&estate);

    let repo = dir.path().join("source-repo");
    init_repo(&repo);
    write_file(&repo, "alpha.rs", "fn alpha() {}\n");
    commit_all(&repo, "content");

    (
        Fixture {
            _dir: dir,
            estate,
            repo,
            documents,
        },
        wirkd,
    )
}

fn commit_all(repo: &Path, message: &str) {
    for args in [
        vec!["add", "-A"],
        vec![
            "-c",
            "user.name=estate-doctrine-test",
            "-c",
            "user.email=estate-doctrine@example.test",
            "commit",
            "-q",
            "-m",
            message,
        ],
    ] {
        let status = Command::new("git")
            .args(&args)
            .current_dir(repo)
            .status()
            .expect("git runs");
        assert!(status.success(), "git {args:?}");
    }
}

fn submit_actor(fixture: &Fixture) -> Submitted {
    submit_kind(
        &fixture.estate,
        "smoke",
        &fixture.repo,
        &["alpha:write"],
        None,
        Some("actor"),
    )
    .expect("submit an Actor Work")
}

fn doctrine_of(world: &serde_json::Value) -> Vec<(String, String, String)> {
    world["doctrine"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|document| {
            (
                document["id"].as_str().unwrap_or("").to_string(),
                document["version"].as_str().unwrap_or("").to_string(),
                document["digest"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect()
}

fn sha256_of_file(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(fs::read(path).expect("read"));
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

// ---------------------------------------------------------------------
// The decisive relation
// ---------------------------------------------------------------------

/// **The decisive one, and the meaningful red.** An owner selects a
/// document that lives nowhere near the repository — not in it, not
/// above it, not in the estate — through the public CLI, and the very
/// next Actor reservation binds it: named by the owner's own id and
/// version, identified by the digest of the exact bytes, stored durably
/// in the estate, and readable back through the bound public World
/// interface the actor itself uses.
///
/// Watched red at b0972dc: `wirk estate doctrine` does not exist there,
/// so the verb fails outright and no reservation can carry a separately
/// identified governing document.
#[test]
fn an_owner_selects_a_document_outside_every_ancestry_and_the_next_reservation_binds_it() {
    let (fixture, wirkd) = fixture();

    let declared = json(&[
        "estate",
        "doctrine",
        "set",
        "--estate",
        fixture.estate_arg(),
        "--id",
        "house-rules",
        "--path",
        &fixture.document("house-rules.md"),
        "--version",
        "2026-09-15.1",
        "--admin",
        "--json",
    ]);
    assert_eq!(declared["id"], "house-rules");
    assert_eq!(declared["replaced"], false);
    let expected_digest = declared["digest"].as_str().expect("a digest").to_string();
    assert_eq!(
        expected_digest,
        sha256_of_file(&fixture.documents.join("house-rules.md")),
        "the identity is the digest of the owner's own bytes, not a label"
    );

    let submitted = submit_actor(&fixture);
    let world = fixture.world_show(&submitted.work_id, &submitted.run_id);
    assert_eq!(
        doctrine_of(&world),
        vec![(
            "house-rules".to_string(),
            "2026-09-15.1".to_string(),
            expected_digest.clone()
        )],
        "the bound Run reads back exactly what its reservation fixed: {world}"
    );

    // The bytes are in the estate's own store, and they are the owner's
    // bytes — copied for durability, never moved or rewritten.
    let stored = fixture
        .estate
        .join(".wirk")
        .join("doctrine")
        .join(format!("{expected_digest}.md"));
    assert_eq!(
        fs::read_to_string(&stored).expect("stored bytes"),
        fs::read_to_string(fixture.documents.join("house-rules.md")).expect("owner's bytes"),
    );
    assert!(
        fixture.documents.join("house-rules.md").exists(),
        "the owner's own file is never consumed"
    );

    // And it is a measured, cleanable class of the estate's — not an
    // invisible directory that grows forever.
    let storage = json(&[
        "estate",
        "storage",
        "--estate",
        fixture.estate_arg(),
        "--admin",
        "--json",
    ]);
    let class = storage["classes"]
        .as_array()
        .expect("classes")
        .iter()
        .find(|entry| entry["class"] == "doctrine")
        .unwrap_or_else(|| panic!("no doctrine class: {storage}"));
    assert_eq!(class["retained_items"].as_u64(), Some(1));
    assert_eq!(class["removable_items"].as_u64(), Some(0));

    stop_wirkd(&fixture.estate, wirkd);
}

/// A deliberate change reaches the **next** applicable reservation and
/// leaves a bound one exactly where it was. This is the property that
/// makes doctrine a contract rather than a mutable global: an owner can
/// improve the rules without rewriting what a Run already in flight was
/// told it operates under.
#[test]
fn a_changed_declaration_moves_the_next_reservation_and_never_a_bound_one() {
    let (fixture, wirkd) = fixture();
    let set = |version: &str| {
        json(&[
            "estate",
            "doctrine",
            "set",
            "--estate",
            fixture.estate_arg(),
            "--id",
            "house-rules",
            "--path",
            &fixture.document("house-rules.md"),
            "--version",
            version,
            "--admin",
            "--json",
        ])
    };
    set("v1");
    let bound = submit_actor(&fixture);
    let before = doctrine_of(&fixture.world_show(&bound.work_id, &bound.run_id));
    assert_eq!(before[0].1, "v1");
    let first_digest = before[0].2.clone();

    // The owner edits the document *and* re-declares it under a new
    // version — both halves of a real change.
    fs::write(
        fixture.documents.join("house-rules.md"),
        "# House rules\n\nCite the rung, and name what you did not check.\n",
    )
    .expect("edit");
    let replaced = set("v2");
    assert_eq!(replaced["replaced"], true);
    assert_ne!(
        replaced["digest"].as_str().unwrap(),
        first_digest,
        "editing the bytes changes the content identity"
    );

    // The bound Run is untouched, down to the digest.
    assert_eq!(
        doctrine_of(&fixture.world_show(&bound.work_id, &bound.run_id)),
        before,
        "an owner's later change must not rewrite what a bound Run operates under"
    );

    // The next reservation binds the new one.
    let next = submit_actor(&fixture);
    let after = doctrine_of(&fixture.world_show(&next.work_id, &next.run_id));
    assert_eq!(after[0].1, "v2");
    assert_ne!(after[0].2, first_digest);

    // Both sets of bytes are still readable — the older Run's reference
    // is a name that still resolves, not a dangling one.
    for (_, _, digest) in before.iter().chain(after.iter()) {
        assert!(
            fixture
                .estate
                .join(".wirk")
                .join("doctrine")
                .join(format!("{digest}.md"))
                .exists(),
            "every bound digest stays resolvable: {digest}"
        );
    }

    stop_wirkd(&fixture.estate, wirkd);
}

/// A declared document that cannot be read refuses the reservation by
/// name, and says how to recover. Two recoveries work, and the estate is
/// never left with a Work reserved under rules nobody can produce.
#[test]
fn a_broken_declaration_refuses_the_reservation_by_name_and_recovers() {
    let (fixture, wirkd) = fixture();
    json(&[
        "estate",
        "doctrine",
        "set",
        "--estate",
        fixture.estate_arg(),
        "--id",
        "house-rules",
        "--path",
        &fixture.document("house-rules.md"),
        "--admin",
        "--json",
    ]);
    fs::remove_file(fixture.documents.join("house-rules.md")).expect("remove the owner's file");

    let refusal = submit_kind(
        &fixture.estate,
        "smoke",
        &fixture.repo,
        &["alpha:write"],
        None,
        Some("actor"),
    )
    .expect_err("a document that cannot be read must refuse the reservation");
    assert!(
        refusal.contains("house-rules"),
        "the refusal names the document: {refusal}"
    );
    assert!(
        refusal.contains("estate doctrine remove --id house-rules"),
        "…and names a recovery the owner can run: {refusal}"
    );

    // Recovery one: put the document back.
    fs::write(
        fixture.documents.join("house-rules.md"),
        "# House rules\n\nBack again.\n",
    )
    .expect("restore");
    let restored = submit_actor(&fixture);
    assert_eq!(
        doctrine_of(&fixture.world_show(&restored.work_id, &restored.run_id)).len(),
        1
    );

    // Recovery two: undeclare it. Reservations go back to being what
    // they were before any doctrine existed.
    fs::remove_file(fixture.documents.join("house-rules.md")).expect("remove again");
    let removed = json(&[
        "estate",
        "doctrine",
        "remove",
        "--estate",
        fixture.estate_arg(),
        "--id",
        "house-rules",
        "--admin",
        "--json",
    ]);
    assert_eq!(removed["removed"], true);
    let plain = submit_actor(&fixture);
    let world = fixture.world_show(&plain.work_id, &plain.run_id);
    assert!(
        world.get("doctrine").is_none(),
        "an estate that declares none reserves none: {world}"
    );

    // Removing a name that was never declared is refused, not silently
    // accepted as done.
    let (ok, _, stderr) = run(&[
        "estate",
        "doctrine",
        "remove",
        "--estate",
        fixture.estate_arg(),
        "--id",
        "never-declared",
        "--admin",
    ]);
    assert!(!ok, "removing an undeclared id must not report success");
    assert!(stderr.contains("never-declared"), "{stderr}");

    stop_wirkd(&fixture.estate, wirkd);
}

/// An estate that selected nothing behaves exactly as it always did: an
/// empty listing that says so, a reservation with no doctrine, and no
/// store on disk. The ordinary case must not acquire a cost or a
/// ceremony because the feature exists.
#[test]
fn an_estate_that_declares_nothing_is_unchanged() {
    let (fixture, wirkd) = fixture();
    let listed = json(&[
        "estate",
        "doctrine",
        "list",
        "--estate",
        fixture.estate_arg(),
        "--admin",
        "--json",
    ]);
    assert_eq!(listed["documents"].as_array().map(Vec::len), Some(0));

    let submitted = submit_actor(&fixture);
    let world = fixture.world_show(&submitted.work_id, &submitted.run_id);
    assert!(world.get("doctrine").is_none(), "{world}");
    assert!(
        !fixture.estate.join(".wirk").join("doctrine").exists(),
        "no store is created for an estate that selected nothing"
    );

    stop_wirkd(&fixture.estate, wirkd);
}

/// Scope, both halves. An owner's document scoped to one repository
/// binding is resolved only for Works that actually hold that binding —
/// and a Work-scoped caller can neither change the estate's selection
/// nor be told about a document that does not apply to it.
#[test]
fn caller_scope_neither_discloses_ungranted_doctrine_nor_lets_an_actor_rewrite_it() {
    let (fixture, wirkd) = fixture();
    for (id, file, repository) in [
        ("house-rules", "house-rules.md", None),
        ("beta-rules", "alpha-rules.md", Some("beta")),
    ] {
        let mut args = vec![
            "estate".to_string(),
            "doctrine".to_string(),
            "set".to_string(),
            "--estate".to_string(),
            fixture.estate_arg().to_string(),
            "--id".to_string(),
            id.to_string(),
            "--path".to_string(),
            fixture.document(file),
            "--admin".to_string(),
            "--json".to_string(),
        ];
        if let Some(repository) = repository {
            args.push("--repository".to_string());
            args.push(repository.to_string());
        }
        let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
        json(&borrowed);
    }

    // This Work binds `alpha`, not `beta`.
    let submitted = submit_actor(&fixture);
    let world = fixture.world_show(&submitted.work_id, &submitted.run_id);
    let bound = doctrine_of(&world);
    assert_eq!(
        bound.iter().map(|d| d.0.as_str()).collect::<Vec<_>>(),
        vec!["house-rules"],
        "a document scoped to a binding this Work does not hold is not reserved for it: {world}"
    );

    // Nor is it named to it, nor is the owner's filesystem laid out for
    // it, when it asks as itself.
    let scoped = json(&[
        "estate",
        "doctrine",
        "list",
        "--estate",
        fixture.estate_arg(),
        "--requesting-work",
        &submitted.work_id,
        "--json",
    ]);
    assert_eq!(scoped["scope"], "requester");
    let ids: Vec<&str> = scoped["documents"]
        .as_array()
        .expect("documents")
        .iter()
        .map(|document| document["id"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(ids, vec!["house-rules"], "{scoped}");
    assert!(
        scoped["documents"][0].get("path").is_none(),
        "a Work is not told where on the owner's disk their documents live: {scoped}"
    );
    // The administrative caller, who is the owner, sees the whole
    // selection including where it lives.
    let administrative = json(&[
        "estate",
        "doctrine",
        "list",
        "--estate",
        fixture.estate_arg(),
        "--admin",
        "--json",
    ]);
    assert_eq!(
        administrative["documents"].as_array().map(Vec::len),
        Some(2)
    );
    assert!(administrative["documents"][0].get("path").is_some());

    // And a Work-scoped caller cannot change what it operates under.
    for action in [
        vec![
            "estate",
            "doctrine",
            "remove",
            "--estate",
            fixture.estate_arg(),
            "--id",
            "house-rules",
            "--requesting-work",
            &submitted.work_id,
        ],
        vec![
            "estate",
            "doctrine",
            "set",
            "--estate",
            fixture.estate_arg(),
            "--id",
            "house-rules",
            "--path",
            &fixture.document("alpha-rules.md"),
            "--requesting-work",
            &submitted.work_id,
        ],
    ] {
        let (ok, _, stderr) = run(&action);
        assert!(!ok, "an actor must not be able to rewrite estate doctrine");
        assert!(
            stderr.contains("AdministrativeOnly") || stderr.contains("no authority"),
            "the refusal says whose selection this is: {stderr}"
        );
    }
    // Nothing moved.
    assert_eq!(
        json(&[
            "estate",
            "doctrine",
            "list",
            "--estate",
            fixture.estate_arg(),
            "--admin",
            "--json",
        ])["documents"]
            .as_array()
            .map(Vec::len),
        Some(2)
    );

    stop_wirkd(&fixture.estate, wirkd);
}

/// **Rulings 0397/0398, the meaningful red.** An owner's rules are as
/// long as the owner's rules are. Wirk declares no size policy over
/// them, so a document far past the bound that used to exist is
/// declared, bound and stored **whole** — the reservation's digest is
/// the digest of every byte the owner wrote, and the stored bytes are
/// byte-identical to their file.
///
/// Watched red at `afd0cc0c`: `wirk estate doctrine set` refused this
/// exact file with "is 204800 bytes, over the 65536-byte limit a
/// document must stay inside to cross every delivery mechanism wirk
/// supports" — a transport's constraint stated as a rule about the
/// owner's writing. The transport's own constraint is now handled at
/// the one mechanism that has it, and refuses there by falling back to
/// disclosed prompt delivery rather than by refusing the document
/// (`wirk-herdr/tests/codex_live_composition.rs`).
#[test]
fn a_long_document_is_declared_and_bound_whole_rather_than_refused_by_size() {
    let (fixture, wirkd) = fixture();

    // Ordinary prose, repeated: what a long set of house rules looks
    // like, not one pathological token.
    let mut long = String::new();
    while long.len() < 200 * 1024 {
        long.push_str(
            "Every file you create in this estate starts with a one-line provenance comment \
             naming the Work id.\n",
        );
    }
    let path = fixture.documents.join("long-rules.md");
    fs::write(&path, &long).expect("write the owner's long document");

    let declared = json(&[
        "estate",
        "doctrine",
        "set",
        "--estate",
        fixture.estate_arg(),
        "--id",
        "long-rules",
        "--path",
        &fixture.document("long-rules.md"),
        "--version",
        "1",
        "--admin",
        "--json",
    ]);
    assert_eq!(
        declared["bytes"].as_u64(),
        Some(long.len() as u64),
        "the whole document was read, not a prefix of it: {declared}"
    );
    let expected_digest = declared["digest"].as_str().expect("a digest").to_string();
    assert_eq!(expected_digest, sha256_of_file(&path));

    let submitted = submit_actor(&fixture);
    let world = fixture.world_show(&submitted.work_id, &submitted.run_id);
    assert_eq!(
        doctrine_of(&world),
        vec![(
            "long-rules".to_string(),
            "1".to_string(),
            expected_digest.clone()
        )],
        "the bound Run reads back the digest of every byte the owner wrote: {world}"
    );

    let stored = fixture
        .estate
        .join(".wirk")
        .join("doctrine")
        .join(format!("{expected_digest}.md"));
    assert_eq!(
        fs::read_to_string(&stored).expect("stored bytes"),
        long,
        "stored whole, never truncated"
    );

    stop_wirkd(&fixture.estate, wirkd);
}
