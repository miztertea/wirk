//! P3 W-C2, the real-service half: the full selector set assembled by a
//! real `wirkd` out of a real Atlas, over real Git objects, read back
//! through the public CLI an actor actually types.
//!
//! Nothing here is a fake (ruling 0040). Every governing edge is one the
//! shipped `wirk atlas relate` admitted, every prior-stage artifact is
//! one a real `wirk claim` validated and recorded a digest for, every
//! ranked hit comes out of the same `wirk_atlas::search` the public
//! `wirk atlas search` runs, and every coordinate and handle this file
//! asserts on is followed back to committed bytes through the shipped
//! binary.

#[path = "support/nested_harness.rs"]
mod harness;
#[path = "support/read_barrier.rs"]
mod read_barrier;
#[path = "../src/wirkd/mod.rs"]
mod wirkd;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use harness::{
    KillOnDrop, claim_ok, init_repo, materialize_actor, start_wirkd, status, stop_wirkd,
    submit_kind, wirk_bin, write_file,
};
use serde_json::Value;

// ---- the estate -----------------------------------------------------------

/// Two real repositories. `demo` is what the Work binds; `other` is a
/// published, indexed source it never binds, and exists so every scope
/// assertion here has something real to fail to leak.
fn source_repo(root: &Path) -> PathBuf {
    let repo = root.join("source-repo");
    fs::create_dir_all(&repo).expect("source repo dir");
    init_repo(&repo);
    fs::create_dir_all(repo.join("src")).expect("src dir");
    fs::create_dir_all(repo.join("notes")).expect("notes dir");
    write_file(
        &repo,
        "src/server.rs",
        "pub fn claim_boundary_refusal(path: &str) -> bool {\n    path.starts_with(\"src/\")\n}\n",
    );
    write_file(
        &repo,
        "notes/boundary.md",
        "# Boundary\n\nThe governing rule: a Waypoint may write only inside its declared \
         boundary.\n",
    );
    write_file(
        &repo,
        "notes/adjudication.md",
        "# Adjudication\n\nThe evidence the boundary rule was admitted on.\n",
    );
    commit_all(&repo);
    repo
}

fn foreign_repo(root: &Path) -> PathBuf {
    let repo = root.join("other-repo");
    fs::create_dir_all(&repo).expect("other repo dir");
    init_repo(&repo);
    fs::create_dir_all(repo.join("notes")).expect("notes dir");
    write_file(
        &repo,
        "notes/quarantinedmarker.md",
        "# Elsewhere\n\nquarantinedmarker: claim_boundary_refusal is discussed here too, in a \
         source no Work below binds.\n",
    );
    commit_all(&repo);
    repo
}

fn commit_all(repo: &Path) {
    for args in [
        vec!["add", "-A"],
        vec![
            "-c",
            "user.name=assembly-test",
            "-c",
            "user.email=assembly@example.test",
            "commit",
            "-q",
            "-m",
            "content",
        ],
    ] {
        assert!(
            Command::new("git")
                .args(&args)
                .current_dir(repo)
                .env("GIT_AUTHOR_DATE", "2020-01-01T00:00:00+0000")
                .env("GIT_COMMITTER_DATE", "2020-01-01T00:00:00+0000")
                .status()
                .expect("git runs")
                .success(),
            "git {args:?} failed"
        );
    }
}

fn atlas(estate: &Path, args: &[&str]) -> (bool, Value, String) {
    let mut full = vec!["atlas"];
    full.extend_from_slice(args);
    full.push("--estate");
    let estate_str = estate.to_str().unwrap();
    full.push(estate_str);
    full.push("--json");
    let output = Command::new(wirk_bin())
        .args(&full)
        .output()
        .expect("wirk atlas runs");
    (
        output.status.success(),
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).unwrap_or(Value::Null),
        String::from_utf8_lossy(&output.stderr).trim().to_string(),
    )
}

fn publish(estate: &Path, alias: &str, repo: &Path) {
    publish_reporting(estate, alias, repo);
}

/// The same acquire-and-publish, returning the acquisition reply — whose
/// `generation.coverage` is the estate's own count of what it indexed,
/// excluded and could not support.
fn publish_reporting(estate: &Path, alias: &str, repo: &Path) -> Value {
    let (ok, acquired, err) = atlas(
        estate,
        &[
            "acquire",
            "--source",
            alias,
            "--repository",
            repo.to_str().unwrap(),
            "--revision",
            "HEAD",
        ],
    );
    assert!(ok, "atlas acquire {alias}: {err}");
    let generation = acquired["generation"]["generation"]
        .as_str()
        .expect("acquired generation id")
        .to_string();
    let (ok, _, err) = atlas(
        estate,
        &["publish", "--source", alias, "--generation", &generation],
    );
    assert!(ok, "atlas publish {alias}: {err}");
    acquired
}

struct Estate {
    _dir: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    daemon: Option<KillOnDrop>,
    socket: PathBuf,
}

impl Estate {
    fn new() -> Estate {
        let dir = tempfile::tempdir().expect("temp estate");
        let root = dir.path().join("estate");
        fs::create_dir_all(&root).expect("estate dir");
        let repo = source_repo(dir.path());
        let other = foreign_repo(dir.path());
        let (daemon, pointer) = start_wirkd(&root);
        publish(&root, "demo", &repo);
        publish(&root, "unadmittedsource", &other);
        Estate {
            _dir: dir,
            root,
            repo,
            daemon: Some(daemon),
            socket: pointer.socket,
        }
    }

    fn stop(&mut self) {
        if let Some(daemon) = self.daemon.take() {
            stop_wirkd(&self.root, daemon);
        }
    }

    fn restart(&mut self) {
        self.stop();
        let (daemon, pointer) = start_wirkd(&self.root);
        self.daemon = Some(daemon);
        self.socket = pointer.socket;
    }
}

// ---- routes ---------------------------------------------------------------

/// Two orienting Actor leaves over the same governed resource. The second
/// is the one under test: by the time it is reserved, the first has
/// claimed a real artifact and a real `GovernedBy` edge has been admitted.
/// One orienting Actor leaf carrying exactly the authored text a test
/// wants resolved, and nothing else.
fn one_stage_route(estate: &Path, name: &str, question: &str) -> PathBuf {
    one_stage_route_with(estate, name, question, r#"["demo"]"#, "")
}

/// The same one-leaf Route, with the source filter and any further
/// `orient` keys the test authors itself — `,"semantic":{...}` for a
/// Route that configures its own query backend (ruling 0128 F3).
fn one_stage_route_with(
    estate: &Path,
    name: &str,
    question: &str,
    sources: &str,
    extra: &str,
) -> PathBuf {
    let dir = estate.join("routes");
    fs::create_dir_all(&dir).expect("routes dir");
    let path = dir.join(format!("{name}.json"));
    fs::write(
        &path,
        format!(
            r#"{{"id":{id},"waypoints":[
              {{"id":{leaf},"kind":"Actor",
                "declared_outputs":[{{"name":"out.md","required":true}}],
                "intent":"Do the work.",
                "orient":{{"question":{question},"sources":{sources}{extra}}}}}
            ]}}"#,
            id = serde_json::to_string(name).unwrap(),
            leaf = serde_json::to_string(&format!("{name}/only")).unwrap(),
            question = serde_json::to_string(question).unwrap(),
        ),
    )
    .expect("write route");
    path
}

fn two_stage_route(estate: &Path, name: &str, budget: &str) -> PathBuf {
    let dir = estate.join("routes");
    fs::create_dir_all(&dir).expect("routes dir");
    let path = dir.join(format!("{name}.json"));
    fs::write(
        &path,
        format!(
            r#"{{"id":{id},"waypoints":[
              {{"id":{first},"kind":"Actor",
                "declared_outputs":[{{"name":"survey.md","required":true}}],
                "intent":"Survey src/server.rs and notes/boundary.md and notes/adjudication.md.",
                "orient":{{"question":"Where is claim_boundary_refusal decided, in src/server.rs, and what does notes/boundary.md require? See notes/adjudication.md.","sources":["demo"]{budget}}}}},
              {{"id":{second},"kind":"Actor",
                "declared_outputs":[{{"name":"change.md","required":true}}],
                "intent":"Change src/server.rs.",
                "orient":{{"question":"Change claim_boundary_refusal in src/server.rs so the boundary decision is explicit.","sources":["demo"]{budget}}}}}
            ]}}"#,
            id = serde_json::to_string(name).unwrap(),
            first = serde_json::to_string(&format!("{name}/survey")).unwrap(),
            second = serde_json::to_string(&format!("{name}/change")).unwrap(),
        ),
    )
    .expect("write route");
    path
}

// ---- reading the delivered context ---------------------------------------

/// `wirk world show` exactly as an actor types it: the injected triple in
/// the environment and no arguments at all.
fn world_show(estate: &Path, work: &str, run: &str) -> Value {
    let output = Command::new(wirk_bin())
        .args(["world", "show", "--json"])
        .env("WIRK_ESTATE_ROOT", estate)
        .env("WIRK_WORK_ID", work)
        .env("WIRK_RUN_ID", run)
        .output()
        .expect("wirk world show runs");
    assert_eq!(
        output.status.code(),
        Some(0),
        "world show: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim())
        .expect("world show emits json")
}

/// The plain-text rendering, which is what a human and a fresh actor
/// actually read.
fn world_show_text(estate: &Path, work: &str, run: &str) -> String {
    let output = Command::new(wirk_bin())
        .args(["world", "show"])
        .env("WIRK_ESTATE_ROOT", estate)
        .env("WIRK_WORK_ID", work)
        .env("WIRK_RUN_ID", run)
        .output()
        .expect("wirk world show runs");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// A command run with nothing in the environment but the injected triple
/// — the way the line a projection prints is actually typed.
fn in_pane(estate: &Path, work: &str, run: &str, args: &[&str]) -> (Option<i32>, String, String) {
    let output = Command::new(wirk_bin())
        .args(args)
        .env("WIRK_ESTATE_ROOT", estate)
        .env("WIRK_WORK_ID", work)
        .env("WIRK_RUN_ID", run)
        .output()
        .expect("wirk runs");
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).trim().to_string(),
        String::from_utf8_lossy(&output.stderr).trim().to_string(),
    )
}

fn items<'a>(projection: &'a Value, key: &str) -> &'a Vec<Value> {
    projection[key]
        .as_array()
        .unwrap_or_else(|| panic!("{key} must be a list: {projection}"))
}

fn reasons(projection: &Value, key: &str) -> Vec<String> {
    items(projection, key)
        .iter()
        .map(|item| item["reason"].as_str().unwrap_or_default().to_string())
        .collect()
}

fn coordinate_for(projection: &Value, key: &str, needle: &str) -> String {
    items(projection, key)
        .iter()
        .find(|item| item["reason"].as_str().unwrap_or_default().contains(needle))
        .unwrap_or_else(|| panic!("no {key} item mentioning {needle}: {projection}"))["coordinate"]
        .as_str()
        .expect("coordinate string")
        .to_string()
}

/// The `demo` coordinate of one path, taken out of a delivered
/// projection rather than constructed — the coordinate under test is
/// always one the product itself produced.
fn bound_coordinate(projection: &Value, path: &str) -> String {
    coordinate_for(projection, "bound", &format!("`{path}`"))
}

/// The bound item this projection delivered for one authored path.
fn bound_item(projection: &Value, path: &str) -> Value {
    items(projection, "bound")
        .iter()
        .find(|item| {
            item["reason"]
                .as_str()
                .unwrap_or_default()
                .contains(&format!("the authored text names the path `{path}`"))
        })
        .unwrap_or_else(|| panic!("no bound item for {path}: {projection}"))
        .clone()
}

/// Every bound item whose delivered content identity is this Git object
/// — how "the resource arrived once" is counted without depending on
/// which byte span the assembler chose to deliver.
fn bound_items_of_object<'a>(projection: &'a Value, object_id: &str) -> Vec<&'a Value> {
    items(projection, "bound")
        .iter()
        .filter(|item| item["identity"]["object_id"] == object_id)
        .collect()
}

/// The one generation this projection captured for `demo`.
fn captured_generation(projection: &Value) -> String {
    projection["generations"]
        .as_array()
        .and_then(|pairs| pairs.first())
        .and_then(|pair| pair.get(1))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("a captured generation vector: {projection}"))
        .to_string()
}

/// The omission of one kind, if this projection carries it.
fn omission<'a>(projection: &'a Value, kind: &str) -> Option<&'a Value> {
    items(projection, "omitted")
        .iter()
        .find(|item| item["kind"] == kind)
}

