//! P3 W-C4, the consultation increment: a stage projection that consults
//! recorded learning — this Work's own findings and the estate's
//! genuinely settled EstateLocal publications — and freezes the *actual*
//! scoped health of the findings index beside them.
//!
//! Nothing here is a fake (ruling 0040). Every finding is one the shipped
//! `wirk finding raise` journaled, every settlement is one the shipped
//! `wirk finding settle` minted against a real estate policy and a real
//! declared obligation, every index state is one a real `wirkd` recorded
//! about a real file, and every assertion reads what an actor reads
//! through `wirk world show`.
//!
//! The properties, and each one is a defect if it does not hold:
//!
//! * a Work's own recorded findings reach its **next stage** and a
//!   **later revision** of the stage that raised them, with their real
//!   status, across Runs — a new Run's projection chain restarts at
//!   revision 0 and erases nothing;
//! * consulting a finding never reads through to evidence this
//!   assembly's own admission step did not capture;
//! * a foreign record is consulted only when it is a genuinely settled
//!   EstateLocal publication this requester independently admits —
//!   never merely raised, asserted or on a lineage;
//! * a second estate never appears in any field, even under a colliding
//!   alias;
//! * two Works naming each other's published findings both complete;
//! * a refreshed and republished source leaves the older finding present
//!   with an honest generation relation, never deleted and never
//!   endorsed;
//! * the findings-index note is the estate's *actual* scoped health,
//!   frozen with the projection, re-observed by an expansion, and it
//!   leaks no path, no count and no administrative detail.

#[path = "support/nested_harness.rs"]
mod harness;
#[path = "support/route_fixture.rs"]
mod route_fixture;
#[path = "../src/wirkd/mod.rs"]
mod wirkd;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use harness::{
    KillOnDrop, claim_ok, init_repo, journal_events, materialize_actor, start_wirkd, status,
    stop_wirkd, submit, submit_kind, wirk_bin, write_file,
};
use serde_json::Value;
use wirk_core::{EventKind, WaypointId};

// ---- the estate -----------------------------------------------------------

fn commit_all(repo: &Path) {
    for args in [
        vec!["add", "-A"],
        vec![
            "-c",
            "user.name=consult-test",
            "-c",
            "user.email=consult@example.test",
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

/// Acquire and publish `repo` under `alias`, returning the published
/// generation id.
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

/// The exact coordinate of the unit carrying `marker` in `alias`.
fn locate(estate: &Path, alias: &str, marker: &str) -> String {
    let (ok, search, err) = atlas(estate, &["search", "--source", alias, "--query", marker]);
    assert!(ok, "{err}");
    search["hits"][0]["coordinate"]
        .as_str()
        .unwrap_or_else(|| panic!("no hit for {marker} in {alias}: {search}"))
        .to_string()
}

fn source_repo(root: &Path) -> PathBuf {
    let repo = root.join("source-repo");
    fs::create_dir_all(repo.join("src")).expect("src dir");
    fs::create_dir_all(repo.join("notes")).expect("notes dir");
    init_repo(&repo);
    write_file(
        &repo,
        "src/server.rs",
        "pub fn consult_boundary_refusal(path: &str) -> bool {\n    path.starts_with(\"src/\")\n}\n",
    );
    write_file(
        &repo,
        "notes/policy.md",
        "# Policy\n\ndemomarker: the rule consult_boundary_refusal implements.\n",
    );
    commit_all(&repo);
    repo
}

/// A second source the Work binds and the stage's own `orient.sources`
/// filter deliberately excludes, so "admitted to this Work" and
/// "captured by this assembly" are genuinely different sets.
fn sidecar_repo(root: &Path) -> PathBuf {
    let repo = root.join("sidecar-repo");
    fs::create_dir_all(repo.join("notes")).expect("notes dir");
    init_repo(&repo);
    write_file(
        &repo,
        "notes/sidecar.md",
        "# Sidecar\n\nsidecarcanary: evidence this Work may read and this stage did not capture.\n",
    );
    commit_all(&repo);
    repo
}

fn unadmitted_repo(root: &Path) -> PathBuf {
    let repo = root.join("unadmitted-repo");
    fs::create_dir_all(repo.join("notes")).expect("notes dir");
    init_repo(&repo);
    write_file(
        &repo,
        "notes/elsewhere.md",
        "# Elsewhere\n\nunadmittedcanary: consult_boundary_refusal is discussed in a source no \
         Work below binds.\n",
    );
    commit_all(&repo);
    repo
}

struct Estate {
    dir: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    #[allow(dead_code)]
    sidecar: PathBuf,
    daemon: Option<KillOnDrop>,
    socket: PathBuf,
}

impl Estate {
    fn new() -> Estate {
        let dir = tempfile::tempdir().expect("temp estate");
        let root = dir.path().join("estate");
        fs::create_dir_all(&root).expect("estate dir");
        let repo = source_repo(dir.path());
        let sidecar = sidecar_repo(dir.path());
        let other = unadmitted_repo(dir.path());
        let (daemon, pointer) = start_wirkd(&root);
        publish(&root, "demo", &repo);
        publish(&root, "sidecar", &sidecar);
        publish(&root, "unadmittedsource", &other);
        Estate {
            dir,
            root,
            repo,
            sidecar,
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

/// Two orienting Actor stages over `demo`, so a finding raised while the
/// first is open is consulted by the second — a different Run, a fresh
/// projection chain at revision 0.
fn two_stage_route(estate: &Path, name: &str, sources: &str) -> PathBuf {
    let dir = estate.join("routes");
    fs::create_dir_all(&dir).expect("routes dir");
    let path = dir.join(format!("{name}.json"));
    fs::write(
        &path,
        format!(
            r#"{{"id":{id},"waypoints":[
              {{"id":{first},"kind":"Actor",
                "declared_outputs":[{{"name":"survey.md","required":true}}],
                "intent":"Survey consult_boundary_refusal in src/server.rs.",
                "orient":{{"question":"Where is consult_boundary_refusal decided in src/server.rs?","sources":{sources}}}}},
              {{"id":{second},"kind":"Actor",
                "declared_outputs":[{{"name":"change.md","required":true}}],
                "intent":"Change consult_boundary_refusal in src/server.rs.",
                "orient":{{"question":"How should consult_boundary_refusal in src/server.rs change?","sources":{sources}}}}}
            ]}}"#,
            id = serde_json::to_string(name).unwrap(),
            first = serde_json::to_string(&format!("{name}/survey")).unwrap(),
            second = serde_json::to_string(&format!("{name}/change")).unwrap(),
        ),
    )
    .expect("write route");
    path
}

/// One orienting Actor leaf, for a Work that only ever reads.
fn one_stage_route(estate: &Path, name: &str, sources: &str) -> PathBuf {
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
                "orient":{{"question":"Where is consult_boundary_refusal decided in src/server.rs?","sources":{sources}}}}}
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

fn expand_ok(estate: &Path, work: &str, run: &str, args: &[&str]) -> Value {
    let mut full = vec!["world", "expand", "--json"];
    full.extend_from_slice(args);
    let output = Command::new(wirk_bin())
        .args(&full)
        .env("WIRK_ESTATE_ROOT", estate)
        .env("WIRK_WORK_ID", work)
        .env("WIRK_RUN_ID", run)
        .output()
        .expect("wirk world expand runs");
    assert_eq!(
        output.status.code(),
        Some(0),
        "world expand {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim())
        .expect("world expand emits json")
}

fn raise_cli(estate: &Path, work: &str, run: &str, args: &[&str]) -> (Option<i32>, Value, String) {
    let mut full = vec!["finding", "raise"];
    full.extend_from_slice(args);
    full.push("--json");
    let output = Command::new(wirk_bin())
        .args(&full)
        .env("WIRK_ESTATE_ROOT", estate)
        .env("WIRK_WORK_ID", work)
        .env("WIRK_RUN_ID", run)
        .output()
        .expect("wirk finding raise runs");
    (
        output.status.code(),
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).unwrap_or(Value::Null),
        String::from_utf8_lossy(&output.stderr).trim().to_string(),
    )
}

fn finding_cli(estate: &Path, args: &[&str]) -> (Option<i32>, Value, String) {
    let mut full = vec!["finding"];
    full.extend_from_slice(args);
    full.push("--estate");
    let estate_str = estate.to_str().unwrap();
    full.push(estate_str);
    full.push("--json");
    let output = Command::new(wirk_bin())
        .args(&full)
        .output()
        .expect("wirk finding runs");
    (
        output.status.code(),
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).unwrap_or(Value::Null),
        String::from_utf8_lossy(&output.stderr).trim().to_string(),
    )
}

// ---- reading a delivered projection --------------------------------------

fn projection_of(show: &Value) -> Value {
    assert_eq!(show["orientation"], "delivered", "{show}");
    show["projection"].clone()
}

fn consulted(projection: &Value) -> Vec<Value> {
    projection["consulted"]
        .as_array()
        .unwrap_or_else(|| panic!("a projection carries a consulted list: {projection}"))
        .clone()
}

fn consulted_ids(projection: &Value) -> Vec<String> {
    consulted(projection)
        .iter()
        .map(|item| item["id"].as_str().unwrap_or_default().to_string())
        .collect()
}

fn consulted_by_id(projection: &Value, id: &str) -> Value {
    consulted(projection)
        .into_iter()
        .find(|item| item["id"].as_str() == Some(id))
        .unwrap_or_else(|| panic!("no consulted finding {id}: {projection}"))
}

fn index_note(projection: &Value) -> Value {
    projection["findings_index"].clone()
}

/// Every string anywhere in `value`, flattened — so a leak assertion
/// inspects the *whole* answer and not the one field a fix happened to
/// remember (`disclosure.rs`'s own helper, reused).
fn all_strings(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(text) => out.push(text.clone()),
        Value::Array(items) => items.iter().for_each(|item| all_strings(item, out)),
        Value::Object(map) => map.iter().for_each(|(key, item)| {
            out.push(key.clone());
            all_strings(item, out)
        }),
        other => out.push(other.to_string()),
    }
}

