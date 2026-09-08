//! P3 W-C3: expansion of a delivered stage context, proved through the
//! public verbs against a real `wirkd`, a real Atlas and real Git
//! objects.
//!
//! Nothing here is a fake (ruling 0040). Every revision is one the
//! shipped `wirk world expand` assembled and wrote, every coordinate is
//! followed back to committed bytes through the shipped `wirk atlas
//! resolve`, and every refusal is the daemon's own.
//!
//! The properties, and each one is a defect if it does not hold:
//!
//! * an expansion **adds** a revision — the World, its hash, and every
//!   file already written are untouched, and revision 0 stays readable
//!   byte-identical;
//! * a `reachable` handle expands into **real source evidence**, not a
//!   restated listing;
//! * a handle no revision of this Run's own context delivered opens
//!   nothing;
//! * the captured generation vector is preserved: an expansion never
//!   reads today's bytes under the identity of the generation the stage
//!   was pinned to;
//! * a superseded Run cannot expand, and a retry starts a fresh chain at
//!   revision 0 while the superseded Run's revisions stay intact;
//! * two concurrent expansions produce an ordered chain or an explicit
//!   `Conflict`, never a lost update;
//! * the chain survives a daemon restart, in order;
//! * a narrowed status reader is told the chain is withheld, and is
//!   never shown a coordinate.

#[path = "support/nested_harness.rs"]
mod harness;
#[path = "../src/wirkd/mod.rs"]
mod wirkd;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use harness::{
    KillOnDrop, init_repo, start_wirkd, status, stop_wirkd, submit_kind, wirk_bin, write_file,
};
use serde_json::Value;

// ---- the estate -----------------------------------------------------------

/// One real repository the Work binds, with enough distinct material
/// that an expansion has something to find that the initial assembly did
/// not bind.
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
    // Nothing in the Route's authored question names either of these, so
    // neither is bound at revision 0. They are what an expansion has to
    // actually reach.
    write_file(
        &repo,
        "notes/quarantine.md",
        "# Quarantine\n\nquarantinemarker: a held container releases its leaf only when the \
         latest required receipt is present.\n",
    );
    write_file(
        &repo,
        "notes/receipts.md",
        "# Receipts\n\nquarantinemarker is decided here: a receipt from a superseded attempt is \
         never borrowed.\n",
    );
    commit_all(&repo);
    repo
}

/// A second real repository a stage can be oriented to alongside `demo`,
/// so the captured generation vector names more than one membership and
/// a `--reference` handle genuinely narrows it. Without a second source
/// there is no difference between "what this Work still binds" and "what
/// this request chose to read", and the two cannot be told apart.
fn extra_repo(root: &Path) -> PathBuf {
    let repo = root.join("extra-repo");
    fs::create_dir_all(&repo).expect("extra repo dir");
    init_repo(&repo);
    fs::create_dir_all(repo.join("notes")).expect("notes dir");
    write_file(
        &repo,
        "notes/extra-receipts.md",
        "# Extra receipts\n\nquarantinemarker also appears in the extra source, which this \
         stage is oriented to as well.\n",
    );
    write_file(
        &repo,
        "notes/extra-policy.md",
        "# Extra policy\n\nquarantinemarker policy lives here, in the extra source.\n",
    );
    commit_all(&repo);
    repo
}

/// A published, indexed source this Work never binds — so every scope
/// assertion has something real to fail to leak.
fn foreign_repo(root: &Path) -> PathBuf {
    let repo = root.join("other-repo");
    fs::create_dir_all(&repo).expect("other repo dir");
    init_repo(&repo);
    fs::create_dir_all(repo.join("notes")).expect("notes dir");
    write_file(
        &repo,
        "notes/elsewhere.md",
        "# Elsewhere\n\nquarantinemarker is discussed here too, in a source no Work below \
         binds.\n",
    );
    commit_all(&repo);
    repo
}

fn commit_all(repo: &Path) {
    for args in [
        vec!["add", "-A"],
        vec![
            "-c",
            "user.name=expansion-test",
            "-c",
            "user.email=expansion@example.test",
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

fn publish(estate: &Path, alias: &str, repo: &Path) -> String {
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
    generation
}

struct Estate {
    _dir: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    daemon: Option<KillOnDrop>,
    socket: PathBuf,
}

/// `<estate>/atlas/generations/<id>` — the immutable, content-addressed
/// generation directory `atlas.generation()` reads a pinned generation
/// back out of. Taking its manifest away is how a test makes exactly one
/// captured membership unreadable, without touching any other.
fn generation_dir(estate: &Path, generation: &str) -> PathBuf {
    estate.join("atlas").join("generations").join(generation)
}

impl Estate {
    fn new() -> Estate {
        let dir = tempfile::tempdir().expect("temp estate");
        let root = dir.path().join("estate");
        fs::create_dir_all(&root).expect("estate dir");
        let repo = source_repo(dir.path());
        let extra = extra_repo(dir.path());
        let other = foreign_repo(dir.path());
        let (daemon, pointer) = start_wirkd(&root);
        publish(&root, "demo", &repo);
        publish(&root, "extra", &extra);
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

/// One orienting Actor leaf. Its question names `src/server.rs` and
/// `claim_boundary_refusal` and nothing else, so `notes/quarantine.md`
/// and `notes/receipts.md` are reachable but unbound at revision 0.
fn one_stage_route(estate: &Path, name: &str) -> PathBuf {
    let dir = estate.join("routes");
    fs::create_dir_all(&dir).expect("routes dir");
    let path = dir.join(format!("{name}.json"));
    fs::write(
        &path,
        format!(
            r#"{{"id":{id},"waypoints":[
              {{"id":{leaf},"kind":"Actor",
                "declared_outputs":[{{"name":"out.md","required":true}}],
                "intent":"Decide the boundary refusal.",
                "orient":{{"question":"Where is claim_boundary_refusal decided in src/server.rs?","sources":["demo"]}}}}
            ]}}"#,
            id = serde_json::to_string(name).unwrap(),
            leaf = serde_json::to_string(&format!("{name}/only")).unwrap(),
        ),
    )
    .expect("write route");
    path
}

/// The same leaf, oriented to **two** sources. Its question still names
/// only `src/server.rs`, so both sources are reachable and neither is
/// fully bound at revision 0.
fn two_source_route(estate: &Path, name: &str) -> PathBuf {
    let dir = estate.join("routes");
    fs::create_dir_all(&dir).expect("routes dir");
    let path = dir.join(format!("{name}.json"));
    fs::write(
        &path,
        format!(
            r#"{{"id":{id},"waypoints":[
              {{"id":{leaf},"kind":"Actor",
                "declared_outputs":[{{"name":"out.md","required":true}}],
                "intent":"Decide the boundary refusal.",
                "orient":{{"question":"Where is claim_boundary_refusal decided in src/server.rs?","sources":["demo","extra"]}}}}
            ]}}"#,
            id = serde_json::to_string(name).unwrap(),
            leaf = serde_json::to_string(&format!("{name}/only")).unwrap(),
        ),
    )
    .expect("write route");
    path
}

// ---- the public verbs, exactly as an actor types them ---------------------

fn world_show_args(estate: &Path, work: &str, run: &str, args: &[&str]) -> (Option<i32>, Value) {
    let mut full = vec!["world", "show", "--json"];
    full.extend_from_slice(args);
    let output = Command::new(wirk_bin())
        .args(&full)
        .env("WIRK_ESTATE_ROOT", estate)
        .env("WIRK_WORK_ID", work)
        .env("WIRK_RUN_ID", run)
        .output()
        .expect("wirk world show runs");
    (
        output.status.code(),
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).unwrap_or(Value::Null),
    )
}

fn world_show(estate: &Path, work: &str, run: &str) -> Value {
    let (code, value) = world_show_args(estate, work, run, &[]);
    assert_eq!(code, Some(0), "world show: {value}");
    value
}

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

fn expand(estate: &Path, work: &str, run: &str, args: &[&str]) -> (Option<i32>, Value, String) {
    let mut full = vec!["world", "expand", "--json"];
    full.extend_from_slice(args);
    let output = Command::new(wirk_bin())
        .args(&full)
        .env("WIRK_ESTATE_ROOT", estate)
        .env("WIRK_WORK_ID", work)
        .env("WIRK_RUN_ID", run)
        .output()
        .expect("wirk world expand runs");
    (
        output.status.code(),
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).unwrap_or(Value::Null),
        String::from_utf8_lossy(&output.stderr).trim().to_string(),
    )
}

fn expand_ok(estate: &Path, work: &str, run: &str, args: &[&str]) -> Value {
    let (code, value, err) = expand(estate, work, run, args);
    assert_eq!(code, Some(0), "world expand {args:?}: {err}");
    value
}

fn resolve(estate: &Path, work: &str, run: &str, coordinate: &str) -> (Option<i32>, Value, String) {
    let output = Command::new(wirk_bin())
        .args(["atlas", "resolve", "--coordinate", coordinate, "--json"])
        .env("WIRK_ESTATE_ROOT", estate)
        .env("WIRK_WORK_ID", work)
        .env("WIRK_RUN_ID", run)
        .output()
        .expect("wirk atlas resolve runs");
    (
        output.status.code(),
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).unwrap_or(Value::Null),
        String::from_utf8_lossy(&output.stderr).trim().to_string(),
    )
}

fn projection_file(estate: &Path, work: &str, observation: &str) -> Vec<u8> {
    fs::read(
        estate
            .join("works")
            .join(work)
            .join("projections")
            .join(format!("{observation}.json")),
    )
    .expect("projection file is readable")
}

fn items<'a>(projection: &'a Value, key: &str) -> &'a Vec<Value> {
    projection[key]
        .as_array()
        .unwrap_or_else(|| panic!("{key} must be a list: {projection}"))
}

