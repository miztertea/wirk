//! Real-daemon, real-Git proof of the published-currentness fix
//! (`published_recorded_generations` in `wirkd/server.rs`, ruling 0141,
//! knowledge/work/p3-world-loop/published-currentness): a `ChildReceipt`-
//! settled `EstateLocal` publication whose own `Finding` carries admitted
//! *source* evidence must carry a real `generation_relation` for a later
//! consulting Work, not the `unknown` the gap left every such row
//! reporting (`native-learning-use/ROOT-CURRENTNESS-CAUSE.md`).
//!
//! Every finding here is one the shipped `wirk finding raise` journaled,
//! every settlement one the shipped `wirk finding settle` minted against
//! a real estate policy, every generation one `wirk atlas
//! acquire`/`publish` really recorded, and every assertion reads what an
//! actor reads through `wirk world show` (ruling 0040: no fake proof).

#[path = "support/nested_harness.rs"]
mod harness;
#[path = "support/route_fixture.rs"]
mod route_fixture;
#[path = "../src/wirkd/mod.rs"]
mod wirkd;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use harness::{ParentRef, claim_ok, init_repo, journal_events, start_wirkd, wirk_bin, write_file};
use serde_json::Value;
use wirk_core::EventKind;

// ---- generic CLI/atlas plumbing, the same shape every consult test uses ---

fn commit_all(repo: &Path) {
    for args in [
        vec!["add", "-A"],
        vec![
            "-c",
            "user.name=published-currentness-test",
            "-c",
            "user.email=published-currentness@example.test",
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

fn locate(estate: &Path, alias: &str, marker: &str) -> String {
    let (ok, search, err) = atlas(estate, &["search", "--source", alias, "--query", marker]);
    assert!(ok, "{err}");
    search["hits"][0]["coordinate"]
        .as_str()
        .unwrap_or_else(|| panic!("no hit for {marker} in {alias}: {search}"))
        .to_string()
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

fn world_show(estate: &Path, work: &str, run: &str) -> Value {
    let output = Command::new(wirk_bin())
        .args(["world", "show", "--json"])
        .env("WIRK_ESTATE_ROOT", estate)
        .env("WIRK_WORK_ID", work)
        .env("WIRK_RUN_ID", run)
        .output()
        .expect("wirk world show runs");
    let value: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).unwrap_or(Value::Null);
    assert_eq!(value["orientation"], "delivered", "{value}");
    value["projection"].clone()
}

fn consulted_by_id(projection: &Value, id: &str) -> Value {
    projection["consulted"]
        .as_array()
        .unwrap_or_else(|| panic!("a projection carries a consulted list: {projection}"))
        .iter()
        .find(|item| item["id"].as_str() == Some(id))
        .unwrap_or_else(|| panic!("no consulted finding {id}: {projection}"))
        .clone()
}

/// Every string anywhere in `value`, flattened (`projection_consult.rs`'s
/// own leak-assertion helper, reused verbatim).
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

fn claim_event_id(estate: &Path, work_id: &str) -> String {
    journal_events(estate, work_id)
        .into_iter()
        .find_map(|event| {
            matches!(&event.kind, EventKind::ClaimRecorded { .. }).then_some(event.id.0)
        })
        .expect("a ClaimRecorded event exists in this journal")
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
    let id = wirk_core::WaypointId(waypoint.to_string());
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

fn write_policy(estate: &Path, json: &str) {
    let dir = estate.join("policy");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("settlement.json"), json).unwrap();
}

// ---- the container/child chain (findings.rs's own fixture, trimmed to
// ---- the one positive path this file needs) -------------------------------

const CONTAINER_OBLIGATION: &str = r#""verifies":{"id":"socket-mode-investigated","edition":"1","proves":"the helper child Work ran the admitted socket-mode check and closed its role with a validated Claim","outputs":["helper"],"requires":{"id":"socket-mode-checked","edition":"1"}}"#;
const CHILD_OBLIGATION: &str = r#""verifies":{"id":"socket-mode-checked","edition":"1","proves":"the socket-mode check ran and recorded socket-mode.txt","outputs":["socket-mode.txt"]}"#;

fn write_container_route(estate: &Path, name: &str, waypoint: &str, leaf_command: &str) {
    route_fixture::write_route(
        estate,
        name,
        &format!(
            r#"{{"id":"{name}","waypoints":[
            {{"id":"{waypoint}","kind":"Container",
             "declared_outputs":[{{"name":"a.md","required":true}}],
             "required_child_outcomes":[{{"role":"helper","required":true}}],
             {CONTAINER_OBLIGATION},
             "leaves":[
               {{"id":"{waypoint}/leaf-a","kind":"Deterministic","command":["sh","-c","{leaf_command}"],
                "declared_outputs":[{{"name":"a.md","required":true}}]}}
             ]}}
        ]}}"#
        ),
    );
}