fn assert_discloses_nothing(what: &str, value: &Value, needles: &[&str]) {
    let mut strings = Vec::new();
    all_strings(value, &mut strings);
    for needle in needles {
        for text in &strings {
            assert!(
                !text.contains(needle),
                "{what} disclosed {needle:?} in {text:?}"
            );
        }
    }
}

fn obligation_basis_for(estate: &Path, work_id: &str, waypoint: &str) -> String {
    let events = journal_events(estate, work_id);
    let defs = events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::WorkSubmitted { waypoint_defs, .. } => Some(waypoint_defs.clone()),
            _ => None,
        })
        .expect("the Work's own WorkSubmitted carries its Route definitions");
    let id = WaypointId(waypoint.to_string());
    let def = wirk_core::find_definition(&defs, &id).expect("the named Waypoint is on the Route");
    let world_hash = events.iter().rev().find_map(|event| match &event.kind {
        EventKind::WaypointReserved {
            waypoint: reserved,
            world_hash,
            ..
        } if reserved == &id => Some(world_hash.clone()),
        _ => None,
    });
    wirk_core::obligation_basis(def, world_hash.as_ref())
        .expect("the Waypoint declares an obligation with a derivable basis")
}

fn write_policy_admitting(estate: &Path, id: &str, edition: &str, basis: &str) {
    let dir = estate.join("policy");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("settlement.json"),
        format!(
            r#"{{"version":2,"classes":[{{"class":"deterministic_verified","scope":"estate_local","kinds":["verified_outcome"],"obligations":[{{"id":"{id}","edition":"{edition}","basis":"{basis}"}}]}}]}}"#
        ),
    )
    .unwrap();
}

fn claim_stage(estate: &Estate, work: &str, run: &str, artifact: &str, body: &str) {
    let worktree = materialize_actor(&estate.socket, &estate.root, work, run);
    fs::write(worktree.join(artifact), body).expect("write artifact");
    claim_ok(&estate.root, work, run, &format!("{artifact}={artifact}"));
}

// ---- a real settled publication, produced end to end ---------------------

/// A producer Work whose Container leaf really ran its declared command
/// and was really claimed — everything a settlement needs except the
/// policy, which one estate writes once for however many producers it
/// admits.
struct Producer {
    work: harness::Submitted,
    basis: String,
    claim_event: String,
}

/// A Route that first produces something a settlement policy can admit,
/// and then opens an orienting Actor stage on the same Work.
///
/// The Container half is `a_settled_estate_finding_…`'s producer Route
/// verbatim: a Deterministic leaf that really runs its declared command
/// and carries a `verifies` block, which is what gives the Waypoint a
/// derivable `obligation_basis`. The Actor half is what makes the same
/// Work a *consumer* as well — a Work with no orienting Waypoint has no
/// projection chain at all, and the mutual case needs both halves on
/// both sides.
fn producer_then_actor_route(estate: &Path, name: &str, obligation: &str) {
    route_fixture::write_route(
        estate,
        name,
        &format!(
            r#"{{"id":{id},"waypoints":[
                {{"id":"outer","kind":"Container",
                 "declared_outputs":[{{"name":"a.md","required":true}}],
                 "leaves":[
                   {{"id":"outer/leaf-a","kind":"Deterministic",
                    "command":["sh","-c","echo a > a.md"],
                    "declared_outputs":[{{"name":"a.md","required":true}}],
                    "verifies":{{"id":{obligation},"edition":"1",
                                "proves":"outer/leaf-a ran its declared command and produced a.md",
                                "outputs":["a.md"]}}}}
                 ]}},
                {{"id":"review","kind":"Actor",
                 "declared_outputs":[{{"name":"review.md","required":true}}],
                 "intent":"Review what the estate already recorded.",
                 "orient":{{"question":"Where is consult_boundary_refusal decided in src/server.rs?","sources":["demo"]}}}}
            ]}}"#,
            id = serde_json::to_string(name).unwrap(),
            obligation = serde_json::to_string(obligation).unwrap(),
        ),
    );
}

/// Submit that Route, run its leaf and claim it, leaving the Work on its
/// orienting `review` stage with a real derived basis in hand.
fn produce_claimed_leaf(estate: &Estate, route: &str, name: &str, bindings: &[&str]) -> Producer {
    let repo = estate.dir.path().join(format!("{name}-repo"));
    init_repo(&repo);
    // Distinct content, so this producer's Waypoint really does derive
    // its own `obligation_basis`: the basis is over the Waypoint *and*
    // its World, and two Works over byte-identical checkouts would
    // derive the same one and be admitted by the same policy entry —
    // which would make the mutual case below prove less than its name.
    write_file(&repo, "producer.md", &format!("{name} produced this\n"));
    commit_all(&repo);
    let work = submit(&estate.root, route, &repo, bindings, None)
        .unwrap_or_else(|err| panic!("submit {name}: {err}"));
    write_file(
        &estate.root.join("worktrees").join(&work.work_id),
        "a.md",
        "a\n",
    );
    claim_ok(&estate.root, &work.work_id, &work.run_id, "a.md=a.md");
    let basis = obligation_basis_for(&estate.root, &work.work_id, "outer/leaf-a");
    let claim_event = journal_events(&estate.root, &work.work_id)
        .into_iter()
        .find_map(|event| {
            matches!(&event.kind, EventKind::ClaimRecorded { .. }).then_some(event.id.0)
        })
        .expect("the producer's own ClaimRecorded event")
        .to_string();
    Producer {
        work,
        basis,
        claim_event,
    }
}

