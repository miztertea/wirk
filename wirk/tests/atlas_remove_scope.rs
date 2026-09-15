//! `wirk atlas remove` under a caller's own scope.
//!
//! Real daemon, real `git`, real `wirk work submit`, real CLI — the same
//! discipline `estate_storage.rs` keeps, because the thing under test is
//! precisely what the wire path decides and a library call would skip.
//!
//! **What was actually confirmed before these were written.** The CLI's
//! `remove_command` accepted no caller-scope flag at all
//! (`--requesting-work`/`--admin`, which its sibling `refresh_command`
//! has resolved through `crate::resolve_scope` since ruling 0117),
//! `AtlasRemovePayload` carried only `source`, and `handle_atlas_remove`
//! disclosed its retention refusals at
//! `Disclosure::Administrative` unconditionally. So an actor executing
//! inside one Work removed any registered source's membership in its
//! estate, admitted to it or not, and the daemon never learned who
//! asked.
//!
//! **What these pin.** The admission rule `atlas status --work` and
//! every other Work-scoped query already apply — a grant must name the
//! membership's alias — now applied to this verb, and the disclosure
//! level that follows from the caller.

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
/// pane, and under ruling 0117 an inherited triple would decide the
/// default scope of the very verb under test.
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
        // A refusal renders to stderr; keep it visible to the assertion.
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
    docs: PathBuf,
}

/// One estate with its own host pool, a Git repository a Work can bind
/// to, and a published document source named `docs`.
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
    // `init_repo` leaves a real HEAD behind, which is all `work submit`
    // needs of it here.
    init_repo(&repo);

    let docs = dir.path().join("docs");
    fs::create_dir_all(&docs).expect("create docs");
    fs::write(docs.join("brief.md"), "# Client brief\n\nHello.\n").expect("write brief");
    let (ok, acquired) = operator(
        &estate,
        &[
            "acquire",
            "--source",
            "docs",
            "--repository",
            docs.to_str().unwrap(),
            "--kind",
            "document-tree",
        ],
    );
    assert!(ok, "acquire: {acquired}");
    let generation = acquired["generation"]["generation"]
        .as_str()
        .expect("staged generation")
        .to_string();
    let (ok, published) = operator(
        &estate,
        &["publish", "--source", "docs", "--generation", &generation],
    );
    assert!(ok, "publish: {published}");

    (
        Fixture {
            _dir: dir,
            estate,
            repo,
            docs,
        },
        wirkd,
    )
}

fn registered(estate: &Path, source: &str) -> bool {
    let (ok, status) = operator(estate, &["status", "--source", source]);
    assert!(ok, "status: {status}");
    status["registered"].as_bool().unwrap_or(false)
}

/// **The reproduction.** An actor bound to `repo` asks to remove `docs`,
/// a source nothing in its Work admits it to. Watched red against the
/// pre-change binary: the membership was gone and the reply said
/// `removed`.
#[test]
fn an_actor_cannot_remove_a_source_its_own_work_does_not_admit() {
    let (fx, wirkd) = fixture();
    let submitted = submit(&fx.estate, "smoke", &fx.repo, &["repo:read"], None).expect("submit");

    let (ok, reply) = as_actor(
        &fx.estate,
        &submitted.work_id,
        &["remove", "--source", "docs"],
    );
    assert!(
        !ok,
        "an unadmitted source must not be removable by a bound actor: {reply}"
    );
    assert!(
        registered(&fx.estate, "docs"),
        "and the membership must still be registered afterwards"
    );
    // Told the way every other Work-scoped query tells it: a name this
    // caller is not admitted to reads as a name that is not there, so
    // the verb cannot be used to probe the catalog.
    let told = reply.to_string();
    assert!(
        told.contains("UnknownSource") || told.contains("no registered source"),
        "an unadmitted name answers as an unknown one, disclosing no membership: {reply}"
    );
    // The source's own files were never in question, and still are not.
    assert_eq!(
        fs::read_to_string(fx.docs.join("brief.md")).unwrap(),
        "# Client brief\n\nHello.\n"
    );

    stop_wirkd(&fx.estate, wirkd);
}

/// The operator's own control, said out loud. `--admin` from inside an
/// actor context is the deliberate administrative removal ruling 0117
/// preserves; it is now spellable on this verb at all, which it was not.
#[test]
fn an_explicit_admin_removal_from_inside_an_actor_context_is_allowed_and_disclosed() {
    let (fx, wirkd) = fixture();
    let submitted = submit(&fx.estate, "smoke", &fx.repo, &["repo:read"], None).expect("submit");

    let mut command = wirk_cli();
    command
        .env("WIRK_ESTATE_ROOT", &fx.estate)
        .env("WIRK_WORK_ID", &submitted.work_id)
        .env("WIRK_RUN_ID", "run-fixture");
    let output = command
        .args([
            "atlas",
            "remove",
            "--source",
            "docs",
            "--admin",
            "--estate",
            fx.estate.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("wirk atlas runs");
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    assert!(
        output.status.success(),
        "an explicit administrative removal is allowed: {stdout} {stderr}"
    );
    let reply: serde_json::Value = serde_json::from_str(&stdout).expect("json reply");
    assert_eq!(reply["outcome"].as_str(), Some("removed"), "{reply}");
    assert!(
        stderr.contains("--admin named"),
        "and an administrative act from inside an actor context is said out loud: {stderr}"
    );
    assert!(!registered(&fx.estate, "docs"));

    stop_wirkd(&fx.estate, wirkd);
}

/// The admitted case, encoding the existing alias rule and nothing
/// stricter: a Work whose own binding names `docs` reaches it.
#[test]
fn an_actor_whose_binding_names_the_source_reaches_it() {
    let (fx, wirkd) = fixture();
    let submitted = submit(&fx.estate, "smoke", &fx.repo, &["docs:write"], None).expect("submit");

    let (ok, reply) = as_actor(
        &fx.estate,
        &submitted.work_id,
        &["remove", "--source", "docs"],
    );
    assert!(ok, "a granted alias is admitted: {reply}");
    assert_eq!(reply["outcome"].as_str(), Some("removed"), "{reply}");
    assert!(!registered(&fx.estate, "docs"));

    stop_wirkd(&fx.estate, wirkd);
}