fn coordinates(projection: &Value, key: &str) -> Vec<String> {
    items(projection, key)
        .iter()
        .filter_map(|item| item["coordinate"].as_str().map(str::to_string))
        .collect()
}

fn submit_oriented(estate: &Estate, name: &str) -> harness::Submitted {
    let route = one_stage_route(&estate.root, name);
    submit_kind(
        &estate.root,
        route.to_str().unwrap(),
        &estate.repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .unwrap_or_else(|err| panic!("submit {name}: {err}"))
}

fn submit_two_source(estate: &Estate, name: &str) -> harness::Submitted {
    let route = two_source_route(&estate.root, name);
    submit_kind(
        &estate.root,
        route.to_str().unwrap(),
        &estate.repo,
        &["demo:write", "extra:read"],
        None,
        Some("actor"),
    )
    .unwrap_or_else(|err| panic!("submit {name}: {err}"))
}

fn statements(projection: &Value, key: &str) -> Vec<(String, String)> {
    items(projection, key)
        .iter()
        .map(|statement| {
            (
                statement["text"].as_str().unwrap_or_default().to_string(),
                statement["attributed_to"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect()
}

/// **This** request's own report of what happened to the captured
/// generation vector, selected by the identity of the request that
/// produced it and never by where it sits.
///
/// The chain carries every revision's statement forward as the
/// historical fact it is, and exact-text, same-attribution Assembly
/// statements are then collapsed to one copy — the *first* — in the
/// list being assembled (`prepare_expansion`, the D3 correction). So
/// position stopped being an identity the moment two requests could
/// make the same one: a request repeating an earlier request's scope
/// verbatim leaves its own surviving sentence at the earlier revision's
/// position, with a later, differently-scoped one after it, and the
/// last statement in the list is then some other request's
/// (`a_repeated_earlier_scope_is_still_reported_as_this_requests_own_scope`,
/// which is red on the `rfind` this replaces).
///
/// The identity is the one the sentence itself carries: the delivered
/// handle a narrowed request read through, or the absence of any handle
/// clause for an unnarrowed one.
///
/// **A handle is a better identity than a position, and it is still not a
/// request identity.** The sentence also reports how many of the selected
/// memberships were still readable, so two requests narrowing to the same
/// handle across a readability change produce two sentences that differ
/// in exactly that number — and both survive the dedup, because both are
/// true and neither is a copy of the other
/// (`the_same_handle_across_a_readability_change_leaves_two_truthful_statements`).
/// Two matches here therefore means *this* question was under-specified,
/// never that the product's dedup is broken and never that its history
/// should be collapsed further; `vector_statement_where` is how a caller
/// in that position says which observation it means. None means the
/// revision reported no captured vector at all.
fn vector_statement_for(projection: &Value, handle: Option<&str>) -> String {
    vector_statement_where(projection, handle, &[])
}

/// The same selection, narrowed by what this request actually observed.
///
/// `also` is matched against the statement text alongside the handle:
/// the readability count a caller knows its own request found is enough
/// to name one of two legitimate historical statements, without the
/// helper guessing and without a single byte of history being dropped to
/// make the question easier.
fn vector_statement_where(projection: &Value, handle: Option<&str>, also: &[&str]) -> String {
    let needle = handle.map(|handle| format!("the delivered handle `{handle}` names"));
    let mut matched: Vec<String> = statements(projection, "assumptions")
        .into_iter()
        .map(|(text, _)| text)
        .filter(|text| text.contains("preserved the captured generation vector"))
        .filter(|text| match &needle {
            Some(needle) => text.contains(needle.as_str()),
            None => !text.contains("the delivered handle"),
        })
        .filter(|text| also.iter().all(|extra| text.contains(extra)))
        .collect();
    assert_eq!(
        matched.len(),
        1,
        "exactly one captured-vector statement is the one {} produced. More than one is this \
         question being under-specified — the same handle read across a readability change leaves \
         two truthful historical statements, told apart by the readability count each reports — \
         and none means the revision reported no captured vector at all. Matched: {matched:?} in \
         {projection}",
        match (handle, also) {
            (Some(handle), []) => format!("a request narrowed to `{handle}`"),
            (Some(handle), also) =>
                format!("a request narrowed to `{handle}` that observed {also:?}"),
            (None, []) => "an unnarrowed request".to_string(),
            (None, also) => format!("an unnarrowed request that observed {also:?}"),
        }
    );
    matched.remove(0)
}

/// The unnarrowed request's own captured-vector statement.
fn vector_statement(projection: &Value) -> String {
    vector_statement_for(projection, None)
}

/// The captured `(membership, generation)` pairs of a delivered
/// revision, in the order the vector names them.
fn captured_generations(projection: &Value) -> Vec<String> {
    items(projection, "generations")
        .iter()
        .map(|pair| pair[1].as_str().expect("generation id").to_string())
        .collect()
}

fn unavailable_coordinates(projection: &Value) -> Vec<String> {
    items(projection, "omitted")
        .iter()
        .filter(|omission| omission["kind"] == "unavailable")
        .filter_map(|omission| omission["coordinate"].as_str().map(str::to_string))
        .collect()
}

// ---- the properties -------------------------------------------------------

/// An expansion adds a revision. It does not edit one.
///
/// The three things that must not move are checked by identity, not by
/// inspection: the reserved World's own `world_hash` (the stage's resume
/// key), the exact bytes of revision 0's file on disk, and the
/// `ProjectionId` revision 0 re-hashes to. If an expansion were
/// implemented by rewriting the delivered file — the obvious shortcut —
/// all three move at once.
#[test]
fn an_expansion_adds_a_revision_and_never_edits_the_one_it_expands() {
    let mut estate = Estate::new();
    let work = submit_oriented(&estate, "expand-adds");

    let before = world_show(&estate.root, &work.work_id, &work.run_id);
    assert_eq!(before["orientation"], "delivered", "{before}");
    assert_eq!(before["reference"]["revision"], 0);
    assert_eq!(before["latest_revision"], 0);
    assert_eq!(items(&before, "revisions").len(), 1);
    let initial_observation = before["reference"]["observation"]
        .as_str()
        .expect("observation")
        .to_string();
    let initial_projection = before["reference"]["projection"]
        .as_str()
        .expect("projection id")
        .to_string();
    let initial_bytes = projection_file(&estate.root, &work.work_id, &initial_observation);
    let initial_world_hash = status(&estate.socket, &work.work_id)["world_hash"]
        .as_str()
        .expect("world hash")
        .to_string();

    let expanded = expand_ok(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &[
            "--question",
            "What does notes/quarantine.md say about quarantinemarker?",
            "--reason",
            "the boundary decision turned on a held container",
        ],
    );
    assert_eq!(expanded["revision"], 1, "{expanded}");
    assert_eq!(
        expanded["parent"],
        initial_observation.as_str(),
        "the new revision names the revision it expands: {expanded}"
    );
    // The reply renders through the same code `world show`'s does, so it
    // carries the same facts: a Run that just proved it was current must
    // not be rendered as though it were not.
    assert_eq!(expanded["current"], true, "{expanded}");
    assert_eq!(expanded["latest_revision"], 1, "{expanded}");
    assert_eq!(items(&expanded, "revisions").len(), 2, "{expanded}");
    assert_eq!(items(&expanded, "revisions")[0]["initial"], true);
    let revision_one = expanded["projection"].clone();
    assert_eq!(revision_one["revision"], 1);
    assert_eq!(
        revision_one["expansion"]["parent_projection"],
        initial_projection.as_str(),
        "{revision_one}"
    );
    assert_eq!(
        revision_one["expansion"]["parent_observation"],
        initial_observation.as_str(),
        "{revision_one}"
    );
    assert_eq!(
        revision_one["expansion"]["expanded_by"],
        work.run_id.as_str(),
        "{revision_one}"
    );
    assert_eq!(
        revision_one["expansion"]["basis"], "preserved_captured_vector",
        "{revision_one}"
    );
    assert_eq!(
        revision_one["expansion"]["request"]["authored_question"], true,
        "{revision_one}"
    );
    assert_eq!(
        revision_one["expansion"]["request"]["reason"],
        "the boundary decision turned on a held container"
    );
    // The stage's own orientation question is not overwritten by what
    // the expansion asked: a reader can tell the two apart.
    assert_eq!(revision_one["question"], before["projection"]["question"]);

    // Nothing that was already delivered moved.
    assert_eq!(
        projection_file(&estate.root, &work.work_id, &initial_observation),
        initial_bytes,
        "revision 0's file must be byte-identical after an expansion"
    );
    assert_eq!(
        status(&estate.socket, &work.work_id)["world_hash"]
            .as_str()
            .unwrap(),
        initial_world_hash,
        "an expansion must not move the stage's resume key"
    );
    let (code, historical) = world_show_args(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &["--revision", "0"],
    );
    assert_eq!(code, Some(0));
    assert_eq!(historical["orientation"], "delivered", "{historical}");
    assert_eq!(
        historical["reference"]["projection"],
        initial_projection.as_str(),
        "revision 0 still re-hashes to the id its journal recorded: {historical}"
    );
    assert_eq!(historical["projection"], before["projection"]);

    // The default is the latest, and the chain is discoverable.
    let latest = world_show(&estate.root, &work.work_id, &work.run_id);
    assert_eq!(latest["reference"]["revision"], 1, "{latest}");
    assert_eq!(latest["latest_revision"], 1);
    let chain = items(&latest, "revisions");
    assert_eq!(chain.len(), 2, "{latest}");
    assert_eq!(chain[0]["revision"], 0);
    assert_eq!(chain[0]["initial"], true);
    assert_eq!(chain[1]["revision"], 1);
    assert_eq!(chain[1]["initial"], false);

    // The expansion actually found the material the initial assembly did
    // not bind — an added revision that added nothing would satisfy every
    // identity check above and be useless.
    let added: Vec<String> = coordinates(&revision_one, "bound")
        .into_iter()
        .filter(|coordinate| !coordinates(&before["projection"], "bound").contains(coordinate))
        .collect();
    assert!(
        !added.is_empty(),
        "an expansion that binds nothing new is not an expansion: {revision_one}"
    );
    let mut reached_quarantine = false;
    let mut real_bytes = String::new();
    for coordinate in &added {
        let (code, resolved, err) = resolve(&estate.root, &work.work_id, &work.run_id, coordinate);
        assert_eq!(code, Some(0), "resolve {coordinate}: {err}");
        assert_eq!(
            resolved["outcome"], "resolved",
            "an expanded coordinate must resolve, not merely parse: {resolved}"
        );
        let path = resolved["path"].as_str().unwrap_or_default().to_string();
        if path.contains("quarantine") {
            reached_quarantine = true;
        }
        real_bytes.push_str(resolved["text"].as_str().unwrap_or_default());
    }
    assert!(
        reached_quarantine,
        "the authored expansion named notes/quarantine.md by path; it must be bound: {added:?}"
    );
    assert!(
        real_bytes.contains("quarantinemarker") || {
            // A path reference binds the file's units; the marker may be
            // in a unit the request did not select. The delivered
            // summaries are then the evidence that real bytes arrived.
            items(&revision_one, "bound").iter().any(|item| {
                item["summary"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("quarantinemarker")
            })
        },
        "an expansion must deliver committed bytes, not a restated listing: {real_bytes}"
    );

    // The plain text a human and a fresh actor actually read says the
    // context has a history and how to reach it.
    let text = world_show_text(&estate.root, &work.work_id, &work.run_id);
    assert!(
        text.contains("context revisions 2"),
        "the rendering must say the context has a history: {text}"
    );
    assert!(
        text.contains("wirk world show --revision"),
        "and how to read an earlier one: {text}"
    );
    // A verb is advertised where the evidence for it is, and nowhere
    // else: the question form once, on a Run that can actually run it,
    // and the reference form beside each handle this context delivered.
    assert!(
        text.contains("add to it with: wirk world expand --question"),
        "the rendering must say the context can be added to: {text}"
    );
    for handle in items(&latest["projection"], "reachable") {
        let handle = handle["handle"].as_str().expect("handle");
        assert!(
            text.contains(&format!("wirk world expand --reference {handle}")),
            "each delivered handle carries the line that binds it: {text}"
        );
    }
    assert!(
        !text.contains("--reference unadmittedsource"),
        "and no handle this context did not deliver is offered: {text}"
    );

    estate.stop();
}

/// Expanding a delivered `reachable` handle binds real source evidence,
/// and a handle this Run's own context never delivered binds nothing.
///
/// The positive arm is the one that matters: a handle that expanded into
/// a restated listing — the handle string, its resource count, its fetch
/// line — would look like an expansion and give a stage nothing. What is
/// asserted is that the added items carry coordinates the shipped `wirk
/// atlas resolve` follows to the committed bytes.
#[test]
fn a_delivered_handle_expands_into_real_evidence_and_a_forged_one_into_nothing() {
    let mut estate = Estate::new();
    let work = submit_oriented(&estate, "expand-handle");
    let before = world_show(&estate.root, &work.work_id, &work.run_id);
    let handles: Vec<String> = items(&before["projection"], "reachable")
        .iter()
        .filter_map(|entry| entry["handle"].as_str().map(str::to_string))
        .collect();
    let handle = handles
        .iter()
        .find(|handle| handle.starts_with("demo:knowledge"))
        .unwrap_or_else(|| panic!("the initial context must offer a demo handle: {handles:?}"))
        .clone();

    let expanded = expand_ok(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &["--reference", &handle],
    );
    let revision = expanded["projection"].clone();
    assert_eq!(revision["revision"], 1, "{revision}");
    assert_eq!(
        revision["expansion"]["request"]["reference"],
        handle.as_str()
    );
    // No question was authored, and the record says so rather than
    // presenting the stage's question as the actor's words.
    assert_eq!(revision["expansion"]["request"]["authored_question"], false);
    assert_eq!(
        revision["expansion"]["request"]["question"],
        before["projection"]["question"]
    );

    let added: Vec<String> = coordinates(&revision, "bound")
        .into_iter()
        .filter(|coordinate| !coordinates(&before["projection"], "bound").contains(coordinate))
        .collect();
    assert!(
        !added.is_empty(),
        "expanding a handle must bind evidence, not repeat the listing: {revision}"
    );
    let mut real_bytes = 0usize;
    for coordinate in &added {
        let (code, resolved, err) = resolve(&estate.root, &work.work_id, &work.run_id, coordinate);
        assert_eq!(code, Some(0), "resolve {coordinate}: {err}");
        assert_eq!(resolved["outcome"], "resolved", "{resolved}");
        if serde_json::to_string(&resolved)
            .expect("serialize")
            .contains("quarantinemarker")
        {
            real_bytes += 1;
        }
    }
    assert!(
        real_bytes > 0,
        "at least one expanded coordinate must resolve to the committed bytes: {added:?}"
    );
    // The counts are content, and they are the truth about this
    // revision: an actor must be able to read "how much of this is new"
    // off the record rather than diffing two `bound` lists.
    assert_eq!(
        revision["expansion"]["delivered"].as_u64(),
        Some(added.len() as u64),
        "{revision}"
    );
    assert_eq!(revision["expansion"]["already_bound"], 0, "{revision}");

    // A second expansion of the same handle adds nothing, and says so
    // in a number rather than leaving a rising revision count to imply
    // new material. Not a refusal: "everything there is already here" is
    // an honest answer to a request an actor really made.
    let repeat = expand_ok(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &["--reference", &handle],
    );
    assert_eq!(repeat["revision"], 2, "{repeat}");
    assert_eq!(
        repeat["projection"]["expansion"]["delivered"], 0,
        "{repeat}"
    );
    assert_eq!(
        repeat["projection"]["expansion"]["already_bound"].as_u64(),
        Some(added.len() as u64),
        "{repeat}"
    );
    assert_eq!(
        items(&repeat["projection"], "bound").len(),
        items(&revision, "bound").len(),
        "nothing was delivered twice: {repeat}"
    );

    // And every added item says it came from inside the handle, so a
    // reader can tell why it is here.
    for item in items(&revision, "bound") {
        if added.contains(&item["coordinate"].as_str().unwrap_or_default().to_string()) {
            assert!(
                item["reason"]
                    .as_str()
                    .unwrap_or_default()
                    .contains(&handle),
                "an expanded item names the handle that reached it: {item}"
            );
        }
    }

    // A handle nothing in this Run's own chain delivered. `unadmittedsource`
    // is a real, published, indexed source — so this refusal is about the
    // chain, not about the string failing to name anything.
    for forged in [
        "unadmittedsource:knowledge",
        "demo:secrets",
        "../demo:knowledge",
    ] {
        let (code, _, err) = expand(
            &estate.root,
            &work.work_id,
            &work.run_id,
            &["--reference", forged],
        );
        assert_eq!(
            code,
            Some(3),
            "forged handle {forged} must be refused: {err}"
        );
        assert!(
            err.contains("UnknownHandle"),
            "forged handle {forged}: {err}"
        );
        assert!(
            !err.contains("unadmittedsource") || forged.contains("unadmittedsource"),
            "a refusal must not name a source the caller did not: {err}"
        );
    }
    // And the refusals appended nothing: the chain is still the two
    // expansions this Run really made, and no third.
    let after = world_show(&estate.root, &work.work_id, &work.run_id);
    assert_eq!(after["latest_revision"], 2, "{after}");
    assert_eq!(items(&after, "revisions").len(), 3, "{after}");

    estate.stop();
}

/// An expansion preserves the captured generation vector of the revision
/// it expands, and says so.
///
/// The estate publishes a **new** generation of the bound source between
/// the reservation and the expansion, with new content. The expansion
/// must still be pinned to the generation the stage was reserved at: the
/// vector is unchanged, and no coordinate in the new revision names the
/// newly published generation. Reading today's bytes under a historical
/// identity is exactly the substituted provenance ruling 0126 refuses.
#[test]
fn an_expansion_preserves_the_captured_vector_and_never_reads_todays_bytes() {
    let mut estate = Estate::new();
    let work = submit_oriented(&estate, "expand-pinned");
    let before = world_show(&estate.root, &work.work_id, &work.run_id);
    let captured: Vec<Value> = items(&before["projection"], "generations").clone();
    assert!(!captured.is_empty(), "{before}");
    let captured_generations: Vec<String> = captured
        .iter()
        .map(|pair| pair[1].as_str().expect("generation id").to_string())
        .collect();

    // The estate moves on: new bytes, a new generation, published.
    write_file(
        &estate.repo,
        "notes/quarantine.md",
        "# Quarantine\n\nquarantinemarker: REWRITTEN AFTER RESERVATION, this text was never in \
         the generation the stage was pinned to.\n",
    );
    commit_all(&estate.repo);
    let newer = publish(&estate.root, "demo", &estate.repo);
    assert!(
        !captured_generations.contains(&newer),
        "the test needs a genuinely new generation"
    );

    let expanded = expand_ok(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &[
            "--question",
            "What does notes/quarantine.md say about quarantinemarker?",
        ],
    );
    let revision = expanded["projection"].clone();
    assert_eq!(
        items(&revision, "generations"),
        &captured,
        "the expansion must carry the parent's vector unchanged: {revision}"
    );
    assert_eq!(
        revision["publication_revision"], before["projection"]["publication_revision"],
        "{revision}"
    );
    assert_eq!(revision["expansion"]["basis"], "preserved_captured_vector");

    let document = serde_json::to_string(&revision).expect("serialize");
    assert!(
        !document.contains(&newer),
        "no coordinate or identity in the expansion may name the newly published generation"
    );
    assert!(
        !document.contains("REWRITTEN AFTER RESERVATION"),
        "an expansion must never deliver today's bytes under the captured identity: {document}"
    );
    // And what it did bind resolves, at the generation it names.
    for item in items(&revision, "bound") {
        if let Some(generation) = item["identity"]["generation"].as_str() {
            assert!(
                captured_generations.contains(&generation.to_string()),
                "every bound item resolves at a captured generation: {item}"
            );
        }
    }
    // The assumption says all of this in the document the actor reads,
    // rather than leaving it to be inferred from the absence of a string.
    let assumptions = serde_json::to_string(items(&revision, "assumptions")).expect("serialize");
    assert!(
        assumptions.contains("preserved the captured generation vector")
            && assumptions.contains("observed no new one"),
        "{assumptions}"
    );

    estate.stop();
}

/// The chain is ordered, and a daemon restart does not reorder, lose or
/// invent a revision. Every revision stays individually readable at the
/// bytes it was delivered with.
#[test]
fn the_expansion_chain_is_ordered_and_survives_a_restart() {
    let mut estate = Estate::new();
    let work = submit_oriented(&estate, "expand-restart");
    let initial = world_show(&estate.root, &work.work_id, &work.run_id);
    let mut expected = vec![(
        0u64,
        initial["reference"]["observation"]
            .as_str()
            .unwrap()
            .to_string(),
        initial["reference"]["projection"]
            .as_str()
            .unwrap()
            .to_string(),
    )];
    for question in [
        "What does notes/quarantine.md say?",
        "What does notes/receipts.md say?",
    ] {
        let expanded = expand_ok(
            &estate.root,
            &work.work_id,
            &work.run_id,
            &["--question", question],
        );
        expected.push((
            expanded["revision"].as_u64().expect("revision"),
            expanded["reference"]["observation"]
                .as_str()
                .unwrap()
                .to_string(),
            expanded["reference"]["projection"]
                .as_str()
                .unwrap()
                .to_string(),
        ));
    }
    let before: Vec<Vec<u8>> = expected
        .iter()
        .map(|(_, observation, _)| projection_file(&estate.root, &work.work_id, observation))
        .collect();

    estate.restart();

    let after = world_show(&estate.root, &work.work_id, &work.run_id);
    let chain = items(&after, "revisions");
    assert_eq!(chain.len(), 3, "{after}");
    for (index, (revision, observation, projection)) in expected.iter().enumerate() {
        assert_eq!(chain[index]["revision"], *revision, "{after}");
        assert_eq!(chain[index]["observation"], observation.as_str(), "{after}");
        assert_eq!(chain[index]["projection"], projection.as_str(), "{after}");
        let (code, shown) = world_show_args(
            &estate.root,
            &work.work_id,
            &work.run_id,
            &["--revision", &revision.to_string()],
        );
        assert_eq!(code, Some(0));
        assert_eq!(shown["orientation"], "delivered", "{shown}");
        assert_eq!(
            shown["reference"]["projection"],
            projection.as_str(),
            "revision {revision} re-hashes to the id its journal recorded: {shown}"
        );
        assert_eq!(
            projection_file(&estate.root, &work.work_id, observation),
            before[index],
            "revision {revision}'s file is byte-identical across a restart"
        );
    }
    // A revision this Run was never delivered is refused by name, never
    // by silently handing back one it was.
    let (code, missing) = world_show_args(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &["--revision", "9"],
    );
    assert_eq!(code, Some(0));
    assert_eq!(missing["orientation"], "unavailable", "{missing}");
    assert_eq!(missing["reason"], "no-such-revision", "{missing}");

    // The daemon's startup sweep removes the temp files a crash between
    // write and rename leaves, and removes nothing else: a renamed file
    // nothing references is harmless residue, never deleted by age.
    let projections = estate
        .root
        .join("works")
        .join(&work.work_id)
        .join("projections");
    fs::write(projections.join(".tmp-obs-crashed"), b"{").expect("plant a temp file");
    fs::write(projections.join("obs-orphaned.json"), b"{}").expect("plant an orphan");
    estate.restart();
    // Synchronize through an actual completed request, never through the
    // pointer file: the pointer is published before the listener
    // accepts, and the sweep runs between the two. A reply to a real
    // request is proof the sweep already ran; looking at the filesystem
    // on the pointer's word is the readiness race ruling 0128 F4 names.
    let _ = world_show(&estate.root, &work.work_id, &work.run_id);
    assert!(
        !projections.join(".tmp-obs-crashed").exists(),
        "the startup sweep removes an unreachable temp file"
    );
    assert!(
        projections.join("obs-orphaned.json").exists(),
        "a renamed file nothing references is harmless residue and is never swept"
    );
    // And the residue changed nothing about what the chain is.
    let after = world_show(&estate.root, &work.work_id, &work.run_id);
    assert_eq!(items(&after, "revisions").len(), 3, "{after}");

    estate.stop();
}

/// Current authority is re-derived, never carried. A Run superseded by a
/// retry cannot expand the context it was delivered, and the revisions it
/// *was* delivered stay readable exactly as they were.
///
/// The retried Waypoint's new Run starts a fresh chain at revision 0: a
/// retry is a new reservation, so it inherits no revision from the
/// attempt it supersedes.
#[test]
fn a_superseded_run_cannot_expand_and_a_retry_starts_a_fresh_chain() {
    let mut estate = Estate::new();
    let work = submit_oriented(&estate, "expand-retry");
    let first = expand_ok(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &["--question", "What does notes/quarantine.md say?"],
    );
    assert_eq!(first["revision"], 1);
    let historical_observation = first["reference"]["observation"]
        .as_str()
        .unwrap()
        .to_string();
    let historical_bytes = projection_file(&estate.root, &work.work_id, &historical_observation);

    // A Run is superseded by a retry, and a retry needs a Work that is
    // waiting on something. The real path: this Run's own actor files a
    // Question claim, which is validated and puts the Work in
    // `NeedsInput` without completing the stage.
    let (code, out) = harness::claim(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &["--question", "which container holds the receipt?"],
    );
    assert_eq!(code, Some(0), "question claim: {out}");
    let (code, out) = harness::retry_cli(&estate.root, &work.work_id);
    assert_eq!(code, Some(0), "work retry: {out}");
    let retried = status(&estate.socket, &work.work_id);
    let new_run = retried["run_id"].as_str().expect("a new run").to_string();
    assert_ne!(new_run, work.run_id, "the retry opened a new Run");

    // The superseded Run's actor is refused, in the daemon, before
    // anything is written.
    let (code, _, err) = expand(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &["--question", "What does notes/receipts.md say?"],
    );
    assert_eq!(code, Some(3), "a superseded Run must not expand: {err}");
    assert!(err.contains("not current for its waypoint"), "{err}");

    // Its own revisions are untouched and still readable.
    assert_eq!(
        projection_file(&estate.root, &work.work_id, &historical_observation),
        historical_bytes
    );
    let historical = world_show(&estate.root, &work.work_id, &work.run_id);
    assert_eq!(historical["current"], false, "{historical}");
    assert_eq!(historical["latest_revision"], 1, "{historical}");
    // And its rendering advertises no verb it cannot run. An honest
    // absence: the superseded Run still reads its own delivered context,
    // and is not offered a command that would be refused.
    let historical_text = world_show_text(&estate.root, &work.work_id, &work.run_id);
    assert!(
        historical_text.contains("context revisions 2"),
        "{historical_text}"
    );
    // The delivered document's own prose still explains what expansion
    // is — that is content, and it is the same in every revision. What
    // must be absent is the *offer*: the lines that hand an actor a
    // command to type.
    for offer in [
        "add to it with: wirk world expand",
        "bind it into this context with: wirk world expand",
    ] {
        assert!(
            !historical_text.contains(offer),
            "a superseded Run must not be offered a verb that would refuse it: \
             {historical_text}"
        );
    }
    // The current Run is.
    let fresh_text = world_show_text(&estate.root, &work.work_id, &new_run);
    assert!(
        fresh_text.contains("wirk world expand --question"),
        "{fresh_text}"
    );

    // The new Run starts at revision 0 and inherits nothing.
    let fresh = world_show(&estate.root, &work.work_id, &new_run);
    assert_eq!(fresh["current"], true, "{fresh}");
    assert_eq!(fresh["latest_revision"], 0, "{fresh}");
    assert_eq!(items(&fresh, "revisions").len(), 1, "{fresh}");
    assert_ne!(
        fresh["reference"]["observation"],
        historical_observation.as_str(),
        "a retry writes its own file"
    );
    assert!(
        fresh["projection"]["expansion"].is_null(),
        "revision 0 expands nothing: {fresh}"
    );
    // And the new Run can expand its own context.
    let again = expand_ok(
        &estate.root,
        &work.work_id,
        &new_run,
        &["--question", "What does notes/receipts.md say?"],
    );
    assert_eq!(again["revision"], 1, "{again}");
    assert_eq!(
        again["parent"], fresh["reference"]["observation"],
        "the new chain's first expansion names the new Run's own revision 0: {again}"
    );

    estate.stop();
}

/// Two concurrent expansions of the same revision produce an ordered
/// chain or an explicit `Conflict`. Never a lost update, and never two
/// revisions claiming the same parent.
///
/// This is not a statistical race amplification: whatever the two
/// processes do, the invariant asserted is total — every revision in the
/// chain names its immediate predecessor, and the revision numbers are
/// exactly `0..n`.
#[test]
fn concurrent_expansions_chain_or_conflict_and_never_lose_one() {
    let mut estate = Estate::new();
    let work = submit_oriented(&estate, "expand-race");
    let initial = world_show(&estate.root, &work.work_id, &work.run_id);
    let initial_observation = initial["reference"]["observation"]
        .as_str()
        .unwrap()
        .to_string();

    let spawn = |question: &str| {
        Command::new(wirk_bin())
            .args([
                "world",
                "expand",
                "--json",
                "--question",
                question,
                "--reason",
                "concurrent",
            ])
            .env("WIRK_ESTATE_ROOT", &estate.root)
            .env("WIRK_WORK_ID", &work.work_id)
            .env("WIRK_RUN_ID", &work.run_id)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("world expand spawns")
    };
    let left = spawn("What does notes/quarantine.md say?");
    let right = spawn("What does notes/receipts.md say?");
    let mut outcomes = Vec::new();
    for child in [left, right] {
        let output = child.wait_with_output().expect("world expand finishes");
        outcomes.push((
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    let succeeded = outcomes.iter().filter(|(code, _)| *code == Some(0)).count();
    for (code, err) in &outcomes {
        assert!(
            *code == Some(0) || err.contains("Conflict"),
            "an expansion either lands or says Conflict: {code:?} {err}"
        );
    }
    assert!(
        succeeded >= 1,
        "at least one expansion must land: {outcomes:?}"
    );

    // The invariant, whatever the two processes did: revisions 0..n, each
    // naming its immediate predecessor as parent, every file readable.
    let after = world_show(&estate.root, &work.work_id, &work.run_id);
    let chain = items(&after, "revisions");
    assert_eq!(
        chain.len(),
        1 + succeeded,
        "every expansion that reported success is in the chain, and nothing else is: {after}"
    );
    let mut parent = initial_observation;
    for (index, entry) in chain.iter().enumerate().skip(1) {
        assert_eq!(entry["revision"], index as u64, "{after}");
        let (code, shown) = world_show_args(
            &estate.root,
            &work.work_id,
            &work.run_id,
            &["--revision", &index.to_string()],
        );
        assert_eq!(code, Some(0));
        assert_eq!(shown["orientation"], "delivered", "{shown}");
        assert_eq!(
            shown["projection"]["expansion"]["parent_observation"],
            parent.as_str(),
            "revision {index} must name revision {} as its parent: {shown}",
            index - 1
        );
        parent = entry["observation"].as_str().unwrap().to_string();
    }

    estate.stop();
}

/// Materializing an Actor Run's checkout preserves the delivered
/// projection and the World's hash, and an attempt to *replace* the
/// projection reference while materializing is refused.
///
/// `wirk run`'s materialization re-records `WaypointReserved` with the
/// worktree path filled in. That is the one legitimate re-record of a
/// World, and it is exactly the surface on which a substituted context
/// would arrive.
#[test]
fn materialization_preserves_the_delivered_projection_and_refuses_a_replacement() {
    use crate::wirkd::RecordPayload;
    use wirk_core::{EventKind, RunId, WaypointId, WorkId, World, WorldHash};
    use wirkd::{Reply, Request};

    let mut estate = Estate::new();
    let work = submit_oriented(&estate, "expand-materialize");
    let before = world_show(&estate.root, &work.work_id, &work.run_id);
    let initial_observation = before["reference"]["observation"]
        .as_str()
        .unwrap()
        .to_string();
    let initial_hash = status(&estate.socket, &work.work_id)["world_hash"]
        .as_str()
        .unwrap()
        .to_string();

    // A second, real, readable projection file for this same Run — so
    // the substituted reference below names a file that genuinely exists
    // and genuinely re-hashes. The refusal must not depend on the
    // substitute being unreadable.
    let expanded = expand_ok(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &["--question", "What does notes/quarantine.md say?"],
    );
    let substitute: wirk_core::EvidenceProjectionRef =
        serde_json::from_value(json_reference(&expanded["reference"], &expanded["receipt"]))
            .expect("an EvidenceProjectionRef");

    // Materialization's own first half, exactly as `wirk run` records it.
    let result = status(&estate.socket, &work.work_id);
    let mut world: World = serde_json::from_value(result["world"].clone()).expect("Actor World");
    let World::Actor(actor) = &mut world else {
        panic!("expected an Actor World");
    };
    let waypoint = result["current_waypoint"].as_str().unwrap().to_string();
    let worktree = estate.root.join("worktrees").join(&work.work_id);
    let head = wirk_herdr::git::worktree_add(
        Path::new(&actor.repository),
        &worktree,
        &actor.branch,
        &actor.base_sha,
    )
    .expect("materialize worktree");
    let created = wirkd::client::call(
        &estate.socket,
        &Request::record(RecordPayload {
            work_id: WorkId(work.work_id.clone()),
            run: Some(RunId(work.run_id.clone())),
            kind: EventKind::WorktreeCreated {
                repo: actor.repository.clone(),
                base_sha: head,
            },
        }),
    )
    .expect("record WorktreeCreated");
    assert!(matches!(created, Reply::Ok { .. }), "{created:?}");

    // The substitution, on the one surface where a World is legitimately
    // re-recorded and while the Run is genuinely unmaterialized — so
    // nothing but the projection check can be doing the refusing.
    let mut tampered = world.clone();
    let World::Actor(tampered_actor) = &mut tampered else {
        panic!("Actor World")
    };
    tampered_actor.worktree_path = worktree.clone();
    tampered_actor.evidence = Some(Box::new(substitute));
    for hash in [
        // The honest attempt: a hash that matches the tampered World.
        WorldHash::of(&tampered).0.clone(),
        // And the dishonest one: the Run's own recorded hash.
        initial_hash.clone(),
    ] {
        let reply = wirkd::client::call(
            &estate.socket,
            &Request::record(RecordPayload {
                work_id: WorkId(work.work_id.clone()),
                run: Some(RunId(work.run_id.clone())),
                kind: EventKind::WaypointReserved {
                    waypoint: WaypointId(waypoint.clone()),
                    world_hash: WorldHash(hash.clone()),
                    world: tampered.clone(),
                },
            }),
        )
        .expect("record call");
        assert!(
            matches!(reply, Reply::Err { .. }),
            "replacing the delivered projection must be refused (hash {hash}): {reply:?}"
        );
    }

    // The honest second half still lands, and preserves both.
    let World::Actor(honest) = &mut world else {
        panic!("Actor World")
    };
    honest.worktree_path = worktree.clone();
    let reserved = wirkd::client::call(
        &estate.socket,
        &Request::record(RecordPayload {
            work_id: WorkId(work.work_id.clone()),
            run: Some(RunId(work.run_id.clone())),
            kind: EventKind::WaypointReserved {
                waypoint: WaypointId(waypoint.clone()),
                world_hash: WorldHash::of(&world),
                world: world.clone(),
            },
        }),
    )
    .expect("record WaypointReserved");
    assert!(matches!(reserved, Reply::Ok { .. }), "{reserved:?}");

    let after = world_show_args(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &["--revision", "0"],
    );
    assert_eq!(
        after.1["reference"]["observation"],
        initial_observation.as_str(),
        "materialization must preserve the delivered projection: {:?}",
        after.1
    );
    assert_eq!(after.1["projection"], before["projection"]);
    assert_eq!(
        status(&estate.socket, &work.work_id)["world_hash"]
            .as_str()
            .unwrap(),
        initial_hash,
        "materialization must not move the World hash"
    );
    // And the expansion the Run really did make is still its revision 1.
    let latest = world_show(&estate.root, &work.work_id, &work.run_id);
    assert_eq!(latest["latest_revision"], 1, "{latest}");

    estate.stop();
}

/// The reference an expansion reply carries, reassembled into the exact
/// `EvidenceProjectionRef` shape a World holds — the reply prints the
/// reference without the receipt digest (which is provenance the caller
/// already has beside it), and a substitution attempt needs the whole
/// struct.
fn json_reference(reference: &Value, receipt: &Value) -> Value {
    use sha2::{Digest, Sha256};
    let receipt: wirk_core::ObservationReceipt =
        serde_json::from_value(receipt.clone()).expect("an ObservationReceipt");
    let _ = Sha256::new();
    serde_json::json!({
        "observation": reference["observation"],
        "projection": reference["projection"],
        "revision": reference["revision"],
        "format": reference["format"],
        "receipt": receipt.digest(),
    })
}

/// A narrowed status reader learns that this Run's delivered context has
/// a history it is not being shown, and is never shown a coordinate, an
/// observation id or a projection id.
///
/// A real lineage, not an assertion about one: a container Work declaring
/// a `helper` role, a real child Work attached to it, and the *parent*
/// reading the child's status. The parent is bound to a different source
/// than the child, so `admits_work_checkout` is genuinely false — which
/// is what makes this the narrowed answer rather than the
/// administrative one wearing a different label.
#[test]
fn a_narrowed_status_reader_is_told_the_chain_is_withheld() {
    let mut estate = Estate::new();

    // The parent: a container declaring the `helper` role whose own leaf
    // is the orienting Actor stage under test. It binds `demo`.
    let container = estate.root.join("routes").join("expand-container.json");
    fs::create_dir_all(container.parent().unwrap()).expect("routes dir");
    fs::write(
        &container,
        r#"{"id":"expand-container","waypoints":[
            {"id":"outer","kind":"Container",
             "declared_outputs":[{"name":"out.md","required":true}],
             "required_child_outcomes":[{"role":"helper","required":true}],
             "leaves":[
               {"id":"outer/leaf","kind":"Actor",
                "declared_outputs":[{"name":"out.md","required":true}],
                "intent":"Decide the boundary refusal.",
                "orient":{"question":"Where is claim_boundary_refusal decided in src/server.rs?","sources":["demo"]}}
             ]}
        ]}"#,
    )
    .expect("write container route");
    let parent = submit_kind(
        &estate.root,
        container.to_str().unwrap(),
        &estate.repo,
        &["demo:write", "scratch:write"],
        None,
        Some("actor"),
    )
    .expect("submit parent");

    // The child: a real helper Work, narrowed to a binding that does not
    // include `demo`. This is what makes the read below the narrowed
    // answer rather than the administrative one with a different label.
    let child = submit_kind(
        &estate.root,
        container.to_str().unwrap(),
        &estate.repo,
        &["scratch:write"],
        Some(harness::ParentRef {
            work: &parent.work_id,
            waypoint: "outer",
            run: &parent.run_id,
            role: "helper",
            attempt: None,
        }),
        Some("actor"),
    )
    .expect("submit child");

    let expanded = expand_ok(
        &estate.root,
        &parent.work_id,
        &parent.run_id,
        &["--question", "What does notes/quarantine.md say?"],
    );
    let observation = expanded["reference"]["observation"].as_str().unwrap();
    let projection = expanded["reference"]["projection"].as_str().unwrap();

    // The scoped read, on the same wire the CLI uses: `wirk work status`
    // renders text, and the assertions below are about the exact JSON
    // shape a scoped consultation returns.
    let scoped = {
        use wirk_core::WorkId;
        use wirkd::{Reply, Request, StatusPayload};
        let reply = wirkd::client::call(
            &estate.socket,
            &Request::status(StatusPayload::scoped(
                WorkId(parent.work_id.clone()),
                WorkId(child.work_id.clone()),
            )),
        )
        .expect("scoped status call");
        match reply {
            Reply::Ok { result, .. } => result,
            Reply::Err { error, .. } => {
                panic!("scoped status refused: {} {}", error.code, error.message)
            }
        }
    };
    assert_eq!(scoped["scope"], "requester", "{scoped}");
    let document = serde_json::to_string(&scoped).expect("serialize");
    assert!(
        !document.contains(observation),
        "a narrowed reader must not learn an observation id: {document}"
    );
    assert!(
        !document.contains(projection),
        "a narrowed reader must not learn a projection id: {document}"
    );
    assert!(
        !document.contains("quarantinemarker") && !document.contains("notes/quarantine.md"),
        "and certainly not the delivered content: {document}"
    );
    let marker = serde_json::json!({"withheld": true});
    let run_entry = scoped["runs"]
        .as_array()
        .expect("runs")
        .iter()
        .find(|entry| entry["run"]["id"] == parent.run_id.as_str())
        .unwrap_or_else(|| panic!("the run must be listed: {scoped}"));
    assert_eq!(
        run_entry["orientation"], marker,
        "the chain is marked withheld rather than silently omitted: {run_entry}"
    );
    assert_eq!(
        run_entry["run"]["expansions"], marker,
        "the folded tail is withheld too, not published beside a withheld summary: {run_entry}"
    );
    assert!(
        scoped["disclosure"]["withheld"].as_u64().unwrap_or(0) > 0,
        "the withheld count is real: {scoped}"
    );

    // The administrative reader, which asked for no scope, sees it.
    let admin = status(&estate.socket, &parent.work_id);
    let admin_entry = admin["runs"]
        .as_array()
        .expect("runs")
        .iter()
        .find(|entry| entry["run"]["id"] == parent.run_id.as_str())
        .expect("the run is listed");
    assert_eq!(
        admin_entry["orientation"].as_array().map(Vec::len),
        Some(2),
        "{admin_entry}"
    );
    assert_eq!(admin_entry["orientation"][0]["initial"], true);
    assert_eq!(admin_entry["orientation"][1]["observation"], observation);

    estate.stop();
}

/// `record` cannot mint a projection revision. The verb that assembles,
/// writes and journals one is the only producer.
#[test]
fn record_refuses_to_mint_a_projection_revision() {
    use crate::wirkd::RecordPayload;
    use wirk_core::{EventKind, ObservationId, RunId, WorkId};
    use wirkd::{Reply, Request};

    let mut estate = Estate::new();
    let work = submit_oriented(&estate, "expand-record");
    let before = world_show(&estate.root, &work.work_id, &work.run_id);
    let reference: wirk_core::EvidenceProjectionRef =
        serde_json::from_value(json_reference(&before["reference"], &before["receipt"]))
            .expect("an EvidenceProjectionRef");

    let reply = wirkd::client::call(
        &estate.socket,
        &Request::record(RecordPayload {
            work_id: WorkId(work.work_id.clone()),
            run: Some(RunId(work.run_id.clone())),
            kind: EventKind::ProjectionExpanded {
                waypoint: wirk_core::WaypointId(work.waypoint.clone()),
                parent: ObservationId(
                    before["reference"]["observation"]
                        .as_str()
                        .unwrap()
                        .to_string(),
                ),
                reference: Box::new(reference),
            },
        }),
    )
    .expect("record call");
    match reply {
        Reply::Err { error, .. } => assert_eq!(error.code, "Forbidden", "{error:?}"),
        other => panic!("record must refuse to mint a revision: {other:?}"),
    }
    let after = world_show(&estate.root, &work.work_id, &work.run_id);
    assert_eq!(after["latest_revision"], 0, "{after}");

    estate.stop();
}

// ---- what the delivered document is allowed to claim -----------------------
//
// The four tests below pin the properties `loop-c3-verify/VERDICT.md`
// found unpinned (D1, D2, D3) and the one the first correction of them
// got wrong: that narrowing a request must not turn into a claim about
// sources the request never looked at.

/// A `--reference` expansion narrows *what this request reads*. The
/// document must say that, and must not restate it as a fact about how
/// much of the captured estate is still bound and readable.
///
/// The two are only distinguishable when the vector names more than one
/// membership, which is why this Work is oriented to two real sources.
/// Both are healthy throughout — every number below is measured at an
/// instant when nothing at all is wrong with the estate — so any
/// sentence that reads as "half your sources have gone away" is false by
/// construction, not merely unproven.
#[test]
fn a_narrowed_expansion_states_its_own_query_scope_not_the_estates_health() {
    let mut estate = Estate::new();
    let work = submit_two_source(&estate, "expand-scope");
    let before = world_show(&estate.root, &work.work_id, &work.run_id);
    assert_eq!(
        captured_generations(&before["projection"]).len(),
        2,
        "this property needs a two-membership vector: {before}"
    );

    // Unnarrowed, the whole vector really is what was read, and the
    // sentence is the plain one it always was.
    let unnarrowed = expand_ok(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &["--question", "What is decided about quarantinemarker?"],
    )["projection"]
        .clone();
    let whole = vector_statement(&unnarrowed);
    assert!(
        whole.contains("2 of the 2 membership(s)")
            && whole.contains("were still bound by this Work and still readable"),
        "an unnarrowed expansion read the whole vector and says so: {whole}"
    );

    // Narrowed to one of the two, at the same instant, with both sources
    // still perfectly healthy.
    for (handle, other) in [("demo:knowledge", "extra"), ("extra:knowledge", "demo")] {
        let narrowed = expand_ok(
            &estate.root,
            &work.work_id,
            &work.run_id,
            &["--reference", handle],
        )["projection"]
            .clone();
        let text = vector_statement_for(&narrowed, Some(handle));

        // The whole-vector claim is the thing that must not be made from
        // a narrowed count. It is false here and it is the sentence an
        // actor reads to decide whether its estate is still intact.
        assert!(
            !text.contains("1 of the 2 membership(s)"),
            "a narrowing must never be reported as one of two memberships being bound and \
             readable: {text}"
        );
        // What it may say, it says: how many the vector names, how many
        // this Work still binds, and how many this request selected.
        assert!(
            text.contains(
                "of the 2 membership(s) that vector names, 2 are still bound by this \
                           Work"
            ),
            "the boundness of the captured vector is a fact this expansion has: {text}"
        );
        assert!(
            text.contains(&format!(
                "read only the 1 of those that the delivered handle `{handle}` names"
            )),
            "the narrowing is stated as this request's own scope: {text}"
        );
        assert!(
            text.contains(
                "that is this expansion's own query scope, not a finding about whether \
                           they are readable"
            ),
            "the unselected membership is explicitly not being reported on: {text}"
        );

        // And nothing about the source this request did not select
        // reached the record: no omission, no coverage change.
        assert!(
            !unavailable_coordinates(&narrowed).contains(&other.to_string()),
            "a source this request did not select is not reported unavailable: {narrowed}"
        );
        assert_eq!(
            narrowed["coverage"]["state"], "complete",
            "narrowing does not degrade coverage: {narrowed}"
        );
    }

    estate.stop();
}

/// A request that repeats an earlier request's scope verbatim is still
/// reported as **its own** scope, and the document has to be read by
/// what a sentence names rather than by where it sits.
///
/// Exact-text, same-attribution Assembly statements are collapsed to one
/// copy in the assumptions list being assembled (`prepare_expansion`, the
/// D3 correction), and the copy that survives is the *first*. So the
/// three requests below — narrowed to `demo:knowledge`, unnarrowed,
/// narrowed to `demo:knowledge` again — leave revision 3's own
/// captured-vector sentence sitting at revision 1's position, with
/// revision 2's differently-worded one after it. Position is therefore
/// not an identity here, and reading the list positionally answers with
/// the unnarrowed request's sentence: a reader asking "what did *this*
/// request read" is told it read the whole vector when it read one
/// membership of it.
#[test]
fn a_repeated_earlier_scope_is_still_reported_as_this_requests_own_scope() {
    let mut estate = Estate::new();
    let work = submit_two_source(&estate, "expand-repeat");
    let before = world_show(&estate.root, &work.work_id, &work.run_id);
    assert_eq!(
        captured_generations(&before["projection"]).len(),
        2,
        "this property needs a two-membership vector: {before}"
    );

    let first = expand_ok(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &["--reference", "demo:knowledge"],
    );
    assert_eq!(first["reference"]["revision"], 1, "{first}");
    let second = expand_ok(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &["--question", "What is decided about quarantinemarker?"],
    );
    assert_eq!(second["reference"]["revision"], 2, "{second}");
    let third = expand_ok(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &["--reference", "demo:knowledge"],
    );
    assert_eq!(third["reference"]["revision"], 3, "{third}");
    let third = third["projection"].clone();

    // The shape this stands on, asserted rather than assumed: revision
    // 3 carries exactly one copy of the sentence its own scope produced
    // (deduplicated against revision 1's identical one), and revision
    // 2's unnarrowed sentence is carried forward after it.
    let vectors: Vec<String> = statements(&third, "assumptions")
        .into_iter()
        .map(|(text, _)| text)
        .filter(|text| text.contains("preserved the captured generation vector"))
        .collect();
    assert_eq!(
        vectors
            .iter()
            .filter(|text| text.contains("the delivered handle `demo:knowledge` names"))
            .count(),
        1,
        "one copy of the narrowed sentence, not one per revision that made it: {vectors:?}"
    );
    assert!(
        vectors
            .iter()
            .any(|text| !text.contains("the delivered handle")),
        "revision 2's unnarrowed sentence is carried forward: {vectors:?}"
    );
    assert!(
        vectors
            .iter()
            .position(|text| text.contains("the delivered handle `demo:knowledge` names"))
            < vectors
                .iter()
                .position(|text| !text.contains("the delivered handle")),
        "the surviving narrowed copy really does sit before the unnarrowed one, which is what \
         makes position the wrong question to ask: {vectors:?}"
    );

    // And the answer to "what did this request read" is the narrowed
    // sentence, whatever its position.
    let text = vector_statement_for(&third, Some("demo:knowledge"));
    assert!(
        text.contains("read only the 1 of those that the delivered handle `demo:knowledge` names"),
        "revision 3 narrowed to `demo:knowledge` and that is what its own captured-vector \
         statement must say: {text}"
    );

    estate.stop();
}

/// A statement carried forward into later revisions never tells one of
/// them that it is revision 0.
///
/// The initial assembly's closing assumption says what is *not* in the
/// document and where more can be got, and every expansion clones the
/// parent's assumptions verbatim — that is the whole point of a chain,
/// and it is why the sentence has to be true of every revision that will
/// carry it, not only of the one that wrote it. It was not: it asserted
/// "this is revision 0" as a constant, so revision 1 of this Run's own
/// chain read as revision 0 of it. `loop-c3-native-assessment-verify`
/// observed exactly that on a real native Run ("expanded projections
/// repeat the revision0 assumption", ruling 0135).
///
/// Both revisions are checked, because either half alone is passable by
/// the wrong fix: dropping the sentence from expansions would lose a
/// true disclosure the actor needs, and rewriting it per revision would
/// mean editing a statement the chain is supposed to carry unchanged.
/// What is asserted is that one and the same sentence is present, and
/// honest, in both.
#[test]
fn a_carried_assumption_never_tells_a_later_revision_it_is_revision_zero() {
    let mut estate = Estate::new();
    let work = submit_oriented(&estate, "expand-revision-claim");

    let expansion_note = |projection: &Value| -> String {
        let mut found: Vec<String> = statements(projection, "assumptions")
            .into_iter()
            .map(|(text, _)| text)
            .filter(|text| text.contains("consulted estate findings"))
            .collect();
        assert_eq!(
            found.len(),
            1,
            "one carried copy of the closing disclosure: {projection}"
        );
        found.remove(0)
    };

    let initial = world_show(&estate.root, &work.work_id, &work.run_id);
    assert_eq!(initial["reference"]["revision"], 0, "{initial}");
    let at_zero = expansion_note(&initial["projection"]);
    assert!(
        at_zero.contains("`wirk world expand` adds a later revision to this Run's own chain"),
        "revision 0 tells the actor the verb exists: {at_zero}"
    );

    let expanded = expand_ok(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &["--question", "What does notes/quarantine.md say?"],
    );
    assert_eq!(expanded["reference"]["revision"], 1, "{expanded}");
    let at_one = expansion_note(&expanded["projection"]);

    assert_eq!(
        at_one, at_zero,
        "the chain carries this statement unchanged; it is not re-authored per revision"
    );
    assert!(
        !at_one.contains("revision 0"),
        "revision 1 of this Run's chain must not be told it is revision 0: {at_one}"
    );
    assert!(
        at_one.contains(
            "consulted estate findings and the findings-index health note are not assembled here"
        ) && at_one.contains("is not evidence that the estate holds none"),
        "the disclosure the sentence exists to make survives the correction: {at_one}"
    );

    // Revision 0 is still readable, and still says the same true thing:
    // nothing here was fixed by editing the document it was written in.
    let (code, reread) = world_show_args(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &["--revision", "0"],
    );
    assert_eq!(code, Some(0), "world show --revision 0: {reread}");
    assert_eq!(reread["reference"]["revision"], 0, "{reread}");
    assert_eq!(expansion_note(&reread["projection"]), at_zero);

    estate.stop();
}

/// The decisive one: a source this request did **not** select is not
/// read, so it cannot be reported — not as readable, and not as
/// unavailable either.
///
/// `extra`'s pinned generation is made genuinely unreadable, then the
/// same estate is asked two different questions. The narrowed request
/// never looks at `extra` and says nothing about it; the unnarrowed
/// request does look, does find it gone, and reports it. If the
/// generation lookup runs before the handle narrows the loop, the
/// narrowed request silently acquires the unnarrowed request's omission
/// and its `partial` coverage — evidence about a source it never asked
/// to read.
#[test]
fn an_unselected_source_that_is_unreadable_does_not_reach_a_narrowed_expansion() {
    let mut estate = Estate::new();
    let work = submit_two_source(&estate, "expand-unselected");
    let before = world_show(&estate.root, &work.work_id, &work.run_id);
    let captured = captured_generations(&before["projection"]);
    assert_eq!(captured.len(), 2, "{before}");

    // Which captured generation belongs to `extra`. Taking its manifest
    // away is a real fault, not a fake: `atlas.generation()` reads that
    // file, and nothing else in the estate is touched.
    let (_, extra_generation, _) = atlas(&estate.root, &["status", "--source", "extra"]);
    let extra_id = serde_json::to_string(&extra_generation).expect("serialize");
    let extra_captured = captured
        .iter()
        .find(|generation| extra_id.contains(generation.as_str()))
        .unwrap_or_else(|| panic!("extra's captured generation: {extra_id} / {captured:?}"))
        .clone();
    let manifest = generation_dir(&estate.root, &extra_captured).join("manifest.json");
    let saved = fs::read(&manifest).expect("read the generation manifest");
    fs::remove_file(&manifest).expect("make exactly one captured generation unreadable");

    // Narrowed to `demo`. `extra` is unreadable and irrelevant: this
    // request never selected it.
    let narrowed = expand_ok(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &["--reference", "demo:knowledge"],
    )["projection"]
        .clone();
    assert!(
        !unavailable_coordinates(&narrowed).contains(&"extra".to_string()),
        "a narrowed request must not report a source it did not read: {narrowed}"
    );
    assert_eq!(
        narrowed["coverage"]["state"], "complete",
        "an unselected source cannot degrade this request's coverage: {narrowed}"
    );
    let text = vector_statement_for(&narrowed, Some("demo:knowledge"));
    assert!(
        text.contains("read only the 1 of those") && !text.contains("1 of the 2 membership(s)"),
        "{text}"
    );

    // The same estate, unnarrowed: now `extra` really is being read, it
    // really is gone, and the record says so. The property is that the
    // request decides what is reported, not that unavailability is
    // suppressed.
    let unnarrowed = expand_ok(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &["--question", "What is decided about quarantinemarker?"],
    )["projection"]
        .clone();
    assert!(
        unavailable_coordinates(&unnarrowed).contains(&"extra".to_string()),
        "a request that does read the source reports it unavailable: {unnarrowed}"
    );
    assert_eq!(unnarrowed["coverage"]["state"], "partial", "{unnarrowed}");
    let text = vector_statement(&unnarrowed);
    assert!(
        text.contains("1 of the 2 membership(s)"),
        "unnarrowed, one of the two really was unreadable: {text}"
    );

    fs::write(&manifest, &saved).expect("restore the generation manifest");
    estate.stop();
}

/// An unreadable revision is named by the revision it is. Only revision
/// 0 is the reserved World's; a later one is this Run's own, and the
/// refusal must not blame the reservation for a file it does not name —
/// nor claim to know which component produced the record.
#[test]
fn an_unreadable_later_revision_is_named_by_its_revision_not_the_reserved_world() {
    let mut estate = Estate::new();
    let work = submit_oriented(&estate, "expand-blame");
    let initial = world_show(&estate.root, &work.work_id, &work.run_id);
    let revision_zero = initial["reference"]["observation"]
        .as_str()
        .expect("observation")
        .to_string();
    let expanded = expand_ok(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &["--question", "What does notes/quarantine.md say?"],
    );
    let revision_one = expanded["reference"]["observation"]
        .as_str()
        .expect("observation")
        .to_string();

    let path = |observation: &str| {
        estate
            .root
            .join("works")
            .join(&work.work_id)
            .join("projections")
            .join(format!("{observation}.json"))
    };

    // Revision 1, unreadable.
    let one = path(&revision_one);
    let saved_one = fs::read(&one).expect("read revision 1");
    fs::write(&one, b"{ not a projection").expect("corrupt revision 1");
    let (code, shown) = world_show_args(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &["--revision", "1"],
    );
    assert_eq!(code, Some(0));
    assert_eq!(shown["orientation"], "unavailable", "{shown}");
    let detail = shown["detail"].as_str().expect("detail").to_string();
    assert!(
        detail.contains("revision 1 of this Run's delivered context"),
        "the refusal names the revision that was asked for: {detail}"
    );
    assert!(
        !detail.contains("the reserved World names"),
        "revision 1 is not the reserved World's: {detail}"
    );
    // And it does not narrate an implementation it is not in a position
    // to assert: `record` refuses a `ProjectionExpanded` outright, so
    // "this Run's own actor recorded it" is not a fact this sentence has.
    assert!(
        !detail.contains("actor recorded") && !detail.contains("ProjectionExpanded"),
        "the refusal states the reference, not a producer it did not observe: {detail}"
    );
    assert!(
        shown["reason"]
            .as_str()
            .is_some_and(|reason| reason.starts_with("file-") || reason == "content-mismatch"),
        "the reason code is unchanged by the attribution wording: {shown}"
    );
    fs::write(&one, &saved_one).expect("restore revision 1");

    // Revision 0, unreadable: the reserved World really does name it,
    // and that sentence is unchanged.
    let zero = path(&revision_zero);
    let saved_zero = fs::read(&zero).expect("read revision 0");
    fs::write(&zero, b"{ not a projection").expect("corrupt revision 0");
    let (code, shown) = world_show_args(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &["--revision", "0"],
    );
    assert_eq!(code, Some(0));
    assert_eq!(shown["orientation"], "unavailable", "{shown}");
    let detail = shown["detail"].as_str().expect("detail").to_string();
    assert!(
        detail.starts_with("the reserved World names a projection this estate cannot deliver"),
        "revision 0 keeps the reservation's own sentence: {detail}"
    );
    fs::write(&zero, &saved_zero).expect("restore revision 0");

    // Both files are back to the bytes they were delivered with.
    assert_eq!(fs::read(&zero).expect("revision 0"), saved_zero);
    assert_eq!(fs::read(&one).expect("revision 1"), saved_one);

    estate.stop();
}

/// A long chain stops repeating itself, and drops nothing that is not an
/// exact repeat.
///
/// The fixed Assembly-attributed sentences are the same bytes every
/// revision; carrying one copy is the whole change. Everything else is
/// held: a statement whose wording differs (each revision's own "expands
/// revision N" line), a statement the actor's own unresolved reference
/// produced, and every revision already on disk.
#[test]
fn a_chain_carries_each_assembled_sentence_once_and_drops_nothing_distinct() {
    let mut estate = Estate::new();
    let work = submit_oriented(&estate, "expand-repeat");
    let initial = world_show(&estate.root, &work.work_id, &work.run_id);
    let mut files: Vec<(String, Vec<u8>)> = vec![(
        initial["reference"]["observation"]
            .as_str()
            .unwrap()
            .to_string(),
        Vec::new(),
    )];
    files[0].1 = projection_file(&estate.root, &work.work_id, &files[0].0);

    let handle = "demo:knowledge";
    let mut previous_assembly: Vec<String> = statements(&initial["projection"], "assumptions")
        .into_iter()
        .filter(|(_, origin)| origin == "assembly")
        .map(|(text, _)| text)
        .collect();

    let mut last = Value::Null;
    for revision in 1..=5u64 {
        // Every other revision names a path that resolves nowhere, so
        // the same Intent-attributed unknown is produced twice by two
        // different revisions and must survive as two.
        let expanded = if revision % 2 == 0 {
            expand_ok(
                &estate.root,
                &work.work_id,
                &work.run_id,
                &["--question", "What does notes/no-such-note.md decide?"],
            )
        } else {
            expand_ok(
                &estate.root,
                &work.work_id,
                &work.run_id,
                &["--reference", handle],
            )
        };
        assert_eq!(expanded["revision"], revision, "{expanded}");
        let projection = expanded["projection"].clone();

        let assembly: Vec<String> = statements(&projection, "assumptions")
            .into_iter()
            .filter(|(_, origin)| origin == "assembly")
            .map(|(text, _)| text)
            .collect();
        let mut distinct = assembly.clone();
        distinct.sort();
        distinct.dedup();
        assert_eq!(
            distinct.len(),
            assembly.len(),
            "revision {revision} carries an exact repeat of an assembled sentence: {assembly:?}"
        );
        for text in &previous_assembly {
            assert!(
                assembly.contains(text),
                "revision {revision} dropped a statement its parent carried: {text}"
            );
        }
        // A sentence whose wording differs per revision is kept per
        // revision — this is what tells "carry the identical one once"
        // apart from "collapse the assumption list".
        assert_eq!(
            assembly
                .iter()
                .filter(|text| text.contains("expands revision"))
                .count() as u64,
            revision,
            "every revision's own lineage sentence is distinct and kept: {assembly:?}"
        );
        previous_assembly = assembly;

        files.push((
            expanded["reference"]["observation"]
                .as_str()
                .unwrap()
                .to_string(),
            Vec::new(),
        ));
        let index = files.len() - 1;
        files[index].1 = projection_file(&estate.root, &work.work_id, &files[index].0);
        last = projection;
    }

    // The actor's own unresolved reference is not an assembled sentence
    // and is never collapsed: two revisions named the same missing path,
    // and both said so.
    let unknowns: Vec<(String, String)> = statements(&last, "unknowns");
    assert!(
        unknowns.iter().all(|(_, origin)| origin == "intent"),
        "{unknowns:?}"
    );
    let repeated = unknowns
        .iter()
        .filter(|(text, _)| text.contains("no-such-note.md"))
        .count();
    assert_eq!(
        repeated, 2,
        "each revision that could not resolve the reference says so in its own right: \
         {unknowns:?}"
    );

    // And every revision already written is still exactly the bytes it
    // was delivered with. Deduplication shapes the document being
    // assembled now; it never reaches one already on disk.
    for (observation, bytes) in &files {
        assert_eq!(
            &projection_file(&estate.root, &work.work_id, observation),
            bytes,
            "revision file {observation} moved"
        );
    }

    estate.stop();
}

/// The limit the integration review found in the helper above, executed:
/// **a delivered handle is not a request identity.**
///
/// Two requests narrowing to the same handle produce byte-identical
/// captured-vector sentences and deduplicate to one — that is the case
/// `a_repeated_earlier_scope_is_still_reported_as_this_requests_own_scope`
/// covers. But readability is part of what the sentence reports, so if
/// the selected membership stops being readable between them, the two
/// sentences differ in exactly one number and **both survive**: "1 of
/// them were still readable" from the earlier revision, carried forward
/// as the immutable history it is, and "0 of them were still readable"
/// from this one.
///
/// Both are true, the dedup is working correctly, and the product is
/// right — so a helper that answers "which one is this request's" by
/// handle alone cannot, and must not claim the dedup is broken when it
/// finds two. The request is identified here by the observation it
/// actually made.
#[test]
fn the_same_handle_across_a_readability_change_leaves_two_truthful_statements() {
    let mut estate = Estate::new();
    let work = submit_two_source(&estate, "expand-readability");
    let before = world_show(&estate.root, &work.work_id, &work.run_id);
    let captured = captured_generations(&before["projection"]);
    assert_eq!(captured.len(), 2, "{before}");

    // Revision 1: narrowed to `demo:knowledge`, which is readable.
    let first = expand_ok(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &["--reference", "demo:knowledge"],
    );
    assert_eq!(first["reference"]["revision"], 1, "{first}");
    let first_text = vector_statement_for(&first["projection"], Some("demo:knowledge"));
    assert!(
        first_text.contains("1 of them were still readable"),
        "the selected membership was readable at revision 1: {first_text}"
    );

    // A real fault on the selected membership, by the same mechanism
    // `an_unselected_source_that_is_unreadable_does_not_reach_a_narrowed_expansion`
    // uses: `atlas.generation()`'s own manifest is taken away.
    let (_, demo_generation, _) = atlas(&estate.root, &["status", "--source", "demo"]);
    let demo_id = serde_json::to_string(&demo_generation).expect("serialize");
    let demo_captured = captured
        .iter()
        .find(|generation| demo_id.contains(generation.as_str()))
        .unwrap_or_else(|| panic!("demo's captured generation: {demo_id} / {captured:?}"))
        .clone();
    let manifest = generation_dir(&estate.root, &demo_captured).join("manifest.json");
    let saved = fs::read(&manifest).expect("read the generation manifest");
    fs::remove_file(&manifest).expect("make the selected captured generation unreadable");

    // Revision 2: the same handle, now unreadable.
    let second = expand_ok(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &["--reference", "demo:knowledge"],
    );
    assert_eq!(second["reference"]["revision"], 2, "{second}");
    let second = second["projection"].clone();

    // The shape, asserted rather than assumed: two handle-matching
    // captured-vector statements, differing only in the readability
    // count, both carried as history.
    let handled: Vec<String> = statements(&second, "assumptions")
        .into_iter()
        .map(|(text, _)| text)
        .filter(|text| text.contains("preserved the captured generation vector"))
        .filter(|text| text.contains("the delivered handle `demo:knowledge` names"))
        .collect();
    assert_eq!(
        handled.len(),
        2,
        "the same handle across a readability change is two different observations, and both \
         are legitimate history: {handled:?}"
    );
    assert!(
        handled
            .iter()
            .any(|text| text.contains("1 of them were still readable")),
        "revision 1's observation is unchanged and still carried: {handled:?}"
    );

    // And "what did *this* request read" is answered by this request's
    // own observation, not by a handle two requests share.
    let text = vector_statement_where(
        &second,
        Some("demo:knowledge"),
        &["0 of them were still readable"],
    );
    assert!(
        text.contains("read only the 1 of those that the delivered handle `demo:knowledge` names"),
        "revision 2 read the same handle and found it unreadable, and that is the statement it \
         gets back: {text}"
    );
    // And the under-specified question is still refused rather than
    // answered with whichever of the two came first: the helper never
    // guesses which observation a bare handle meant.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let bare = std::panic::catch_unwind(|| vector_statement_for(&second, Some("demo:knowledge")));
    std::panic::set_hook(previous);
    assert!(
        bare.is_err(),
        "a bare handle cannot name one of two truthful observations"
    );

    fs::write(&manifest, &saved).expect("restore the generation manifest");
    estate.stop();
}