fn write_child_check_route(estate: &Path, name: &str, check_command: &str) {
    route_fixture::write_route(
        estate,
        name,
        &format!(
            r#"{{"id":"{name}","waypoints":[
            {{"id":"check","kind":"Deterministic","command":["sh","-c","{check_command}"],
             "declared_outputs":[{{"name":"socket-mode.txt","required":true}}],
             {CHILD_OBLIGATION}}},
            {{"id":"report","kind":"Deterministic","command":["sh","-c","echo done > report.md"],
             "declared_outputs":[{{"name":"report.md","required":true}}]}}
        ]}}"#
        ),
    );
}

fn write_child_chain_policy(estate: &Path, container_basis: &str, child_basis: &str) {
    write_policy(
        estate,
        &format!(
            r#"{{"version":2,"classes":[
              {{"class":"child_investigation_confirmed","scope":"estate_local","kinds":["contradicted_assumption"],
                "obligations":[{{"id":"socket-mode-investigated","edition":"1","basis":"{container_basis}","mechanisms":["{child_basis}"]}}]}},
              {{"class":"deterministic_verified","scope":"work_local","kinds":["verified_outcome"],
                "obligations":[{{"id":"socket-mode-checked","edition":"1","basis":"{child_basis}"}}]}}
            ]}}"#
        ),
    );
}

/// One orienting Actor leaf, exactly `projection_consult.rs`'s own
/// `one_stage_route`, so a consulting Work has a projection chain at
/// all.
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
                "intent":"Consult what the estate already recorded.",
                "orient":{{"question":"What does the estate know?","sources":{sources}}}}}
            ]}}"#,
            id = serde_json::to_string(name).unwrap(),
            leaf = serde_json::to_string(&format!("{name}/only")).unwrap(),
        ),
    )
    .expect("write route");
    path
}

/// Two orienting Actor stages over `sources`, so a refresh-and-republish
/// between them is observed by a **new Run's** own captured generation
/// (`projection_consult.rs`'s own `two_stage_route`).
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
                "intent":"Survey what the estate already recorded.",
                "orient":{{"question":"What does the estate know?","sources":{sources}}}}},
              {{"id":{second},"kind":"Actor",
                "declared_outputs":[{{"name":"change.md","required":true}}],
                "intent":"Reconsider what the estate now records.",
                "orient":{{"question":"What does the estate know now?","sources":{sources}}}}}
            ]}}"#,
            id = serde_json::to_string(name).unwrap(),
            first = serde_json::to_string(&format!("{name}/survey")).unwrap(),
            second = serde_json::to_string(&format!("{name}/change")).unwrap(),
        ),
    )
    .expect("write route");
    path
}

fn claim_stage(estate: &Path, socket: &Path, work: &str, run: &str, artifact: &str, body: &str) {
    let worktree = harness::materialize_actor(socket, estate, work, run);
    fs::write(worktree.join(artifact), body).expect("write artifact");
    claim_ok(estate, work, run, &format!("{artifact}={artifact}"));
}

