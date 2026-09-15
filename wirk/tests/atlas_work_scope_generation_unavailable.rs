//! An unreadable published generation, seen from inside a real Work.
//!
//! The whole-estate tests in `atlas_status_generation_unavailable.rs`
//! establish that one broken source no longer aborts the call. Under a
//! Work scope there is a second contract on top of that one: a source
//! the Work was never granted must not become visible *because* it is
//! broken. A per-source error, a count, or a coverage flag that leaks
//! out of a source the caller cannot search would disclose exactly what
//! the admission rule exists to withhold.
//!
//! Real daemon, real `git`, real `wirk work submit`, real CLI — the
//! Work's admission is the one its own journalled repository bindings
//! give it, never a grant set this test hands the daemon.

use wirk::wirkd;

#[path = "support/nested_harness.rs"]
mod harness;
#[path = "support/route_fixture.rs"]
mod route_fixture;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use harness::*;

/// An operator's call: this test binary may itself run inside a Wirk
/// pane, and an inherited triple would otherwise decide the scope of
/// the very verb under test.
fn operator_cli() -> Command {
    let mut command = wirk_cli();
    command
        .env_remove("WIRK_ESTATE_ROOT")
        .env_remove("WIRK_WORK_ID")
        .env_remove("WIRK_RUN_ID");
    command
}

fn atlas_json(command: &mut Command, args: &[&str], estate: &Path) -> (bool, serde_json::Value) {
    let mut full = vec!["atlas"];
    full.extend_from_slice(args);
    full.push("--estate");
    full.push(estate.to_str().expect("estate path is utf-8"));
    full.push("--json");
    let output = command.args(&full).output().expect("wirk atlas runs");
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let value = serde_json::from_str(&stdout).unwrap_or(serde_json::Value::Null);
    if value.is_null() {
        return (
            output.status.success(),
            serde_json::json!({ "stderr": stderr }),
        );
    }
    (output.status.success(), value)
}

fn operator(estate: &Path, args: &[&str]) -> (bool, serde_json::Value) {
    atlas_json(&mut operator_cli(), args, estate)
}

/// The same call an actor makes: the injected triple and nothing else,
/// exactly as `wirk run` sets it in a pane.
fn as_actor(estate: &Path, work: &str, args: &[&str]) -> (bool, serde_json::Value) {
    let mut command = wirk_cli();
    command
        .env("WIRK_ESTATE_ROOT", estate)
        .env("WIRK_WORK_ID", work)
        .env("WIRK_RUN_ID", "run-fixture");
    atlas_json(&mut command, args, estate)
}

struct Fixture {
    _dir: tempfile::TempDir,
    estate: PathBuf,
    repo: PathBuf,
    broken_generation: String,
}

fn publish(estate: &Path, alias: &str, docs: &Path, body: &str) -> String {
    fs::create_dir_all(docs).expect("create docs");
    fs::write(docs.join("note.md"), body).expect("write note");
    let (ok, acquired) = operator(
        estate,
        &[
            "acquire",
            "--source",
            alias,
            "--repository",
            docs.to_str().unwrap(),
            "--kind",
            "document-tree",
        ],
    );
    assert!(ok, "acquire {alias}: {acquired}");
    let generation = acquired["generation"]["generation"]
        .as_str()
        .expect("staged generation")
        .to_string();
    let (ok, published) = operator(
        estate,
        &["publish", "--source", alias, "--generation", &generation],
    );
    assert!(ok, "publish {alias}: {published}");
    generation
}