/// Admits a real `GovernedBy` edge through the shipped CLI, from the
/// coordinates a real projection delivered. `wirk atlas relate` requires
/// this Work's current producing action, so this is the ordinary
/// admission path and not a back door.
fn relate(estate: &Path, work: &str, from: &str, to: &str, evidence: &str) -> String {
    let (ok, result, err) = atlas(
        estate,
        &[
            "relate",
            "--work",
            work,
            "--kind",
            "governed_by",
            "--from",
            from,
            "--to",
            to,
            "--evidence",
            evidence,
        ],
    );
    assert!(ok, "atlas relate: {err} {result}");
    result["id"].as_str().expect("relationship id").to_string()
}

/// Runs the first stage to a validated Claim, so the next reservation has
/// a real prior-stage artifact with a real recorded digest to bind.
fn claim_first_stage(estate: &Estate, work: &str, run: &str, body: &str) -> PathBuf {
    let worktree = materialize_actor(&estate.socket, &estate.root, work, run);
    fs::write(worktree.join("survey.md"), body).expect("write artifact");
    claim_ok(&estate.root, work, run, "survey.md=survey.md");
    worktree
}

// ---- the decisive check ---------------------------------------------------

/// **The decisive check** (BUILD.md §8, W-C2): two successive
/// reservations of the same Work are handed observably different,
/// stage-appropriate context — and everything in the second one is
/// followed back to committed bytes through the public CLI.
///
/// The second reservation must carry, beyond what the first did:
///
/// * a **governing record**, `Standing`, reached by following the
///   `GovernedBy` edge the estate admitted out of a resource the authored
///   text named, plus the evidence that edge was admitted on;
/// * a **prior-stage artifact**, `Working`, bound by the exact digest the
///   first stage's Claim validated;
/// * **ranked `referenced`** entries for its own question, with a real
///   retrieval note;
/// * **usable `reachable` handles**, whose printed command runs;
/// * a `reason` on every single item, exact identity on every single
///   item, and a real total on every cut.
#[test]
fn two_reservations_of_one_work_deliver_different_stage_context_an_actor_can_follow() {
    let mut estate = Estate::new();
    let route = two_stage_route(&estate.root, "gov", "");
    let submitted = submit_kind(
        &estate.root,
        route.to_str().unwrap(),
        &estate.repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .expect("submit");

    // --- first reservation ------------------------------------------------
    let first = world_show(&estate.root, &submitted.work_id, &submitted.run_id);
    let first_projection = first["projection"].clone();
    assert_eq!(first["orientation"], "delivered");
    assert!(
        items(&first_projection, "bound")
            .iter()
            .all(|item| item["identity"]["kind"] == "generation"),
        "before any stage has claimed, nothing prior-stage can be bound: {first_projection}"
    );

    // A real governing edge, admitted through the shipped verb, from
    // coordinates this projection itself delivered.
    let governed = bound_coordinate(&first_projection, "src/server.rs");
    let governing = bound_coordinate(&first_projection, "notes/boundary.md");
    let evidence = bound_coordinate(&first_projection, "notes/adjudication.md");
    let edge = relate(
        &estate.root,
        &submitted.work_id,
        &governed,
        &governing,
        &evidence,
    );

    // --- the first stage really finishes ----------------------------------
    let worktree = claim_first_stage(
        &estate,
        &submitted.work_id,
        &submitted.run_id,
        "# Survey\n\nclaim_boundary_refusal is at src/server.rs:1.\n",
    );

    // --- second reservation ------------------------------------------------
    let advanced = status(&estate.socket, &submitted.work_id);
    assert_eq!(advanced["current_waypoint"], "gov/change");
    let second_run = advanced["run_id"].as_str().expect("run id").to_string();
    let second = world_show(&estate.root, &submitted.work_id, &second_run);
    let projection = second["projection"].clone();

    assert_eq!(projection["format"], wirk_core::PROJECTION_FORMAT);
    assert_ne!(
        second["reference"]["projection"], first["reference"]["projection"],
        "two stages of one Work were handed the same context"
    );

    // Governance: the `to` end, Standing, naming the edge that put it
    // there; and the evidence that edge was admitted on.
    let bound_reasons = reasons(&projection, "bound");
    let record = items(&projection, "bound")
        .iter()
        .find(|item| {
            item["reason"]
                .as_str()
                .unwrap_or_default()
                .contains("is governed by this record")
        })
        .unwrap_or_else(|| panic!("no governing record was followed: {bound_reasons:?}"));
    assert!(
        record["reason"].as_str().unwrap().contains(&edge),
        "the item must name the edge that put it there: {record}"
    );
    assert_eq!(
        record["lifetime"], "standing",
        "a governing record's lifetime is Standing"
    );
    // An edge crosses sources, so the reason names both sides. Naming
    // the governing record's source as the governed file's is a false
    // statement about where that file lives — found by running this on a
    // real two-source estate.
    assert!(
        record["reason"].as_str().unwrap().contains(
            "`src/server.rs` in source `demo` is governed by this record, which is in \
                       source `demo`"
        ),
        "the reason must name the governed side and the governing side separately: {record}"
    );
    assert!(
        bound_reasons.iter().any(
            |reason| reason.contains("this is evidence the GovernedBy edge")
                && reason.contains(&edge)
        ),
        "the edge's own evidence must be bound too: {bound_reasons:?}"
    );

    // The prior stage, by exact recorded digest.
    let artifact = items(&projection, "bound")
        .iter()
        .find(|item| item["identity"]["kind"] == "artifact_digest")
        .unwrap_or_else(|| panic!("the prior stage's artifact must bind: {bound_reasons:?}"));
    assert_eq!(artifact["lifetime"], "working");
    assert!(
        artifact["reason"]
            .as_str()
            .unwrap()
            .contains("the prior stage `gov/survey`"),
        "{artifact}"
    );
    let recorded = artifact["identity"]["digest"].as_str().expect("digest");
    assert_eq!(
        recorded,
        sha256_of(&worktree.join("survey.md")),
        "the bound digest is the one the Claim validated over the real bytes"
    );
    assert!(
        artifact["summary"]
            .as_str()
            .unwrap()
            .contains("claim_boundary_refusal is at src/server.rs:1."),
        "the summary must be derived from the same bytes that were hashed: {artifact}"
    );
    assert!(
        artifact["coordinate"]
            .as_str()
            .unwrap()
            .starts_with("claim/"),
        "a prior-stage artifact is addressed by this Work's own Claim: {artifact}"
    );

    // Ranked retrieval, with a real note.
    assert!(
        !items(&projection, "referenced").is_empty(),
        "the stage question must be ranked: {projection}"
    );
    let retrieval = &projection["retrieval"];
    assert_eq!(retrieval["mode"], "lexical");
    assert!(
        retrieval["total_candidates"].as_u64().unwrap() >= 1,
        "{retrieval}"
    );
    assert!(
        retrieval["semantic_reason"].as_str().is_some(),
        "a semantic status that is not Applied owes a reason: {retrieval}"
    );

    // Every item, in every list, carries a reason and an exact identity.
    for key in ["bound", "referenced"] {
        for item in items(&projection, key) {
            assert!(
                !item["reason"].as_str().unwrap_or_default().is_empty(),
                "{key} item with no reason: {item}"
            );
            let identity = &item["identity"];
            match identity["kind"].as_str() {
                Some("generation") => {
                    assert!(!identity["generation"].as_str().unwrap().is_empty());
                    assert!(!identity["object_id"].as_str().unwrap().is_empty());
                }
                Some("artifact_digest") => {
                    assert_eq!(identity["digest"].as_str().unwrap().len(), 64);
                }
                other => panic!("unknown identity kind {other:?}: {item}"),
            }
        }
    }

    // Every Atlas coordinate the projection delivered resolves, through
    // the public verb, from inside the pane, to real committed bytes.
    for key in ["bound", "referenced"] {
        for item in items(&projection, key) {
            if item["identity"]["kind"] != "generation" {
                continue;
            }
            let coordinate = item["coordinate"].as_str().unwrap();
            let (code, out, err) = in_pane(
                &estate.root,
                &submitted.work_id,
                &second_run,
                &["atlas", "resolve", "--coordinate", coordinate, "--json"],
            );
            assert_eq!(code, Some(0), "resolve {coordinate}: {err}");
            let resolved: Value = serde_json::from_str(&out).expect("resolve json");
            assert_eq!(resolved["outcome"], "resolved", "{resolved}");
        }
    }

    // A reachable handle is a discovery handle, not a decorative string:
    // the command the projection prints under it runs, from the pane,
    // and finds real content.
    let reachable = items(&projection, "reachable");
    assert!(!reachable.is_empty(), "{projection}");
    let knowledge = reachable
        .iter()
        .find(|entry| entry["family"] == "knowledge")
        .unwrap_or_else(|| panic!("the knowledge family is admitted here: {reachable:?}"));
    assert_eq!(knowledge["handle"], "demo:knowledge");
    assert_eq!(knowledge["source"], "demo");
    assert!(knowledge["resources"].as_u64().unwrap() >= 2, "{knowledge}");
    let fetch = knowledge["fetch"].as_str().expect("fetch line");
    // Run the printed line verbatim, substituting only the terms it
    // says are the actor's own.
    let mut argv: Vec<String> = fetch
        .split_whitespace()
        .skip(1)
        .map(str::to_string)
        .collect();
    let terms = argv
        .iter_mut()
        .find(|arg| *arg == "<terms>")
        .expect("the fetch line asks for the actor's own terms");
    *terms = "boundary".to_string();
    argv.push("--json".to_string());
    let borrowed: Vec<&str> = argv.iter().map(String::as_str).collect();
    let (code, out, err) = in_pane(&estate.root, &submitted.work_id, &second_run, &borrowed);
    assert_eq!(code, Some(0), "the printed discovery line must run: {err}");
    let found: Value = serde_json::from_str(&out).expect("search json");
    assert!(
        !found["hits"].as_array().unwrap().is_empty(),
        "the handle must actually discover something: {found}"
    );

    // The fallback that makes the printed line runnable is the same
    // rule `wirk atlas resolve` runs on, and it is not a widening:
    // outside any actor context the verb still refuses, and a half
    // triple is refused rather than read as "no context".
    let bare = Command::new(wirk_bin())
        .args(["atlas", "search", "--query", "boundary", "--json"])
        .env_remove("WIRK_ESTATE_ROOT")
        .env_remove("WIRK_WORK_ID")
        .env_remove("WIRK_RUN_ID")
        .output()
        .expect("wirk runs");
    assert_ne!(
        bare.status.code(),
        Some(0),
        "outside an actor context nothing changes: {bare:?}"
    );
    let half = Command::new(wirk_bin())
        .args(["atlas", "search", "--query", "boundary", "--json"])
        .env("WIRK_ESTATE_ROOT", &estate.root)
        .env_remove("WIRK_WORK_ID")
        .env_remove("WIRK_RUN_ID")
        .output()
        .expect("wirk runs");
    assert_eq!(
        half.status.code(),
        Some(1),
        "a half-injected environment is refused, never widened"
    );
    assert!(
        String::from_utf8_lossy(&half.stderr).contains("incomplete"),
        "{:?}",
        String::from_utf8_lossy(&half.stderr)
    );

    // The plain rendering carries both lines a fresh actor needs.
    let text = world_show_text(&estate.root, &submitted.work_id, &second_run);
    assert!(
        text.contains("resolve with: wirk atlas resolve --coordinate"),
        "{text}"
    );
    assert!(
        text.contains("discover with: wirk atlas search --source demo"),
        "{text}"
    );
    assert!(
        text.contains("next State of the delivered evidence:"),
        "{text}"
    );

    // Nothing outside this Work's bindings is named anywhere.
    assert_no_scope_leak(&second);

    estate.stop();
}

fn sha256_of(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(fs::read(path).expect("read artifact"));
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The whole delivered document, scanned for anything the Work's own
/// bindings never admitted: the unbound source's alias, its paths, and
/// the distinctive content only it holds. Coordinates are hex-encoded,
/// so the encoded forms are scanned too.
fn assert_no_scope_leak(shown: &Value) {
    let rendered = shown.to_string();
    for needle in ["unadmittedsource", "quarantinedmarker", "other-repo"] {
        assert!(
            !rendered.contains(needle),
            "the unadmitted source leaked {needle:?} into a delivered projection"
        );
    }
    let hex: String = "quarantinedmarker"
        .bytes()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert!(
        !rendered.contains(&hex),
        "an unadmitted path leaked inside an encoded coordinate"
    );
}

/// Ruling 0126 F2 and BUILD.md §4.7, as behaviour: **a budget controls
/// presentation and nothing else.**
///
/// The same Work, the same stage, the same estate, assembled once with no
/// budget and once with `referenced_max: 1, reachable_max: 1`. What must
/// be identical: the factual coverage state, the `next_action` sentence,
/// and the whole `bound` list — which holds what the stage requires and
/// has no budget at all. What must differ: only how much of `referenced`
/// and `reachable` was rendered, each cut disclosed with the **real**
/// total the query itself reported.
#[test]
fn a_budget_of_one_cuts_only_the_rendering_and_reports_the_real_totals() {
    let mut estate = Estate::new();

    let open = assemble_first_stage(&estate, "openbudget", "");
    let tight = assemble_first_stage(
        &estate,
        "tightbudget",
        r#","budget":{"referenced_max":1,"reachable_max":1}"#,
    );

    // Fact, unchanged.
    assert_eq!(
        open["coverage"], tight["coverage"],
        "a rendering budget must not decide factual coverage"
    );
    assert_eq!(
        open["next_action"], tight["next_action"],
        "next_action is chosen only by coverage and unknowns, so it is identical under any budget"
    );
    assert_eq!(
        reasons(&open, "bound"),
        reasons(&tight, "bound"),
        "bound holds what the stage requires and is never cut"
    );
    assert_eq!(
        open["retrieval"]["total_candidates"], tight["retrieval"]["total_candidates"],
        "the candidate count is what the corpus held, not what was shown"
    );

    // Presentation, cut, and honest about it.
    assert_eq!(items(&tight, "referenced").len(), 1);
    assert_eq!(items(&tight, "reachable").len(), 1);
    assert_eq!(tight["truncated"], true);
    assert_eq!(open["truncated"], false, "{open}");

    let cut = |projection: &Value, of: &str| -> Value {
        items(projection, "omitted")
            .iter()
            .find(|item| item["kind"] == "over_budget" && item["of"] == of)
            .unwrap_or_else(|| panic!("the {of} list was cut and must say so: {projection}"))
            .clone()
    };
    let referenced_cut = cut(&tight, "referenced");
    assert_eq!(referenced_cut["shown"], 1);
    assert_eq!(
        referenced_cut["total"], tight["retrieval"]["total_candidates"],
        "the disclosed total is the query's own count, not the shown count"
    );
    assert!(
        referenced_cut["total"].as_u64().unwrap() > 1,
        "the cut must be over something real: {referenced_cut}"
    );
    let reachable_cut = cut(&tight, "reachable");
    assert_eq!(reachable_cut["shown"], 1);
    assert_eq!(
        reachable_cut["total"].as_u64().unwrap(),
        items(&open, "reachable").len() as u64,
        "the disclosed reachable total is the whole admitted set"
    );

    // And a cut is never an unavailability: nothing here was withheld or
    // could not be read, so the coverage reason may not say either.
    for projection in [&open, &tight] {
        assert!(
            !items(projection, "omitted")
                .iter()
                .any(|item| item["kind"] == "unavailable"),
            "a budget cut is not an unavailability: {projection}"
        );
    }

    estate.stop();
}

/// One first-stage assembly of a freshly submitted Work on this estate.
fn assemble_first_stage(estate: &Estate, name: &str, budget: &str) -> Value {
    let route = two_stage_route(&estate.root, name, budget);
    let submitted = submit_kind(
        &estate.root,
        route.to_str().unwrap(),
        &estate.repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .expect("submit");
    world_show(&estate.root, &submitted.work_id, &submitted.run_id)["projection"].clone()
}

/// BUILD-AMENDMENTS.md and ruling 0124, executed: **a prior stage's
/// artifact whose bytes have changed is an explicit unavailability, and
/// is never bound.**
///
/// The first stage claims a real artifact and its digest is recorded. The
/// next reservation binds it. Then the file is rewritten in the worktree
/// and the stage is reserved again: the later bytes are not attributed to
/// the earlier Claim, the item does not bind at all, and the projection
/// says `artifact_bytes_changed` rather than reporting absence or falling
/// back to the newer content under the old name.
///
/// Beside it, two things that must not move: the projection already
/// delivered keeps its exact bytes on disk, and a restarted daemon still
/// serves it unchanged.
#[test]
fn a_rewritten_prior_stage_artifact_is_unavailable_and_the_delivered_projection_is_untouched() {
    let mut estate = Estate::new();
    let route = two_stage_route(&estate.root, "rewrite", "");
    let submitted = submit_kind(
        &estate.root,
        route.to_str().unwrap(),
        &estate.repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .expect("submit");
    let worktree = claim_first_stage(
        &estate,
        &submitted.work_id,
        &submitted.run_id,
        "# Survey\n\nthe original claimed bytes\n",
    );

    // Positive control: with the claimed bytes intact, it binds.
    let advanced = status(&estate.socket, &submitted.work_id);
    let second_run = advanced["run_id"].as_str().expect("run id").to_string();
    let delivered = world_show(&estate.root, &submitted.work_id, &second_run);
    let projection = delivered["projection"].clone();
    assert!(
        items(&projection, "bound")
            .iter()
            .any(|item| item["identity"]["kind"] == "artifact_digest"),
        "the unchanged artifact must bind: {projection}"
    );
    let observation = delivered["reference"]["observation"]
        .as_str()
        .expect("observation")
        .to_string();
    let path = estate
        .root
        .join("works")
        .join(&submitted.work_id)
        .join("projections")
        .join(format!("{observation}.json"));
    let delivered_bytes = fs::read(&path).expect("read delivered projection");

    // Rewrite the claimed artifact and reserve the same stage again.
    fs::write(
        worktree.join("survey.md"),
        "# Survey\n\nrewritten after the Claim\n",
    )
    .expect("rewrite artifact");
    harness::fail_via_socket(
        &estate.socket,
        &estate.root,
        &submitted.work_id,
        &second_run,
    );
    let (code, out) = harness::retry_cli(&estate.root, &submitted.work_id);
    assert_eq!(code, Some(0), "retry: {out}");
    let retried = status(&estate.socket, &submitted.work_id);
    let retried_run = retried["run_id"].as_str().expect("run id").to_string();
    let after = world_show(&estate.root, &submitted.work_id, &retried_run);
    let after_projection = after["projection"].clone();

    assert!(
        !items(&after_projection, "bound")
            .iter()
            .any(|item| item["identity"]["kind"] == "artifact_digest"),
        "rewritten bytes must not be attributed to the earlier Claim: {after_projection}"
    );
    let unavailable = items(&after_projection, "omitted")
        .iter()
        .find(|item| item["reason"] == "artifact_bytes_changed")
        .unwrap_or_else(|| {
            panic!("the change must be stated, not silently dropped: {after_projection}")
        })
        .clone();
    assert!(
        unavailable["coordinate"]
            .as_str()
            .unwrap()
            .starts_with("claim/"),
        "{unavailable}"
    );
    assert!(
        unavailable["coordinate"]
            .as_str()
            .unwrap()
            .ends_with("/artifact/survey.md"),
        "{unavailable}"
    );
    // Something this Work's own record names could not be read back at
    // the identity it was recorded against — a factual coverage state,
    // and not one a budget produced.
    assert_eq!(after_projection["coverage"]["state"], "partial");
    assert_eq!(
        after_projection["coverage"]["reason"], "evidence_unavailable",
        "{after_projection}"
    );

    // The already-delivered projection is byte-identical, and survives a
    // real restart.
    assert_eq!(
        fs::read(&path).expect("re-read delivered projection"),
        delivered_bytes,
        "a delivered projection is immutable"
    );
    estate.restart();
    let reread = world_show(&estate.root, &submitted.work_id, &second_run);
    assert_eq!(reread["orientation"], "delivered");
    assert_eq!(reread["projection"], projection, "across a restart");

    estate.stop();
}

/// Two properties a governance follower must have, in one real estate:
///
/// * **A cycle is safe without a depth cap.** Two admitted edges point at
///   each other. The assembly terminates, and each resource is delivered
///   exactly once.
/// * **An edge is never a read grant, and a filtered edge names nothing.**
///   An edge whose far side lies in a source the assembling Work never
///   bound comes back as a count, not a coordinate — and the coordinate
///   itself, handed over explicitly, still resolves to a refusal for that
///   Work. Reaching something through governance grants exactly what the
///   bindings already granted, which is nothing here.
#[test]
fn governance_survives_a_cycle_and_never_grants_a_read_the_bindings_did_not() {
    let mut estate = Estate::new();

    // A Work bound to both sources: the only one that may admit an edge
    // crossing between them, which is `admit_relationship`'s own rule.
    let wide_route = two_stage_route(&estate.root, "wide", "");
    let wide = submit_kind(
        &estate.root,
        wide_route.to_str().unwrap(),
        &estate.repo,
        &["demo:write", "unadmittedsource:read"],
        None,
        Some("actor"),
    )
    .expect("submit wide");
    let wide_shown = world_show(&estate.root, &wide.work_id, &wide.run_id);
    let wide_projection = wide_shown["projection"].clone();
    let server = bound_coordinate(&wide_projection, "src/server.rs");
    let boundary = bound_coordinate(&wide_projection, "notes/boundary.md");
    let adjudication = bound_coordinate(&wide_projection, "notes/adjudication.md");

    // A coordinate in the source the narrow Work never binds, taken from
    // the wide Work's own ranked answer rather than constructed.
    let (ok, found, err) = atlas(
        &estate.root,
        &[
            "search",
            "--work",
            &wide.work_id,
            "--query",
            "quarantinedmarker",
            "--source",
            "unadmittedsource",
        ],
    );
    assert!(ok, "search: {err}");
    let foreign = found["hits"][0]["coordinate"]
        .as_str()
        .unwrap_or_else(|| panic!("a hit in the unbound source: {found}"))
        .to_string();

    // The cycle, and the crossing edge, both admitted for real.
    relate(
        &estate.root,
        &wide.work_id,
        &server,
        &boundary,
        &adjudication,
    );
    relate(
        &estate.root,
        &wide.work_id,
        &boundary,
        &server,
        &adjudication,
    );
    relate(
        &estate.root,
        &wide.work_id,
        &server,
        &foreign,
        &adjudication,
    );

    // The narrow Work: bound to `demo` alone, and asking about the
    // governed resource **only** — so everything governance reaches is
    // something the authored text did not name, and the traversal is
    // what put it there.
    let narrow_route = one_stage_route(
        &estate.root,
        "narrow",
        "Change claim_boundary_refusal in src/server.rs.",
    );
    let narrow = submit_kind(
        &estate.root,
        narrow_route.to_str().unwrap(),
        &estate.repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .expect("submit narrow");
    let shown = world_show(&estate.root, &narrow.work_id, &narrow.run_id);
    let projection = shown["projection"].clone();

    // The cycle was followed, and it terminated. Two facts, and ruling
    // 0128 F2 is the difference between them: the governing record's
    // **bytes** arrive exactly once, and both **relationships** of the
    // cycle are stated. The edge pointing back at an already-delivered
    // resource does not deliver it again and does not read it again —
    // and it is no longer discarded either, which is what the single
    // visited set used to do. No depth cap was consulted and none
    // exists.
    let bound_reasons = reasons(&projection, "bound");
    let record = items(&projection, "bound")
        .iter()
        .find(|item| {
            item["reason"]
                .as_str()
                .unwrap_or_default()
                .contains("`src/server.rs` in source `demo` is governed by this record")
        })
        .expect("the governing record the forward edge reached")
        .clone();
    let record_object = record["identity"]["object_id"]
        .as_str()
        .expect("object id")
        .to_string();
    assert_eq!(
        bound_items_of_object(&projection, &record_object).len(),
        1,
        "the cycle must deliver its governing record's bytes exactly once: {bound_reasons:?}"
    );
    assert!(
        bound_reasons.iter().any(|reason| reason
            .contains("`notes/boundary.md` in source `demo` is governed by this record")),
        "the edge back around the cycle is attributed, not dropped: {bound_reasons:?}"
    );
    assert_eq!(
        bound_reasons
            .iter()
            .filter(|reason| reason.contains("this is evidence the GovernedBy edge"))
            .count(),
        1,
        "the edge's evidence is one delivered item, whichever edge reaches it first, carrying \
         what it is evidence of for each: {bound_reasons:?}"
    );
    assert_eq!(record["lifetime"], "standing");
    assert!(
        record["summary"]
            .as_str()
            .unwrap()
            .contains("The governing rule"),
        "the governing record is the real committed bytes: {record}"
    );

    // The crossing edge is a count, and nothing else.
    let withheld = items(&projection, "omitted")
        .iter()
        .find(|item| item["kind"] == "inadmissible")
        .unwrap_or_else(|| panic!("the filtered edge must be counted: {projection}"))
        .clone();
    assert!(withheld["count"].as_u64().unwrap() >= 1, "{withheld}");
    assert_eq!(
        withheld.as_object().unwrap().len(),
        2,
        "an inadmissible omission carries a count and nothing else: {withheld}"
    );
    assert_no_scope_leak(&shown);

    // And possessing the coordinate is not authority: handed the exact
    // foreign coordinate, the narrow Work is still refused — while the
    // wide Work, the positive control, resolves it.
    let (code, _, err) = in_pane(
        &estate.root,
        &narrow.work_id,
        &narrow.run_id,
        &["atlas", "resolve", "--coordinate", &foreign, "--json"],
    );
    assert_ne!(
        code,
        Some(0),
        "a coordinate reached through governance confers no read: {err}"
    );
    let (code, out, err) = in_pane(
        &estate.root,
        &wide.work_id,
        &wide.run_id,
        &["atlas", "resolve", "--coordinate", &foreign, "--json"],
    );
    assert_eq!(code, Some(0), "the control must resolve it: {err} {out}");

    estate.stop();
}

// ---- F1: governance recorded at an edition this assembly did not capture ---

/// **Ruling 0128 F1.** A `GovernedBy` edge this estate really admitted,
/// about a resource this projection really delivers, recorded against a
/// generation this assembly did not capture, is reported as a **count**.
/// Before this it was reported as nothing at all.
///
/// The sharp case, run for real: the only thing that changes in the
/// governed source is an **unrelated file**. `src/server.rs` keeps its
/// bytes and keeps its Git object id — the projection itself delivers
/// that object id before and after, and this test compares them — but
/// the source generation is content-addressed over the whole tree, so
/// the label the edge was admitted against is superseded. The edge is
/// still not followed, and that is right: ruling 0126 forbids
/// substituting today's bytes under a coordinate the projection is not
/// pinned to, and nothing here decides whether the relationship still
/// holds. What is wrong is saying nothing, because
/// `coverage: complete` with no count and no unknown is exactly the
/// document a stage gets when the estate governs nothing at all — so a
/// stage that must decide something was handed a projection that looked
/// complete with the governing rule missing.
///
/// Four arms, one estate, one real admitted edge:
///
/// * **before** — the edge is followed, coverage is complete, no count;
/// * **after** — the same question at the new generation: not followed,
///   `admitted_at_another_edition { count: 1 }`, coverage moves;
/// * **the count says nothing else** — no relationship id, no coordinate;
/// * **the negative control** — a question about a resource no admitted
///   edge names gets no count and complete coverage, on the same estate
///   at the same generation. "Nothing is admitted here" and "something
///   is admitted here at an edition you did not capture" are two
///   different documents, which is the whole finding.
#[test]
fn governance_admitted_at_another_edition_is_a_count_and_never_silence() {
    let mut estate = Estate::new();

    // The edge, admitted through the shipped verb from coordinates a
    // real projection delivered.
    let wide_route = two_stage_route(&estate.root, "edition", "");
    let wide = submit_kind(
        &estate.root,
        wide_route.to_str().unwrap(),
        &estate.repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .expect("submit wide");
    let wide_projection =
        world_show(&estate.root, &wide.work_id, &wide.run_id)["projection"].clone();
    let edge = relate(
        &estate.root,
        &wide.work_id,
        &bound_coordinate(&wide_projection, "src/server.rs"),
        &bound_coordinate(&wide_projection, "notes/boundary.md"),
        &bound_coordinate(&wide_projection, "notes/adjudication.md"),
    );

    let question = "Change claim_boundary_refusal in src/server.rs.";

    // --- arm 1: the positive control, at the generation the edge names --
    let before_route = one_stage_route(&estate.root, "before-edition", question);
    let before = submit_kind(
        &estate.root,
        before_route.to_str().unwrap(),
        &estate.repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .expect("submit before");
    let before_projection =
        world_show(&estate.root, &before.work_id, &before.run_id)["projection"].clone();
    assert!(
        reasons(&before_projection, "bound")
            .iter()
            .any(|reason| reason.contains("is governed by this record")),
        "the control must follow the edge: {before_projection}"
    );
    assert_eq!(before_projection["coverage"]["state"], "complete");
    assert!(
        omission(&before_projection, "admitted_at_another_edition").is_none(),
        "nothing is pinned elsewhere yet: {before_projection}"
    );
    let governed_object = bound_item(&before_projection, "src/server.rs")["identity"]["object_id"]
        .as_str()
        .expect("object id")
        .to_string();

    // --- the estate moves, in the one way that matters ------------------
    // An unrelated file. The governed file is not touched at all.
    write_file(
        &estate.repo,
        "notes/unrelated.md",
        "# Unrelated\n\nThis file has nothing to do with the boundary rule.\n",
    );
    commit_all(&estate.repo);
    publish(&estate.root, "demo", &estate.repo);

    // --- arm 2: the same question, one generation later -----------------
    let after_route = one_stage_route(&estate.root, "after-edition", question);
    let after = submit_kind(
        &estate.root,
        after_route.to_str().unwrap(),
        &estate.repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .expect("submit after");
    let after_shown = world_show(&estate.root, &after.work_id, &after.run_id);
    let after_projection = after_shown["projection"].clone();

    assert_ne!(
        captured_generation(&before_projection),
        captured_generation(&after_projection),
        "the republish must really have moved the generation label"
    );
    assert_eq!(
        bound_item(&after_projection, "src/server.rs")["identity"]["object_id"]
            .as_str()
            .expect("object id"),
        governed_object,
        "the governed file's own bytes and Git object must be unchanged: only an unrelated file \
         moved, which is what makes this the sharp case"
    );

    // Still not followed — a historical projection is never re-pointed
    // at today's bytes (ruling 0126).
    assert!(
        !reasons(&after_projection, "bound")
            .iter()
            .any(|reason| reason.contains("is governed by this record")),
        "an edge admitted at another edition is not followed: {after_projection}"
    );
    // And no longer silent.
    let counted = omission(&after_projection, "admitted_at_another_edition")
        .unwrap_or_else(|| {
            panic!("a dropped governing record must be counted, not silent: {after_projection}")
        })
        .clone();
    assert_eq!(counted["count"], 1, "{counted}");
    assert_eq!(
        counted.as_object().unwrap().len(),
        2,
        "the count carries a count and nothing else — no id, no coordinate: {counted}"
    );
    assert!(
        !serde_json::to_string(&after_shown)
            .expect("serialize")
            .contains(&edge),
        "the count must not name the relationship it counts"
    );
    // A governing record that exists and was not delivered is a
    // completeness fact, so it moves coverage exactly as the mirror
    // case — a governing endpoint that no longer resolves — already
    // does. It is not a budget: `truncated` is a separate field and a
    // separate sentence.
    assert_eq!(after_projection["coverage"]["state"], "partial");
    assert_eq!(
        after_projection["coverage"]["reason"], "governance_outside_captured_editions",
        "{after_projection}"
    );
    assert!(
        after_projection["next_action"]
            .as_str()
            .unwrap()
            .contains("recorded at an edition this assembly did not capture"),
        "{after_projection}"
    );
    // C2 D1. The sentence F1 exists to deliver must ship well-formed —
    // read out of the **raw delivered document on disk**, not out of a
    // reply some renderer may have normalized, because that is the
    // document a stage actually reads and it is what the `ProjectionId`
    // covers. `cargo fmt --check` does not reflow string literals, so a
    // `\`-continuation missing from one arm of a `match` is invisible to
    // every gate this repository has; the first candidate shipped this
    // arm with two runs of fourteen spaces in it.
    let raw = fs::read_to_string(
        estate
            .root
            .join("works")
            .join(&after.work_id)
            .join("projections")
            .join(format!(
                "{}.json",
                after_shown["reference"]["observation"]
                    .as_str()
                    .expect("observation")
            )),
    )
    .expect("the delivered document is readable");
    let delivered: Value = serde_json::from_str(&raw).expect("the delivered document parses");
    let sentence = delivered["content"]["next_action"]
        .as_str()
        .expect("the delivered document carries next_action");
    assert!(
        sentence.contains("recorded at an edition this assembly did not capture"),
        "the raw document must be the one asserted on: {sentence}"
    );
    assert!(
        !sentence.contains("  "),
        "the delivered sentence must not carry a run of spaces from an un-continued string \
         literal: {sentence:?}"
    );
    // Nothing here asserts the relationship stopped being true.
    let document = serde_json::to_string(&after_shown).expect("serialize");
    for forbidden in ["no longer governs", "is no longer", "invalidated"] {
        assert!(
            !document.contains(forbidden),
            "the projection must not claim the relationship became false: {forbidden}"
        );
    }

    // --- arm 3: the negative control, same estate, same generation ------
    let quiet_route = one_stage_route(
        &estate.root,
        "quiet-edition",
        "What does notes/adjudication.md record?",
    );
    let quiet = submit_kind(
        &estate.root,
        quiet_route.to_str().unwrap(),
        &estate.repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .expect("submit quiet");
    let quiet_projection =
        world_show(&estate.root, &quiet.work_id, &quiet.run_id)["projection"].clone();
    assert!(
        omission(&quiet_projection, "admitted_at_another_edition").is_none(),
        "no admitted edge names this resource at any edition, so there is nothing to count: \
         {quiet_projection}"
    );
    assert_eq!(
        quiet_projection["coverage"]["state"], "complete",
        "{quiet_projection}"
    );

    estate.stop();
}

// ---- F2: a resource is deduplicated; a relationship is not ----------------

/// **Ruling 0128 F2.** Naming the governing record in the authored
/// question must not delete the fact that it governs anything.
///
/// The traversal's visited set is seeded with everything step 3
/// delivered, so a governing record the author happened to name by path
/// was already "visited" when its edge was read, and the edge was
/// skipped entirely: the record arrived saying only "the authored text
/// names the path", and "0 further item(s)" was then read as "nothing
/// governs this". A stage whose question is thorough enough to name the
/// rule is exactly the stage that lost the rule's connection to the
/// code.
///
/// Both halves are pinned here, on one estate with one real admitted
/// edge and two Works that differ only in what their question names:
///
/// * the record is still delivered **once** — deduplicating the resource
///   is right, and its bytes are not read again;
/// * its item still carries the governing relationship, the edge id, the
///   admission producer and `Standing` — deduplicating the resource must
///   not erase why it governs another one.
///
/// The third arm is a governing record that governs **two** resources:
/// one record, one delivery, both relationships stated on it.
#[test]
fn naming_the_governing_record_keeps_the_relationship_that_makes_it_governing() {
    let mut estate = Estate::new();
    let seed_route = two_stage_route(&estate.root, "named", "");
    let seed = submit_kind(
        &estate.root,
        seed_route.to_str().unwrap(),
        &estate.repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .expect("submit seed");
    let seed_projection =
        world_show(&estate.root, &seed.work_id, &seed.run_id)["projection"].clone();
    let server = bound_coordinate(&seed_projection, "src/server.rs");
    let boundary = bound_coordinate(&seed_projection, "notes/boundary.md");
    let adjudication = bound_coordinate(&seed_projection, "notes/adjudication.md");
    let edge = relate(
        &estate.root,
        &seed.work_id,
        &server,
        &boundary,
        &adjudication,
    );

    // --- the control: a question that does not name the record ---------
    let unnamed_route = one_stage_route(
        &estate.root,
        "unnamed-gov",
        "Change claim_boundary_refusal in src/server.rs.",
    );
    let unnamed = submit_kind(
        &estate.root,
        unnamed_route.to_str().unwrap(),
        &estate.repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .expect("submit unnamed");
    let unnamed_projection =
        world_show(&estate.root, &unnamed.work_id, &unnamed.run_id)["projection"].clone();
    let record = items(&unnamed_projection, "bound")
        .iter()
        .find(|item| {
            item["reason"]
                .as_str()
                .unwrap_or_default()
                .contains("is governed by this record")
        })
        .expect("the control follows the edge")
        .clone();
    assert_eq!(record["lifetime"], "standing");
    let governing_object = record["identity"]["object_id"]
        .as_str()
        .expect("object id")
        .to_string();

    // --- the finding: the same edge, a question that names the record ---
    let named_route = one_stage_route(
        &estate.root,
        "named-gov",
        "Change claim_boundary_refusal in src/server.rs; the rule is notes/boundary.md and the \
         evidence is notes/adjudication.md.",
    );
    let named = submit_kind(
        &estate.root,
        named_route.to_str().unwrap(),
        &estate.repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .expect("submit named");
    let named_projection =
        world_show(&estate.root, &named.work_id, &named.run_id)["projection"].clone();

    // Delivered once. The resource is deduplicated, exactly as before.
    let deliveries = bound_items_of_object(&named_projection, &governing_object);
    assert_eq!(
        deliveries.len(),
        1,
        "the governing record's bytes are delivered once: {named_projection}"
    );
    let delivered = deliveries[0].clone();

    // And the relationship survives the deduplication.
    let reason = delivered["reason"].as_str().expect("reason").to_string();
    assert!(
        reason.contains("the authored text names the path `notes/boundary.md`"),
        "the reason it first arrived is kept: {reason}"
    );
    assert!(
        reason.contains("`src/server.rs` in source `demo` is governed by this record"),
        "the relationship the authored literal used to erase must be stated: {reason}"
    );
    assert!(
        reason.contains(&edge),
        "the exact admitted edge is named: {reason}"
    );
    assert!(
        reason.contains("admitted by"),
        "the admission evidence is preserved verbatim: {reason}"
    );
    assert_eq!(
        delivered["lifetime"], "standing",
        "a record that governs something is Standing however this assembly first reached it: \
         {delivered}"
    );
    // The evidence coordinate the edge was admitted on, likewise: named
    // by the author, and still carrying what it is evidence *of*.
    let evidence_item = bound_item(&named_projection, "notes/adjudication.md");
    assert!(
        evidence_item["reason"]
            .as_str()
            .unwrap()
            .contains("this is evidence the GovernedBy edge"),
        "{evidence_item}"
    );
    // Coverage is untouched by any of this: nothing was withheld.
    assert_eq!(named_projection["coverage"]["state"], "complete");

    // --- one record, two governed resources ----------------------------
    // The same governing document, admitted over a second resource. It
    // is still delivered once, and both relationships are stated on it.
    let second_edge = relate(
        &estate.root,
        &seed.work_id,
        &adjudication,
        &boundary,
        &server,
    );
    let shared_route = one_stage_route(
        &estate.root,
        "shared-gov",
        "Change claim_boundary_refusal in src/server.rs; see notes/adjudication.md.",
    );
    let shared = submit_kind(
        &estate.root,
        shared_route.to_str().unwrap(),
        &estate.repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .expect("submit shared");
    let shared_projection =
        world_show(&estate.root, &shared.work_id, &shared.run_id)["projection"].clone();
    let shared_deliveries = bound_items_of_object(&shared_projection, &governing_object);
    assert_eq!(
        shared_deliveries.len(),
        1,
        "one governing document, delivered once: {shared_projection}"
    );
    let shared_reason = shared_deliveries[0]["reason"]
        .as_str()
        .expect("reason")
        .to_string();
    for (edge_id, governed) in [
        (&edge, "src/server.rs"),
        (&second_edge, "notes/adjudication.md"),
    ] {
        assert!(
            shared_reason.contains(edge_id.as_str()),
            "both admitted edges must be named on the record they share: {shared_reason}"
        );
        assert!(
            shared_reason.contains(&format!(
                "`{governed}` in source `demo` is governed by this record"
            )),
            "both governed resources must be named: {shared_reason}"
        );
    }

    estate.stop();
}

// ---- F3: the stage assembly can ask for the estate's semantic editions ----

/// **Ruling 0128 F3.** The one surface whose job is to hand a stage its
/// estate evidence was the only retrieval surface that could not ask for
/// semantic ranking: `ranked_answer` passed `semantic_query: None`
/// unconditionally and `OrientationRequest` had no field that could
/// carry a backend. It then explained its own degradation by saying the
/// product ships no backend — false about the product (the query path,
/// the flags and the adapter are all shipped, ruling 0109) and false
/// about any estate that has editions built and selected.
///
/// Two arms on one estate, and the difference between them is one
/// authored `orient.semantic` block:
///
/// * **unconfigured** — the reason is about *this request*, and names no
///   product limitation at all;
/// * **configured** — the same question, with an explicitly configured
///   backend and offline model, reaches the estate's own retrieval
///   admission: the reason is now the estate's answer about its
///   editions, which is only reachable if the configuration travelled
///   from the Route through the assembly into `wirk_atlas::search`.
///
/// This estate has no editions built, so the configured arm's honest
/// answer is that none is selected — a real fallback, stated as a fact
/// about the estate rather than about the product. Execution of a real
/// backend against real selected editions is proved outside the suite,
/// against the installed Semble backend and the pinned model snapshot,
/// because a test may not depend on a host path.
#[test]
fn a_route_configures_its_own_semantic_backend_and_the_reason_is_about_the_request() {
    let mut estate = Estate::new();
    let question = "Where is claim_boundary_refusal decided in src/server.rs?";

    let unconfigured_route = one_stage_route(&estate.root, "no-backend", question);
    let unconfigured = submit_kind(
        &estate.root,
        unconfigured_route.to_str().unwrap(),
        &estate.repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .expect("submit unconfigured");
    let unconfigured_projection =
        world_show(&estate.root, &unconfigured.work_id, &unconfigured.run_id)["projection"].clone();
    let retrieval = &unconfigured_projection["retrieval"];
    assert_eq!(retrieval["mode"], "lexical");
    assert_eq!(retrieval["semantic"], "unavailable");
    let reason = retrieval["semantic_reason"]
        .as_str()
        .expect("an unavailable semantic status owes a reason")
        .to_string();
    assert!(
        reason.contains("this request named neither"),
        "the degradation is a fact about the request: {reason}"
    );
    assert!(
        !reason.contains("this product ships"),
        "and never a false statement about what the product ships: {reason}"
    );

    // The same question, with the Route naming its own backend. The
    // paths need not run here — what this arm pins is that the
    // configuration reaches `wirk_atlas::search`, which is observable
    // because the reason stops being about the request and becomes the
    // estate's own retrieval admission.
    let configured_route = one_stage_route_with(
        &estate.root,
        "with-backend",
        question,
        r#"["demo"]"#,
        r#","semantic":{"backend":"/usr/bin/false","backend_args":["--adapter"],"model":"/nonexistent/model-snapshot"}"#,
    );
    let configured = submit_kind(
        &estate.root,
        configured_route.to_str().unwrap(),
        &estate.repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .expect("submit configured");
    let configured_projection =
        world_show(&estate.root, &configured.work_id, &configured.run_id)["projection"].clone();
    let configured_reason = configured_projection["retrieval"]["semantic_reason"]
        .as_str()
        .expect("a reason")
        .to_string();
    assert_ne!(
        configured_reason, reason,
        "a Route that configures a backend must not get the unconfigured answer"
    );
    assert!(
        !configured_reason.contains("named neither"),
        "the request did name a backend: {configured_reason}"
    );
    assert!(
        configured_reason.contains("its selected edition is none"),
        "the configured request reaches the estate's own retrieval admission: {configured_reason}"
    );
    // The fallback is still honest in every other respect: lexical hits,
    // an empty edition list, and no claim that a semantic ranking ran.
    assert_eq!(configured_projection["retrieval"]["mode"], "lexical");
    assert_eq!(
        configured_projection["retrieval"]["semantic"],
        "unavailable"
    );
    assert!(
        !items(&configured_projection, "referenced").is_empty(),
        "the lexical fallback still answers: {configured_projection}"
    );
    assert_eq!(
        configured_projection["retrieval"]["editions"]
            .as_array()
            .expect("editions list")
            .len(),
        0,
        "no edition was ranked through, so none is claimed"
    );

    estate.stop();
}

// ---- P3 world-capacity-correction: budget must never choose the ranking
// on the World's own semantic path (ruling 0172, query-capacity-review
// B1/B2) ----

/// The pinned development `semble` interpreter (`DEVELOPMENT.md`), read
/// from the required environment input — never a host-specific default
/// baked into the product (R2: same `#[ignore]`d-native-test shape
/// `wirk-atlas/tests/semantic_retrieval.rs`'s T5–T8 already use). Only
/// reached once this test itself runs, i.e. under an explicit
/// `--ignored`; panics with a clear reason rather than skipping, so an
/// opt-in run with a missing or wrong prerequisite fails loudly instead
/// of quietly recording a pass.
fn pinned_semble_python() -> PathBuf {
    let path = PathBuf::from(
        std::env::var("WIRK_TEST_SEMBLE_PYTHON").unwrap_or_else(|_| {
            panic!(
                "this test is opted in (--ignored) but WIRK_TEST_SEMBLE_PYTHON is unset: point it \
             at the pinned semble python3 interpreter (see DEVELOPMENT.md); this test never \
             falls back to a host-specific default"
            )
        }),
    );
    assert!(
        path.is_file(),
        "WIRK_TEST_SEMBLE_PYTHON={} is not a file: point it at the pinned semble python3 \
         interpreter (see DEVELOPMENT.md)",
        path.display()
    );
    path
}

/// The pinned offline `minishlab/potion-code-16M-v2` snapshot
/// (`DEVELOPMENT.md`), read the same way as `pinned_semble_python`.
fn pinned_semble_model() -> PathBuf {
    let path = PathBuf::from(std::env::var("WIRK_TEST_SEMBLE_MODEL").unwrap_or_else(|_| {
        panic!(
            "this test is opted in (--ignored) but WIRK_TEST_SEMBLE_MODEL is unset: point it \
             at the pinned offline potion-code-16M-v2 snapshot directory (see DEVELOPMENT.md); \
             this test never falls back to a host-specific default"
        )
    }));
    assert!(
        path.is_dir(),
        "WIRK_TEST_SEMBLE_MODEL={} is not a directory: point it at the pinned offline \
         potion-code-16M-v2 snapshot (see DEVELOPMENT.md)",
        path.display()
    );
    path
}

/// The product's own `wirk-embed/v2` + `wirk-query/v2` backend, unmodified
/// — not a copy, not a stub.
fn real_semble_backend_script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../wirk-atlas/backends/semble_backend.py")
}

/// A repository with more chunked rows than any rendering budget this
/// test authors, so a capacity that tracked the budget (the regression)
/// and a capacity that does not (the fix) are distinguishable by more
/// than a count.
fn capacity_repo(root: &Path) -> PathBuf {
    let repo = root.join("capacity-repo");
    fs::create_dir_all(&repo).expect("capacity repo dir");
    init_repo(&repo);
    const VOCAB: [&str; 24] = [
        "claim",
        "route",
        "journal",
        "ledger",
        "boundary",
        "capacity",
        "budget",
        "window",
        "continuation",
        "estate",
        "membership",
        "evidence",
        "candidate",
        "pool",
        "penalty",
        "selection",
        "rank",
        "score",
        "vector",
        "chunk",
        "generation",
        "coordinate",
        "referenced",
        "reachable",
    ];
    let mut seed: u64 = 183_092_026;
    let mut next = move || {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        (seed >> 33) as usize
    };
    for dir in ["engine", "shell", "vendor", "notes"] {
        fs::create_dir_all(repo.join(dir)).expect("capacity repo subdir");
        for index in 0..6 {
            let mut text = String::new();
            for block in 0..8 {
                text.push_str(&format!("def block_{block}():\n    # "));
                for _ in 0..30 {
                    text.push_str(VOCAB[next() % VOCAB.len()]);
                    text.push(' ');
                }
                text.push_str(&format!("\n    return {block}\n\n"));
            }
            write_file(&repo, &format!("{dir}/unit_{index}.py"), &text);
        }
    }
    commit_all(&repo);
    repo
}

/// B1/B2 (`query-capacity-review/VERIFIED.md`): `orient.budget` is
/// documented as how much of a ranked result gets *rendered*, never a
/// second control on what gets ranked. Before ruling 0172's correction,
/// the World assembly passed `budget.referenced()` straight through as
/// this query's own result capacity, so two reservations of the same
/// question that differ only in `budget.referenced_max` ranked at two
/// different `top_k` values and could deliver a different first result
/// and a different `total_candidates` — the invariant the sibling
/// lexical-path test
/// `a_budget_of_one_cuts_only_the_rendering_and_reports_the_real_totals`
/// already pins, but which nothing pinned on the semantic path, because
/// that test's own assembly has no semantic edition and falls to the
/// lexical path (B2).
///
/// Real corpus, real built-and-selected edition, real `wirk world show`
/// — no stub stands in for the ranker. `#[ignore]`d and opted in exactly
/// as `wirk-atlas/tests/semantic_retrieval.rs`'s T5–T8 are, with the same
/// loud failure on a missing prerequisite rather than a silent pass.
#[test]
#[ignore]
fn a_semantic_budget_of_one_and_forty_agree_on_the_ranking_and_the_real_total() {
    let python = pinned_semble_python();
    let model = pinned_semble_model();
    let script = real_semble_backend_script();

    let mut estate = Estate::new();
    let repo = capacity_repo(estate.root.parent().expect("estate parent"));
    let acquired = publish_reporting(&estate.root, "capacity", &repo);
    let generation = acquired["generation"]["generation"]
        .as_str()
        .expect("acquired generation id")
        .to_string();

    let (ok, built, err) = atlas(
        &estate.root,
        &[
            "semantic",
            "build",
            "--source",
            "capacity",
            "--generation",
            &generation,
            "--backend",
            python.to_str().unwrap(),
            "--backend-arg",
            script.to_str().unwrap(),
            "--model",
            model.to_str().unwrap(),
        ],
    );
    assert!(ok, "semantic build: {err}");
    let edition_id = built["edition"]["edition"]
        .as_str()
        .expect("staged edition id")
        .to_string();
    let (ok, _, err) = atlas(
        &estate.root,
        &[
            "semantic",
            "select",
            "--source",
            "capacity",
            "--edition",
            &edition_id,
        ],
    );
    assert!(ok, "semantic select: {err}");

    let question = "capacity window budget frozen boundary route candidate pool";
    let semantic = format!(
        r#","semantic":{{"backend":{backend},"backend_args":[{arg}],"model":{model}}}"#,
        backend = serde_json::to_string(python.to_str().unwrap()).unwrap(),
        arg = serde_json::to_string(script.to_str().unwrap()).unwrap(),
        model = serde_json::to_string(model.to_str().unwrap()).unwrap(),
    );

    let assemble = |name: &str, referenced_max: usize| -> Value {
        let route = one_stage_route_with(
            &estate.root,
            name,
            question,
            r#"["capacity"]"#,
            &format!(r#","budget":{{"referenced_max":{referenced_max}}}{semantic}"#),
        );
        let submitted = submit_kind(
            &estate.root,
            route.to_str().unwrap(),
            &repo,
            &["capacity:write"],
            None,
            Some("actor"),
        )
        .unwrap_or_else(|err| panic!("submit {name}: {err}"));
        world_show(&estate.root, &submitted.work_id, &submitted.run_id)["projection"].clone()
    };

    let tight = assemble("capacity-tight", 1);
    let wide = assemble("capacity-wide", 40);

    assert_eq!(tight["retrieval"]["mode"], "semantic", "{tight}");
    assert_eq!(wide["retrieval"]["mode"], "semantic", "{wide}");
    assert_eq!(
        tight["retrieval"]["semantic"], "applied",
        "the ranked query must actually run: {tight}"
    );
    assert_eq!(
        wide["retrieval"]["semantic"], "applied",
        "the ranked query must actually run: {wide}"
    );

    // B1, the mechanism: the World's own fixed capacity, not the
    // rendering budget, decides what gets ranked, so the real total the
    // query found is identical under both budgets.
    assert_eq!(
        tight["retrieval"]["total_candidates"], wide["retrieval"]["total_candidates"],
        "a rendering budget must not change the ranked result set (ruling 0172, B1): {tight} / \
         {wide}"
    );
    // B1, restated on the delivered bytes: the one item a budget of one
    // renders is the same item that leads the budget-of-forty list — not
    // a different top-1 that a different top_k produced.
    assert_eq!(
        items(&tight, "referenced")[0]["coordinate"],
        items(&wide, "referenced")[0]["coordinate"],
        "the top-ranked item must not move when only the rendering budget does: {tight} / {wide}"
    );

    // B3/B4: the delivered projection states the capacity it ranked at,
    // and it is the same capacity under both budgets — never something a
    // reader has to infer from a budget that no longer tracks it.
    let tight_capacity = &tight["retrieval"]["capacity"];
    let wide_capacity = &wide["retrieval"]["capacity"];
    assert!(
        tight_capacity.is_object(),
        "a real semantic answer must publish its capacity note (ruling 0172, B4): {tight}"
    );
    assert_eq!(
        tight_capacity, wide_capacity,
        "the published capacity must not move with the rendering budget: {tight} / {wide}"
    );
    assert_eq!(tight_capacity["max"].as_u64(), Some(200));

    // B3, restated: a projection under a tight rendering budget must
    // still say more was found than was shown — never `truncated: false`
    // beside a shorter delivered list, which is what B1 made happen.
    if items(&tight, "referenced").len()
        < tight["retrieval"]["total_candidates"].as_u64().unwrap_or(0) as usize
    {
        assert_eq!(tight["truncated"], true, "{tight}");
        let cut = items(&tight, "omitted")
            .iter()
            .find(|item| item["kind"] == "over_budget" && item["of"] == "referenced")
            .unwrap_or_else(|| panic!("the referenced list was cut and must say so: {tight}"));
        assert_eq!(
            cut["total"], tight["retrieval"]["total_candidates"],
            "the omission's total is the real ranked total, not the rendering budget: {tight}"
        );
    }

    estate.stop();
}

/// `world-capacity-review/VERIFIED.md` §2: a Route that authors no
/// `orient.capacity` gets the World's documented default of
/// `CAPACITY_MAX`, but before this correction the default and an
/// explicitly authored `200` were indistinguishable on the published
/// `retrieval.capacity.source` — both said `"explicit"`, the exact
/// confusion `capacity_source` (ruling 0171) exists to prevent. Same
/// question, same budget, same corpus; the only difference between the
/// two Routes is whether `orient.capacity` is authored at all.
#[test]
#[ignore]
fn a_route_that_authors_no_capacity_is_not_labelled_explicit() {
    let python = pinned_semble_python();
    let model = pinned_semble_model();
    let script = real_semble_backend_script();

    let mut estate = Estate::new();
    let repo = capacity_repo(estate.root.parent().expect("estate parent"));
    let acquired = publish_reporting(&estate.root, "capacity", &repo);
    let generation = acquired["generation"]["generation"]
        .as_str()
        .expect("acquired generation id")
        .to_string();

    let (ok, built, err) = atlas(
        &estate.root,
        &[
            "semantic",
            "build",
            "--source",
            "capacity",
            "--generation",
            &generation,
            "--backend",
            python.to_str().unwrap(),
            "--backend-arg",
            script.to_str().unwrap(),
            "--model",
            model.to_str().unwrap(),
        ],
    );
    assert!(ok, "semantic build: {err}");
    let edition_id = built["edition"]["edition"]
        .as_str()
        .expect("staged edition id")
        .to_string();
    let (ok, _, err) = atlas(
        &estate.root,
        &[
            "semantic",
            "select",
            "--source",
            "capacity",
            "--edition",
            &edition_id,
        ],
    );
    assert!(ok, "semantic select: {err}");

    let question = "capacity window budget frozen boundary route candidate pool";
    let semantic = format!(
        r#","semantic":{{"backend":{backend},"backend_args":[{arg}],"model":{model}}}"#,
        backend = serde_json::to_string(python.to_str().unwrap()).unwrap(),
        arg = serde_json::to_string(script.to_str().unwrap()).unwrap(),
        model = serde_json::to_string(model.to_str().unwrap()).unwrap(),
    );

    let assemble = |name: &str, extra: &str| -> Value {
        let route = one_stage_route_with(
            &estate.root,
            name,
            question,
            r#"["capacity"]"#,
            &format!(r#","budget":{{"referenced_max":8}}{semantic}{extra}"#),
        );
        let submitted = submit_kind(
            &estate.root,
            route.to_str().unwrap(),
            &repo,
            &["capacity:write"],
            None,
            Some("actor"),
        )
        .unwrap_or_else(|err| panic!("submit {name}: {err}"));
        world_show(&estate.root, &submitted.work_id, &submitted.run_id)["projection"].clone()
    };

    let unauthored = assemble("capacity-unauthored", "");
    let authored = assemble("capacity-authored-200", r#","capacity":200"#);

    assert_eq!(unauthored["retrieval"]["mode"], "semantic", "{unauthored}");
    assert_eq!(authored["retrieval"]["mode"], "semantic", "{authored}");

    let unauthored_capacity = &unauthored["retrieval"]["capacity"];
    let authored_capacity = &authored["retrieval"]["capacity"];
    assert!(
        unauthored_capacity.is_object() && authored_capacity.is_object(),
        "both must publish a capacity note: {unauthored} / {authored}"
    );

    // Same World default value in both — the fix is about the label, not
    // the number.
    assert_eq!(
        unauthored_capacity["capacity"], authored_capacity["capacity"],
        "an unauthored Route gets the same default 200 an authored one names explicitly: \
         {unauthored} / {authored}"
    );
    assert_eq!(unauthored_capacity["capacity"].as_u64(), Some(200));

    // The label must distinguish them: only the Route that actually named
    // a capacity gets "explicit".
    assert_eq!(
        authored_capacity["source"], "explicit",
        "a Route that authored capacity 200 named it: {authored}"
    );
    assert_ne!(
        unauthored_capacity["source"], "explicit",
        "a Route that authored no capacity must not be reported as having named one \
         (world-capacity-review/VERIFIED.md §2): {unauthored}"
    );

    estate.stop();
}

// ---- M7: `reachable` counts the indexed resources and nothing else -------

/// A `reachable` entry's `resources` is "how many indexed resources of
/// that family the captured generation actually holds". The suite owned
/// no case where that could be wrong, because every fixture resource was
/// `Indexed`.
///
/// A real source with real non-indexed resources: a secret-policy path
/// and a `.pem` (both `Excluded`), a blob with a NUL byte and a file
/// with no extractor (both `Unsupported`), beside one indexed code file
/// and one indexed knowledge file. The generation's own coverage report,
/// through the shipped `wirk atlas acquire --json`, is the positive
/// control that all six really are in the generation — and the
/// projection's handles count exactly the two that are indexed.
#[test]
fn reachable_counts_only_the_indexed_resources_of_the_captured_generation() {
    let mut estate = Estate::new();
    let mixed = estate
        .root
        .parent()
        .expect("estate parent")
        .join("mixed-repo");
    fs::create_dir_all(&mixed).expect("mixed repo dir");
    init_repo(&mixed);
    for sub in ["src", "notes", "notes/secret"] {
        fs::create_dir_all(mixed.join(sub)).expect("mixed subdir");
    }
    write_file(
        &mixed,
        "src/engine.rs",
        "pub fn mixed_marker() -> u8 { 7 }\n",
    );
    write_file(
        &mixed,
        "notes/rule.md",
        "# Rule\n\nmixed_marker is discussed here.\n",
    );
    write_file(
        &mixed,
        "notes/secret/hidden.md",
        "# Hidden\n\nmixed_marker\n",
    );
    write_file(&mixed, "deploy.pem", "-----BEGIN KEY-----\nmixed_marker\n");
    write_file(&mixed, "notes/blob.md", "mixed_marker\u{0}binary\n");
    write_file(&mixed, "notes/table.xyz", "mixed_marker\n");
    commit_all(&mixed);

    let acquired = publish_reporting(&estate.root, "mixed", &mixed);
    let coverage = &acquired["generation"]["coverage"];
    assert_eq!(coverage["total"], 6, "{coverage}");
    assert_eq!(coverage["indexed"], 2, "{coverage}");
    assert_eq!(coverage["excluded"], 2, "{coverage}");
    assert_eq!(coverage["unsupported"], 2, "{coverage}");

    let route = one_stage_route_with(
        &estate.root,
        "mixed-reach",
        "What decides mixed_marker?",
        r#"["mixed"]"#,
        "",
    );
    let submitted = submit_kind(
        &estate.root,
        route.to_str().unwrap(),
        &estate.repo,
        &["mixed:write"],
        None,
        Some("actor"),
    )
    .expect("submit mixed");
    let projection =
        world_show(&estate.root, &submitted.work_id, &submitted.run_id)["projection"].clone();

    let reachable = items(&projection, "reachable");
    let total: u64 = reachable
        .iter()
        .map(|entry| entry["resources"].as_u64().expect("a resource count"))
        .sum();
    assert_eq!(
        total, 2,
        "a handle counts indexed resources only: four of these six are excluded or unsupported \
         and none of them is something this handle can fetch: {reachable:?}"
    );
    for (family, expected) in [("code", 1u64), ("knowledge", 1u64)] {
        let entry = reachable
            .iter()
            .find(|entry| entry["family"] == family)
            .unwrap_or_else(|| panic!("the {family} family is admitted here: {reachable:?}"));
        assert_eq!(entry["resources"], expected, "{entry}");
        assert_eq!(entry["handle"], format!("mixed:{family}"));
    }

    // The positive control that the handle really addresses the indexed
    // ones: the printed line finds them, and finds nothing else.
    let fetch = reachable
        .iter()
        .find(|entry| entry["family"] == "knowledge")
        .expect("knowledge handle")["fetch"]
        .as_str()
        .expect("fetch line")
        .to_string();
    let mut argv: Vec<String> = fetch
        .split_whitespace()
        .skip(1)
        .map(str::to_string)
        .collect();
    *argv
        .iter_mut()
        .find(|arg| *arg == "<terms>")
        .expect("the fetch line asks for the actor's own terms") = "mixed_marker".to_string();
    argv.push("--json".to_string());
    let borrowed: Vec<&str> = argv.iter().map(String::as_str).collect();
    let (code, out, err) = in_pane(
        &estate.root,
        &submitted.work_id,
        &submitted.run_id,
        &borrowed,
    );
    assert_eq!(code, Some(0), "the printed discovery line must run: {err}");
    let found: Value = serde_json::from_str(&out).expect("search json");
    let hits = found["hits"].as_array().expect("hits");
    assert!(
        !hits.is_empty(),
        "the indexed knowledge file is found: {found}"
    );
    for hit in hits {
        let coordinate = hit["coordinate"].as_str().expect("coordinate");
        let (code, resolved, err) = in_pane(
            &estate.root,
            &submitted.work_id,
            &submitted.run_id,
            &["atlas", "resolve", "--coordinate", coordinate, "--json"],
        );
        assert_eq!(code, Some(0), "resolve: {err}");
        let resolved: Value = serde_json::from_str(&resolved).expect("resolve json");
        assert_eq!(
            resolved["outcome"], "resolved",
            "every counted resource is one the handle can actually fetch: {resolved}"
        );
    }

    estate.stop();
}

// ---- M4: the current Run's Claim, where a wrong Run is really reachable ---

/// The suite owned no case in which step 5 could pick the wrong Run:
/// every fixture Waypoint had exactly one. Here it has two, both real
/// and both opened through the public verbs — a Run that was failed and
/// a Run that was retried into existence and then claimed. Only the
/// second carries a Validated `Done` Claim and only the second reserved
/// a World, so an assembler that took the *first* Run of the Waypoint
/// binds nothing at all and reports nothing either.
///
/// The positive control is the same assertion in the same assembly: the
/// artifact does bind, at the digest that Claim validated, from the
/// checkout that Run reserved.
///
/// Limit, stated rather than manufactured: two *Validated* Claims on one
/// Waypoint are not reachable through a flat Route's public verbs — a
/// clean Claim advances the Work, and `wirk work retry` reopens only a
/// `NeedsInput` or held Waypoint. So this pins Run **selection**, not
/// the ordering of two competing validated Claims; ruling 0128 already
/// records that the forward/reverse scan of `validated_done_claim` is
/// equivalent while the Run filter stands.
#[test]
fn a_prior_stage_binds_the_run_that_claimed_it_not_the_first_run_of_that_waypoint() {
    let mut estate = Estate::new();
    let route = two_stage_route(&estate.root, "runselect", "");
    let submitted = submit_kind(
        &estate.root,
        route.to_str().unwrap(),
        &estate.repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .expect("submit");
    let first_run = submitted.run_id.clone();
    assert_eq!(submitted.waypoint, "runselect/survey");

    // A first, real Run of the survey stage that never claims anything.
    harness::fail_via_socket(&estate.socket, &estate.root, &submitted.work_id, &first_run);
    let (code, out) = harness::retry_cli(&estate.root, &submitted.work_id);
    assert_eq!(code, Some(0), "retry: {out}");
    let retried = status(&estate.socket, &submitted.work_id);
    assert_eq!(
        retried["current_waypoint"], "runselect/survey",
        "the retry must reopen the same Waypoint: {retried}"
    );
    let second_run = retried["run_id"].as_str().expect("run id").to_string();
    assert_ne!(
        second_run, first_run,
        "the retry must really be a second Run of one Waypoint"
    );

    // The second Run is the one that does the work and claims it.
    let worktree = claim_first_stage(
        &estate,
        &submitted.work_id,
        &second_run,
        "# Survey\n\nthe second Run's own claimed bytes\n",
    );

    let advanced = status(&estate.socket, &submitted.work_id);
    assert_eq!(advanced["current_waypoint"], "runselect/change");
    let change_run = advanced["run_id"].as_str().expect("run id").to_string();
    let projection =
        world_show(&estate.root, &submitted.work_id, &change_run)["projection"].clone();

    let artifact = items(&projection, "bound")
        .iter()
        .find(|item| item["identity"]["kind"] == "artifact_digest")
        .unwrap_or_else(|| {
            panic!(
                "step 5 must bind the Run that actually claimed, not the first Run this Waypoint \
                 ever opened: {projection}"
            )
        })
        .clone();
    assert_eq!(
        artifact["identity"]["digest"]
            .as_str()
            .expect("recorded digest"),
        sha256_of(&worktree.join("survey.md")),
        "the bound digest is the one the retried Run's Claim validated"
    );
    assert!(
        artifact["summary"]
            .as_str()
            .unwrap()
            .contains("the second Run's own claimed bytes"),
        "{artifact}"
    );
    assert!(
        artifact["reason"]
            .as_str()
            .unwrap()
            .contains("the prior stage `runselect/survey`"),
        "{artifact}"
    );
    // And nothing was reported unavailable: the first Run's absence of a
    // Claim is not a fact about the second Run's artifact.
    assert_eq!(projection["coverage"]["state"], "complete", "{projection}");

    estate.stop();
}

// ---- M3: the capture barrier, and a rewrite applied at it ----------------

/// The capture contract (`bind_prior_stage_artifacts`, ruling 0124): a
/// prior-stage artifact's bytes are read **once**; the digest is checked
/// over *those* bytes and the summary is derived from the *same* bytes.
/// There is no `digest_of(path)` followed by a second `read(path)`,
/// which is the window BUILD-AMENDMENTS names and which would let bytes
/// that arrived after the capture be delivered under the earlier Claim's
/// digest.
///
/// Nothing else in the suite can tell one read from two, because nothing
/// else changes the file between them. The predecessor of this test
/// tried to create that difference by **amplification**: two 8 MB files
/// swapped by `rename` in a loop while six real reservations assembled,
/// with the number of swaps asserted. A swap count proves a thread ran.
/// It does not establish that the decisive between-reads interleaving
/// occurred — the mutant happened to die on the second reservation, and
/// the count itself depends on scheduling. Ruling 0044: a test is
/// deterministic, or it is not a test.
///
/// This one **orders** it. The claimed artifact's path is replaced by a
/// FIFO (`support/read_barrier.rs`, R4 — the same "park the real daemon
/// inside its own code with a native facility and no product
/// instrumentation" move `git_gate.rs` already makes with `PATH`), and
/// the reservation is reopened through `wirk work retry --run`, whose
/// request reads the artifact exactly once, in the assembly. The real
/// daemon blocks in that one `std::fs::read`. The test then, in this
/// order:
///
/// 1. **supplies the captured bytes** — it is the pipe's only writer, so
///    which bytes this capture captured is known exactly rather than
///    inferred, and they are byte for byte the ones the first stage's
///    `wirk claim` recorded a digest over;
/// 2. **applies the rewrite** — an atomic `rename` of a different, whole
///    file over the same path, read back to show the path now names the
///    other bytes;
/// 3. **releases** the pipe, which is the only way that read can reach
///    EOF and return.
///
/// So the rewrite *happens-before* the capture completes, and therefore
/// before any second read of that path could open it. A faithful "read
/// twice" mutant cannot miss it: its second read is guaranteed the other
/// bytes, and it fails on the **attribution** — the rewritten bytes
/// summarized under the digest of the claimed ones — not on a timeout
/// and not on a compile error. A barrier that parked the wrong read
/// cannot produce a false green either: the assembly would then read the
/// rewritten file and refuse, and the assertion below fails.
///
/// What this establishes exactly, and what it does not: the delivered
/// summary and the delivered digest are attributions of one and the same
/// captured byte string, and this test knows that string. It carries no
/// promise that a rewrite *after* the only read was detected — the third
/// arm is that limit stated positively.
#[test]
fn a_rewrite_applied_at_the_capture_barrier_never_attributes_the_other_bytes() {
    let mut estate = Estate::new();
    let route = two_stage_route(&estate.root, "capture", "");
    let submitted = submit_kind(
        &estate.root,
        route.to_str().unwrap(),
        &estate.repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .expect("submit");

    // Small. The window this test opens is an order, not a duration, so
    // there is nothing left for file size to buy. Both markers sit
    // inside the first `ASSEMBLY_SUMMARY_BYTES`, which is where a
    // bounded summary comes from.
    let claimed = "# Survey\n\nclaimedmarker: the bytes the first stage claimed.\n";
    let other = "# Survey\n\nrewrittenmarker: bytes that arrived after the capture.\n";

    let worktree = claim_first_stage(&estate, &submitted.work_id, &submitted.run_id, claimed);
    let target = worktree.join("survey.md");
    let claimed_digest = sha256_of(&target);

    // The summary the capture owes for those exact bytes: their first
    // `ASSEMBLY_SUMMARY_BYTES`, newlines flattened. This and
    // `claimed_digest` are two attributions of the *same* byte string,
    // which is the whole property under test.
    let claimed_summary = claimed.replace('\n', " ");

    // --- control, before the barrier: an ordinary capture ------------------
    let mut run = status(&estate.socket, &submitted.work_id)["run_id"]
        .as_str()
        .expect("run id")
        .to_string();
    let before = world_show(&estate.root, &submitted.work_id, &run)["projection"].clone();
    let artifact = bound_artifact(&before, "the ordinary capture");
    assert_eq!(artifact["identity"]["digest"], claimed_digest, "{artifact}");
    assert_eq!(artifact["summary"], claimed_summary, "{artifact}");

    // --- the barrier ------------------------------------------------------
    let other_master = worktree.join("survey.other");
    fs::write(&other_master, other).expect("write the other bytes");
    harness::fail_via_socket(&estate.socket, &estate.root, &submitted.work_id, &run);

    let barrier = read_barrier::ReadBarrier::arm(&target);
    let retry_root = estate.root.clone();
    let retry_work = submitted.work_id.clone();
    let retry_run = run.clone();
    // `--run` names the leaf directly, so this reopen is one request and
    // the assembly's read is the only read of the artifact in it.
    let retry =
        std::thread::spawn(move || harness::retry_run_cli(&retry_root, &retry_work, &retry_run));

    // Returns exactly when the daemon has opened the claimed artifact
    // for its one read; it cannot proceed past that read until released.
    let mut held = barrier.park("the daemon assembles the reopened reservation");

    // 1. these bytes, and no others, are what this capture captured.
    held.supply(claimed.as_bytes());
    // 2. the rewrite, applied at the barrier. Atomic: the daemon's open
    //    file description still names the pipe, and the path now names a
    //    different, whole file.
    fs::rename(&other_master, &target).expect("atomic rewrite over the artifact path");
    assert_eq!(
        fs::read_to_string(&target).expect("read the rewritten path"),
        other,
        "the rewrite must have landed before the capture completes"
    );
    // 3. only now can that read reach EOF and return.
    held.release();

    let (code, out) = retry.join().expect("retry thread");
    assert_eq!(code, Some(0), "retry under the barrier: {out}");
    run = status(&estate.socket, &submitted.work_id)["run_id"]
        .as_str()
        .expect("run id")
        .to_string();

    // --- the decisive assertion -------------------------------------------
    let shown = world_show(&estate.root, &submitted.work_id, &run)["projection"].clone();
    let artifact = bound_artifact(&shown, "the capture ordered against a rewrite");
    assert_eq!(
        artifact["identity"]["digest"], claimed_digest,
        "a bound artifact carries the digest the Claim recorded: {artifact}"
    );
    assert_eq!(
        artifact["summary"], claimed_summary,
        "the summary must be an attribution of the same bytes the digest was taken over, and \
         those bytes are the ones this test supplied to the capture: {artifact}"
    );
    assert!(
        !artifact["summary"]
            .as_str()
            .expect("summary")
            .contains("rewrittenmarker"),
        "bytes that arrived after the capture were summarized under the earlier Claim's digest: \
         {artifact}"
    );

    // --- the limit, stated positively --------------------------------------
    // The contract qualifies *captured* bytes; it promises nothing about
    // a rewrite after the only read. What it does promise is that a
    // capture which reads the rewritten bytes refuses rather than binds
    // — and the path holds them now, put there by the rewrite above.
    harness::fail_via_socket(&estate.socket, &estate.root, &submitted.work_id, &run);
    let (code, out) = harness::retry_run_cli(&estate.root, &submitted.work_id, &run);
    assert_eq!(code, Some(0), "changed-bytes retry: {out}");
    run = status(&estate.socket, &submitted.work_id)["run_id"]
        .as_str()
        .expect("run id")
        .to_string();
    let changed = world_show(&estate.root, &submitted.work_id, &run)["projection"].clone();
    assert!(
        items(&changed, "bound")
            .iter()
            .all(|item| item["identity"]["kind"] != "artifact_digest"),
        "the rewritten artifact must not bind: {changed}"
    );
    let unavailable = items(&changed, "omitted")
        .iter()
        .find(|item| item["reason"] == "artifact_bytes_changed")
        .unwrap_or_else(|| panic!("an unbound artifact owes a closed reason: {changed}"))
        .clone();
    assert!(
        unavailable["coordinate"]
            .as_str()
            .expect("coordinate")
            .ends_with("/artifact/survey.md"),
        "{unavailable}"
    );

    // --- positive control, after -------------------------------------------
    // The claimed bytes back at the path and nothing scheduled against
    // the read: the artifact binds again with its own summary. So
    // neither outcome above came from a broken estate.
    fs::write(&target, claimed).expect("restore the claimed bytes");
    harness::fail_via_socket(&estate.socket, &estate.root, &submitted.work_id, &run);
    let (code, out) = harness::retry_run_cli(&estate.root, &submitted.work_id, &run);
    assert_eq!(code, Some(0), "control retry: {out}");
    let control_run = status(&estate.socket, &submitted.work_id)["run_id"]
        .as_str()
        .expect("run id")
        .to_string();
    let control = world_show(&estate.root, &submitted.work_id, &control_run)["projection"].clone();
    let artifact = bound_artifact(&control, "the restored capture");
    assert_eq!(artifact["identity"]["digest"], claimed_digest, "{artifact}");
    assert_eq!(artifact["summary"], claimed_summary, "{artifact}");

    estate.stop();
}

/// The one bound item whose identity is a prior-stage artifact digest.
fn bound_artifact(projection: &Value, arm: &str) -> Value {
    items(projection, "bound")
        .iter()
        .find(|item| item["identity"]["kind"] == "artifact_digest")
        .unwrap_or_else(|| panic!("a prior-stage artifact must be bound in {arm}: {projection}"))
        .clone()
}

// ---- source coverage the World is honest about ---------------------------

/// Real, valid UTF-8 Rust text past `wirk-atlas/src/extract.rs`'s
/// `MAX_TEXT_BYTES` (1 MiB). The extractor reads it, measures it and
/// refuses it exactly as it refuses any other oversize blob — the same
/// budget the actual product file `wirk/src/wirkd/server.rs` (902,199
/// bytes at `73d6d2d`) is approaching.
fn oversize_source_text() -> String {
    let mut text = String::with_capacity(1_200_000);
    let mut line = 0u32;
    while text.len() <= 1024 * 1024 {
        text.push_str(&format!(
            "pub fn oversize_{line}(argument: u32) -> u32 {{ argument.wrapping_add({line}) }}\n"
        ));
        line += 1;
    }
    text
}

fn expand_world(estate: &Path, work: &str, run: &str, question: &str, reason: &str) -> Value {
    let output = Command::new(wirk_bin())
        .args([
            "world",
            "expand",
            "--json",
            "--question",
            question,
            "--reason",
            reason,
        ])
        .env("WIRK_ESTATE_ROOT", estate)
        .env("WIRK_WORK_ID", work)
        .env("WIRK_RUN_ID", run)
        .output()
        .expect("wirk world expand runs");
    assert!(
        output.status.success(),
        "world expand: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("expansion json")
}

/// Ruling 0135 C4-R12, on the assembled World rather than on `atlas
/// search`: a captured generation that records a resource the extractor
/// could not turn into retrieval units at all is a completeness fact
/// about the delivered evidence, and the projection says so — as a
/// count, never as a path.
///
/// It then proves the two halves the source-coverage brief names
/// together: an **expansion** inherits the captured vector and therefore
/// keeps saying it (publication alone does not rebase that chain), while
/// a **repaired, republished** source assembled into a *new* World
/// recovers — and the frozen document that was already delivered is
/// untouched by either.
#[test]
fn a_world_over_a_generation_that_failed_to_extract_a_resource_discloses_it_as_a_count() {
    let mut estate = Estate::new();
    let holed = estate
        .root
        .parent()
        .expect("estate parent")
        .join("holed-repo");
    fs::create_dir_all(&holed).expect("holed repo dir");
    init_repo(&holed);
    write_file(&holed, "engine.rs", "pub fn holedmarker() -> u8 { 7 }\n");
    write_file(&holed, "quarantinedhuge.rs", &oversize_source_text());
    commit_all(&holed);

    // Positive control, from the estate's own acquisition reply: this is
    // one genuine extraction failure, not an exclusion and not an
    // unsupported family.
    let acquired = publish_reporting(&estate.root, "holed", &holed);
    let coverage = &acquired["generation"]["coverage"];
    assert_eq!(coverage["total"], 2, "{coverage}");
    assert_eq!(coverage["indexed"], 1, "{coverage}");
    assert_eq!(coverage["error"], 1, "{coverage}");
    assert_eq!(coverage["unsupported"], 0, "{coverage}");
    assert_eq!(coverage["excluded"], 0, "{coverage}");

    let route = one_stage_route_with(
        &estate.root,
        "holed-coverage",
        "What decides holedmarker?",
        r#"["holed"]"#,
        "",
    );
    let submitted = submit_kind(
        &estate.root,
        route.to_str().unwrap(),
        &estate.repo,
        &["holed:write"],
        None,
        Some("actor"),
    )
    .expect("submit holed");
    let shown = world_show(&estate.root, &submitted.work_id, &submitted.run_id);
    let projection = shown["projection"].clone();
    // The delivered document's own content identity, so "untouched"
    // below is a statement about bytes and not about a re-render.
    let initial_projection_id = shown["reference"]["projection"]
        .as_str()
        .expect("an initial projection identity")
        .to_string();

    assert_eq!(
        projection["coverage"]["state"], "partial",
        "CONTRACT FAILURE (ruling 0135 C4-R12): the captured generation holds a \
         resource nothing could extract, and the World called its evidence \
         complete: {projection}"
    );
    assert_eq!(
        projection["coverage"]["reason"], "source_extraction_incomplete",
        "the reason must name the source limitation, not borrow another one: \
         {projection}"
    );
    let omitted = omission(&projection, "source_extraction_incomplete")
        .unwrap_or_else(|| panic!("a counted omission: {projection}"));
    assert_eq!(omitted["count"], 1, "{omitted}");
    assert!(
        omitted.get("coordinate").is_none() && omitted.get("path").is_none(),
        "a count, and only a count — naming the resource would disclose a path \
         the projection never delivered: {omitted}"
    );
    assert!(
        projection["next_action"]
            .as_str()
            .expect("a next_action sentence")
            .contains("could not be extracted"),
        "the plain sentence must say what is missing: {projection}"
    );

    // Not a leak, and not the extractor's own diagnostic either: the
    // reason text carries a budget number no scope admitted.
    let rendered = shown.to_string();
    for needle in ["quarantinedhuge", "exceeds bounded", "holed-repo"] {
        assert!(
            !rendered.contains(needle),
            "the failing resource leaked {needle:?} into the delivered World"
        );
    }

    // An expansion inherits the captured vector, so it inherits the fact.
    let expanded = expand_world(
        &estate.root,
        &submitted.work_id,
        &submitted.run_id,
        "What else decides holedmarker?",
        "the first pass did not settle it",
    );
    assert_eq!(expanded["revision"], 1, "{expanded}");
    assert_eq!(
        expanded["projection"]["coverage"]["state"], "partial",
        "an expansion keeps the captured vector, so it keeps what that vector \
         could not deliver: {expanded}"
    );
    assert_eq!(
        expanded["projection"]["coverage"]["reason"], "source_extraction_incomplete",
        "{expanded}"
    );

    // A real refresh of the source: a new commit with no blob past the
    // extractor's budget, acquired and published as a new generation.
    // Nothing already recorded is edited.
    fs::remove_file(holed.join("quarantinedhuge.rs")).expect("remove the oversize blob");
    commit_all(&holed);
    let repaired = publish_reporting(&estate.root, "holed", &holed);
    let repaired_coverage = &repaired["generation"]["coverage"];
    assert_eq!(repaired_coverage["error"], 0, "{repaired_coverage}");
    assert_eq!(repaired_coverage["indexed"], 1, "{repaired_coverage}");

    // A new Work, bound the same way, captures the repaired generation.
    let recovered_route = one_stage_route_with(
        &estate.root,
        "holed-recovered",
        "What decides holedmarker?",
        r#"["holed"]"#,
        "",
    );
    let recovered = submit_kind(
        &estate.root,
        recovered_route.to_str().unwrap(),
        &estate.repo,
        &["holed:write"],
        None,
        Some("actor"),
    )
    .expect("submit recovered");
    let recovered_projection =
        world_show(&estate.root, &recovered.work_id, &recovered.run_id)["projection"].clone();
    assert_eq!(
        recovered_projection["coverage"]["state"], "complete",
        "a repaired, republished source assembled into a new World recovers: \
         {recovered_projection}"
    );
    assert!(
        omission(&recovered_projection, "source_extraction_incomplete").is_none(),
        "{recovered_projection}"
    );

    // And the document that was already delivered is untouched by
    // either the expansion or the republication: revision 0 still hashes
    // to exactly the bytes it was delivered with.
    let frozen = world_show(&estate.root, &submitted.work_id, &submitted.run_id);
    assert_eq!(
        frozen["revisions"][0]["projection"], initial_projection_id,
        "the initial revision is immutable: {frozen}"
    );
    assert_eq!(frozen["revisions"][0]["initial"], true, "{frozen}");

    estate.stop();
}

/// The mirror control, on the World: `indexed < total` for the reasons a
/// source declares — an unsupported family, a deliberately excluded path
/// — is not a failure and must not move coverage (ruling 0135's own
/// qualification to R12: "treating every intentionally unsupported file
/// as failed would obscure the map").
#[test]
fn a_world_over_declared_exclusions_and_unsupported_families_stays_complete() {
    let mut estate = Estate::new();
    let declared = estate
        .root
        .parent()
        .expect("estate parent")
        .join("declared-repo");
    fs::create_dir_all(&declared).expect("declared repo dir");
    init_repo(&declared);
    write_file(
        &declared,
        "engine.rs",
        "pub fn declaredmarker() -> u8 { 7 }\n",
    );
    write_file(&declared, "table.xyz", "declaredmarker\n");
    write_file(
        &declared,
        "deploy.pem",
        "-----BEGIN KEY-----\ndeclaredmarker\n",
    );
    commit_all(&declared);

    let acquired = publish_reporting(&estate.root, "declared", &declared);
    let coverage = &acquired["generation"]["coverage"];
    assert_eq!(coverage["indexed"], 1, "{coverage}");
    assert_eq!(coverage["unsupported"], 1, "{coverage}");
    assert_eq!(coverage["excluded"], 1, "{coverage}");
    assert_eq!(coverage["error"], 0, "{coverage}");

    let route = one_stage_route_with(
        &estate.root,
        "declared-coverage",
        "What decides declaredmarker?",
        r#"["declared"]"#,
        "",
    );
    let submitted = submit_kind(
        &estate.root,
        route.to_str().unwrap(),
        &estate.repo,
        &["declared:write"],
        None,
        Some("actor"),
    )
    .expect("submit declared");
    let projection =
        world_show(&estate.root, &submitted.work_id, &submitted.run_id)["projection"].clone();
    assert_eq!(
        projection["coverage"]["state"], "complete",
        "declared coverage is not a hole: {projection}"
    );
    assert!(
        omission(&projection, "source_extraction_incomplete").is_none(),
        "{projection}"
    );

    estate.stop();
}