/// One estate settlement policy admitting one obligation under however
/// many distinct bases the producers below actually derived. Two Works on
/// the same Route derive *different* bases — the basis is over the
/// Waypoint and its World — so a policy naming only one of them admits
/// only one of them, and the mutual case needs both.
fn write_policy_admitting_bases(estate: &Path, id: &str, edition: &str, bases: &[&str]) {
    let dir = estate.join("policy");
    fs::create_dir_all(&dir).unwrap();
    let obligations = bases
        .iter()
        .map(|basis| format!(r#"{{"id":"{id}","edition":"{edition}","basis":"{basis}"}}"#))
        .collect::<Vec<_>>()
        .join(",");
    fs::write(
        dir.join("settlement.json"),
        format!(
            r#"{{"version":2,"classes":[{{"class":"deterministic_verified","scope":"estate_local","kinds":["verified_outcome"],"obligations":[{obligations}]}}]}}"#
        ),
    )
    .unwrap();
}

/// The Work's currently reserved Run, which is the one a raise is
/// journaled against once the Container half has been claimed.
fn current_run(estate: &Estate, work_id: &str) -> String {
    status(&estate.socket, work_id)["run_id"]
        .as_str()
        .expect("the Work's current run")
        .to_string()
}

/// Raise the producer's own verified outcome against that policy and
/// settle it, so what a later Work consults is a genuinely settled
/// EstateLocal publication and never a merely raised row.
fn raise_and_settle(
    estate: &Estate,
    producer: &Producer,
    canary: &str,
    obligation: &str,
) -> String {
    let run = current_run(estate, &producer.work.work_id);
    let (code, raised, stderr) = raise_cli(
        &estate.root,
        &producer.work.work_id,
        &run,
        &[
            "--kind",
            "verified_outcome",
            "--scope",
            "estate_local",
            "--claim",
            canary,
            "--evidence",
            &format!(
                "work/{}/event/{}",
                producer.work.work_id, producer.claim_event
            ),
            "--obligation",
            &format!("{obligation}@1"),
        ],
    );
    assert_eq!(code, Some(0), "raise: {stderr} {raised}");
    let id = raised["id"].as_str().expect("finding id").to_string();
    let (code, settled, stderr) =
        finding_cli(&estate.root, &["settle", "--finding", &id, "--admin"]);
    assert_eq!(code, Some(0), "settle: {stderr}");
    assert!(
        settled["settled"].is_object(),
        "a real settlement, not an empty result: {settled}"
    );
    id
}

/// The estate's own administrative view of its findings index, so a test
/// that says "the index really recovered" has read the estate saying so
/// rather than inferred it from the projection it is about to assert on.
fn admin_index(estate: &Path) -> Value {
    let (ok, findings, err) = atlas(estate, &["findings", "--admin"]);
    assert!(ok, "atlas findings --admin: {err}");
    findings
}

/// Everything a revision assembled over a genuinely healthy index has to
/// say — asserted in the enum, in the list itself, **and** in the two
/// pieces of prose a fresh actor actually reads.
///
/// The four are asserted together on purpose. V1 was not a wrong enum:
/// it was one frozen document whose `findings_index` said
/// `synchronized`, whose `consulted` list carried a settled publication,
/// and whose `next_action` told the reader in plain English that the
/// index could not be read and that no settled publication was
/// consulted. Only reading all four catches that.
fn assert_complete_consultation(projection: &Value, own: &str, published: &str, canary: &str) {
    assert_eq!(
        index_note(projection)["state"],
        "synchronized",
        "{projection}"
    );
    assert_eq!(index_note(projection)["complete"], true, "{projection}");
    assert_eq!(
        projection["coverage"]["state"], "complete",
        "a revision that read a synchronized index reports its own observation: {projection}"
    );
    assert!(
        projection["coverage"].get("reason").is_none(),
        "a complete coverage carries no reason: {projection}"
    );

    let own_item = consulted_by_id(projection, own);
    assert_eq!(own_item["origin"], "own_work", "{own_item}");
    let published_item = consulted_by_id(projection, published);
    assert_eq!(
        published_item["origin"], "estate_publication",
        "{published_item}"
    );
    assert_eq!(
        published_item["status"]["state"], "settled",
        "{published_item}"
    );
    assert_eq!(published_item["claim_verified"], false, "{published_item}");
    assert!(
        published_item["claim"]
            .as_str()
            .unwrap_or_default()
            .contains(canary),
        "the record delivered is the one that was published: {published_item}"
    );

    let next_action = projection["next_action"].as_str().unwrap_or_default();
    assert!(
        !next_action.contains("no settled publication was consulted"),
        "the document lists a settled publication and then denies it: {next_action}"
    );
    assert!(
        !next_action.contains("could not be read"),
        "the document read the index and then says it could not: {next_action}"
    );
    assert!(
        next_action.contains("every reference the authored text named resolved"),
        "{next_action}"
    );

    let sentence = consulted_sentences(projection);
    assert_eq!(
        sentence.len(),
        1,
        "a revision carries exactly one consulted sentence — its own, not also \
         a parent's about a read this revision did not make: {sentence:?}"
    );
    assert!(
        sentence[0].contains(
            "1 record(s) of this Work's own journal and 1 settled estate \
                              publication(s) were consulted"
        ),
        "the sentence counts what is actually delivered: {}",
        sentence[0]
    );
    assert!(
        sentence[0].contains("was a complete projection of its journals when this was read"),
        "and describes the index it was actually read from: {}",
        sentence[0]
    );
}

/// Every Assembly-attributed sentence in `projection` that is a
/// consulted sentence. More than one means a revision is carrying a
/// statement about a read some *other* revision made.
fn consulted_sentences(projection: &Value) -> Vec<String> {
    projection["assumptions"]
        .as_array()
        .expect("assumptions")
        .iter()
        .filter_map(|statement| statement["text"].as_str())
        .filter(|text| text.contains("record(s) of this Work's own journal and"))
        .map(str::to_string)
        .collect()
}

// ---- 1. This Work's own recorded findings --------------------------------

/// **The decisive own-learning check.** A finding this Work raised while
/// its first stage was open reaches, with its real status:
///
/// * a **later revision** of the very stage that raised it, through
///   `wirk world expand` — the initial assembly is immutable and was
///   observed before the raise, so the revision is where it lands;
/// * the **next stage's** own initial assembly, which is a different
///   Run whose projection chain restarts at revision 0 and erases
///   nothing.
///
/// And the projection never asserts the recorded sentence: the claim is
/// captioned unverified and `claim_verified` is the `false` the daemon
/// already renders (BUILD.md §6, acceptance 17).
#[test]
fn an_own_recorded_finding_reaches_a_later_revision_and_the_next_stage_with_its_real_status() {
    let mut estate = Estate::new();
    let route = two_stage_route(&estate.root, "own", r#"["demo"]"#);
    let work = submit_kind(
        &estate.root,
        route.to_str().unwrap(),
        &estate.repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .expect("submit");

    // The initial assembly happened before anything was raised, and is
    // immutable: it consults nothing, and says so rather than being
    // silently empty.
    let initial = projection_of(&world_show(&estate.root, &work.work_id, &work.run_id));
    assert!(
        consulted(&initial).is_empty(),
        "nothing had been recorded when this was assembled: {initial}"
    );
    assert_eq!(
        index_note(&initial)["state"],
        "synchronized",
        "a real daemon reconciles its index at startup: {initial}"
    );

    let coordinate = locate(&estate.root, "demo", "consult_boundary_refusal");
    let (code, raised, stderr) = raise_cli(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &[
            "--kind",
            "gap",
            "--scope",
            "work_local",
            "--claim",
            "consult_boundary_refusal never states which boundary it refuses",
            "--evidence",
            &coordinate,
        ],
    );
    assert_eq!(code, Some(0), "{stderr} {raised}");
    let finding_id = raised["id"].as_str().expect("finding id").to_string();

    // Revision 0 is unchanged — an immutable delivered context is not
    // edited by anything that happens after it.
    let reread = projection_of(&world_show(&estate.root, &work.work_id, &work.run_id));
    assert_eq!(reread, initial, "revision 0 was rewritten");

    // The later revision re-observes and carries it.
    let expanded = expand_ok(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &["--question", "What did this stage already record?"],
    );
    let revision = projection_of(&expanded);
    assert_eq!(expanded["reference"]["revision"], 1, "{expanded}");
    let item = consulted_by_id(&revision, &finding_id);
    assert_eq!(item["origin"], "own_work", "{item}");
    assert_eq!(item["status"]["state"], "provisional", "{item}");
    assert_eq!(item["claim_verified"], false, "{item}");
    assert!(
        item["claim"]
            .as_str()
            .unwrap_or_default()
            .starts_with("recorded claim: "),
        "the recorded sentence is captioned, never asserted: {item}"
    );
    assert!(
        item["claim"]
            .as_str()
            .unwrap_or_default()
            .ends_with(", unverified"),
        "{item}"
    );

    // The next stage: a different Run, a fresh chain at revision 0, and
    // the earlier Run's finding is still consulted.
    claim_stage(
        &estate,
        &work.work_id,
        &work.run_id,
        "survey.md",
        "# Survey\n\nconsult_boundary_refusal is at src/server.rs:1.\n",
    );
    let advanced = status(&estate.socket, &work.work_id);
    assert_eq!(advanced["current_waypoint"], "own/change");
    let next_run = advanced["run_id"].as_str().expect("run id").to_string();
    let next = world_show(&estate.root, &work.work_id, &next_run);
    assert_eq!(
        next["latest_revision"], 0,
        "a new Run's projection chain starts at 0: {next}"
    );
    let next_projection = projection_of(&next);
    assert!(
        consulted_ids(&next_projection).contains(&finding_id),
        "a new Run does not erase this Work's earlier findings: {next_projection}"
    );

    // And the human rendering says it, not only the JSON.
    let text = world_show_text(&estate.root, &work.work_id, &next_run);
    assert!(
        text.contains("consulted") && text.contains("unverified"),
        "world show must render the consulted set for a reader: {text}"
    );

    estate.stop();
}

// ---- 2. No read-through ---------------------------------------------------

/// A Work's own provisional finding grants no read-through (BUILD.md
/// §4.1). The Work is bound to `sidecar` and really could resolve the
/// coordinate itself; this *stage* is oriented to `demo` alone, so the
/// assembly never captured `sidecar` — and consulting the finding must
/// not deliver what the assembly did not capture. The entry is a count
/// and nothing else: no alias, no path, no coordinate, no generation.
#[test]
fn consulting_an_own_provisional_finding_does_not_admit_its_out_of_scope_evidence() {
    let mut estate = Estate::new();
    let route = two_stage_route(&estate.root, "scoped", r#"["demo"]"#);
    let work = submit_kind(
        &estate.root,
        route.to_str().unwrap(),
        &estate.repo,
        &["demo:write", "sidecar:read"],
        None,
        Some("actor"),
    )
    .expect("submit");

    let outside = locate(&estate.root, "sidecar", "sidecarcanary");
    let (code, raised, stderr) = raise_cli(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &[
            "--kind",
            "gap",
            "--scope",
            "work_local",
            "--claim",
            "an unadmitted note is the basis of this gap",
            "--evidence",
            &outside,
        ],
    );
    assert_eq!(code, Some(0), "{stderr} {raised}");
    let finding_id = raised["id"].as_str().expect("finding id").to_string();

    let expanded = expand_ok(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &["--question", "What did this stage already record?"],
    );
    let projection = projection_of(&expanded);
    let item = consulted_by_id(&projection, &finding_id);
    assert!(
        item["evidence"]
            .as_array()
            .expect("evidence list")
            .is_empty(),
        "an entry outside this assembly's captured admission is not delivered: {item}"
    );
    assert!(
        item["evidence_not_delivered"].as_u64().unwrap_or(0) >= 1,
        "and it is counted rather than silently dropped: {item}"
    );
    assert_discloses_nothing(
        "an out-of-scope evidence entry",
        &projection,
        &["sidecarcanary", "notes/sidecar.md", "sidecar", &outside],
    );

    estate.stop();
}

// ---- 3. The publication route, and only it -------------------------------

/// **The decisive foreign-learning check** (BUILD.md §6, W-C4,
/// acceptance 16 and 19).
///
/// One genuinely settled EstateLocal publication, produced by a Work
/// bound to `embargo` as well as `demo`. Three later, genuinely
/// independent Works assemble a stage projection:
///
/// | requester | bindings | consulted? |
/// |---|---|---|
/// | `peer` | every one of the producer's | yes, through the publication route |
/// | `neighbour` | everything but `embargo` | no — a count, and nothing else |
///
/// And a second finding of the same producer that was **raised and never
/// settled** reaches nobody: raised is not published.
#[test]
fn a_settled_estate_finding_is_consulted_by_a_later_work_through_the_publication_route_only() {
    let mut estate = Estate::new();
    let embargo_repo = estate.dir.path().join("embargo-repo");
    fs::create_dir_all(&embargo_repo).unwrap();
    init_repo(&embargo_repo);
    write_file(
        &embargo_repo,
        "embargoed.md",
        "embargomarker: the embargoed basis\n",
    );
    commit_all(&embargo_repo);
    publish(&estate.root, "embargo", &embargo_repo);

    route_fixture::write_route(
        &estate.root,
        "consult_producer",
        r#"{"id":"consult-producer","waypoints":[
            {"id":"outer","kind":"Container",
             "declared_outputs":[{"name":"a.md","required":true}],
             "required_child_outcomes":[{"role":"helper","required":true}],
             "leaves":[
               {"id":"outer/leaf-a","kind":"Deterministic",
                "command":["sh","-c","echo a > a.md"],
                "declared_outputs":[{"name":"a.md","required":true}],
                "verifies":{"id":"consult-produced","edition":"1",
                            "proves":"outer/leaf-a ran its declared command and produced a.md",
                            "outputs":["a.md"]}}
             ]}
        ]}"#,
    );
    let producer_repo = estate.dir.path().join("producer-repo");
    init_repo(&producer_repo);
    let producer = submit(
        &estate.root,
        "consult_producer",
        &producer_repo,
        &["embargo:write", "demo:read", "sidecar:read"],
        None,
    )
    .expect("submit producer");

    // The leaf really claims, out of the producing checkout — now this
    // Work's own worktree (P3 execution-recovery item 1), not
    // `producer_repo` directly.
    write_file(
        &estate.root.join("worktrees").join(&producer.work_id),
        "a.md",
        "a\n",
    );
    claim_ok(
        &estate.root,
        &producer.work_id,
        &producer.run_id,
        "a.md=a.md",
    );

    let basis = obligation_basis_for(&estate.root, &producer.work_id, "outer/leaf-a");
    write_policy_admitting(&estate.root, "consult-produced", "1", &basis);
    let claim_event = journal_events(&estate.root, &producer.work_id)
        .into_iter()
        .find_map(|event| {
            matches!(&event.kind, EventKind::ClaimRecorded { .. }).then_some(event.id.0)
        })
        .expect("the producer's own ClaimRecorded event");
    let (code, raised, stderr) = raise_cli(
        &estate.root,
        &producer.work_id,
        &producer.run_id,
        &[
            "--kind",
            "verified_outcome",
            "--scope",
            "estate_local",
            "--claim",
            "producedcanary: outer/leaf-a ran its declared command",
            "--evidence",
            &format!("work/{}/event/{claim_event}", producer.work_id),
            "--obligation",
            "consult-produced@1",
        ],
    );
    assert_eq!(code, Some(0), "{stderr} {raised}");
    let settled_id = raised["id"].as_str().expect("finding id").to_string();
    let (code, settled, stderr) = finding_cli(
        &estate.root,
        &["settle", "--finding", &settled_id, "--admin"],
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert!(settled["settled"].is_object(), "{settled}");

    // A second finding of the same producer, raised and never settled.
    let (code, unsettled, stderr) = raise_cli(
        &estate.root,
        &producer.work_id,
        &producer.run_id,
        &[
            "--kind",
            "gap",
            "--scope",
            "estate_local",
            "--claim",
            "unsettledcanary: this one was never settled",
            "--evidence",
            &format!("work/{}/event/{claim_event}", producer.work_id),
        ],
    );
    assert_eq!(code, Some(0), "{stderr} {unsettled}");
    let unsettled_id = unsettled["id"].as_str().expect("finding id").to_string();

    let (ok, _, err) = atlas(&estate.root, &["findings", "--rebuild", "--admin"]);
    assert!(ok, "{err}");

    let consult_route = one_stage_route(&estate.root, "consumer", r#"["demo"]"#);
    let submit_consumer = |name: &str, repos: &[&str]| -> harness::Submitted {
        let repo = estate.dir.path().join(format!("{name}-repo"));
        init_repo(&repo);
        submit_kind(
            &estate.root,
            consult_route.to_str().unwrap(),
            &repo,
            repos,
            None,
            Some("actor"),
        )
        .unwrap_or_else(|err| panic!("submit {name}: {err}"))
    };

    let peer = submit_consumer("peer", &["demo:write", "embargo:write", "sidecar:read"]);
    let neighbour = submit_consumer("neighbour", &["demo:write", "sidecar:read"]);

    let peer_projection = projection_of(&world_show(&estate.root, &peer.work_id, &peer.run_id));
    let item = consulted_by_id(&peer_projection, &settled_id);
    assert_eq!(item["origin"], "estate_publication", "{item}");
    assert_eq!(item["status"]["state"], "settled", "{item}");
    assert_eq!(item["claim_verified"], false, "{item}");
    assert!(
        item["claim"]
            .as_str()
            .unwrap_or_default()
            .contains("producedcanary"),
        "{item}"
    );
    assert!(
        !consulted_ids(&peer_projection).contains(&unsettled_id),
        "a raised, unsettled record is not a publication: {peer_projection}"
    );
    assert_discloses_nothing(
        "a published row consulted by an independent Work",
        &peer_projection,
        &["unsettledcanary"],
    );

    let neighbour_projection = projection_of(&world_show(
        &estate.root,
        &neighbour.work_id,
        &neighbour.run_id,
    ));
    assert!(
        !consulted_ids(&neighbour_projection).contains(&settled_id),
        "a requester that cannot admit the producer's own bindings learns a count only: \
         {neighbour_projection}"
    );
    let inadmissible: u64 = neighbour_projection["omitted"]
        .as_array()
        .expect("omitted")
        .iter()
        .filter(|item| item["kind"] == "inadmissible")
        .filter_map(|item| item["count"].as_u64())
        .sum();
    assert!(
        inadmissible >= 1,
        "the omission is a real count: {neighbour_projection}"
    );
    assert_discloses_nothing(
        "a neighbour without the producer's own bindings",
        &neighbour_projection,
        &[
            "producedcanary",
            "unsettledcanary",
            "embargomarker",
            "embargo",
            &settled_id,
            &unsettled_id,
        ],
    );

    estate.stop();
}

// ---- 4. Estate isolation --------------------------------------------------

/// A second estate, holding a source under the **same alias** with the
/// same shape of content, never appears in any field of the first
/// estate's projections. Estate isolation is total: there is no
/// cross-estate lookup and no `Shared` variant at any layer.
#[test]
fn estate_b_never_appears_in_any_projection_field() {
    let mut estate = Estate::new();

    let b_root = estate.dir.path().join("estate-b");
    fs::create_dir_all(&b_root).expect("estate b dir");
    let b_repo = estate.dir.path().join("b-repo");
    fs::create_dir_all(b_repo.join("src")).expect("src dir");
    init_repo(&b_repo);
    write_file(
        &b_repo,
        "src/server.rs",
        "pub fn consult_boundary_refusal(path: &str) -> bool {\n    // estatebcanary\n    true\n}\n",
    );
    commit_all(&b_repo);
    let (b_daemon, _) = start_wirkd(&b_root);
    publish(&b_root, "demo", &b_repo);

    let route = one_stage_route(&estate.root, "isolated", r#"["demo"]"#);
    let work = submit_kind(
        &estate.root,
        route.to_str().unwrap(),
        &estate.repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .expect("submit");

    let show = world_show(&estate.root, &work.work_id, &work.run_id);
    assert_discloses_nothing(
        "a projection in estate A",
        &show,
        &["estatebcanary", "estate-b", "b-repo"],
    );
    let projection = projection_of(&show);
    assert!(
        consulted(&projection).is_empty(),
        "estate B's records are not this estate's: {projection}"
    );

    stop_wirkd(&b_root, b_daemon);
    estate.stop();
}

// ---- 5. Two Works, each naming the other's publication -------------------

/// **The decisive mutual-consultation check** (BUILD.md §4.6 race 5, and
/// the correction of `loop-c4-consult-verify/VERDICT.md` V2).
///
/// The test this replaces carried this name over an estate with no
/// policy, no finding, no settlement and no publication: with an empty
/// index, `consult_findings` reads zero rows and therefore never opens
/// another Work's journal at all, so the exact lock-ordering hazard the
/// check exists to rule out was never entered. It proved that two
/// unrelated Works can assemble at once.
///
/// This one establishes the real case. Two Works, A and B, each really
/// produce: a Deterministic leaf really runs its declared command, is
/// really claimed, and its own derived `obligation_basis` is really
/// admitted by one estate settlement policy naming both bases. Each then
/// raises its own `verified_outcome` against that policy and really
/// settles it, so the estate holds two genuinely settled EstateLocal
/// publications from two different producers. Both Works bind
/// compatibly, so each independently admits the other's.
///
/// Then A and B expand **at the same instant**, released together by a
/// barrier rather than by elapsed time, for several rounds. Each round
/// asserts what the contract is actually about: A's revision carries B's
/// published finding with B as its producer, B's carries A's, both
/// non-empty, both beside the Work's own record — every one of which
/// required really reading the other Work's journal while the other side
/// was doing the same. The daemon's own `no_journal_guard_held`
/// assertion firing, or either side deadlocking, ends the round rather
/// than being asserted away.
#[test]
fn two_works_each_consulting_the_others_settled_publication_finish_together() {
    const ROUNDS: usize = 3;
    const BOUND: std::time::Duration = std::time::Duration::from_secs(180);

    let mut estate = Estate::new();
    producer_then_actor_route(&estate.root, "mutual_a", "mutual-produced");
    producer_then_actor_route(&estate.root, "mutual_b", "mutual-produced");

    // Compatible current bindings: each side admits the other's, so an
    // omission below would be a defect and not a disclosure decision.
    let a = produce_claimed_leaf(&estate, "mutual_a", "mutuala", &["demo:write"]);
    let b = produce_claimed_leaf(&estate, "mutual_b", "mutualb", &["demo:write"]);

    // One policy, both real derived bases. A basis is over the Waypoint
    // *and* its World, so these genuinely differ.
    assert_ne!(a.basis, b.basis, "two Works derive two bases");
    write_policy_admitting_bases(
        &estate.root,
        "mutual-produced",
        "1",
        &[a.basis.as_str(), b.basis.as_str()],
    );

    let a_finding = raise_and_settle(
        &estate,
        &a,
        "acanary: A's leaf ran its declared command",
        "mutual-produced",
    );
    let b_finding = raise_and_settle(
        &estate,
        &b,
        "bcanary: B's leaf ran its declared command",
        "mutual-produced",
    );
    assert_ne!(a_finding, b_finding);

    let (ok, _, err) = atlas(&estate.root, &["findings", "--rebuild", "--admin"]);
    assert!(ok, "{err}");
    let admin = admin_index(&estate.root);
    let settled: Vec<&str> = admin["rows"]
        .as_array()
        .expect("rows")
        .iter()
        .filter(|row| row["kind"] == "settled")
        .filter_map(|row| row["finding"]["id"].as_str())
        .collect();
    assert!(
        settled.contains(&a_finding.as_str()) && settled.contains(&b_finding.as_str()),
        "the estate really holds both settled publications: {admin}"
    );

    let a_run = current_run(&estate, &a.work.work_id);
    let b_run = current_run(&estate, &b.work.work_id);

    for round in 1..=ROUNDS {
        // Causal coordination, not elapsed time: neither side is
        // released until both are at the barrier, so the two assemblies
        // really do overlap.
        let gate = std::sync::Arc::new(std::sync::Barrier::new(2));
        let (done, finished) = std::sync::mpsc::channel::<()>();
        let mut threads = Vec::new();
        for (root, work, run) in [
            (estate.root.clone(), a.work.work_id.clone(), a_run.clone()),
            (estate.root.clone(), b.work.work_id.clone(), b_run.clone()),
        ] {
            let gate = std::sync::Arc::clone(&gate);
            let done = done.clone();
            threads.push(std::thread::spawn(move || {
                gate.wait();
                let expanded = expand_ok(
                    &root,
                    &work,
                    &run,
                    &["--question", "What has the other side already settled?"],
                );
                let _ = done.send(());
                expanded
            }));
        }
        drop(done);
        // A test-only termination bound: a deadlock must fail this test
        // rather than hang the suite until something else kills it.
        for side in 0..2 {
            finished.recv_timeout(BOUND).unwrap_or_else(|_| {
                panic!(
                    "round {round}: side {side} did not finish within {BOUND:?} — the two \
                        assemblies did not both complete"
                )
            });
        }
        let mut assembled = threads.into_iter().map(|handle| {
            projection_of(&handle.join().expect("an assembling thread must not panic"))
        });
        let a_projection = assembled.next().expect("A's revision");
        let b_projection = assembled.next().expect("B's revision");

        // A really received B's published record, and B really received
        // A's: a non-empty foreign consulted set on both sides, each
        // attributed to the producer that actually published it.
        for (label, projection, own, own_canary, foreign, foreign_canary) in [
            (
                "A",
                &a_projection,
                &a_finding,
                "acanary",
                &b_finding,
                "bcanary",
            ),
            (
                "B",
                &b_projection,
                &b_finding,
                "bcanary",
                &a_finding,
                "acanary",
            ),
        ] {
            let item = consulted_by_id(projection, foreign);
            assert_eq!(
                item["origin"], "estate_publication",
                "round {round}: {label} consulted the other side's publication as one: {item}"
            );
            assert_eq!(item["status"]["state"], "settled", "{item}");
            assert_eq!(
                item["claim_verified"], false,
                "a consulted record is never asserted true: {item}"
            );
            assert!(
                item["claim"]
                    .as_str()
                    .unwrap_or_default()
                    .contains(foreign_canary),
                "round {round}: {label} received the other side's own recorded sentence: {item}"
            );
            assert!(
                item["claim"]
                    .as_str()
                    .unwrap_or_default()
                    .ends_with(", unverified"),
                "the unverified caption survives the mutual case: {item}"
            );
            // And its own record is still its own, so "producer" is a
            // real distinction here and not a label on one list.
            let mine = consulted_by_id(projection, own);
            assert_eq!(
                mine["origin"], "own_work",
                "round {round}: {label}'s own record stays its own: {mine}"
            );
            assert!(
                mine["claim"]
                    .as_str()
                    .unwrap_or_default()
                    .contains(own_canary),
                "{mine}"
            );
            assert!(
                projection["findings_index"].is_object(),
                "round {round}: {label} froze a real index note: {projection}"
            );
        }
    }

    // The daemon that served both sides is still the daemon that served
    // both sides: a `no_journal_guard_held` assertion would have taken
    // it down, and a clean stop is only possible if it did not fire.
    estate.stop();
}

// ---- 6. Generation refresh -----------------------------------------------

/// A source refreshed and republished under the assembler does not
/// delete an older finding and does not endorse it. The record stays
/// present, its recorded generation stays what it was, the captured
/// current generation is stated beside it, and the relation says only
/// what it knows (BUILD.md §6, acceptance 18).
#[test]
fn after_refresh_and_publish_the_same_finding_reads_recorded_superseded_and_stays_present() {
    let mut estate = Estate::new();
    let route = two_stage_route(&estate.root, "refresh", r#"["demo"]"#);
    let work = submit_kind(
        &estate.root,
        route.to_str().unwrap(),
        &estate.repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .expect("submit");

    let coordinate = locate(&estate.root, "demo", "consult_boundary_refusal");
    let (code, raised, stderr) = raise_cli(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &[
            "--kind",
            "gap",
            "--scope",
            "work_local",
            "--claim",
            "consult_boundary_refusal never states which boundary it refuses",
            "--evidence",
            &coordinate,
        ],
    );
    assert_eq!(code, Some(0), "{stderr} {raised}");
    let finding_id = raised["id"].as_str().expect("finding id").to_string();

    let before = projection_of(&expand_ok(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &["--question", "What has this stage recorded?"],
    ));
    let at_recording = consulted_by_id(&before, &finding_id);
    assert_eq!(
        at_recording["generation_relation"], "recorded_still_published",
        "nothing has moved yet: {at_recording}"
    );

    // A real refresh and a real publish of the same source.
    write_file(
        &estate.repo,
        "notes/policy.md",
        "# Policy\n\ndemomarker: the rule consult_boundary_refusal implements, restated.\n",
    );
    commit_all(&estate.repo);
    let republished = publish(&estate.root, "demo", &estate.repo);

    claim_stage(
        &estate,
        &work.work_id,
        &work.run_id,
        "survey.md",
        "# Survey\n\nconsult_boundary_refusal is at src/server.rs:1.\n",
    );
    let advanced = status(&estate.socket, &work.work_id);
    let next_run = advanced["run_id"].as_str().expect("run id").to_string();
    let after = projection_of(&world_show(&estate.root, &work.work_id, &next_run));
    let item = consulted_by_id(&after, &finding_id);
    assert_eq!(
        item["generation_relation"], "recorded_superseded",
        "the pair is recorded and not collapsed: {item}"
    );
    assert!(
        item["current_generations"]
            .as_array()
            .expect("current generations")
            .iter()
            .any(|pair| pair[1] == republished.as_str()),
        "the captured current generation is stated beside the recorded one: {item}"
    );
    assert_eq!(
        item["claim_verified"], false,
        "a changed generation is not a verdict on the claim: {item}"
    );

    estate.stop();
}

// ---- 7. The index note is the estate's actual scoped health --------------

/// The findings-index note is the **actual** health this daemon
/// recorded, frozen into the projection at assembly and never
/// retro-corrected. An index that cannot be read degrades the consulted
/// coverage, keeps this Work's own admissible findings, and leaks no
/// path and no administrative detail (BUILD.md §9, acceptance 20,
/// rulings 0135/0137).
#[test]
fn an_unreadable_findings_index_degrades_the_consulted_coverage_and_leaks_no_path() {
    let mut estate = Estate::new();
    let route = two_stage_route(&estate.root, "health", r#"["demo"]"#);
    let work = submit_kind(
        &estate.root,
        route.to_str().unwrap(),
        &estate.repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .expect("submit");

    let coordinate = locate(&estate.root, "demo", "consult_boundary_refusal");
    let (code, raised, stderr) = raise_cli(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &[
            "--kind",
            "gap",
            "--scope",
            "work_local",
            "--claim",
            "the index state must not decide what this Work already recorded",
            "--evidence",
            &coordinate,
        ],
    );
    assert_eq!(code, Some(0), "{stderr} {raised}");
    let finding_id = raised["id"].as_str().expect("finding id").to_string();

    // Healthy first: a real daemon that reconciled at startup.
    let healthy = projection_of(&expand_ok(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &["--question", "How healthy is the backing index?"],
    ));
    assert_eq!(index_note(&healthy)["state"], "synchronized", "{healthy}");
    assert_eq!(index_note(&healthy)["complete"], true, "{healthy}");
    let frozen_healthy = healthy.clone();

    // Now make the index genuinely unreadable, the way the index suite
    // already does: bytes that are not rows.
    let index_path = estate
        .root
        .join("atlas")
        .join(wirk_atlas::FINDINGS_INDEX_FILE);
    fs::write(&index_path, b"this is not a findings row\n").expect("corrupt the index");

    let degraded = projection_of(&expand_ok(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &["--question", "How healthy is the backing index now?"],
    ));
    assert_eq!(
        index_note(&degraded)["state"],
        "unreadable",
        "an index that cannot be read says so: {degraded}"
    );
    assert_eq!(index_note(&degraded)["complete"], false, "{degraded}");
    assert_eq!(
        degraded["coverage"]["state"], "degraded",
        "the consulted set is degraded, not silently empty: {degraded}"
    );
    assert!(
        consulted_ids(&degraded).contains(&finding_id),
        "this Work's own admissible findings survive an unreadable index: {degraded}"
    );
    assert_discloses_nothing(
        "an unreadable findings index",
        &degraded,
        &[
            wirk_atlas::FINDINGS_INDEX_FILE,
            estate.root.to_str().unwrap(),
            "atlas/",
            "No such file",
            "expected value",
        ],
    );

    // Frozen: the earlier revision still says what it said, and reading
    // it changed nothing.
    let (code, reread) = world_show_args(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &["--revision", "1"],
    );
    assert_eq!(code, Some(0), "{reread}");
    assert_eq!(
        projection_of(&reread),
        frozen_healthy,
        "a historical revision is not retro-corrected by a later observation"
    );

    // A restart re-reads the same estate and the historical record is
    // still exactly what it was.
    estate.restart();
    let (code, after_restart) = world_show_args(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &["--revision", "1"],
    );
    assert_eq!(code, Some(0), "{after_restart}");
    assert_eq!(projection_of(&after_restart), frozen_healthy);

    estate.stop();
}

/// A backing index file that is simply **gone**, beside a health record
/// formed over one that was there, is neither complete nor unreadable —
/// it is the estate saying it cannot attest completeness (ruling 0137).
/// The consulted coverage is `partial`, never `complete`, and the scoped
/// projection carries no count, no detail and no path.
#[test]
fn a_missing_backing_index_reads_behind_and_never_certifies_the_consulted_set() {
    let mut estate = Estate::new();
    let route = two_stage_route(&estate.root, "missing", r#"["demo"]"#);
    let work = submit_kind(
        &estate.root,
        route.to_str().unwrap(),
        &estate.repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .expect("submit");

    // Something must really have been published, so the recorded health
    // is formed over an index file that was genuinely there.
    let coordinate = locate(&estate.root, "demo", "consult_boundary_refusal");
    let (code, raised, stderr) = raise_cli(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &[
            "--kind",
            "gap",
            "--scope",
            "estate_local",
            "--claim",
            "the missing index must not answer complete",
            "--evidence",
            &coordinate,
        ],
    );
    assert_eq!(code, Some(0), "{stderr} {raised}");
    let (ok, _, err) = atlas(&estate.root, &["findings", "--rebuild", "--admin"]);
    assert!(ok, "{err}");
    let index_path = estate
        .root
        .join("atlas")
        .join(wirk_atlas::FINDINGS_INDEX_FILE);
    assert!(index_path.exists(), "the estate really wrote an index");
    fs::remove_file(&index_path).expect("take the index away");

    let projection = projection_of(&expand_ok(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &["--question", "What does the estate still know?"],
    ));
    assert_eq!(
        index_note(&projection)["state"],
        "behind",
        "a file that was there and is gone is not an empty estate: {projection}"
    );
    assert_eq!(index_note(&projection)["complete"], false, "{projection}");
    assert_eq!(projection["coverage"]["state"], "partial", "{projection}");
    assert_eq!(
        projection["coverage"]["reason"], "index_cannot_attest_completeness",
        "{projection}"
    );
    // No administrative count, detail or path reaches a scoped reader.
    let note = index_note(&projection);
    for key in ["pending_rows", "detail", "preserved_index_copies", "since"] {
        assert!(
            note.get(key).is_none(),
            "the scoped note carries no {key}: {note}"
        );
    }
    assert_discloses_nothing(
        "a missing findings index",
        &projection,
        &["findings.jsonl", estate.root.to_str().unwrap()],
    );

    estate.stop();
}

// ---- 8. Typed disagreement, and superseded history ----------------------

/// A `contradicts` entry becomes a contradiction on the consulted record
/// **only** when it names a coordinate this projection actually
/// delivered, and it decides nothing (BUILD.md §6).
///
/// Two entries on one record: one naming a bound coordinate, one naming a
/// coordinate this stage was never oriented to. The first is carried,
/// attributed to the finding it came from; the second is not, and nothing
/// about it — not the alias, not the path, not the coordinate — appears
/// anywhere. No prose is read or compared in either direction, and
/// neither side is endorsed or invalidated.
///
/// And the second half: a Work that replaces its own provisional record
/// with a traceable newer one keeps the older one, delivered, with an
/// honest `superseded` status. History is delivered, not erased.
#[test]
fn a_typed_contradiction_reaches_the_projection_only_where_it_names_delivered_evidence() {
    let mut estate = Estate::new();
    let route = two_stage_route(&estate.root, "typed", r#"["demo"]"#);
    let work = submit_kind(
        &estate.root,
        route.to_str().unwrap(),
        &estate.repo,
        &["demo:write", "sidecar:read"],
        None,
        Some("actor"),
    )
    .expect("submit");

    // A coordinate this projection really delivered, taken out of the
    // projection itself rather than constructed.
    let initial = projection_of(&world_show(&estate.root, &work.work_id, &work.run_id));
    let delivered = initial["bound"]
        .as_array()
        .expect("bound")
        .iter()
        .find_map(|item| item["coordinate"].as_str())
        .expect("the initial assembly bound something")
        .to_string();
    let outside = locate(&estate.root, "sidecar", "sidecarcanary");

    let (code, raised, stderr) = raise_cli(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &[
            "--kind",
            "contradicted_assumption",
            "--scope",
            "work_local",
            "--claim",
            "the delivered rule does not hold for the case this stage found",
            "--evidence",
            &delivered,
            "--contradicts",
            &delivered,
            "--contradicts",
            &outside,
        ],
    );
    assert_eq!(code, Some(0), "{stderr} {raised}");
    let first = raised["id"].as_str().expect("finding id").to_string();

    let expanded = projection_of(&expand_ok(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &["--question", "What has this stage already recorded?"],
    ));
    let item = consulted_by_id(&expanded, &first);
    let contradictions = item["contradictions"].as_array().expect("contradictions");
    assert_eq!(
        contradictions.len(),
        1,
        "only the entry naming delivered evidence contributes: {item}"
    );
    assert_eq!(
        contradictions[0]["coordinate"],
        delivered.as_str(),
        "{item}"
    );
    assert!(
        contradictions[0]["text"]
            .as_str()
            .unwrap_or_default()
            .contains("no prose was read or compared here"),
        "a contradiction decides nothing: {item}"
    );
    assert_discloses_nothing(
        "a contradiction naming evidence this stage never captured",
        &expanded,
        &["sidecarcanary", "notes/sidecar.md", &outside],
    );

    // The superseding half: a later record of this same Work naming the
    // first one leaves the first delivered, and says so.
    let (code, later, stderr) = raise_cli(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &[
            "--kind",
            "contradicted_assumption",
            "--scope",
            "work_local",
            "--claim",
            "the delivered rule does not hold, restated with the case named",
            "--evidence",
            &delivered,
            "--supersedes",
            &first,
        ],
    );
    assert_eq!(code, Some(0), "{stderr} {later}");
    let second = later["id"].as_str().expect("finding id").to_string();

    let after = projection_of(&expand_ok(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &["--question", "And now?"],
    ));
    let ids = consulted_ids(&after);
    assert!(
        ids.contains(&first) && ids.contains(&second),
        "the superseded record is history, not an error, and stays delivered: {after}"
    );
    let older = consulted_by_id(&after, &first);
    assert_eq!(older["status"]["state"], "superseded", "{older}");
    assert_eq!(older["status"]["by"], second.as_str(), "{older}");
    assert_eq!(
        consulted_by_id(&after, &second)["status"]["state"],
        "provisional",
        "{after}"
    );

    estate.stop();
}

/// Coverage never *improves* by expanding — and the rule that enforces
/// it must not also stop it getting worse.
///
/// Found by running the real binary on a real estate rather than by
/// reading the code: an index taken away makes revision 2 `partial`
/// (this estate cannot attest completeness), and an index that then
/// cannot be read at all must make revision 3 `degraded` (no publication
/// was consulted at all). The carry-forward rule kept the *parent's*
/// coverage whenever the parent was anything but `complete`, so the
/// worse local fact was silently discarded and a revision assembled with
/// an unreadable index reported the milder state of the revision before
/// it.
#[test]
fn an_expansion_whose_own_observation_is_worse_reports_its_own_state() {
    let mut estate = Estate::new();
    let route = two_stage_route(&estate.root, "worsening", r#"["demo"]"#);
    let work = submit_kind(
        &estate.root,
        route.to_str().unwrap(),
        &estate.repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .expect("submit");

    let coordinate = locate(&estate.root, "demo", "consult_boundary_refusal");
    let (code, raised, stderr) = raise_cli(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &[
            "--kind",
            "gap",
            "--scope",
            "estate_local",
            "--claim",
            "a worsening index must not be reported as the milder earlier one",
            "--evidence",
            &coordinate,
        ],
    );
    assert_eq!(code, Some(0), "{stderr} {raised}");
    let (ok, _, err) = atlas(&estate.root, &["findings", "--rebuild", "--admin"]);
    assert!(ok, "{err}");
    let index_path = estate
        .root
        .join("atlas")
        .join(wirk_atlas::FINDINGS_INDEX_FILE);

    // Revision 1: the file is gone, so this estate cannot attest that
    // its consulted set is complete.
    fs::remove_file(&index_path).expect("take the index away");
    let missing = projection_of(&expand_ok(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &["--question", "What does the estate still know?"],
    ));
    assert_eq!(index_note(&missing)["state"], "behind", "{missing}");
    assert_eq!(missing["coverage"]["state"], "partial", "{missing}");

    // Revision 2: the file is back and is not rows, which is strictly
    // worse — and this revision must say so rather than inheriting the
    // milder state of the one it expands.
    fs::write(&index_path, b"this is not a findings row\n").expect("corrupt the index");
    let unreadable = projection_of(&expand_ok(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &["--question", "And now?"],
    ));
    assert_eq!(
        index_note(&unreadable)["state"],
        "unreadable",
        "{unreadable}"
    );
    assert_eq!(
        unreadable["coverage"]["state"], "degraded",
        "a worse local observation is this revision's own fact: {unreadable}"
    );
    assert_eq!(
        unreadable["coverage"]["reason"], "findings_index_unreadable",
        "{unreadable}"
    );

    // And the milder revision it expands is unchanged: nothing was
    // fixed by editing a document already delivered.
    let (code, reread) = world_show_args(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &["--revision", "1"],
    );
    assert_eq!(code, Some(0), "{reread}");
    assert_eq!(projection_of(&reread), missing);

    estate.stop();
}

/// The mirror of the test above, and the defect it did not catch
/// (`loop-c4-consult-verify/VERDICT.md` V1): coverage must be free to
/// **recover**, because the consulted set it describes is re-observed
/// and replaced wholesale by every revision.
///
/// The carry-forward rule was applied in one direction only. A revision
/// that could not read the index once made every later revision of that
/// Run — for the whole life of the chain — say, in the prose a fresh
/// actor reads, that the index could not be read and that *no settled
/// publication was consulted*, while that same frozen document listed
/// one, called its own index `synchronized`, and reported `complete:
/// true`. A delivered document contradicting itself about its own
/// contents is the one thing this phase exists to prevent.
///
/// So: a real chain driven Complete → Behind/partial →
/// Unreadable/degraded → a genuinely reconciled index again, with a real
/// settled EstateLocal publication and this Work's own record both in
/// the estate throughout. The recovered revision must restore both
/// records, say `complete`, and say so in `next_action` and in the
/// consulted sentence — and every historical revision must still be
/// byte-identical across a real restart.
#[test]
fn an_expansion_whose_index_recovers_reports_its_own_recovered_state() {
    const CANARY: &str = "publishedrecoverycanary";
    let mut estate = Estate::new();
    producer_then_actor_route(&estate.root, "recovery_producer", "recovery-produced");
    let producer = produce_claimed_leaf(
        &estate,
        "recovery_producer",
        "recoveryproducer",
        &["demo:write"],
    );
    write_policy_admitting_bases(
        &estate.root,
        "recovery-produced",
        "1",
        &[producer.basis.as_str()],
    );
    let published = raise_and_settle(
        &estate,
        &producer,
        "publishedrecoverycanary: outer/leaf-a ran its declared command",
        "recovery-produced",
    );

    // A genuinely independent consumer Work, bound so that it really
    // admits the producer's own bindings.
    let route = two_stage_route(&estate.root, "recovery", r#"["demo"]"#);
    let work = submit_kind(
        &estate.root,
        route.to_str().unwrap(),
        &estate.repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .expect("submit consumer");

    let coordinate = locate(&estate.root, "demo", "consult_boundary_refusal");
    let (code, raised, stderr) = raise_cli(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &[
            "--kind",
            "gap",
            "--scope",
            "work_local",
            "--claim",
            "a recovered index must not be reported as the unreadable one before it",
            "--evidence",
            &coordinate,
        ],
    );
    assert_eq!(code, Some(0), "{stderr} {raised}");
    let own = raised["id"].as_str().expect("finding id").to_string();

    let (ok, _, err) = atlas(&estate.root, &["findings", "--rebuild", "--admin"]);
    assert!(ok, "{err}");
    let index_path = estate
        .root
        .join("atlas")
        .join(wirk_atlas::FINDINGS_INDEX_FILE);

    // The full set, before anything is broken: this Work's own record
    // and the estate's settled publication, over a synchronized index.
    let healthy = projection_of(&expand_ok(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &["--question", "What does the estate already know?"],
    ));
    assert_complete_consultation(&healthy, &own, &published, CANARY);

    // Down: the file is gone, so this estate cannot attest completeness.
    fs::remove_file(&index_path).expect("take the index away");
    let behind = projection_of(&expand_ok(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &["--question", "And with the index gone?"],
    ));
    assert_eq!(index_note(&behind)["state"], "behind", "{behind}");
    assert_eq!(behind["coverage"]["state"], "partial", "{behind}");

    // Further down: bytes that are not rows.
    fs::write(&index_path, b"this is not a findings row\n").expect("corrupt the index");
    let degraded = projection_of(&expand_ok(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &["--question", "And with the index unreadable?"],
    ));
    assert_eq!(index_note(&degraded)["state"], "unreadable", "{degraded}");
    assert_eq!(degraded["coverage"]["state"], "degraded", "{degraded}");
    assert_eq!(
        degraded["coverage"]["reason"], "findings_index_unreadable",
        "{degraded}"
    );
    assert!(
        !consulted_ids(&degraded).contains(&published),
        "an unreadable index really does lose the published half: {degraded}"
    );

    // Back up, for real: the corrupt file is taken away and the estate
    // reconciles a genuine index out of its own journals. The estate's
    // own administrative view says so before the projection is asserted
    // on, so this is a recovery and not a hopeful re-read.
    fs::remove_file(&index_path).expect("remove the corrupt file");
    let (ok, _, err) = atlas(&estate.root, &["findings", "--rebuild", "--admin"]);
    assert!(ok, "rebuild: {err}");
    let admin = admin_index(&estate.root);
    assert_eq!(
        admin["index"]["complete"], true,
        "the estate itself says its index is complete again: {admin}"
    );
    let recovered = projection_of(&expand_ok(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &[
            "--question",
            "And once the index is a real projection again?",
        ],
    ));
    assert_complete_consultation(&recovered, &own, &published, CANARY);

    // Once more, healthy: a recovered chain keeps recovering.
    let again = projection_of(&expand_ok(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &["--question", "Once more, with the index healthy?"],
    ));
    assert_complete_consultation(&again, &own, &published, CANARY);

    // Down and up a second time, so the direction is a property of the
    // assembler and not of the one transition above.
    fs::write(&index_path, b"still not a findings row\n").expect("corrupt the index again");
    let degraded_again = projection_of(&expand_ok(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &["--question", "Unreadable a second time?"],
    ));
    assert_eq!(
        degraded_again["coverage"]["state"], "degraded",
        "{degraded_again}"
    );
    fs::remove_file(&index_path).expect("remove the corrupt file again");
    let (ok, _, err) = atlas(&estate.root, &["findings", "--rebuild", "--admin"]);
    assert!(ok, "rebuild: {err}");
    let recovered_again = projection_of(&expand_ok(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &["--question", "Healthy a second time?"],
    ));
    assert_complete_consultation(&recovered_again, &own, &published, CANARY);

    // Nothing leaked on the way through either direction.
    for projection in [&degraded, &recovered, &degraded_again, &recovered_again] {
        assert_discloses_nothing(
            "a chain whose index went unreadable and recovered",
            projection,
            &[
                wirk_atlas::FINDINGS_INDEX_FILE,
                estate.root.to_str().unwrap(),
                "atlas/",
                "No such file",
            ],
        );
    }

    // Every revision this chain ever delivered, frozen: reading changed
    // nothing, recovering retro-corrected nothing, and a real restart
    // reads back exactly the same bytes. The live envelope's
    // `latest_revision` and its revision list are the two things that
    // must move when a revision is added, so the frozen *documents* are
    // what is compared.
    let latest = world_show(&estate.root, &work.work_id, &work.run_id)["latest_revision"]
        .as_u64()
        .expect("a latest revision");
    assert_eq!(latest, 7, "seven expansions on one chain");
    let before: Vec<Value> = (0..=latest)
        .map(|revision| {
            let (code, show) = world_show_args(
                &estate.root,
                &work.work_id,
                &work.run_id,
                &["--revision", &revision.to_string()],
            );
            assert_eq!(code, Some(0), "{show}");
            projection_of(&show)
        })
        .collect();
    estate.restart();
    for (revision, frozen) in before.iter().enumerate() {
        let (code, show) = world_show_args(
            &estate.root,
            &work.work_id,
            &work.run_id,
            &["--revision", &revision.to_string()],
        );
        assert_eq!(code, Some(0), "{show}");
        assert_eq!(
            &projection_of(&show),
            frozen,
            "revision {revision} is byte-identical across a real restart"
        );
    }

    estate.stop();
}

/// The recovered index must not be an amnesty. The parent's single
/// `coverage` reason is one label over two different facts, and a
/// `degraded / findings_index_unreadable` parent *hides* any source
/// limitation underneath it — so a fix that merely stops carrying the
/// index reason would report a chain with unresolved references as
/// `complete`, which is the same lie pointing the other way.
///
/// Here the stage's own authored question names a path no admitted
/// source records, which is a fact about `bound` and the captured
/// vector: it is genuinely carried, and no later expansion re-derives
/// it. The index is then broken, which masks it, and then genuinely
/// recovered. The recovered revision must say `partial /
/// unresolved_references` — neither the stale index reason, nor
/// `complete`.
#[test]
fn a_recovered_index_does_not_erase_a_carried_source_limitation() {
    let mut estate = Estate::new();
    let dir = estate.root.join("routes");
    fs::create_dir_all(&dir).expect("routes dir");
    let route = dir.join("mixed.json");
    fs::write(
        &route,
        r#"{"id":"mixed","waypoints":[
          {"id":"mixed/only","kind":"Actor",
           "declared_outputs":[{"name":"out.md","required":true}],
           "intent":"Decide the boundary refusal.",
           "orient":{"question":"Where is consult_boundary_refusal decided, and what does src/absent_module_zzz.rs say about it?","sources":["demo"]}}
        ]}"#,
    )
    .expect("write route");
    let work = submit_kind(
        &estate.root,
        route.to_str().unwrap(),
        &estate.repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .expect("submit");

    // The carried fact, observed by the initial assembly itself.
    let initial = projection_of(&world_show(&estate.root, &work.work_id, &work.run_id));
    assert_eq!(initial["coverage"]["state"], "partial", "{initial}");
    assert_eq!(
        initial["coverage"]["reason"], "unresolved_references",
        "the authored question names a path no admitted source records: {initial}"
    );
    assert!(
        !initial["unknowns"].as_array().expect("unknowns").is_empty(),
        "{initial}"
    );

    let index_path = estate
        .root
        .join("atlas")
        .join(wirk_atlas::FINDINGS_INDEX_FILE);
    let (ok, _, err) = atlas(&estate.root, &["findings", "--rebuild", "--admin"]);
    assert!(ok, "{err}");

    // Broken: the index reason is strictly worse, so it is what the
    // document says — and it hides the unresolved reference underneath.
    fs::write(&index_path, b"this is not a findings row\n").expect("corrupt the index");
    let masked = projection_of(&expand_ok(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &["--question", "What does the estate know?"],
    ));
    assert_eq!(masked["coverage"]["state"], "degraded", "{masked}");
    assert_eq!(
        masked["coverage"]["reason"], "findings_index_unreadable",
        "{masked}"
    );

    // Recovered — and the reference the authored text named still
    // resolves nowhere, so this is `partial`, never `complete`.
    fs::remove_file(&index_path).expect("remove the corrupt file");
    let (ok, _, err) = atlas(&estate.root, &["findings", "--rebuild", "--admin"]);
    assert!(ok, "rebuild: {err}");
    let recovered = projection_of(&expand_ok(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &["--question", "And once the index is real again?"],
    ));
    assert_eq!(
        index_note(&recovered)["state"],
        "synchronized",
        "the index really did recover: {recovered}"
    );
    assert_eq!(index_note(&recovered)["complete"], true, "{recovered}");
    assert_eq!(
        recovered["coverage"]["state"], "partial",
        "a recovered index does not resolve a reference that still resolves nowhere: {recovered}"
    );
    assert_eq!(
        recovered["coverage"]["reason"], "unresolved_references",
        "the carried source limitation is what is left, not the stale index reason: {recovered}"
    );
    assert!(
        !recovered["unknowns"]
            .as_array()
            .expect("unknowns")
            .is_empty(),
        "and it is still listed as an unknown: {recovered}"
    );
    let next_action = recovered["next_action"].as_str().unwrap_or_default();
    assert!(
        next_action.contains("did not resolve"),
        "the sentence names the fact that is actually true here: {next_action}"
    );
    assert!(
        !next_action.contains("could not be read"),
        "and not the one that is not: {next_action}"
    );

    estate.stop();
}