fn source_repo(root: &Path) -> PathBuf {
    let repo = root.join("demo-source-repo");
    fs::create_dir_all(repo.join("notes")).expect("notes dir");
    init_repo(&repo);
    write_file(
        &repo,
        "notes/finding.md",
        "# Notes\n\nchildreceiptcanary: the fact this chain's Finding names.\n",
    );
    commit_all(&repo);
    repo
}

fn other_repo(root: &Path) -> PathBuf {
    let repo = root.join("other-source-repo");
    fs::create_dir_all(repo.join("notes")).expect("notes dir");
    init_repo(&repo);
    write_file(&repo, "notes/other.md", "# Other\n\nunrelated to demo.\n");
    commit_all(&repo);
    repo
}

/// The decisive check: a `ChildReceipt`-settled `EstateLocal` Finding
/// whose own admitted evidence names a real *source* coordinate now
/// carries that generation into a later consulting Work's projection —
/// `recorded_still_published` here, never the `unknown`
/// `published_recorded_generations` reported for every non-`ActorReview`
/// settlement before this fix — and a source refresh moves it to
/// `recorded_superseded` without touching the older record or its World.
/// A consumer this requester's own bindings do not admit to the source
/// still sees `unknown` and an empty `recorded_generations`: the
/// derivation is gated by disclosure, never a blanket read of the
/// finding's own evidence.
#[test]
fn a_child_receipt_settled_publication_carries_its_admitted_source_generation_to_a_later_consumer()
{
    let dir = tempfile::tempdir().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();

    write_container_route(&estate, "obliged_container", "outer", "echo a > a.md");
    write_child_check_route(&estate, "helper_check", "echo 0775 > socket-mode.txt");
    one_stage_route(&estate, "consumer_bound", r#"["demo"]"#);
    two_stage_route(&estate, "consumer_refresh", r#"["demo"]"#);
    one_stage_route(&estate, "consumer_unbound", r#"["other"]"#);

    let (_wirkd_child, pointer) = start_wirkd(&estate);

    let demo_repo = source_repo(dir.path());
    let generation_1 = publish(&estate, "demo", &demo_repo);
    let other_source_repo = other_repo(dir.path());
    publish(&estate, "other", &other_source_repo);
    let demo_coordinate = locate(&estate, "demo", "childreceiptcanary");

    // ---- the container/child chain: a real ChildReceipt settlement ----

    let parent_repo = dir.path().join("parent-repo");
    init_repo(&parent_repo);
    let parent = harness::submit(
        &estate,
        "obliged_container",
        &parent_repo,
        &["demo:write", "helper:write"],
        None,
    )
    .expect("parent submits");
    let container_basis = obligation_basis_for(&estate, &parent.work_id, "outer");

    write_file(&parent_repo, "a.md", "a\n");
    claim_ok(&estate, &parent.work_id, &parent.run_id, "a.md=a.md");

    let helper_repo = dir.path().join("helper-repo");
    init_repo(&helper_repo);
    let helper = harness::submit(
        &estate,
        "helper_check",
        &helper_repo,
        &["helper:write"],
        Some(ParentRef {
            work: &parent.work_id,
            waypoint: "outer",
            run: &parent.run_id,
            role: "helper",
            attempt: None,
        }),
    )
    .expect("helper submits");
    let check_basis = obligation_basis_for(&estate, &helper.work_id, "check");

    write_child_chain_policy(&estate, &container_basis, &check_basis);

    write_file(&helper_repo, "socket-mode.txt", "0775\n");
    claim_ok(
        &estate,
        &helper.work_id,
        &helper.run_id,
        "socket-mode.txt=socket-mode.txt",
    );
    let helper_claim = claim_event_id(&estate, &helper.work_id);
    let helper_report_run = harness::status(&pointer.socket, &helper.work_id)["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    let (code, settled_child, stderr) = raise_cli(
        &estate,
        &helper.work_id,
        &helper_report_run,
        &[
            "--kind",
            "verified_outcome",
            "--scope",
            "work_local",
            "--claim",
            "the socket-mode check ran",
            "--evidence",
            &format!("work/{}/event/{helper_claim}", helper.work_id),
            "--obligation",
            "socket-mode-checked@1",
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        settled_child["settled"].is_object(),
        "the child's own verification really settles: {settled_child}"
    );
    let good_child_finding = settled_child["id"].as_str().unwrap().to_string();

    // The parent's Finding: its own evidence is the admitted *source*
    // coordinate, not a journal reference — the exact shape
    // ROOT-CURRENTNESS-CAUSE.md found real (`finding-18d3a1f0b2ebb281-10`).
    let (code, raised, stderr) = raise_cli(
        &estate,
        &parent.work_id,
        &parent.run_id,
        &[
            "--kind",
            "contradicted_assumption",
            "--scope",
            "estate_local",
            "--claim",
            "the socket is bound with a wider mode than the trust boundary states",
            "--evidence",
            &demo_coordinate,
            "--obligation",
            "socket-mode-investigated@1",
            "--confirmed-by",
            &format!("work/{}/finding/{good_child_finding}", helper.work_id),
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let legit = raised["id"].as_str().unwrap().to_string();

    write_file(&helper_repo, "report.md", "done\n");
    claim_ok(
        &estate,
        &helper.work_id,
        &helper_report_run,
        "report.md=report.md",
    );
    assert_eq!(
        harness::state_of(&pointer.socket, &parent.work_id),
        "completed",
        "the container must have closed on the helper's receipt"
    );

    let (code, settled, stderr) = finding_cli(&estate, &["settle", "--finding", &legit, "--admin"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        settled["settled"].is_object(),
        "expected the parent's finding to settle: {settled}"
    );
    assert_eq!(
        settled["settled"]["authority"]["policy"]["class"]
            .as_str()
            .unwrap(),
        "child_investigation_confirmed"
    );
    assert_eq!(
        settled["settled"]["check"]["proof"],
        Value::Null,
        "ChildProof carries no source generation of its own — this fix must not invent one: {settled}"
    );

    // ---- THE POSITIVE: a bound consumer sees a real relation ----------

    let bound_repo = dir.path().join("bound-consumer-repo");
    init_repo(&bound_repo);
    let bound_consumer = harness::submit_kind(
        &estate,
        "consumer_bound",
        &bound_repo,
        &["demo:write", "helper:write"],
        None,
        Some("actor"),
    )
    .expect("bound consumer submits");
    let at_bound = world_show(&estate, &bound_consumer.work_id, &bound_consumer.run_id);
    let item = consulted_by_id(&at_bound, &legit);
    assert_eq!(item["origin"], "estate_publication", "{item}");
    assert_eq!(item["status"]["state"], "settled", "{item}");
    assert_eq!(
        item["status"]["class"], "child_investigation_confirmed",
        "{item}"
    );
    assert_eq!(item["claim_verified"], false, "{item}");
    assert_eq!(
        item["generation_relation"], "recorded_still_published",
        "a ChildReceipt-settled publication with admitted source evidence must not report \
         unknown: {item}"
    );
    let recorded = item["recorded_generations"].as_array().expect("recorded");
    assert_eq!(recorded.len(), 1, "{item}");
    let demo_membership = recorded[0][0].as_str().unwrap().to_string();
    assert_eq!(recorded[0][1], generation_1, "{item}");
    let current = item["current_generations"].as_array().expect("current");
    assert_eq!(current.len(), 1, "{item}");
    assert_eq!(current[0], recorded[0], "{item}");
    // The evidence *content* stays undelivered through the publication
    // route — this fix derives a generation identity, never the
    // coordinate itself (ruling 0141's own preserved boundary).
    assert_eq!(
        item["evidence"].as_array().unwrap().len(),
        0,
        "no authored evidence content is delivered here: {item}"
    );
    assert_eq!(item["evidence_not_delivered"], 1, "{item}");

    // ---- refresh and republish: superseded, history unmoved ------------

    let refresh_consumer_repo = dir.path().join("refresh-consumer-repo");
    init_repo(&refresh_consumer_repo);
    let refresh_consumer = harness::submit_kind(
        &estate,
        "consumer_refresh",
        &refresh_consumer_repo,
        &["demo:write", "helper:write"],
        None,
        Some("actor"),
    )
    .expect("refresh consumer submits");
    let at_first = world_show(&estate, &refresh_consumer.work_id, &refresh_consumer.run_id);
    let first_run = refresh_consumer.run_id.clone();
    let first_item = consulted_by_id(&at_first, &legit);
    assert_eq!(
        first_item["generation_relation"], "recorded_still_published",
        "{first_item}"
    );

    write_file(
        &demo_repo,
        "notes/finding.md",
        "# Notes\n\nchildreceiptcanary: revised.\n",
    );
    commit_all(&demo_repo);
    let generation_2 = publish(&estate, "demo", &demo_repo);
    assert_ne!(
        generation_1, generation_2,
        "a real refresh moves the generation"
    );

    claim_stage(
        &estate,
        &pointer.socket,
        &refresh_consumer.work_id,
        &first_run,
        "survey.md",
        "# Survey\n\nrecorded before the refresh.\n",
    );
    let second_run = harness::status(&pointer.socket, &refresh_consumer.work_id)["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    let at_second = world_show(&estate, &refresh_consumer.work_id, &second_run);
    let second_item = consulted_by_id(&at_second, &legit);
    assert_eq!(
        second_item["generation_relation"], "recorded_superseded",
        "the finding's own recorded generation now differs from what is currently published: \
         {second_item}"
    );
    assert_eq!(
        second_item["recorded_generations"], first_item["recorded_generations"],
        "the record itself is never rewritten: {second_item}"
    );
    let second_current = second_item["current_generations"]
        .as_array()
        .expect("current");
    assert_eq!(second_current.len(), 1, "{second_item}");
    assert_eq!(second_current[0][0], demo_membership, "{second_item}");
    assert_eq!(second_current[0][1], generation_2, "{second_item}");

    // The earlier World is immutable: reading the first run again after
    // the refresh returns byte-identical content.
    let at_first_again = world_show(&estate, &refresh_consumer.work_id, &first_run);
    assert_eq!(
        at_first, at_first_again,
        "an already-delivered World never moves under a later refresh"
    );

    // ---- narrowed binding: no foreign-membership leak -------------------

    let unbound_repo = dir.path().join("unbound-consumer-repo");
    init_repo(&unbound_repo);
    let unbound_consumer = harness::submit_kind(
        &estate,
        "consumer_unbound",
        &unbound_repo,
        &["other:write"],
        None,
        Some("actor"),
    )
    .expect("unbound consumer submits");
    let at_unbound = world_show(&estate, &unbound_consumer.work_id, &unbound_consumer.run_id);
    // A requester whose own bindings do not cover the producing Work's
    // bindings never reaches `published_recorded_generations` at all —
    // `published_row_scoped`'s existing `admits_work_checkout` gate
    // excludes the whole row first, the strongest form of "no foreign
    // membership or generation leaks": not even the row's existence is
    // disclosed, only an honest inadmissible count.
    assert!(
        at_unbound["consulted"]
            .as_array()
            .expect("consulted")
            .iter()
            .all(|item| item["id"].as_str() != Some(legit.as_str())),
        "an unbound requester must not see this row at all: {at_unbound}"
    );
    assert_eq!(
        at_unbound["omitted"],
        serde_json::json!([{"count": 1, "kind": "inadmissible"}]),
        "the exclusion is counted honestly, never silent: {at_unbound}"
    );
    assert_discloses_nothing(
        "an unbound consumer's projection",
        &at_unbound,
        &[&generation_1, &generation_2, &demo_coordinate],
    );

    harness::stop_wirkd(&estate, _wirkd_child);
}