/// One estate with two published document sources — `healthy` and
/// `broken` — and a Git repository a Work can bind to. `broken`'s
/// published generation is left readable; each test breaks it itself.
fn fixture() -> (Fixture, KillOnDrop) {
    let dir = tempfile::tempdir().expect("temp dir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(estate.join(".wirk")).expect("create estate");
    let pool = dir.path().join("host-pool");
    fs::write(
        estate.join(".wirk").join("resources.json"),
        serde_json::json!({ "host_pool_dir": pool.to_string_lossy() }).to_string(),
    )
    .expect("write resources.json");
    route_fixture::install_route_fixture(&estate, "smoke");
    let (wirkd, _pointer) = start_wirkd(&estate);

    let repo = dir.path().join("source-repo");
    init_repo(&repo);

    publish(
        &estate,
        "healthy",
        &dir.path().join("docs-healthy"),
        "the healthy estate boundary concept lives here\n",
    );
    let broken_generation = publish(
        &estate,
        "broken",
        &dir.path().join("docs-broken"),
        "the broken estate boundary concept lives here too\n",
    );

    (
        Fixture {
            _dir: dir,
            estate,
            repo,
            broken_generation,
        },
        wirkd,
    )
}

/// Leaves `broken`'s published generation unreadable in place, and
/// returns the bytes needed to put it back.
fn break_generation(fixture: &Fixture) -> (PathBuf, Vec<u8>) {
    let manifest = fixture
        .estate
        .join("atlas")
        .join("generations")
        .join(&fixture.broken_generation)
        .join("manifest.json");
    let bytes = fs::read(&manifest).expect("read the real manifest");
    fs::write(&manifest, b"{ not json at all").expect("break the manifest");
    (manifest, bytes)
}

fn sources(status: &serde_json::Value) -> &Vec<serde_json::Value> {
    status["sources"]
        .as_array()
        .unwrap_or_else(|| panic!("status has no sources array: {status}"))
}

fn search(estate: &Path, work: &str, extra: &[&str]) -> serde_json::Value {
    let mut args = vec!["search", "--query", "boundary concept"];
    args.extend_from_slice(extra);
    let (ok, answer) = as_actor(estate, work, &args);
    assert!(ok, "search under {work}: {answer}");
    answer
}

/// A Work granted only the healthy source. The broken source is
/// unreadable and entirely outside this Work's admission, so nothing
/// about it — not an error string, not a count, not a coverage flag —
/// may reach this caller, and its own results must be exactly what they
/// would be on a wholly healthy estate.
#[test]
fn an_ungranted_broken_source_neither_shows_nor_poisons_a_works_own_results() {
    let (fx, wirkd) = fixture();
    let submitted = submit(&fx.estate, "smoke", &fx.repo, &["healthy:read"], None).expect("submit");
    let (_manifest, _bytes) = break_generation(&fx);

    // ---- status --------------------------------------------------------
    let (ok, status) = as_actor(&fx.estate, &submitted.work_id, &["status"]);
    assert!(ok, "a Work-scoped status must answer: {status}");
    assert_eq!(
        status["sources_total"].as_u64(),
        Some(1),
        "only the granted source is counted; a broken source the Work cannot search must not \
         appear in its total: {status}"
    );
    let rows = sources(&status);
    assert_eq!(rows.len(), 1, "{status}");
    assert_eq!(rows[0]["membership"]["alias"].as_str(), Some("healthy"));
    assert!(
        rows[0]["published_generation"]["generation"].is_string()
            && rows[0].get("generation_error").is_none(),
        "the granted source is healthy and its row must be ordinary: {status}"
    );
    let rendered = status.to_string();
    assert!(
        !rendered.contains("broken") && !rendered.contains(&fx.broken_generation),
        "neither the ungranted alias nor its generation id may appear anywhere in this \
         answer: {status}"
    );

    // ---- search --------------------------------------------------------
    let answer = search(&fx.estate, &submitted.work_id, &[]);
    assert_eq!(
        answer["hits"].as_array().map(Vec::len),
        Some(1),
        "the granted source's own hit: {answer}"
    );
    assert_eq!(
        answer["coverage"]["complete"].as_bool(),
        Some(true),
        "this Work's corpus was read in full; another Work's broken source is not a hole in \
         it: {answer}"
    );
    assert_eq!(
        answer["coverage"]["generation_unavailable"].as_bool(),
        Some(false),
        "a source outside this scope going unread is not this answer's coverage: {answer}"
    );
    assert!(
        !answer.to_string().contains(&fx.broken_generation),
        "{answer}"
    );

    // ---- naming the ungranted source explicitly ------------------------
    // The pre-existing withholding contract, unchanged by any of this: a
    // name this scope is not admitted to reads as a name that is not
    // there, and a broken one must not answer differently from a healthy
    // one — that difference would itself be the disclosure.
    let (ok, denied) = as_actor(
        &fx.estate,
        &submitted.work_id,
        &["status", "--source", "broken"],
    );
    assert!(ok, "{denied}");
    assert_eq!(
        denied["admitted"].as_bool(),
        Some(false),
        "an ungranted source is not admitted, whatever state its data is in: {denied}"
    );
    assert_eq!(
        denied["registered"],
        serde_json::Value::Null,
        "a Work-scoped call never answers whether an unadmitted alias is registered: {denied}"
    );
    assert!(
        sources(&denied).is_empty() && !denied.to_string().contains(&fx.broken_generation),
        "no row, no reason, no generation id: {denied}"
    );

    let refused = search(&fx.estate, &submitted.work_id, &["--source", "broken"]);
    assert_eq!(
        refused["coverage"]["denied"].as_bool(),
        Some(true),
        "an ungranted source named explicitly is a denial: {refused}"
    );
    assert_eq!(
        refused["coverage"]["no_match"].as_bool(),
        Some(false),
        "a denial is never a searched-and-empty corpus: {refused}"
    );
    assert_eq!(
        refused["coverage"]["generation_unavailable"].as_bool(),
        Some(false),
        "the refusal must read the same whether the denied source is healthy or broken: \
         {refused}"
    );

    stop_wirkd(&fx.estate, wirkd);
}

/// The other half: a Work that *is* granted the broken source. Here the
/// unreadable generation is this caller's own business, so it is
/// disclosed on its row and in its coverage — and repairing the source
/// returns the Work to ordinary operation without anything being
/// re-derived.
#[test]
fn a_granted_source_whose_generation_is_unreadable_is_disclosed_to_that_work() {
    let (fx, wirkd) = fixture();
    let submitted = submit(&fx.estate, "smoke", &fx.repo, &["broken:read"], None).expect("submit");
    let (manifest, manifest_bytes) = break_generation(&fx);

    let (ok, status) = as_actor(&fx.estate, &submitted.work_id, &["status"]);
    assert!(ok, "{status}");
    assert_eq!(status["sources_total"].as_u64(), Some(1), "{status}");
    let row = &sources(&status)[0];
    assert_eq!(row["membership"]["alias"].as_str(), Some("broken"));
    assert!(
        row["published_generation"].is_null() && row["generation_error"].as_str().is_some(),
        "the source this Work is admitted to cannot be read, and its own row says so: {row}"
    );

    let answer = search(&fx.estate, &submitted.work_id, &[]);
    assert!(
        answer["hits"]
            .as_array()
            .is_some_and(|hits| hits.is_empty()),
        "nothing readable was admitted: {answer}"
    );
    assert_eq!(
        answer["coverage"]["generation_unavailable"].as_bool(),
        Some(true),
        "{answer}"
    );
    assert_eq!(
        answer["coverage"]["no_match"].as_bool(),
        Some(false),
        "the corpus was never read, so this is missing evidence, not proven absence: {answer}"
    );
    assert_eq!(
        answer["coverage"]["denied"].as_bool(),
        Some(false),
        "this Work was granted the source; the failure is the data, not the admission: {answer}"
    );
    assert_eq!(
        answer["coverage"]["complete"].as_bool(),
        Some(false),
        "{answer}"
    );

    // ---- restoration returns ordinary operation ------------------------
    fs::write(&manifest, &manifest_bytes).expect("restore the manifest");
    let (ok, recovered) = as_actor(&fx.estate, &submitted.work_id, &["status"]);
    assert!(ok, "{recovered}");
    assert_eq!(
        sources(&recovered)[0]["published_generation"]["generation"].as_str(),
        Some(fx.broken_generation.as_str()),
        "the same generation this fixture published, not a new one: {recovered}"
    );
    let answer = search(&fx.estate, &submitted.work_id, &[]);
    assert_eq!(answer["hits"].as_array().map(Vec::len), Some(1), "{answer}");
    assert_eq!(
        answer["coverage"]["complete"].as_bool(),
        Some(true),
        "{answer}"
    );
    assert_eq!(
        answer["generations"][0]["generation"].as_str(),
        Some(fx.broken_generation.as_str()),
        "the repaired source is read at the identity it always published: {answer}"
    );

    stop_wirkd(&fx.estate, wirkd);
}
