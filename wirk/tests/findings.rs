//! Real-daemon, real-Git proof of W-B: evidence-backed Findings,
//! daemon-derived Settlement under an explicit admitted policy,
//! unverified Assertions kept distinct, Application's separated
//! mechanical/judgement split, the estate's rebuildable Findings index,
//! and restart recovery (`knowledge/work/p3-world-loop/W-B-BUILD.md`,
//! corrected by `loop-b-prepare-correct/HANDOFF.md` and
//! `W-B-CONSTRUCTION-REVIEW.md`). Drives the real built `wirk` binary
//! against a real `wirkd` and real Git repositories, the same
//! discipline `nested_work.rs`/`source_substrate.rs` already use —
//! never a library call for anything the CLI exposes.

#[path = "support/git_gate.rs"]
mod git_gate;
#[path = "support/nested_harness.rs"]
mod harness;
#[path = "support/route_fixture.rs"]
mod route_fixture;
#[path = "../src/wirkd/mod.rs"]
mod wirkd;

use std::fs;
use std::path::Path;
use std::process::Command;

use git_gate::GitGate;
use harness::*;

use wirk_core::{
    AdmittedEvidence, ClaimId, ClaimKind, ClaimVerdict, EventId, EventKind, EvidenceOutcome,
    EvidenceRef, Finding, FindingId, FindingKind, FindingScope, RunId, Timestamp, WaypointId,
    WorkId, WorldHash,
};

fn wirk_bin() -> &'static str {
    env!("CARGO_BIN_EXE_wirk")
}

fn write_policy(estate: &Path, json: &str) {
    let dir = estate.join("policy");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("settlement.json"), json).unwrap();
}

/// The one obligation this file's Deterministic routes declare, as the
/// Route file's own `verifies` object. `wp-1` runs `echo one > out1.md`
/// and proves exactly that, nothing wider.
const WP1_OBLIGATION: &str = r#""verifies":{"id":"out1-produced","edition":"1","proves":"wp-1 ran its declared command and produced out1.md","outputs":["out1.md"]}"#;

/// The `basis` the estate's settlement policy must admit for `waypoint`
/// in `work_id` — re-derived from the Work's own journal exactly as the
/// daemon derives it (the Route definition frozen at submit, plus the
/// World actually reserved for that Waypoint), never transcribed.
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

/// A settlement policy admitting exactly one class for one kind, and
/// exactly one obligation within it — the shape every settling test
/// below writes, always after the basis it admits is real.
fn write_policy_admitting(
    estate: &Path,
    class: &str,
    kind: &str,
    obligations: &[(&str, &str, &str)],
) {
    let admitted: Vec<String> = obligations
        .iter()
        .map(|(id, edition, basis)| {
            format!(r#"{{"id":"{id}","edition":"{edition}","basis":"{basis}"}}"#)
        })
        .collect();
    write_policy(
        estate,
        &format!(
            r#"{{"version":2,"classes":[{{"class":"{class}","scope":"estate_local","kinds":["{kind}"],"obligations":[{}]}}]}}"#,
            admitted.join(",")
        ),
    );
}

/// `wirk atlas <args...> --estate <estate> --json`, same convention as
/// `source_substrate.rs`'s own `atlas()` helper.
fn atlas(estate: &Path, args: &[&str]) -> (bool, serde_json::Value, String) {
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
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let value = serde_json::from_str(&stdout).unwrap_or(serde_json::Value::Null);
    (output.status.success(), value, stderr)
}

/// `wirk finding raise` from the actor's own pane env (triple, no
/// `--estate`) — mirrors `wirk claim`'s own env-injection shape.
fn raise_cli(
    estate: &Path,
    work_id: &str,
    run_id: &str,
    args: &[&str],
) -> (Option<i32>, serde_json::Value, String) {
    let mut full = vec!["finding", "raise"];
    full.extend_from_slice(args);
    full.push("--json");
    let output = Command::new(wirk_bin())
        .args(&full)
        .env("WIRK_ESTATE_ROOT", estate)
        .env("WIRK_WORK_ID", work_id)
        .env("WIRK_RUN_ID", run_id)
        .output()
        .expect("wirk finding raise runs");
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let value = serde_json::from_str(&stdout).unwrap_or(serde_json::Value::Null);
    (output.status.code(), value, stderr)
}

/// `wirk finding applied` from the producer's own pane env (triple, no
/// `--estate`) — W-B-CORRECT.md defect 3: a real, checked, current
/// producer identity, exactly like `raise_cli` above.
fn applied_cli(
    estate: &Path,
    work_id: &str,
    run_id: &str,
    args: &[&str],
) -> (Option<i32>, serde_json::Value, String) {
    let mut full = vec!["finding", "applied"];
    full.extend_from_slice(args);
    full.push("--json");
    let output = Command::new(wirk_bin())
        .args(&full)
        .env("WIRK_ESTATE_ROOT", estate)
        .env("WIRK_WORK_ID", work_id)
        .env("WIRK_RUN_ID", run_id)
        .output()
        .expect("wirk finding applied runs");
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let value = serde_json::from_str(&stdout).unwrap_or(serde_json::Value::Null);
    (output.status.code(), value, stderr)
}

fn finding_cli(estate: &Path, args: &[&str]) -> (Option<i32>, serde_json::Value, String) {
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
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let value = serde_json::from_str(&stdout).unwrap_or(serde_json::Value::Null);
    (output.status.code(), value, stderr)
}

/// The two-leaf Route every deterministic case here submits. `wp-1`
/// declares the verification obligation it discharges; `wp-2` declares
/// none, which is what the frozen candidate's own `wp-1` looked like and
/// is the shape the counterexample below leans on.
fn write_two_leaf_route(estate: &Path) {
    write_two_leaf_route_with(estate, WP1_OBLIGATION);
}

/// The same Route with `wp-1`'s own `verifies` object replaced — an
/// empty `verifies` (`""`) is a `wp-1` that declares no obligation at
/// all, exactly the frozen candidate.
fn write_two_leaf_route_with(estate: &Path, wp1_verifies: &str) {
    let separator = if wp1_verifies.is_empty() { "" } else { "," };
    route_fixture::write_route(
        estate,
        "two_leaf",
        &format!(
            r#"{{"id":"two-leaf","waypoints":[
            {{"id":"wp-1","kind":"Deterministic","command":["sh","-c","echo one > out1.md"],"declared_outputs":[{{"name":"out1.md","required":true}}]{separator}{wp1_verifies}}},
            {{"id":"wp-2","kind":"Deterministic","command":["sh","-c","echo two > out2.md"],"declared_outputs":[{{"name":"out2.md","required":true}}]}}
        ]}}"#
        ),
    );
}

fn claim_event_id(estate: &Path, work_id: &str) -> String {
    journal_events(estate, work_id)
        .into_iter()
        .find_map(|event| {
            matches!(&event.kind, EventKind::ClaimRecorded { .. }).then_some(event.id.0)
        })
        .expect("a ClaimRecorded event exists in this journal")
}

fn submitted_event_id(estate: &Path, work_id: &str) -> String {
    journal_events(estate, work_id)[0].id.0.clone()
}

// ---- 1. WorkLocal: visible immediately, never indexed --------------------

#[test]
fn work_local_finding_is_visible_to_next_stage_and_never_indexed() {
    let dir = tempfile::tempdir().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    route_fixture::install_route_fixture(&estate, "wa_simple_leaf");
    let (wirkd_child, _pointer) = start_wirkd(&estate);

    let repo = dir.path().join("repo");
    init_repo(&repo);
    let work = submit(&estate, "wa_simple_leaf", &repo, &["demo:write"], None).unwrap();

    let evidence = format!(
        "work/{}/event/{}",
        work.work_id,
        submitted_event_id(&estate, &work.work_id)
    );
    let (code, raised, stderr) = raise_cli(
        &estate,
        &work.work_id,
        &work.run_id,
        &[
            "--kind",
            "gap",
            "--scope",
            "work_local",
            "--claim",
            "a gap exists",
            "--evidence",
            &evidence,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let finding_id = raised["id"].as_str().unwrap().to_string();
    assert!(raised["settled"].is_null());

    let (code, listed, stderr) = finding_cli(
        &estate,
        &[
            "list",
            "--requesting-work",
            &work.work_id,
            "--work",
            &work.work_id,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        listed["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["id"] == finding_id)
    );

    let (ok, index, err) = atlas(&estate, &["findings", "--rebuild", "--admin"]);
    assert!(ok, "{err}");
    assert!(
        index["rows"].as_array().unwrap().is_empty(),
        "a WorkLocal finding must never reach the index: {index}"
    );

    stop_wirkd(&estate, wirkd_child);
}

// ---- 2. the decisive negative: an actor cannot settle, only assert -------

#[test]
fn actor_pane_can_assert_but_never_settle_and_raw_record_is_forbidden() {
    let dir = tempfile::tempdir().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    route_fixture::install_route_fixture(&estate, "wa_simple_leaf");
    // An enabled-but-empty policy: still admits nothing, defense at both layers.
    write_policy(&estate, r#"{"version":1,"classes":[]}"#);
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let repo = dir.path().join("repo");
    init_repo(&repo);
    let work = submit(&estate, "wa_simple_leaf", &repo, &["demo:write"], None).unwrap();

    let evidence = format!(
        "work/{}/event/{}",
        work.work_id,
        submitted_event_id(&estate, &work.work_id)
    );
    let (code, raised, stderr) = raise_cli(
        &estate,
        &work.work_id,
        &work.run_id,
        &[
            "--kind",
            "contradicted_assumption",
            "--scope",
            "estate_local",
            "--claim",
            "an actor's own unverified say-so",
            "--evidence",
            &evidence,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let finding_id = raised["id"].as_str().unwrap().to_string();

    // The actor's own pane environment issues `finding assert` — the
    // request succeeds (it is a real, honest surface), but it must not
    // settle anything.
    let (code, asserted, stderr) = finding_cli(
        &estate,
        &[
            "assert",
            "--finding",
            &finding_id,
            "--decision",
            "accepted",
            "--by",
            "root",
            "--admin",
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        asserted["settled"].is_null(),
        "an assertion must never settle: {asserted}"
    );
    assert_eq!(
        asserted["assertions"][0]["verified"],
        serde_json::json!(false)
    );

    let (code, settled, stderr) =
        finding_cli(&estate, &["settle", "--finding", &finding_id, "--admin"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(settled["settled"].is_null());
    assert!(settled["pending"].is_object());

    let (ok, index, err) = atlas(&estate, &["findings", "--rebuild", "--admin"]);
    assert!(ok, "{err}");
    assert!(
        !index["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["kind"] == "settled" && row["finding"]["id"] == finding_id),
        "no settled row for an asserted-only finding: {index}"
    );

    // A crafted `FindingSettled` on the raw wire is refused Forbidden —
    // the mechanism, not merely the CLI's own restraint.
    let forged = wirkd::client::call(
        &pointer.socket,
        &wirkd::Request::record(wirkd::RecordPayload {
            work_id: WorkId(work.work_id.clone()),
            run: None,
            kind: EventKind::FindingSettled {
                finding: FindingId(finding_id.clone()),
                settlement: wirk_core::Settlement {
                    authority: wirk_core::SettlementAuthority {
                        class: wirk_core::SettlementClass::DeterministicVerified,
                        policy_version: 1,
                        policy_digest: "forged".to_string(),
                    },
                    check: wirk_core::SettlementCheck::ValidatedClaim {
                        work: WorkId(work.work_id.clone()),
                        claim: ClaimId("forged".to_string()),
                        claim_event: EventId("forged".to_string()),
                        proof: Some(wirk_core::DeterministicProof {
                            obligation: wirk_core::ObligationRef {
                                id: "forged".to_string(),
                                edition: "forged".to_string(),
                            },
                            basis: "forged".to_string(),
                            proves: "forged".to_string(),
                            waypoint: WaypointId("wp-1".to_string()),
                            attempt: 1,
                            world_hash: WorldHash("forged".to_string()),
                            artifacts: Vec::new(),
                        }),
                        unread: Default::default(),
                    },
                    settled_by: EventId("forged".to_string()),
                    at: Timestamp(0),
                    minted_at_startup: false,
                },
            },
        }),
    )
    .expect("raw socket call succeeds");
    match forged {
        wirkd::Reply::Err { error, .. } => assert_eq!(error.code, "Forbidden"),
        other => panic!("a raw FindingSettled must be Forbidden, got {other:?}"),
    }

    stop_wirkd(&estate, wirkd_child);
}

// ---- 3. deterministic-verified: the obligation proof contract --------

/// The whole contract, against the real daemon, real Git and the real
/// CLI, in the exact shape the defect was reproduced in.
///
/// **The reproduced counterexample** (frozen `0634657`,
/// `loop-b-obligation-build/raw/00-counterexample-frozen-base.txt`): an
/// earlier deterministic leaf runs `sh -c "echo one > out1.md"`, and a
/// Finding whose sentence is *"wirk has no remote code execution
/// vulnerability and its full security audit passed with zero findings"*
/// cites that leaf's Claim and settles `deterministic_verified` — then
/// reaches the estate index as a settled row.
///
/// Every phase below is a real `wirk finding raise` from a real pane
/// triple against a real settled/unsettled reply, and every legitimate
/// positive reaches the same handler through the same verb.
#[test]
fn deterministic_verified_proves_the_named_obligation_and_refuses_every_nearby_shape() {
    let security_sentence = "wirk has no remote code execution vulnerability and its full security audit passed with zero findings";

    let dir = tempfile::tempdir().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    write_two_leaf_route(&estate);
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let repo = dir.path().join("repo");
    init_repo(&repo);
    let work = submit(&estate, "two_leaf", &repo, &["demo:write"], None).unwrap();
    assert_eq!(work.waypoint, "wp-1");
    write_file(&repo, "out1.md", "one\n");
    claim_ok(&estate, &work.work_id, &work.run_id, "out1.md=out1.md");
    assert_eq!(state_of(&pointer.socket, &work.work_id), "active");

    let claim_id = claim_event_id(&estate, &work.work_id);
    let evidence = format!("work/{}/event/{claim_id}", work.work_id);
    let wp2_run = status(&pointer.socket, &work.work_id)["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    let basis = obligation_basis_for(&estate, &work.work_id, "wp-1");

    // The estate operator admits exactly this check, by name, edition
    // **and** content basis. Written now, after the World it admits is
    // real — an operator admits a check it can point at.
    write_policy_admitting(
        &estate,
        "deterministic_verified",
        "verified_outcome",
        &[("out1-produced", "1", &basis)],
    );

    let raise = |args: &[&str]| {
        let mut full = vec![
            "--kind",
            "verified_outcome",
            "--scope",
            "estate_local",
            "--evidence",
            &evidence,
        ];
        full.extend_from_slice(args);
        raise_cli(&estate, &work.work_id, &wp2_run, &full)
    };

    // (a) THE COUNTEREXAMPLE. The same call that settled on the frozen
    // candidate: a real, Validated, Done, current Claim of an earlier
    // Route position, cited by an unrelated sentence that names no
    // obligation at all.
    let (code, raised, stderr) = raise(&["--claim", security_sentence]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        raised["settled"].is_null(),
        "an unrelated sentence naming no obligation must never settle: {raised}"
    );
    let unnamed_id = raised["id"].as_str().unwrap().to_string();
    let (_, pending, _) = finding_cli(&estate, &["settle", "--finding", &unnamed_id, "--admin"]);
    assert_eq!(
        pending["pending"]["reason"].as_str().unwrap(),
        "no-obligation-named"
    );

    // (b) Naming an obligation this estate has not admitted settles
    // nothing — a proposer cannot make a check authoritative by naming
    // it.
    let (code, raised, stderr) = raise(&[
        "--claim",
        security_sentence,
        "--obligation",
        "full-security-audit@1",
    ]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(raised["settled"].is_null(), "{raised}");
    let unadmitted_id = raised["id"].as_str().unwrap().to_string();
    let (_, pending, _) = finding_cli(&estate, &["settle", "--finding", &unadmitted_id, "--admin"]);
    assert_eq!(
        pending["pending"]["reason"].as_str().unwrap(),
        "obligation-not-admitted"
    );

    // (c) The right check identity at an edition `wp-1` does not
    // declare: the admitted `out1-produced` at edition `2`.
    write_policy_admitting(
        &estate,
        "deterministic_verified",
        "verified_outcome",
        &[
            ("out1-produced", "1", &basis),
            ("out1-produced", "2", &basis),
        ],
    );
    let (code, raised, stderr) = raise(&[
        "--claim",
        "wp-1 produced out1.md at edition 2",
        "--obligation",
        "out1-produced@2",
    ]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        raised["settled"].is_null(),
        "a check edition wp-1 never declared discharges nothing, even admitted: {raised}"
    );
    let wrong_edition_id = raised["id"].as_str().unwrap().to_string();

    // (d) The right check identity, admitted at the wrong basis — the
    // operator admitted a *different* World for this obligation, so the
    // command that actually ran is not the admitted one.
    write_policy_admitting(
        &estate,
        "deterministic_verified",
        "verified_outcome",
        &[("out1-produced", "1", &"0".repeat(64))],
    );
    let (code, raised, stderr) = raise(&[
        "--claim",
        "wp-1 produced out1.md",
        "--obligation",
        "out1-produced@1",
    ]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        raised["settled"].is_null(),
        "an obligation admitted at a different content basis discharges nothing: {raised}"
    );
    let wrong_basis_id = raised["id"].as_str().unwrap().to_string();

    // (e) A malformed obligation token is refused at the wire, not
    // silently carried as an unsettleable Finding.
    let (code, _, refusal) = raise(&["--claim", "x", "--obligation", "no-edition"]);
    assert_eq!(code, Some(3), "a refused raise exits 3, not success");
    let refusal = format!(
        "{refusal}{}",
        raise(&["--claim", "x", "--obligation", "no-edition"]).1
    );
    let _ = refusal;
    let output = Command::new(wirk_bin())
        .args([
            "finding",
            "raise",
            "--kind",
            "verified_outcome",
            "--scope",
            "estate_local",
            "--claim",
            "x",
            "--evidence",
            &evidence,
            "--obligation",
            "no-edition",
        ])
        .env("WIRK_ESTATE_ROOT", &estate)
        .env("WIRK_WORK_ID", &work.work_id)
        .env("WIRK_RUN_ID", &wp2_run)
        .output()
        .expect("wirk finding raise runs");
    let printed = String::from_utf8_lossy(&output.stdout);
    assert!(
        printed.contains("Refused: BadRequest") && printed.contains("<id>@<edition>"),
        "a malformed obligation token is refused at the wire: {printed}"
    );

    // (f) THE MATCHED POSITIVE, same verb, same handler, same evidence
    // token, same Route position: the sentence is still the actor's own
    // free text, but the Finding now names the obligation `wp-1` really
    // declares, at the edition and basis this estate really admitted.
    write_policy_admitting(
        &estate,
        "deterministic_verified",
        "verified_outcome",
        &[("out1-produced", "1", &basis)],
    );
    let (code, raised, stderr) = raise(&[
        "--claim",
        security_sentence,
        "--obligation",
        "out1-produced@1",
    ]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        raised["settled"].is_object(),
        "expected settled inline on raise, got {raised}"
    );
    let settled = &raised["settled"];
    assert_eq!(
        settled["authority"]["policy"]["class"].as_str().unwrap(),
        "deterministic_verified"
    );

    // And this is the point of the whole wave: what it settles is the
    // Route-authored, estate-admitted obligation — never the sentence.
    let proves = &settled["proves"];
    assert_eq!(
        proves["statement"].as_str().unwrap(),
        "wp-1 ran its declared command and produced out1.md"
    );
    assert_ne!(proves["statement"].as_str().unwrap(), security_sentence);
    assert_eq!(
        proves["obligation"]["id"].as_str().unwrap(),
        "out1-produced"
    );
    assert_eq!(proves["obligation"]["edition"].as_str().unwrap(), "1");
    assert_eq!(proves["obligation"]["basis"].as_str().unwrap(), basis);
    assert_eq!(
        proves["discharged_by"]["waypoint"].as_str().unwrap(),
        "wp-1"
    );
    assert_eq!(proves["discharged_by"]["attempt"].as_u64().unwrap(), 1);
    let receipts = proves["discharged_by"]["artifacts"].as_array().unwrap();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0]["name"].as_str().unwrap(), "out1.md");
    assert!(
        receipts[0]["digest"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
            || receipts[0]["digest"].as_str().unwrap().len() == 64,
        "the receipt carries the content identity read at validation: {}",
        receipts[0]
    );

    // The free sentence is still recorded, and still rendered as what it
    // is — the same honesty `assert`'s own `by` already carries.
    assert_eq!(
        raised["claim"].as_str().unwrap(),
        format!("recorded claim: {security_sentence}, unverified")
    );
    assert_eq!(raised["claim_text"].as_str().unwrap(), security_sentence);
    assert!(!raised["claim_verified"].as_bool().unwrap());

    let finding_id = raised["id"].as_str().unwrap().to_string();
    let (code, settled, stderr) =
        finding_cli(&estate, &["settle", "--finding", &finding_id, "--admin"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(settled["settled"].is_object());

    // Authority review §8 ("real gap, executed"): the settled row must
    // reach the estate index immediately, with no `--rebuild` and no
    // restart in between — and the index must render the same honest
    // split, because the index is exactly where the counterexample's own
    // free sentence read as the settled thing.
    let (ok, index, err) = atlas(&estate, &["findings", "--admin"]);
    assert!(ok, "{err}");
    let rows = index["rows"].as_array().unwrap();
    let settled_ids: Vec<&str> = rows
        .iter()
        .filter(|row| row["kind"] == "settled")
        .map(|row| row["finding"]["id"].as_str().unwrap())
        .collect();
    assert!(
        settled_ids.contains(&finding_id.as_str()),
        "the legitimate discharge is indexed immediately, with no --rebuild and no restart: {index}"
    );
    for refused in [&unnamed_id, &unadmitted_id, &wrong_edition_id] {
        assert!(
            !settled_ids.contains(&refused.as_str()),
            "a refused shape must never reach the estate index as settled: {refused} in {index}"
        );
    }
    // `wrong_basis_id` is the one instructive exception, and it is the
    // mechanism working rather than leaking: that Finding named the
    // obligation `wp-1` really declares and was refused *only* because
    // this estate had not yet admitted its basis. Once the operator
    // admitted that basis (phase (f)'s own policy write), the same
    // still-`Proposed` Finding legitimately settled — a policy revision
    // changing what may settle next, never rewriting a past decision.
    assert!(
        settled_ids.contains(&wrong_basis_id.as_str()),
        "a Finding refused only for an unadmitted basis settles once the estate admits it: {index}"
    );
    assert_eq!(
        settled_ids.len(),
        2,
        "exactly the two legitimate discharges, and nothing else: {index}"
    );
    for row in rows.iter().filter(|row| row["kind"] == "settled") {
        assert_eq!(
            row["settlement"]["proves"]["statement"].as_str().unwrap(),
            "wp-1 ran its declared command and produced out1.md",
            "every settled row renders what the check proves, not its own sentence"
        );
        assert!(
            row["finding"]["claim"]
                .as_str()
                .unwrap()
                .starts_with("recorded claim: "),
            "the free sentence stays visibly a recorded, unverified claim: {row}"
        );
    }
    assert!(
        rows.iter()
            .any(|row| row["finding"]["claim"].as_str().unwrap()
                == format!("recorded claim: {security_sentence}, unverified")),
        "the security sentence is still recorded honestly beside what was actually proven: {index}"
    );

    stop_wirkd(&estate, wirkd_child);
}

/// The frozen candidate's own `wp-1`: a Deterministic leaf that declares
/// no verification obligation at all. Its Claim is real and its command
/// succeeded; there is simply nothing for it to have discharged, so no
/// Finding naming any obligation can settle from it. This is the
/// separate half of the counterexample — the Route as it was actually
/// authored before this wave.
#[test]
fn a_leaf_declaring_no_obligation_settles_nothing_however_successful() {
    let dir = tempfile::tempdir().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    write_two_leaf_route_with(&estate, "");
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let repo = dir.path().join("repo");
    init_repo(&repo);
    let work = submit(&estate, "two_leaf", &repo, &["demo:write"], None).unwrap();
    write_file(&repo, "out1.md", "one\n");
    claim_ok(&estate, &work.work_id, &work.run_id, "out1.md=out1.md");

    // The policy admits an obligation by that name at a basis derived
    // from a leaf that *does* declare one, so the refusal below cannot
    // be "the estate admitted nothing".
    write_policy_admitting(
        &estate,
        "deterministic_verified",
        "verified_outcome",
        &[("out1-produced", "1", &"f".repeat(64))],
    );

    let claim_id = claim_event_id(&estate, &work.work_id);
    let evidence = format!("work/{}/event/{claim_id}", work.work_id);
    let wp2_run = status(&pointer.socket, &work.work_id)["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    let (code, raised, stderr) = raise_cli(
        &estate,
        &work.work_id,
        &wp2_run,
        &[
            "--kind",
            "verified_outcome",
            "--scope",
            "estate_local",
            "--claim",
            "wp-1 produced out1.md",
            "--evidence",
            &evidence,
            "--obligation",
            "out1-produced@1",
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        raised["settled"].is_null(),
        "a Waypoint declaring no obligation discharges none: {raised}"
    );

    let (ok, index, err) = atlas(&estate, &["findings", "--rebuild", "--admin"]);
    assert!(ok, "{err}");
    assert!(
        index["rows"].as_array().unwrap().is_empty(),
        "nothing settled reaches the estate index: {index}"
    );

    stop_wirkd(&estate, wirkd_child);
}

// ---- 4. no policy file: settles nothing -----------------------------

#[test]
fn estate_local_settlement_without_policy_file_settles_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    write_two_leaf_route(&estate);
    // No policy/settlement.json written at all.
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let repo = dir.path().join("repo");
    init_repo(&repo);
    let work = submit(&estate, "two_leaf", &repo, &["demo:write"], None).unwrap();
    write_file(&repo, "out1.md", "one\n");
    claim_ok(&estate, &work.work_id, &work.run_id, "out1.md=out1.md");

    let claim_id = claim_event_id(&estate, &work.work_id);
    let evidence = format!("work/{}/event/{claim_id}", work.work_id);
    let wp2_run = status(&pointer.socket, &work.work_id)["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    let (code, raised, stderr) = raise_cli(
        &estate,
        &work.work_id,
        &wp2_run,
        &[
            "--kind",
            "verified_outcome",
            "--scope",
            "estate_local",
            "--claim",
            "the deterministic leaf ran",
            "--evidence",
            &evidence,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert!(raised["settled"].is_null());
    let finding_id = raised["id"].as_str().unwrap().to_string();

    let (code, settled, stderr) =
        finding_cli(&estate, &["settle", "--finding", &finding_id, "--admin"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(settled["settled"].is_null());
    assert_eq!(
        settled["pending"]["reason"].as_str().unwrap(),
        "no-policy-file"
    );

    stop_wirkd(&estate, wirkd_child);
}

// ---- 5. child-investigation-confirmed under the same proof contract ---

/// The container obligation the child cases below discharge. Its
/// **outcome** is the obligated role `helper`; its **mechanism** is the
/// child verification obligation `socket-mode-checked@1` that role's
/// child Work must itself have settled. Both halves are hashed into the
/// container's own basis.
const CONTAINER_OBLIGATION: &str = r#""verifies":{"id":"socket-mode-investigated","edition":"1","proves":"the helper child Work ran the admitted socket-mode check and closed its role with a validated Claim","outputs":["helper"],"requires":{"id":"socket-mode-checked","edition":"1"}}"#;

/// The child's own verification obligation: a real `Deterministic`
/// Waypoint whose World hash content-addresses its command, source basis
/// and expected artifacts. This is the immutable execution a container
/// obligation stands for.
const CHILD_OBLIGATION: &str = r#""verifies":{"id":"socket-mode-checked","edition":"1","proves":"the socket-mode check ran and recorded socket-mode.txt","outputs":["socket-mode.txt"]}"#;

/// A container Route whose `outer` declares `CONTAINER_OBLIGATION`, with
/// `leaf_command` for its own declared output. Written inline rather
/// than added to the shared `wa_*` fixtures, which W-A's own
/// nested-work tests read and this wave does not touch.
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

/// The child Route the `helper` role runs: an obliged deterministic
/// check, then a trailing leaf. The trailing leaf exists so the child
/// can raise its own `VerifiedOutcome` Finding citing the check's own
/// Claim *before* completing — the identical shape the deterministic
/// half already proves, now doing real work for the parent.
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

/// The estate policy for the child cases: the child's own deterministic
/// check is admitted for `work_local` `verified_outcome`, and the
/// container obligation is admitted with that check's basis as its one
/// admitted **mechanism**.
fn write_child_chain_policy(
    estate: &Path,
    container_obligations: &[(&str, &str, &str, &[&str])],
    child_bases: &[&str],
) {
    let containers: Vec<String> = container_obligations
        .iter()
        .map(|(id, edition, basis, mechanisms)| {
            let mechanisms: Vec<String> = mechanisms.iter().map(|m| format!("\"{m}\"")).collect();
            format!(
                r#"{{"id":"{id}","edition":"{edition}","basis":"{basis}","mechanisms":[{}]}}"#,
                mechanisms.join(",")
            )
        })
        .collect();
    let children: Vec<String> = child_bases
        .iter()
        .map(|basis| format!(r#"{{"id":"socket-mode-checked","edition":"1","basis":"{basis}"}}"#))
        .collect();
    write_policy(
        estate,
        &format!(
            r#"{{"version":2,"classes":[
              {{"class":"child_investigation_confirmed","scope":"estate_local","kinds":["contradicted_assumption"],"obligations":[{}]}},
              {{"class":"deterministic_verified","scope":"work_local","kinds":["verified_outcome"],"obligations":[{}]}}
            ]}}"#,
            containers.join(","),
            children.join(",")
        ),
    );
}

/// The child-investigation class under the corrected obligation
/// contract: the parent names the obligation and the confirming child
/// Finding, and the container obligation is discharged only when its
/// obligated role's child has itself **settled** the verification
/// obligation the container declares as its mechanism.
///
/// The independent review's executed C1 is the reason this shape exists:
/// a container's own basis content-addresses prose and outcome-contract
/// shape, so two unrelated Routes can collide on it, and "any admitted
/// evidence at all" let a child whose whole investigation was one
/// Finding reading *"I did not investigate anything"* discharge a
/// statement about a socket-mode investigation.
#[test]
fn child_investigation_confirmed_proves_the_named_obligation_and_refuses_every_nearby_shape() {
    let dir = tempfile::tempdir().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    write_container_route(&estate, "obliged_container", "outer", "echo a > a.md");
    write_child_check_route(&estate, "helper_check", "echo 0775 > socket-mode.txt");
    let (wirkd_child, pointer) = start_wirkd(&estate);
    let claim_text = "the socket is bound with a wider mode than the trust boundary states";

    let parent_repo = dir.path().join("parent-repo");
    init_repo(&parent_repo);
    let parent = submit(
        &estate,
        "obliged_container",
        &parent_repo,
        &["demo:write", "helper:write"],
        None,
    )
    .unwrap();
    assert_eq!(parent.waypoint, "outer/leaf-a");
    let container_basis = obligation_basis_for(&estate, &parent.work_id, "outer");

    // The container's own declared output still needs its leaf's Claim;
    // claiming it first holds the container on the child role, so the
    // settlement below rides on the child-driven close.
    write_file(&parent_repo, "a.md", "a\n");
    claim_ok(&estate, &parent.work_id, &parent.run_id, "a.md=a.md");
    assert_eq!(state_of(&pointer.socket, &parent.work_id), "waiting");

    // The helper child: submitted under the obligated role, so its own
    // `check` Waypoint reserves a real World the operator can inspect
    // before admitting anything.
    let helper_repo = dir.path().join("helper-repo");
    init_repo(&helper_repo);
    let helper = submit(
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
    .unwrap();
    let check_basis = obligation_basis_for(&estate, &helper.work_id, "check");

    // The operator admits the check it inspected, and admits it as the
    // one mechanism of the container obligation.
    write_child_chain_policy(
        &estate,
        &[(
            "socket-mode-investigated",
            "1",
            &container_basis,
            &[&check_basis],
        )],
        &[&check_basis],
    );

    // The child really runs the admitted check and really settles its
    // own verification against it.
    write_file(&helper_repo, "socket-mode.txt", "0775\n");
    claim_ok(
        &estate,
        &helper.work_id,
        &helper.run_id,
        "socket-mode.txt=socket-mode.txt",
    );
    let helper_claim = claim_event_id(&estate, &helper.work_id);
    let helper_report_run = status(&pointer.socket, &helper.work_id)["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    let helper_evidence = format!("work/{}/event/{helper_claim}", helper.work_id);
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
            &helper_evidence,
            "--obligation",
            "socket-mode-checked@1",
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        settled_child["settled"].is_object(),
        "the child's own verification really settles against the admitted check: {settled_child}"
    );
    let good_child_finding = settled_child["id"].as_str().unwrap().to_string();

    // The C1 shape, verbatim: the same child, the same admitted
    // evidence, a Finding that names the mechanism and was never settled
    // against it — "any admitted evidence at all" is not an
    // investigation.
    let (code, unsettled_child, stderr) = raise_cli(
        &estate,
        &helper.work_id,
        &helper_report_run,
        &[
            "--kind",
            "contradicted_assumption",
            "--scope",
            "work_local",
            "--claim",
            "I did not investigate anything",
            "--evidence",
            &helper_evidence,
            "--obligation",
            "socket-mode-checked@1",
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        unsettled_child["settled"].is_null(),
        "a `contradicted_assumption` never settles deterministically: {unsettled_child}"
    );
    let unsettled_child_finding = unsettled_child["id"].as_str().unwrap().to_string();

    // An entirely unrelated Work whose own verification really settled —
    // it simply holds no receipt in this container's activation.
    let stranger_repo = dir.path().join("stranger-repo");
    init_repo(&stranger_repo);
    let stranger = submit(
        &estate,
        "helper_check",
        &stranger_repo,
        &["helper:write"],
        None,
    )
    .unwrap();
    write_file(&stranger_repo, "socket-mode.txt", "0775\n");
    claim_ok(
        &estate,
        &stranger.work_id,
        &stranger.run_id,
        "socket-mode.txt=socket-mode.txt",
    );
    let stranger_claim = claim_event_id(&estate, &stranger.work_id);
    let stranger_run = status(&pointer.socket, &stranger.work_id)["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    let (code, stranger_finding, stderr) = raise_cli(
        &estate,
        &stranger.work_id,
        &stranger_run,
        &[
            "--kind",
            "verified_outcome",
            "--scope",
            "work_local",
            "--claim",
            "the socket-mode check ran",
            "--evidence",
            &format!("work/{}/event/{stranger_claim}", stranger.work_id),
            "--obligation",
            "socket-mode-checked@1",
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        stranger_finding["settled"].is_object(),
        "the stranger's own verification is real and settled: {stranger_finding}"
    );
    let stranger_finding_id = stranger_finding["id"].as_str().unwrap().to_string();

    // Every parent Finding, raised through the real verb while the
    // parent is still open.
    let parent_evidence = format!(
        "work/{}/event/{}",
        parent.work_id,
        submitted_event_id(&estate, &parent.work_id)
    );
    let parent_raise = |args: &[&str]| {
        let mut full = vec![
            "--kind",
            "contradicted_assumption",
            "--scope",
            "estate_local",
            "--claim",
            claim_text,
            "--evidence",
            &parent_evidence,
        ];
        full.extend_from_slice(args);
        let (code, raised, stderr) = raise_cli(&estate, &parent.work_id, &parent.run_id, &full);
        assert_eq!(code, Some(0), "{stderr}");
        raised["id"].as_str().unwrap().to_string()
    };

    let good_token = format!("work/{}/finding/{good_child_finding}", helper.work_id);
    let legit = parent_raise(&[
        "--obligation",
        "socket-mode-investigated@1",
        "--confirmed-by",
        &good_token,
    ]);
    let no_confirmation = parent_raise(&["--obligation", "socket-mode-investigated@1"]);
    let no_obligation = parent_raise(&["--confirmed-by", &good_token]);
    let wrong_edition = parent_raise(&[
        "--obligation",
        "socket-mode-investigated@2",
        "--confirmed-by",
        &good_token,
    ]);
    let unsettled_confirmation = parent_raise(&[
        "--obligation",
        "socket-mode-investigated@1",
        "--confirmed-by",
        &format!("work/{}/finding/{unsettled_child_finding}", helper.work_id),
    ]);
    let borrowed = parent_raise(&[
        "--obligation",
        "socket-mode-investigated@1",
        "--confirmed-by",
        &format!("work/{}/finding/{stranger_finding_id}", stranger.work_id),
    ]);
    let ghost = parent_raise(&[
        "--obligation",
        "socket-mode-investigated@1",
        "--confirmed-by",
        &format!("work/{}/finding/finding-that-never-existed", helper.work_id),
    ]);

    // The helper completes; `outer` closes on its receipt.
    write_file(&helper_repo, "report.md", "done\n");
    claim_ok(
        &estate,
        &helper.work_id,
        &helper_report_run,
        "report.md=report.md",
    );
    assert_eq!(state_of(&pointer.socket, &helper.work_id), "completed");
    assert_eq!(state_of(&pointer.socket, &parent.work_id), "completed");
    let events = journal_events(&estate, &parent.work_id);
    assert!(
        events.iter().any(|event| matches!(
            &event.kind,
            EventKind::StageClosed { waypoint, .. } if waypoint.0 == "outer"
        )),
        "the parent's container must have closed on the helper's receipt"
    );

    // THE POSITIVE.
    let (code, settled, stderr) = finding_cli(&estate, &["settle", "--finding", &legit, "--admin"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        settled["settled"].is_object(),
        "expected the parent's finding settled, got {settled}"
    );
    let settlement = &settled["settled"];
    assert_eq!(
        settlement["authority"]["policy"]["class"].as_str().unwrap(),
        "child_investigation_confirmed"
    );
    let check = &settlement["check"];
    assert_eq!(check["child"].as_str().unwrap(), helper.work_id);
    assert_eq!(check["role"].as_str().unwrap(), "helper");
    assert_eq!(check["child_finding"].as_str().unwrap(), good_child_finding);
    assert_eq!(
        check["obligation"]["basis"].as_str().unwrap(),
        container_basis
    );
    assert_eq!(
        check["requires"]["id"].as_str().unwrap(),
        "socket-mode-checked"
    );
    // The recorded mechanism is the child's own settled verification —
    // the actual immutable execution behind the parent's claim.
    let roles = check["obligated_roles"].as_array().unwrap();
    assert_eq!(roles.len(), 1);
    assert_eq!(roles[0]["role"].as_str().unwrap(), "helper");
    assert_eq!(roles[0]["child"].as_str().unwrap(), helper.work_id);
    assert_eq!(roles[0]["finding"].as_str().unwrap(), good_child_finding);
    assert_eq!(
        roles[0]["mechanism_basis"].as_str().unwrap(),
        check_basis,
        "the mechanism basis is the admitted check's own content address"
    );
    assert!(settlement["proves"]["recorded"].as_bool().unwrap());
    assert_eq!(
        settlement["proves"]["statement"].as_str().unwrap(),
        "the helper child Work ran the admitted socket-mode check and closed its role with a validated Claim"
    );
    assert_ne!(
        settlement["proves"]["statement"].as_str().unwrap(),
        claim_text
    );
    assert_eq!(
        settled["claim"].as_str().unwrap(),
        format!("recorded claim: {claim_text}, unverified")
    );

    // AND EVERY NEARBY SHAPE, re-checked after the real close.
    for (label, id) in [
        ("no confirming child named", &no_confirmation),
        ("no obligation named", &no_obligation),
        (
            "a check edition the container never declared",
            &wrong_edition,
        ),
        (
            "a child finding that never settled the declared mechanism",
            &unsettled_confirmation,
        ),
        (
            "a settled verification from a Work holding no receipt here",
            &borrowed,
        ),
        ("a child finding that does not exist", &ghost),
    ] {
        let (code, result, stderr) = finding_cli(&estate, &["settle", "--finding", id, "--admin"]);
        assert_eq!(code, Some(0), "{stderr}");
        assert!(
            result["settled"].is_null(),
            "{label} must never settle, even after the container really closed: {result}"
        );
    }

    stop_wirkd(&estate, wirkd_child);
}

/// The independent review's executed C1, closed. Two container Routes —
/// different route id, different waypoint id, different repository,
/// different leaf command — that copy the same `verifies` object still
/// content-address to the **same** container basis, because a container
/// has no World of its own. That collision is real and is not what makes
/// the settlement honest: the rogue is refused because its helper never
/// settled the verification obligation the container declares as its
/// mechanism, and because this estate never admitted that helper's own
/// check as a mechanism of this obligation.
#[test]
fn a_colliding_container_basis_confers_no_authority_without_the_admitted_mechanism() {
    let dir = tempfile::tempdir().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    write_container_route(&estate, "honest_container", "outer", "echo a > a.md");
    write_container_route(
        &estate,
        "rogue_container",
        "rogue",
        "echo totally-unrelated > a.md",
    );
    write_child_check_route(&estate, "helper_check", "echo 0775 > socket-mode.txt");
    write_child_check_route(&estate, "rogue_check", "echo whatever > socket-mode.txt");
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let honest_repo = dir.path().join("honest-repo");
    init_repo(&honest_repo);
    let honest = submit(
        &estate,
        "honest_container",
        &honest_repo,
        &["demo:write", "helper:write"],
        None,
    )
    .unwrap();
    let rogue_repo = dir.path().join("rogue-repo");
    init_repo(&rogue_repo);
    let rogue = submit(
        &estate,
        "rogue_container",
        &rogue_repo,
        &["demo:write", "helper:write"],
        None,
    )
    .unwrap();

    let honest_basis = obligation_basis_for(&estate, &honest.work_id, "outer");
    let rogue_basis = obligation_basis_for(&estate, &rogue.work_id, "rogue");
    assert_eq!(
        honest_basis, rogue_basis,
        "a container has no World, so its basis is prose plus outcome contract and \
         two unrelated Routes can collide on it — this test is about what that collision \
         is worth, not about preventing it"
    );

    write_file(&honest_repo, "a.md", "a\n");
    claim_ok(&estate, &honest.work_id, &honest.run_id, "a.md=a.md");
    write_file(&rogue_repo, "a.md", "totally-unrelated\n");
    claim_ok(&estate, &rogue.work_id, &rogue.run_id, "a.md=a.md");

    // Discover both children's check bases, then admit ONLY the honest
    // helper's check as the container obligation's mechanism.
    let honest_helper_repo = dir.path().join("honest-helper");
    init_repo(&honest_helper_repo);
    let honest_helper = submit(
        &estate,
        "helper_check",
        &honest_helper_repo,
        &["helper:write"],
        Some(ParentRef {
            work: &honest.work_id,
            waypoint: "outer",
            run: &honest.run_id,
            role: "helper",
            attempt: None,
        }),
    )
    .unwrap();
    let honest_check_basis = obligation_basis_for(&estate, &honest_helper.work_id, "check");

    let rogue_helper_repo = dir.path().join("rogue-helper");
    init_repo(&rogue_helper_repo);
    let rogue_helper = submit(
        &estate,
        "rogue_check",
        &rogue_helper_repo,
        &["helper:write"],
        Some(ParentRef {
            work: &rogue.work_id,
            waypoint: "rogue",
            run: &rogue.run_id,
            role: "helper",
            attempt: None,
        }),
    )
    .unwrap();
    let rogue_check_basis = obligation_basis_for(&estate, &rogue_helper.work_id, "check");
    assert_ne!(
        honest_check_basis, rogue_check_basis,
        "the mechanism is a real Deterministic World, so a different check command \
         is a different admitted basis"
    );

    write_child_chain_policy(
        &estate,
        &[(
            "socket-mode-investigated",
            "1",
            &honest_basis,
            &[&honest_check_basis],
        )],
        &[&honest_check_basis],
    );

    // Both helpers run their own check and try to settle their own
    // verification Finding.
    let mut child_findings = Vec::new();
    for (helper, repo, parent_work, expect_settled) in [
        (&honest_helper, &honest_helper_repo, &honest, true),
        (&rogue_helper, &rogue_helper_repo, &rogue, false),
    ] {
        write_file(repo, "socket-mode.txt", "x\n");
        claim_ok(
            &estate,
            &helper.work_id,
            &helper.run_id,
            "socket-mode.txt=socket-mode.txt",
        );
        let claim = claim_event_id(&estate, &helper.work_id);
        let report_run = status(&pointer.socket, &helper.work_id)["run_id"]
            .as_str()
            .unwrap()
            .to_string();
        let (code, raised, stderr) = raise_cli(
            &estate,
            &helper.work_id,
            &report_run,
            &[
                "--kind",
                "verified_outcome",
                "--scope",
                "work_local",
                "--claim",
                "the socket-mode check ran",
                "--evidence",
                &format!("work/{}/event/{claim}", helper.work_id),
                "--obligation",
                "socket-mode-checked@1",
            ],
        );
        assert_eq!(code, Some(0), "{stderr}");
        assert_eq!(
            raised["settled"].is_object(),
            expect_settled,
            "only the admitted check settles: {raised}"
        );
        child_findings.push((
            helper.work_id.clone(),
            raised["id"].as_str().unwrap().to_string(),
            parent_work.work_id.clone(),
            report_run,
            repo.clone(),
        ));
    }

    // Each parent raises its Finding while still open.
    let mut parent_findings = Vec::new();
    for (child_work, child_finding, parent_work, _, _) in &child_findings {
        let parent_run = if parent_work == &honest.work_id {
            honest.run_id.clone()
        } else {
            rogue.run_id.clone()
        };
        let (code, raised, stderr) = raise_cli(
            &estate,
            parent_work,
            &parent_run,
            &[
                "--kind",
                "contradicted_assumption",
                "--scope",
                "estate_local",
                "--claim",
                "the socket is bound with a wider mode than the trust boundary states",
                "--evidence",
                &format!(
                    "work/{parent_work}/event/{}",
                    submitted_event_id(&estate, parent_work)
                ),
                "--obligation",
                "socket-mode-investigated@1",
                "--confirmed-by",
                &format!("work/{child_work}/finding/{child_finding}"),
            ],
        );
        assert_eq!(code, Some(0), "{stderr}");
        parent_findings.push((
            parent_work.clone(),
            raised["id"].as_str().unwrap().to_string(),
        ));
    }

    // Both helpers complete; both containers close on real receipts.
    for (child_work, _, _, report_run, repo) in &child_findings {
        write_file(repo, "report.md", "done\n");
        claim_ok(&estate, child_work, report_run, "report.md=report.md");
        assert_eq!(state_of(&pointer.socket, child_work), "completed");
    }

    for (parent_work, finding) in &parent_findings {
        let (code, result, stderr) =
            finding_cli(&estate, &["settle", "--finding", finding, "--admin"]);
        assert_eq!(code, Some(0), "{stderr}");
        if parent_work == &honest.work_id {
            assert!(
                result["settled"].is_object(),
                "the honest container, whose helper ran the admitted check, settles: {result}"
            );
            assert_eq!(
                result["settled"]["check"]["obligated_roles"][0]["mechanism_basis"]
                    .as_str()
                    .unwrap(),
                honest_check_basis
            );
        } else {
            assert!(
                result["settled"].is_null(),
                "a rogue container colliding on basis, whose helper ran an unadmitted check, \
                 must never settle: {result}"
            );
        }
    }

    stop_wirkd(&estate, wirkd_child);
}

/// The independent review's executed C2, closed: every obligated role in
/// `VerificationObligation.outputs` must have closed, in this
/// container's current activation, with its own settled mechanism.
/// `outputs: ["auditor"]` discharged through role `scribe` — while
/// `auditor` never existed — is refused; the real auditor discharges;
/// and an obligation naming *both* roles is not discharged by one of
/// them.
#[test]
fn every_obligated_container_role_must_close_with_its_own_settled_mechanism() {
    let dir = tempfile::tempdir().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    // `auditor` is *not* required by the outcome contract and `scribe`
    // is — so the container can close without an auditor, which is
    // exactly how the obligation used to be credited to the wrong role.
    let route = |name: &str, outputs: &str| {
        route_fixture::write_route(
            estate.as_path(),
            name,
            &format!(
                r#"{{"id":"{name}","waypoints":[
                {{"id":"outer","kind":"Container",
                 "declared_outputs":[{{"name":"a.md","required":true}}],
                 "required_child_outcomes":[{{"role":"auditor","required":false}},{{"role":"scribe","required":true}}],
                 "verifies":{{"id":"audit-performed","edition":"1",
                   "proves":"the auditor child Work ran the admitted audit check and closed the auditor role",
                   "outputs":{outputs},
                   "requires":{{"id":"socket-mode-checked","edition":"1"}}}},
                 "leaves":[{{"id":"outer/leaf-a","kind":"Deterministic","command":["sh","-c","echo a > a.md"],
                   "declared_outputs":[{{"name":"a.md","required":true}}]}}]}}
            ]}}"#
            ),
        );
    };
    route("auditor_only", r#"["auditor"]"#);
    route("both_roles", r#"["auditor","scribe"]"#);
    write_child_check_route(&estate, "helper_check", "echo 0775 > socket-mode.txt");
    let (wirkd_child, pointer) = start_wirkd(&estate);

    // `scribe` is required by the outcome contract, so every case runs a
    // scribe child — otherwise the container never closes and nothing is
    // being tested about the obligation at all. What varies is which
    // roles the *obligation* names, and which of them actually ran.
    let run_case = |route_name: &str, tag: &str, roles: &[&str]| {
        let parent_repo = dir.path().join(format!("{tag}-parent"));
        init_repo(&parent_repo);
        let parent = submit(
            &estate,
            route_name,
            &parent_repo,
            &["demo:write", "aux:write"],
            None,
        )
        .unwrap();
        let basis = obligation_basis_for(&estate, &parent.work_id, "outer");
        write_file(&parent_repo, "a.md", "a\n");
        claim_ok(&estate, &parent.work_id, &parent.run_id, "a.md=a.md");

        // Submit every role's child first, so each one's own check World
        // is reserved and inspectable before the operator admits
        // anything — each child has its own repository and therefore its
        // own real content basis.
        let mut children = Vec::new();
        for role in roles {
            let repo = dir.path().join(format!("{tag}-{role}"));
            init_repo(&repo);
            let child = submit(
                &estate,
                "helper_check",
                &repo,
                &["aux:write"],
                Some(ParentRef {
                    work: &parent.work_id,
                    waypoint: "outer",
                    run: &parent.run_id,
                    role,
                    attempt: None,
                }),
            )
            .unwrap();
            let check_basis = obligation_basis_for(&estate, &child.work_id, "check");
            children.push((role.to_string(), child, check_basis, repo));
        }
        let bases: Vec<&str> = children.iter().map(|entry| entry.2.as_str()).collect();
        write_child_chain_policy(&estate, &[("audit-performed", "1", &basis, &bases)], &bases);

        // Every child runs the admitted check and settles its own
        // verification.
        let mut settled_children = Vec::new();
        for (role, child, _, repo) in &children {
            write_file(repo, "socket-mode.txt", "0775\n");
            claim_ok(
                &estate,
                &child.work_id,
                &child.run_id,
                "socket-mode.txt=socket-mode.txt",
            );
            let claim = claim_event_id(&estate, &child.work_id);
            let report_run = status(&pointer.socket, &child.work_id)["run_id"]
                .as_str()
                .unwrap()
                .to_string();
            let (code, raised, stderr) = raise_cli(
                &estate,
                &child.work_id,
                &report_run,
                &[
                    "--kind",
                    "verified_outcome",
                    "--scope",
                    "work_local",
                    "--claim",
                    "the check ran",
                    "--evidence",
                    &format!("work/{}/event/{claim}", child.work_id),
                    "--obligation",
                    "socket-mode-checked@1",
                ],
            );
            assert_eq!(code, Some(0), "{stderr}");
            assert!(
                raised["settled"].is_object(),
                "the {role} child's own verification settles: {raised}"
            );
            settled_children.push((
                child.work_id.clone(),
                raised["id"].as_str().unwrap().to_string(),
                report_run,
                repo.clone(),
            ));
        }

        // The parent names the first child as its confirmation.
        let (cited_work, cited_finding) =
            (settled_children[0].0.clone(), settled_children[0].1.clone());
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
                "the audit found a problem",
                "--evidence",
                &format!(
                    "work/{}/event/{}",
                    parent.work_id,
                    submitted_event_id(&estate, &parent.work_id)
                ),
                "--obligation",
                "audit-performed@1",
                "--confirmed-by",
                &format!("work/{cited_work}/finding/{cited_finding}"),
            ],
        );
        assert_eq!(code, Some(0), "{stderr}");
        let parent_finding = raised["id"].as_str().unwrap().to_string();

        for (child_work, _, report_run, repo) in &settled_children {
            write_file(repo, "report.md", "done\n");
            claim_ok(&estate, child_work, report_run, "report.md=report.md");
        }
        assert_eq!(
            state_of(&pointer.socket, &parent.work_id),
            "completed",
            "the required scribe role closes the container in every case"
        );
        let (code, result, stderr) = finding_cli(
            &estate,
            &["settle", "--finding", &parent_finding, "--admin"],
        );
        assert_eq!(code, Some(0), "{stderr}");
        result
    };

    // C2 verbatim: the obligation names `auditor`, only `scribe` ever
    // ran, and the container closed anyway because `auditor` is not
    // required by its outcome contract.
    let refused = run_case("auditor_only", "wrong-role", &["scribe"]);
    assert!(
        refused["settled"].is_null(),
        "an obligation naming the auditor role is never discharged by a scribe: {refused}"
    );

    // The real auditor, same obligation, same verb.
    let settled = run_case("auditor_only", "right-role", &["auditor", "scribe"]);
    assert!(
        settled["settled"].is_object(),
        "the obligated role's own child discharges it: {settled}"
    );
    assert_eq!(
        settled["settled"]["check"]["obligated_roles"][0]["role"]
            .as_str()
            .unwrap(),
        "auditor"
    );

    // Two obligated roles, only the required one closed: partial
    // completion is not full proof.
    let partial = run_case("both_roles", "partial", &["scribe"]);
    assert!(
        partial["settled"].is_null(),
        "an obligation naming two roles is not discharged by one of them: {partial}"
    );

    // Both obligated roles closed, each with its own settled mechanism.
    let both = run_case("both_roles", "both", &["auditor", "scribe"]);
    assert!(
        both["settled"].is_object(),
        "both obligated roles closed with their own settled mechanisms: {both}"
    );
    let roles = both["settled"]["check"]["obligated_roles"]
        .as_array()
        .unwrap();
    assert_eq!(roles.len(), 2);
    assert_eq!(roles[0]["role"].as_str().unwrap(), "auditor");
    assert_eq!(roles[1]["role"].as_str().unwrap(), "scribe");

    stop_wirkd(&estate, wirkd_child);
}

// ---- 5b. actor-reviewed: the agentic verification mechanism -----------

/// The bounded review contract these cases declare. `recipe` is the
/// procedure and its edition; `targets` are the exact resource paths the
/// review must have applied to; `decisions` is the closed set of
/// structured outcomes it may return. All three are hashed into the
/// obligation's basis, so changing any of them is a fresh admission.
const REVIEW_CONTRACT: &str = r#""review":{"recipe":"socket-boundary-review@3","targets":[{"source":"demo","path":"socket.rs"}],"decisions":["contradicted_assumption","verified_outcome"]}"#;

/// A two-Waypoint Actor Route: an obliged `review` Waypoint, then a
/// trailing `file` Waypoint the reviewer raises its Finding from (raising
/// needs a current, non-terminal Run, and claiming `review` would
/// otherwise complete the Work).
fn write_actor_review_route(estate: &Path, name: &str, intent: &str, outputs: &str) {
    route_fixture::write_route(
        estate,
        name,
        &format!(
            r#"{{"id":"{name}","waypoints":[
            {{"id":"review","kind":"Actor","intent":"{intent}",
             "declared_outputs":[{{"name":"review.md","required":true}}],
             "boundary":["**"],
             "verifies":{{"id":"socket-mode-reviewed","edition":"1",
               "proves":"an admitted independent review of the socket boundary was performed under recipe socket-boundary-review@3 and recorded a declared decision",
               "outputs":{outputs},
               {REVIEW_CONTRACT}}}}},
            {{"id":"file","kind":"Actor","intent":"file the reviewer's own finding",
             "declared_outputs":[{{"name":"done.md","required":true}}],"boundary":["**"]}}
        ]}}"#
        ),
    );
}

/// Registers `demo` in Atlas from a real Git repository and returns an
/// exact coordinate for `marker` — the shape `finding raise --applies-to`
/// admits, and the shape a declared review target must be covered by.
fn publish_and_locate(estate: &Path, repo: &Path, marker: &str) -> String {
    publish_and_locate_as(estate, repo, "demo", marker)
}

/// The same, under an explicitly named source alias — so a test can hold
/// two admitted sources that both carry a file at the same path, which is
/// the shape the target-substitution attack needs.
fn publish_and_locate_as(estate: &Path, repo: &Path, source: &str, marker: &str) -> String {
    let git = |args: &[&str]| {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(repo)
                .status()
                .unwrap()
                .success()
        );
    };
    git(&["add", "."]);
    git(&[
        "-c",
        "user.email=wb@example.test",
        "-c",
        "user.name=wb",
        "commit",
        "-q",
        "-m",
        "sources",
    ]);
    let (ok, acquired, err) = atlas(
        estate,
        &[
            "acquire",
            "--source",
            source,
            "--repository",
            repo.to_str().unwrap(),
            "--revision",
            "HEAD",
        ],
    );
    assert!(ok, "{err}");
    let generation = acquired["generation"]["generation"]
        .as_str()
        .unwrap()
        .to_string();
    let (ok, _, err) = atlas(
        estate,
        &["publish", "--source", source, "--generation", &generation],
    );
    assert!(ok, "{err}");
    let (ok, search, err) = atlas(estate, &["search", "--source", source, "--query", marker]);
    assert!(ok, "{err}");
    search["hits"][0]["coordinate"]
        .as_str()
        .unwrap_or_else(|| panic!("no hit for {marker} in {source}: {search}"))
        .to_string()
}

/// A settlement policy admitting one `actor_reviewed` obligation.
fn write_review_policy(estate: &Path, kinds: &str, id: &str, edition: &str, basis: &str) {
    write_policy(
        estate,
        &format!(
            r#"{{"version":2,"classes":[{{"class":"actor_reviewed","scope":"work_local","kinds":{kinds},"obligations":[{{"id":"{id}","edition":"{edition}","basis":"{basis}"}}]}}]}}"#
        ),
    );
}

/// The agentic mechanism, end to end, against the real daemon, real Git,
/// real Atlas and the real CLI — with the Actor Run driven the way the
/// landed model-free harness drives one (`materialize_actor`: a real
/// `git worktree add` plus the two journal writes a pane launch would
/// have made). **No model is invoked anywhere in this test.**
///
/// The frozen `fad933dd` refused exactly this
/// (`loop-b-agentic-proof/raw/00-red-actor-review-refused.txt`): a real,
/// materialized, claimed, target-applied review for which
/// `obligation_basis` returned `None`, so the operator had no value to
/// admit and nothing could settle.
///
/// What the positive proves, and what it deliberately does not, is
/// asserted here rather than only documented: the settled record carries
/// the reviewing World hash, its actual `intent`, the recipe, the exact
/// reviewed target at its exact generation and object id, the obligated
/// report receipt, and the structured decision — plus a `standing` line
/// saying in the record itself that whether the review's conclusion is
/// true remains judgement.
#[test]
fn an_admitted_actor_review_discharges_its_obligation_and_refuses_every_nearby_shape() {
    let dir = tempfile::tempdir().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    let intent = "Independently review whether bind_socket restricts the wirkd socket mode, and record the decision";
    write_actor_review_route(&estate, "socket_review", intent, r#"["review.md"]"#);
    // The same Route with an obligated report the Claim will never carry:
    // a generic validated Done is not a review report.
    write_actor_review_route(&estate, "missing_report", intent, r#"["absent.md"]"#);
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let repo = dir.path().join("demo-repo");
    init_repo(&repo);
    write_file(&repo, "socket.rs", "fn bind_socket() { socketmarker }\n");
    write_file(&repo, "other.rs", "fn unrelated() { othermarker }\n");
    let target_coordinate = publish_and_locate(&estate, &repo, "socketmarker");
    let (ok, search, err) = atlas(
        &estate,
        &["search", "--source", "demo", "--query", "othermarker"],
    );
    assert!(ok, "{err}");
    let unrelated_coordinate = search["hits"][0]["coordinate"]
        .as_str()
        .unwrap()
        .to_string();

    let work = submit_kind(
        &estate,
        "socket_review",
        &repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .unwrap();
    assert_eq!(work.waypoint, "review");

    // The review really runs: a real checkout, a real report, a real
    // Validated Done Claim.
    let worktree = materialize_actor(&pointer.socket, &estate, &work.work_id, &work.run_id);
    fs::write(
        worktree.join("review.md"),
        "recipe socket-boundary-review@3\ndecision: contradicted_assumption\ntarget: socket.rs\nbind_socket never narrows the socket mode.\n",
    )
    .unwrap();
    claim_ok(&estate, &work.work_id, &work.run_id, "review.md=review.md");

    let basis = obligation_basis_for(&estate, &work.work_id, "review");
    let world_hash = journal_events(&estate, &work.work_id)
        .into_iter()
        .rev()
        .find_map(|event| match event.kind {
            EventKind::WaypointReserved {
                waypoint,
                world_hash,
                ..
            } if waypoint.0 == "review" => Some(world_hash.0),
            _ => None,
        })
        .expect("the reviewing World was reserved");
    write_review_policy(
        &estate,
        r#"["contradicted_assumption","verified_outcome"]"#,
        "socket-mode-reviewed",
        "1",
        &basis,
    );

    let claim_event = claim_event_id(&estate, &work.work_id);
    let evidence = format!("work/{}/event/{claim_event}", work.work_id);
    let file_run = status(&pointer.socket, &work.work_id)["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    let raise = |args: &[&str]| {
        let mut full = vec!["--scope", "work_local", "--evidence", &evidence];
        full.extend_from_slice(args);
        let (code, raised, stderr) = raise_cli(&estate, &work.work_id, &file_run, &full);
        assert_eq!(code, Some(0), "{stderr}");
        raised
    };

    // THE POSITIVE.
    let settled = raise(&[
        "--kind",
        "contradicted_assumption",
        "--claim",
        "the socket is bound with a wider mode than the trust boundary states",
        "--applies-to",
        &target_coordinate,
        "--obligation",
        "socket-mode-reviewed@1",
    ]);
    assert!(
        settled["settled"].is_object(),
        "an admitted independent review discharges its own obligation: {settled}"
    );
    let settlement = &settled["settled"];
    assert_eq!(
        settlement["authority"]["policy"]["class"].as_str().unwrap(),
        "actor_reviewed",
        "a distinct mechanism, never a deterministic check in disguise"
    );
    let proves = &settlement["proves"];
    assert!(proves["recorded"].as_bool().unwrap());
    assert_eq!(
        proves["statement"].as_str().unwrap(),
        "an admitted independent review of the socket boundary was performed under recipe socket-boundary-review@3 and recorded a declared decision"
    );
    assert!(
        proves["standing"]
            .as_str()
            .unwrap()
            .contains("judgement, not proof"),
        "the record says what it leaves as judgement: {proves}"
    );
    let discharged = &proves["discharged_by"];
    assert_eq!(discharged["kind"].as_str().unwrap(), "actor_review");
    assert_eq!(discharged["world_hash"].as_str().unwrap(), world_hash);
    assert_eq!(discharged["intent"].as_str().unwrap(), intent);
    assert_eq!(
        discharged["recipe"].as_str().unwrap(),
        "socket-boundary-review@3"
    );
    assert_eq!(
        discharged["decision"].as_str().unwrap(),
        "contradicted_assumption",
        "the structured outcome, not the prose, is the decision"
    );
    let targets = discharged["targets"].as_array().unwrap();
    assert_eq!(targets.len(), 1);
    assert_eq!(
        targets[0]["selector"]["source"].as_str().unwrap(),
        "demo",
        "the record shows which source was asked for, not only a path"
    );
    assert_eq!(
        targets[0]["selector"]["path"].as_str().unwrap(),
        "socket.rs"
    );
    for field in [
        "estate",
        "membership",
        "source_id",
        "generation",
        "object_id",
    ] {
        assert!(
            !targets[0][field].as_str().unwrap().is_empty(),
            "the complete checked target identity is explicit in the proof, missing {field}: {targets:?}"
        );
    }
    let report = discharged["report"].as_array().unwrap();
    assert_eq!(report.len(), 1);
    assert_eq!(report[0]["name"].as_str().unwrap(), "review.md");
    assert!(report[0]["digest"].as_str().unwrap().len() >= 40);
    // The reviewer's own sentence keeps the standing every other class
    // gives it.
    assert!(
        settled["claim"]
            .as_str()
            .unwrap()
            .starts_with("recorded claim: "),
    );

    // EVERY NEARBY SHAPE, through the same verb on the same open Work.
    let nearby: Vec<(&str, serde_json::Value)> = vec![
        (
            "a decision outside the declared closed set",
            raise(&[
                "--kind",
                "gap",
                "--claim",
                "a decision this recipe never allows",
                "--applies-to",
                &target_coordinate,
                "--obligation",
                "socket-mode-reviewed@1",
            ]),
        ),
        (
            "no declared target applied to at all",
            raise(&[
                "--kind",
                "contradicted_assumption",
                "--claim",
                "a review that applied to nothing",
                "--obligation",
                "socket-mode-reviewed@1",
            ]),
        ),
        (
            "unrelated admitted evidence instead of the declared target",
            raise(&[
                "--kind",
                "contradicted_assumption",
                "--claim",
                "a review pointed at some other file",
                "--applies-to",
                &unrelated_coordinate,
                "--obligation",
                "socket-mode-reviewed@1",
            ]),
        ),
        (
            "naming no obligation",
            raise(&[
                "--kind",
                "contradicted_assumption",
                "--claim",
                "the same sentence, naming nothing",
                "--applies-to",
                &target_coordinate,
            ]),
        ),
        (
            "a check edition the Waypoint never declared",
            raise(&[
                "--kind",
                "contradicted_assumption",
                "--claim",
                "the right recipe at the wrong edition",
                "--applies-to",
                &target_coordinate,
                "--obligation",
                "socket-mode-reviewed@2",
            ]),
        ),
    ];
    for (label, raised) in &nearby {
        assert!(
            raised["settled"].is_null(),
            "{label} must never settle: {raised}"
        );
    }

    // An obligation this estate admitted at a different basis: the review
    // is identical, the operator admitted some other World.
    write_review_policy(
        &estate,
        r#"["contradicted_assumption","verified_outcome"]"#,
        "socket-mode-reviewed",
        "1",
        &"0".repeat(64),
    );
    let unadmitted = raise(&[
        "--kind",
        "contradicted_assumption",
        "--claim",
        "an identical review the estate admitted a different World for",
        "--applies-to",
        &target_coordinate,
        "--obligation",
        "socket-mode-reviewed@1",
    ]);
    assert!(unadmitted["settled"].is_null(), "{unadmitted}");
    let (_, pending, _) = finding_cli(
        &estate,
        &[
            "settle",
            "--finding",
            unadmitted["id"].as_str().unwrap(),
            "--admin",
        ],
    );
    assert_eq!(
        pending["pending"]["reason"].as_str().unwrap(),
        "obligation-basis-not-admitted",
        "the obligation is admitted by name; it is the reviewing World that is not — \
         and the pending reply says which of the two it is"
    );

    // Every refusal above is still a refusal once the estate admits the
    // real basis again: none of them was merely waiting for policy.
    write_review_policy(
        &estate,
        r#"["contradicted_assumption","verified_outcome"]"#,
        "socket-mode-reviewed",
        "1",
        &basis,
    );
    for (label, raised) in &nearby {
        let (code, result, stderr) = finding_cli(
            &estate,
            &[
                "settle",
                "--finding",
                raised["id"].as_str().unwrap(),
                "--admin",
            ],
        );
        assert_eq!(code, Some(0), "{stderr}");
        assert!(
            result["settled"].is_null(),
            "{label} must still never settle under the real policy: {result}"
        );
    }

    stop_wirkd(&estate, wirkd_child);

    // A generic validated `Done` is not a review report: the same Route
    // shape, obliging an output the Claim never carried.
    let dir2 = tempfile::tempdir().unwrap();
    let estate2 = dir2.path().join("estate");
    fs::create_dir_all(&estate2).unwrap();
    write_actor_review_route(&estate2, "missing_report", intent, r#"["absent.md"]"#);
    let (wirkd_child2, pointer2) = start_wirkd(&estate2);
    let repo2 = dir2.path().join("demo-repo");
    init_repo(&repo2);
    write_file(&repo2, "socket.rs", "fn bind_socket() { socketmarker }\n");
    let coordinate2 = publish_and_locate(&estate2, &repo2, "socketmarker");
    let work2 = submit_kind(
        &estate2,
        "missing_report",
        &repo2,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .unwrap();
    let worktree2 = materialize_actor(&pointer2.socket, &estate2, &work2.work_id, &work2.run_id);
    fs::write(worktree2.join("review.md"), "a generic done\n").unwrap();
    claim_ok(
        &estate2,
        &work2.work_id,
        &work2.run_id,
        "review.md=review.md",
    );
    write_review_policy(
        &estate2,
        r#"["contradicted_assumption","verified_outcome"]"#,
        "socket-mode-reviewed",
        "1",
        &obligation_basis_for(&estate2, &work2.work_id, "review"),
    );
    let file_run2 = status(&pointer2.socket, &work2.work_id)["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    let (code, no_report, stderr) = raise_cli(
        &estate2,
        &work2.work_id,
        &file_run2,
        &[
            "--kind",
            "contradicted_assumption",
            "--scope",
            "work_local",
            "--claim",
            "a review whose obligated report never validated",
            "--evidence",
            &format!(
                "work/{}/event/{}",
                work2.work_id,
                claim_event_id(&estate2, &work2.work_id)
            ),
            "--applies-to",
            &coordinate2,
            "--obligation",
            "socket-mode-reviewed@1",
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        no_report["settled"].is_null(),
        "an obligated report the Claim never carried discharges nothing: {no_report}"
    );

    stop_wirkd(&estate2, wirkd_child2);
}

/// The independent re-review's executed C1, closed. An admitted review
/// of `demo`'s current `socket.rs` must not be discharged by admitted
/// evidence for a same-named file in a *different admitted repository*,
/// nor by an *earlier generation* of the same source the reviewing World
/// was never opened against.
///
/// Both attacks settled on `a063c629`
/// (`loop-b-target-binding/raw/00-red-c1-target-substitution.txt`,
/// reproduced here with this stage's own probe), because a declared
/// target was a bare path and `reviewed_targets` compared paths. The
/// declared target is now a selector that wirkd resolves and **freezes**
/// into the reserved World at reservation, `WorldHash::of` covers it, and
/// `obligation_basis` therefore binds it — so the estate admits *this*
/// target, and the reviewer's own admitted evidence must match it on
/// estate, membership, source, generation, path and object.
#[test]
fn a_review_target_binds_the_exact_admitted_source_generation_and_object() {
    let dir = tempfile::tempdir().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    let intent = "Independently review whether bind_socket restricts the wirkd socket mode";
    write_actor_review_route(&estate, "socket_review", intent, r#"["review.md"]"#);
    let (wirkd_child, pointer) = start_wirkd(&estate);

    // The repository the review is actually about, and an unrelated one
    // that happens to carry a file at the same path.
    let repo = dir.path().join("demo-repo");
    init_repo(&repo);
    write_file(&repo, "socket.rs", "fn bind_socket() { socketmarker_v1 }\n");
    let stale_coordinate = publish_and_locate(&estate, &repo, "socketmarker_v1");

    let decoy = dir.path().join("decoy-repo");
    init_repo(&decoy);
    write_file(&decoy, "socket.rs", "fn bind_socket() { decoymarker }\n");
    let decoy_coordinate = publish_and_locate_as(&estate, &decoy, "decoy", "decoymarker");

    // A second, later generation of the real source: the state the
    // review is actually carried out against, and the one the freeze
    // will pick up because it is `demo`'s current publication.
    write_file(&repo, "socket.rs", "fn bind_socket() { socketmarker_v2 }\n");
    let target_coordinate = publish_and_locate(&estate, &repo, "socketmarker_v2");
    assert_ne!(stale_coordinate, target_coordinate);

    // The reviewing Work is bound to BOTH sources, so both memberships
    // are admitted by its own bindings and both coordinates are
    // admissible evidence. Only one of them is the reviewed target.
    let work = submit_kind(
        &estate,
        "socket_review",
        &repo,
        &["demo:write", "decoy:read"],
        None,
        Some("actor"),
    )
    .unwrap();
    assert_eq!(work.waypoint, "review");

    // The freeze happened at reservation, before the review ran.
    let frozen = journal_events(&estate, &work.work_id)
        .into_iter()
        .rev()
        .find_map(|event| match event.kind {
            EventKind::WaypointReserved {
                waypoint,
                world: wirk_core::World::Actor(actor),
                ..
            } if waypoint.0 == "review" => Some(actor.review_targets),
            _ => None,
        })
        .expect("the reviewing World was reserved");
    assert_eq!(frozen.len(), 1, "one declared selector, one frozen target");
    assert_eq!(frozen[0].source, "demo");
    assert_eq!(frozen[0].path, "socket.rs");
    assert!(!frozen[0].generation.is_empty() && !frozen[0].object_id.is_empty());

    let worktree = materialize_actor(&pointer.socket, &estate, &work.work_id, &work.run_id);
    fs::write(
        worktree.join("review.md"),
        "recipe socket-boundary-review@3\ndecision: contradicted_assumption\n",
    )
    .unwrap();
    claim_ok(&estate, &work.work_id, &work.run_id, "review.md=review.md");

    let basis = obligation_basis_for(&estate, &work.work_id, "review");
    write_review_policy(
        &estate,
        r#"["contradicted_assumption","verified_outcome"]"#,
        "socket-mode-reviewed",
        "1",
        &basis,
    );

    let claim_event = claim_event_id(&estate, &work.work_id);
    let evidence = format!("work/{}/event/{claim_event}", work.work_id);
    let file_run = status(&pointer.socket, &work.work_id)["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    let raise = |claim: &str, applies_to: &str| {
        let (code, raised, stderr) = raise_cli(
            &estate,
            &work.work_id,
            &file_run,
            &[
                "--kind",
                "contradicted_assumption",
                "--scope",
                "work_local",
                "--evidence",
                &evidence,
                "--claim",
                claim,
                "--applies-to",
                applies_to,
                "--obligation",
                "socket-mode-reviewed@1",
            ],
        );
        assert_eq!(code, Some(0), "{stderr}");
        raised
    };

    // THE CONTROL POSITIVE: the generation actually reviewed.
    let settled = raise(
        "the socket boundary is wider than stated",
        &target_coordinate,
    );
    assert!(
        settled["settled"].is_object(),
        "the exact frozen target discharges: {settled}"
    );
    let recorded = &settled["settled"]["proves"]["discharged_by"]["targets"][0];
    assert_eq!(recorded["selector"]["source"].as_str().unwrap(), "demo");
    assert_eq!(
        recorded["generation"].as_str().unwrap(),
        frozen[0].generation
    );
    assert_eq!(recorded["object_id"].as_str().unwrap(), frozen[0].object_id);
    assert_eq!(
        recorded["membership"].as_str().unwrap(),
        frozen[0].membership
    );

    // ATTACK A: same path, a different admitted repository.
    let cross_source = raise(
        "a review of a socket.rs that was never the reviewed one",
        &decoy_coordinate,
    );
    assert!(
        cross_source["settled"].is_null(),
        "a same-path file in another admitted repository is not the reviewed target: {cross_source}"
    );

    // ATTACK B: same path and source, an earlier generation the
    // reviewing World was never opened against.
    let stale = raise(
        "a review of a generation the World never saw",
        &stale_coordinate,
    );
    assert!(
        stale["settled"].is_null(),
        "an earlier generation of the reviewed source is not the reviewed target: {stale}"
    );

    // Both refusals survive an explicit settle under the real policy, so
    // neither was merely waiting.
    for (label, raised) in [
        ("cross-source", &cross_source),
        ("stale generation", &stale),
    ] {
        let (code, result, stderr) = finding_cli(
            &estate,
            &[
                "settle",
                "--finding",
                raised["id"].as_str().unwrap(),
                "--admin",
            ],
        );
        assert_eq!(code, Some(0), "{stderr}");
        assert!(
            result["settled"].is_null(),
            "{label} must still never settle: {result}"
        );
    }

    stop_wirkd(&estate, wirkd_child);
}

/// The independent review's executed D1, closed end to end on the public
/// path it was found on.
///
/// The **bare** Actor submit — a Route whose first Waypoint is `Actor`,
/// submitted *without* `--kind actor`, which the CLI accepts — reserves a
/// World with `SourceBasis::Unknown`. On `8840ed9e` that World took the
/// pre-v2 `WorldHash::legacy` encoding, which never hashed the frozen
/// review targets, so the value the operator admitted did not bind the
/// reviewed target even though the review settled. This drives the same
/// public shape and asserts the binding now holds there, alongside the
/// v2 path that already had it.
///
/// No model is invoked: the Actor Run is driven by the landed model-free
/// path, and this Waypoint's own checkout is the estate root, exactly as
/// the bare shape reserves it.
#[test]
fn a_bare_actor_review_binds_its_frozen_target_and_refuses_substitution() {
    let dir = tempfile::tempdir().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    // The bare Actor World's own `worktree_path` is the estate root, so
    // the estate must be a real Git checkout for its Claim to validate —
    // the ordinary case, and what makes this shape reachable at all.
    init_repo(&estate);
    let intent = "Independently review whether bind_socket restricts the wirkd socket mode";
    write_actor_review_route(&estate, "bare_review", intent, r#"["review.md"]"#);
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let repo = dir.path().join("demo-repo");
    init_repo(&repo);
    write_file(&repo, "socket.rs", "fn bind_socket() { socketmarker_v1 }\n");
    let stale_coordinate = publish_and_locate(&estate, &repo, "socketmarker_v1");

    let decoy = dir.path().join("decoy-repo");
    init_repo(&decoy);
    write_file(&decoy, "socket.rs", "fn bind_socket() { decoymarker }\n");
    let decoy_coordinate = publish_and_locate_as(&estate, &decoy, "decoy", "decoymarker");

    write_file(&repo, "socket.rs", "fn bind_socket() { socketmarker_v2 }\n");
    let target_coordinate = publish_and_locate(&estate, &repo, "socketmarker_v2");

    // The bare submit: no `--kind actor`, so the reserved World's basis
    // is `Unknown` — the arm that used to lose the binding.
    let work = submit(
        &estate,
        "bare_review",
        &repo,
        &["demo:write", "decoy:read"],
        None,
    )
    .unwrap();
    assert_eq!(work.waypoint, "review");

    let (world_hash, reserved) = journal_events(&estate, &work.work_id)
        .into_iter()
        .rev()
        .find_map(|event| match event.kind {
            EventKind::WaypointReserved {
                waypoint,
                world_hash,
                world: wirk_core::World::Actor(actor),
            } if waypoint.0 == "review" => Some((world_hash, actor)),
            _ => None,
        })
        .expect("the reviewing World was reserved");
    assert_eq!(
        reserved.source_basis,
        wirk_core::SourceBasis::Unknown,
        "this is the bare shape D1 was found on, not the v2 one"
    );
    assert_eq!(reserved.review_targets.len(), 1, "the target really froze");

    // The decisive assertion: the journaled hash is *not* the pre-v2
    // encoding of this World, so the frozen target is inside the value
    // the operator admits. On the rejected candidate these were equal.
    let as_legacy =
        wirk_core::WorldHash::legacy_for_tests(&wirk_core::World::Actor(reserved.clone()));
    assert_ne!(
        world_hash, as_legacy,
        "an Unknown-basis World carrying frozen targets must not hash as a pre-v2 World"
    );
    let mut without = reserved.clone();
    without.review_targets = Vec::new();
    assert_eq!(
        wirk_core::WorldHash::of(&wirk_core::World::Actor(without.clone())),
        wirk_core::WorldHash::legacy_for_tests(&wirk_core::World::Actor(without)),
        "and the same World with no frozen targets still keeps its historical encoding"
    );

    // The review really runs and really claims, on the estate checkout
    // this shape reserves.
    fs::write(
        estate.join("review.md"),
        "recipe socket-boundary-review@3\ndecision: contradicted_assumption\n",
    )
    .unwrap();
    claim_ok(&estate, &work.work_id, &work.run_id, "review.md=review.md");

    let basis = obligation_basis_for(&estate, &work.work_id, "review");
    write_review_policy(
        &estate,
        r#"["contradicted_assumption","verified_outcome"]"#,
        "socket-mode-reviewed",
        "1",
        &basis,
    );

    let evidence = format!(
        "work/{}/event/{}",
        work.work_id,
        claim_event_id(&estate, &work.work_id)
    );
    let file_run = status(&pointer.socket, &work.work_id)["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    let raise = |claim: &str, applies_to: &str| {
        let (code, raised, stderr) = raise_cli(
            &estate,
            &work.work_id,
            &file_run,
            &[
                "--kind",
                "contradicted_assumption",
                "--scope",
                "work_local",
                "--evidence",
                &evidence,
                "--claim",
                claim,
                "--applies-to",
                applies_to,
                "--obligation",
                "socket-mode-reviewed@1",
            ],
        );
        assert_eq!(code, Some(0), "{stderr}");
        raised
    };

    // The legitimate bare-Actor positive.
    let settled = raise(
        "the socket boundary is wider than stated",
        &target_coordinate,
    );
    assert!(
        settled["settled"].is_object(),
        "the bare public Actor review still discharges its obligation: {settled}"
    );
    let recorded = &settled["settled"]["proves"]["discharged_by"]["targets"][0];
    assert_eq!(recorded["selector"]["source"].as_str().unwrap(), "demo");
    assert_eq!(
        recorded["generation"].as_str().unwrap(),
        reserved.review_targets[0].generation
    );

    // And the two C1 substitutions stay refused on this path too.
    for (label, coordinate) in [
        ("another admitted repository", &decoy_coordinate),
        ("an earlier generation", &stale_coordinate),
    ] {
        let refused = raise(&format!("a review of {label}"), coordinate);
        assert!(
            refused["settled"].is_null(),
            "{label} must never discharge on the bare path either: {refused}"
        );
    }

    stop_wirkd(&estate, wirkd_child);
}

/// The review's L1: an operator who has admitted the basis and whose
/// Route selector never resolved used to be told only
/// `no-admitted-check-holds-yet`. The reason now names the cause, and
/// names nothing else — no source alias, path, membership or generation.
#[test]
fn an_unresolved_review_selector_says_so_without_disclosing_the_selector() {
    let dir = tempfile::tempdir().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    // The declared selector names a source this estate has never
    // registered, so the freeze resolves nothing.
    route_fixture::write_route(
        &estate,
        "unresolvable_review",
        r#"{"id":"unresolvable-review","waypoints":[
        {"id":"review","kind":"Actor","intent":"review something that is not there",
         "declared_outputs":[{"name":"review.md","required":true}],"boundary":["**"],
         "verifies":{"id":"socket-mode-reviewed","edition":"1",
           "proves":"an admitted independent review was performed",
           "outputs":["review.md"],
           "review":{"recipe":"r@1","targets":[{"source":"nowhere","path":"socket.rs"}],
             "decisions":["contradicted_assumption"]}}},
        {"id":"file","kind":"Actor","intent":"file the finding",
         "declared_outputs":[{"name":"done.md","required":true}],"boundary":["**"]}
    ]}"#,
    );
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let repo = dir.path().join("demo-repo");
    init_repo(&repo);
    write_file(&repo, "socket.rs", "fn bind_socket() { socketmarker }\n");
    let coordinate = publish_and_locate(&estate, &repo, "socketmarker");

    let work = submit_kind(
        &estate,
        "unresolvable_review",
        &repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .unwrap();
    let reserved = journal_events(&estate, &work.work_id)
        .into_iter()
        .rev()
        .find_map(|event| match event.kind {
            EventKind::WaypointReserved {
                waypoint,
                world: wirk_core::World::Actor(actor),
                ..
            } if waypoint.0 == "review" => Some(actor.review_targets),
            _ => None,
        })
        .expect("reserved");
    assert!(reserved.is_empty(), "nothing resolved: {reserved:?}");

    let worktree = materialize_actor(&pointer.socket, &estate, &work.work_id, &work.run_id);
    fs::write(worktree.join("review.md"), "decision: contradicted\n").unwrap();
    claim_ok(&estate, &work.work_id, &work.run_id, "review.md=review.md");
    // The operator admits the basis this Work actually reserved, so the
    // pending reason cannot be about admission.
    write_review_policy(
        &estate,
        r#"["contradicted_assumption"]"#,
        "socket-mode-reviewed",
        "1",
        &obligation_basis_for(&estate, &work.work_id, "review"),
    );
    let file_run = status(&pointer.socket, &work.work_id)["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    let (code, raised, stderr) = raise_cli(
        &estate,
        &work.work_id,
        &file_run,
        &[
            "--kind",
            "contradicted_assumption",
            "--scope",
            "work_local",
            "--evidence",
            &format!(
                "work/{}/event/{}",
                work.work_id,
                claim_event_id(&estate, &work.work_id)
            ),
            "--claim",
            "a review whose declared selector never resolved",
            "--applies-to",
            &coordinate,
            "--obligation",
            "socket-mode-reviewed@1",
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert!(raised["settled"].is_null(), "{raised}");

    let (code, pending, stderr) = finding_cli(
        &estate,
        &[
            "settle",
            "--finding",
            raised["id"].as_str().unwrap(),
            "--admin",
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let reason = pending["pending"]["reason"].as_str().unwrap();
    assert_eq!(reason, "review-targets-unresolved");
    assert!(
        !reason.contains("nowhere") && !reason.contains("socket.rs"),
        "the reason names the cause and discloses nothing about the selector: {reason}"
    );
    // The seventh rung of the readiness ladder, in the one shape that
    // reaches it — and `work obligations` names it identically. The
    // other six are walked by
    // `the_readiness_reason_ladder_is_walked_end_to_end_and_both_verbs_agree`,
    // which cannot reach this one: it needs an Actor obligation with a
    // `review` contract (F4).
    assert_eq!(
        obligations_reason(
            &estate,
            &work.work_id,
            "review",
            raised["id"].as_str().unwrap()
        ),
        "review-targets-unresolved",
        "both verbs must name the identical reason at this rung too"
    );

    stop_wirkd(&estate, wirkd_child);
}

/// The frozen target is part of the reviewing World, so it moves the
/// admitted basis: a review of a *newer generation* of the same source
/// and the same path is a different World hash, a different obligation
/// basis, and requires the operator to admit it afresh. The declared
/// selector is unchanged throughout — this is exactly the case a path
/// binding could not express.
#[test]
fn a_newer_reviewed_generation_is_a_different_basis_and_needs_fresh_admission() {
    let dir = tempfile::tempdir().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    let intent = "Independently review whether bind_socket restricts the wirkd socket mode";
    write_actor_review_route(&estate, "socket_review", intent, r#"["review.md"]"#);
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let repo = dir.path().join("demo-repo");
    init_repo(&repo);
    write_file(&repo, "socket.rs", "fn bind_socket() { socketmarker_v1 }\n");
    publish_and_locate(&estate, &repo, "socketmarker_v1");

    let first = submit_kind(
        &estate,
        "socket_review",
        &repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .unwrap();
    let first_basis = obligation_basis_for(&estate, &first.work_id, "review");

    // The source moves on and is republished; a second review of the
    // same selector is reserved against the newer generation.
    write_file(&repo, "socket.rs", "fn bind_socket() { socketmarker_v2 }\n");
    let second_coordinate = publish_and_locate(&estate, &repo, "socketmarker_v2");
    let second = submit_kind(
        &estate,
        "socket_review",
        &repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .unwrap();
    let second_basis = obligation_basis_for(&estate, &second.work_id, "review");
    assert_ne!(
        first_basis, second_basis,
        "the same declared selector over a different published generation is a different \
         admitted basis — the freeze is what makes the generation part of the identity"
    );

    // The estate admits only the FIRST review's basis.
    write_review_policy(
        &estate,
        r#"["contradicted_assumption","verified_outcome"]"#,
        "socket-mode-reviewed",
        "1",
        &first_basis,
    );

    let worktree = materialize_actor(&pointer.socket, &estate, &second.work_id, &second.run_id);
    fs::write(worktree.join("review.md"), "decision: contradicted\n").unwrap();
    claim_ok(
        &estate,
        &second.work_id,
        &second.run_id,
        "review.md=review.md",
    );
    let file_run = status(&pointer.socket, &second.work_id)["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    let (code, raised, stderr) = raise_cli(
        &estate,
        &second.work_id,
        &file_run,
        &[
            "--kind",
            "contradicted_assumption",
            "--scope",
            "work_local",
            "--evidence",
            &format!(
                "work/{}/event/{}",
                second.work_id,
                claim_event_id(&estate, &second.work_id)
            ),
            "--claim",
            "a review of the newer generation",
            "--applies-to",
            &second_coordinate,
            "--obligation",
            "socket-mode-reviewed@1",
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        raised["settled"].is_null(),
        "a review of a generation the estate has not admitted settles nothing: {raised}"
    );
    let (_, pending, _) = finding_cli(
        &estate,
        &[
            "settle",
            "--finding",
            raised["id"].as_str().unwrap(),
            "--admin",
        ],
    );
    assert_eq!(
        pending["pending"]["reason"].as_str().unwrap(),
        "obligation-basis-not-admitted"
    );

    // Admitting the second review's own basis is what makes it settle —
    // a fresh admission, not a reused one.
    write_review_policy(
        &estate,
        r#"["contradicted_assumption","verified_outcome"]"#,
        "socket-mode-reviewed",
        "1",
        &second_basis,
    );
    let (code, now, stderr) = finding_cli(
        &estate,
        &[
            "settle",
            "--finding",
            raised["id"].as_str().unwrap(),
            "--admin",
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert!(now["settled"].is_object(), "{now}");

    stop_wirkd(&estate, wirkd_child);
}

/// The legitimate multiple-target review: a contract declaring two
/// selectors, in two different admitted sources, discharges only when
/// the reviewer's own admitted evidence covers **both** exactly. One of
/// the two, or one plus a same-path substitute, is not the review that
/// was admitted.
#[test]
fn a_multi_target_review_requires_every_frozen_target_exactly() {
    let dir = tempfile::tempdir().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    route_fixture::write_route(
        &estate,
        "two_target_review",
        r#"{"id":"two-target-review","waypoints":[
        {"id":"review","kind":"Actor","intent":"Review the socket boundary across both bound sources",
         "declared_outputs":[{"name":"review.md","required":true}],"boundary":["**"],
         "verifies":{"id":"boundary-reviewed","edition":"1",
           "proves":"an admitted independent review covered both declared sources and recorded a declared decision",
           "outputs":["review.md"],
           "review":{"recipe":"boundary-review@1",
             "targets":[{"source":"demo","path":"socket.rs"},{"source":"helper","path":"socket.rs"}],
             "decisions":["contradicted_assumption"]}}},
        {"id":"file","kind":"Actor","intent":"file the reviewer's own finding",
         "declared_outputs":[{"name":"done.md","required":true}],"boundary":["**"]}
    ]}"#,
    );
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let repo = dir.path().join("demo-repo");
    init_repo(&repo);
    write_file(&repo, "socket.rs", "fn bind_socket() { demomarker }\n");
    let demo_coordinate = publish_and_locate(&estate, &repo, "demomarker");

    let helper = dir.path().join("helper-repo");
    init_repo(&helper);
    write_file(&helper, "socket.rs", "fn bind_socket() { helpermarker }\n");
    let helper_coordinate = publish_and_locate_as(&estate, &helper, "helper", "helpermarker");

    let work = submit_kind(
        &estate,
        "two_target_review",
        &repo,
        &["demo:write", "helper:read"],
        None,
        Some("actor"),
    )
    .unwrap();
    let frozen = journal_events(&estate, &work.work_id)
        .into_iter()
        .rev()
        .find_map(|event| match event.kind {
            EventKind::WaypointReserved {
                waypoint,
                world: wirk_core::World::Actor(actor),
                ..
            } if waypoint.0 == "review" => Some(actor.review_targets),
            _ => None,
        })
        .expect("reserved");
    assert_eq!(frozen.len(), 2, "both selectors froze: {frozen:?}");
    assert_eq!(frozen[0].source, "demo");
    assert_eq!(frozen[1].source, "helper");
    assert_ne!(
        frozen[0].membership, frozen[1].membership,
        "two same-path targets in two different sources are two different identities"
    );

    let worktree = materialize_actor(&pointer.socket, &estate, &work.work_id, &work.run_id);
    fs::write(worktree.join("review.md"), "decision: contradicted\n").unwrap();
    claim_ok(&estate, &work.work_id, &work.run_id, "review.md=review.md");
    write_review_policy(
        &estate,
        r#"["contradicted_assumption"]"#,
        "boundary-reviewed",
        "1",
        &obligation_basis_for(&estate, &work.work_id, "review"),
    );
    let evidence = format!(
        "work/{}/event/{}",
        work.work_id,
        claim_event_id(&estate, &work.work_id)
    );
    let file_run = status(&pointer.socket, &work.work_id)["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    let raise = |claim: &str, applies: &[&str]| {
        let mut full = vec![
            "--kind",
            "contradicted_assumption",
            "--scope",
            "work_local",
            "--evidence",
            &evidence,
            "--claim",
            claim,
            "--obligation",
            "boundary-reviewed@1",
        ];
        for coordinate in applies {
            full.push("--applies-to");
            full.push(coordinate);
        }
        let (code, raised, stderr) = raise_cli(&estate, &work.work_id, &file_run, &full);
        assert_eq!(code, Some(0), "{stderr}");
        raised
    };

    let partial = raise("only one of the two targets", &[&demo_coordinate]);
    assert!(
        partial["settled"].is_null(),
        "covering one declared target of two discharges nothing: {partial}"
    );

    let both = raise(
        "the socket boundary is wider than stated in both sources",
        &[&demo_coordinate, &helper_coordinate],
    );
    assert!(
        both["settled"].is_object(),
        "covering every frozen target exactly discharges: {both}"
    );
    let targets = both["settled"]["proves"]["discharged_by"]["targets"]
        .as_array()
        .unwrap();
    assert_eq!(targets.len(), 2);
    assert_eq!(targets[0]["selector"]["source"].as_str().unwrap(), "demo");
    assert_eq!(targets[1]["selector"]["source"].as_str().unwrap(), "helper");
    assert_ne!(
        targets[0]["membership"].as_str().unwrap(),
        targets[1]["membership"].as_str().unwrap(),
        "the record distinguishes the two same-path targets: {targets:?}"
    );

    stop_wirkd(&estate, wirkd_child);
}

/// Both mechanisms under one container's outcome contract, at the same
/// time: the obligated `auditor` role is discharged by a real Actor
/// review, the obligated `scribe` role by a real deterministic check.
/// Neither is rewritten into the other, and the container obligation
/// records each role's own mechanism basis.
#[test]
fn a_container_obligation_accepts_an_actor_review_and_a_deterministic_check_side_by_side() {
    let dir = tempfile::tempdir().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    let intent = "Independently review the socket boundary for the auditor role";
    route_fixture::write_route(
        &estate,
        "mixed_container",
        r#"{"id":"mixed-container","waypoints":[
        {"id":"outer","kind":"Container",
         "declared_outputs":[{"name":"a.md","required":true}],
         "required_child_outcomes":[{"role":"auditor","required":true},{"role":"scribe","required":true}],
         "verifies":{"id":"boundary-established","edition":"1",
           "proves":"both obligated roles closed, each discharging an estate-admitted verification of the socket boundary",
           "outputs":["auditor","scribe"],
           "requires":{"id":"socket-mode-checked","edition":"1"}},
         "leaves":[{"id":"outer/leaf-a","kind":"Deterministic","command":["sh","-c","echo a > a.md"],
           "declared_outputs":[{"name":"a.md","required":true}]}]}
    ]}"#,
    );
    // The auditor's mechanism: an Actor review of the same obligation id.
    route_fixture::write_route(
        &estate,
        "auditor_review",
        &format!(
            r#"{{"id":"auditor-review","waypoints":[
            {{"id":"review","kind":"Actor","intent":"{intent}",
             "declared_outputs":[{{"name":"review.md","required":true}}],"boundary":["**"],
             "verifies":{{"id":"socket-mode-checked","edition":"1",
               "proves":"an admitted independent review of the socket boundary was performed and recorded a declared decision",
               "outputs":["review.md"],
               "review":{{"recipe":"socket-boundary-review@3","targets":[{{"source":"demo","path":"socket.rs"}}],"decisions":["verified_outcome"]}}}}}},
            {{"id":"file","kind":"Actor","intent":"file the reviewer's own finding",
             "declared_outputs":[{{"name":"done.md","required":true}}],"boundary":["**"]}}
        ]}}"#
        ),
    );
    // The scribe's mechanism: a deterministic check of the same
    // obligation id, at its own content basis.
    route_fixture::write_route(
        &estate,
        "scribe_check",
        r#"{"id":"scribe-check","waypoints":[
        {"id":"check","kind":"Deterministic","command":["sh","-c","echo 0600 > socket-mode.txt"],
         "declared_outputs":[{"name":"socket-mode.txt","required":true}],
         "verifies":{"id":"socket-mode-checked","edition":"1",
           "proves":"the socket-mode check ran and recorded socket-mode.txt","outputs":["socket-mode.txt"]}},
        {"id":"report","kind":"Deterministic","command":["sh","-c","echo done > report.md"],
         "declared_outputs":[{"name":"report.md","required":true}]}
    ]}"#,
    );
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let parent_repo = dir.path().join("parent-repo");
    init_repo(&parent_repo);
    write_file(
        &parent_repo,
        "socket.rs",
        "fn bind_socket() { socketmarker }\n",
    );
    let coordinate = publish_and_locate(&estate, &parent_repo, "socketmarker");
    let parent = submit(
        &estate,
        "mixed_container",
        &parent_repo,
        &["demo:write", "aux:write"],
        None,
    )
    .unwrap();
    let container_basis = obligation_basis_for(&estate, &parent.work_id, "outer");
    write_file(&parent_repo, "a.md", "a\n");
    claim_ok(&estate, &parent.work_id, &parent.run_id, "a.md=a.md");

    // The auditor child: a real Actor review.
    // The auditor reviews the parent's own repository under the same
    // `demo` binding, which is what lets its Finding apply to the exact
    // admitted `socket.rs` coordinate.
    let auditor = submit_kind(
        &estate,
        "auditor_review",
        &parent_repo,
        &["demo:write"],
        Some(ParentRef {
            work: &parent.work_id,
            waypoint: "outer",
            run: &parent.run_id,
            role: "auditor",
            attempt: None,
        }),
        Some("actor"),
    )
    .unwrap();
    let auditor_basis = obligation_basis_for(&estate, &auditor.work_id, "review");

    // The scribe child: a real deterministic check.
    let scribe_repo = dir.path().join("scribe-repo");
    init_repo(&scribe_repo);
    let scribe = submit(
        &estate,
        "scribe_check",
        &scribe_repo,
        &["aux:write"],
        Some(ParentRef {
            work: &parent.work_id,
            waypoint: "outer",
            run: &parent.run_id,
            role: "scribe",
            attempt: None,
        }),
    )
    .unwrap();
    let scribe_basis = obligation_basis_for(&estate, &scribe.work_id, "check");
    assert_ne!(
        auditor_basis, scribe_basis,
        "two different mechanisms for the same obligation id have different bases"
    );

    // The operator admits both mechanisms of the one container
    // obligation, and each mechanism's own class.
    write_policy(
        &estate,
        &format!(
            r#"{{"version":2,"classes":[
              {{"class":"child_investigation_confirmed","scope":"estate_local","kinds":["contradicted_assumption"],
                "obligations":[{{"id":"boundary-established","edition":"1","basis":"{container_basis}","mechanisms":["{auditor_basis}","{scribe_basis}"]}}]}},
              {{"class":"actor_reviewed","scope":"work_local","kinds":["verified_outcome"],
                "obligations":[{{"id":"socket-mode-checked","edition":"1","basis":"{auditor_basis}"}}]}},
              {{"class":"deterministic_verified","scope":"work_local","kinds":["verified_outcome"],
                "obligations":[{{"id":"socket-mode-checked","edition":"1","basis":"{scribe_basis}"}}]}}
            ]}}"#
        ),
    );

    // Run the auditor's review.
    let auditor_worktree =
        materialize_actor(&pointer.socket, &estate, &auditor.work_id, &auditor.run_id);
    fs::write(
        auditor_worktree.join("review.md"),
        "decision: verified_outcome\n",
    )
    .unwrap();
    claim_ok(
        &estate,
        &auditor.work_id,
        &auditor.run_id,
        "review.md=review.md",
    );
    let auditor_file_run = status(&pointer.socket, &auditor.work_id)["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    let (code, auditor_finding, stderr) = raise_cli(
        &estate,
        &auditor.work_id,
        &auditor_file_run,
        &[
            "--kind",
            "verified_outcome",
            "--scope",
            "work_local",
            "--claim",
            "the socket boundary was reviewed",
            "--evidence",
            &format!(
                "work/{}/event/{}",
                auditor.work_id,
                claim_event_id(&estate, &auditor.work_id)
            ),
            "--applies-to",
            &coordinate,
            "--obligation",
            "socket-mode-checked@1",
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        auditor_finding["settled"]["authority"]["policy"]["class"]
            .as_str()
            .unwrap(),
        "actor_reviewed",
        "the auditor's own verification settles as a review: {auditor_finding}"
    );
    let auditor_finding_id = auditor_finding["id"].as_str().unwrap().to_string();

    // Run the scribe's deterministic check.
    write_file(&scribe_repo, "socket-mode.txt", "0600\n");
    claim_ok(
        &estate,
        &scribe.work_id,
        &scribe.run_id,
        "socket-mode.txt=socket-mode.txt",
    );
    let scribe_report_run = status(&pointer.socket, &scribe.work_id)["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    let (code, scribe_finding, stderr) = raise_cli(
        &estate,
        &scribe.work_id,
        &scribe_report_run,
        &[
            "--kind",
            "verified_outcome",
            "--scope",
            "work_local",
            "--claim",
            "the socket-mode check ran",
            "--evidence",
            &format!(
                "work/{}/event/{}",
                scribe.work_id,
                claim_event_id(&estate, &scribe.work_id)
            ),
            "--obligation",
            "socket-mode-checked@1",
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        scribe_finding["settled"]["authority"]["policy"]["class"]
            .as_str()
            .unwrap(),
        "deterministic_verified",
        "the scribe's own verification settles as a deterministic check: {scribe_finding}"
    );

    // The parent's Finding, raised while it is still open.
    let (code, parent_finding, stderr) = raise_cli(
        &estate,
        &parent.work_id,
        &parent.run_id,
        &[
            "--kind",
            "contradicted_assumption",
            "--scope",
            "estate_local",
            "--claim",
            "the socket boundary assumption is contradicted",
            "--evidence",
            &format!(
                "work/{}/event/{}",
                parent.work_id,
                submitted_event_id(&estate, &parent.work_id)
            ),
            "--obligation",
            "boundary-established@1",
            "--confirmed-by",
            &format!("work/{}/finding/{auditor_finding_id}", auditor.work_id),
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let parent_finding_id = parent_finding["id"].as_str().unwrap().to_string();

    // Both children complete; the container closes on both receipts.
    fs::write(auditor_worktree.join("done.md"), "done\n").unwrap();
    claim_ok(
        &estate,
        &auditor.work_id,
        &auditor_file_run,
        "done.md=done.md",
    );
    write_file(&scribe_repo, "report.md", "done\n");
    claim_ok(
        &estate,
        &scribe.work_id,
        &scribe_report_run,
        "report.md=report.md",
    );
    assert_eq!(state_of(&pointer.socket, &parent.work_id), "completed");

    let (code, settled, stderr) = finding_cli(
        &estate,
        &["settle", "--finding", &parent_finding_id, "--admin"],
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        settled["settled"].is_object(),
        "one container obligation, two different admitted mechanisms: {settled}"
    );
    let roles = settled["settled"]["check"]["obligated_roles"]
        .as_array()
        .unwrap();
    assert_eq!(roles.len(), 2);
    assert_eq!(roles[0]["role"].as_str().unwrap(), "auditor");
    assert_eq!(
        roles[0]["mechanism_basis"].as_str().unwrap(),
        auditor_basis,
        "the auditor role records the review's own basis"
    );
    assert_eq!(roles[1]["role"].as_str().unwrap(), "scribe");
    assert_eq!(
        roles[1]["mechanism_basis"].as_str().unwrap(),
        scribe_basis,
        "the scribe role records the deterministic check's own basis"
    );

    stop_wirkd(&estate, wirkd_child);
}

// ---- 6. journal evidence lineage: an unrelated Work is inadmissible ---

#[test]
fn journal_reference_to_an_unrelated_work_is_inadmissible() {
    let dir = tempfile::tempdir().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    route_fixture::install_route_fixture(&estate, "wa_simple_leaf");
    let (wirkd_child, _pointer) = start_wirkd(&estate);

    let repo_a = dir.path().join("repo-a");
    init_repo(&repo_a);
    let work_a = submit(&estate, "wa_simple_leaf", &repo_a, &["demo:write"], None).unwrap();
    let repo_b = dir.path().join("repo-b");
    init_repo(&repo_b);
    let work_b = submit(&estate, "wa_simple_leaf", &repo_b, &["demo:write"], None).unwrap();

    // work_a tries to cite an event from work_b's own journal — the two
    // are unrelated (no parent/child binding at all).
    let evidence = format!(
        "work/{}/event/{}",
        work_b.work_id,
        submitted_event_id(&estate, &work_b.work_id)
    );
    let (code, _reply, stderr) = raise_cli(
        &estate,
        &work_a.work_id,
        &work_a.run_id,
        &[
            "--kind",
            "gap",
            "--scope",
            "work_local",
            "--claim",
            "citing an unrelated Work's own event",
            "--evidence",
            &evidence,
        ],
    );
    assert_eq!(code, Some(3), "expected a Refused exit, got: {stderr}");

    stop_wirkd(&estate, wirkd_child);
}

/// Authority review §4 ("cross-Work evidence admission", executed):
/// journal kinship is a necessary, never a sufficient, evidence grant. A
/// parent bound to both `demo` and `helper` raises a Finding whose own
/// evidence is a real `demo` Source coordinate; a child admitted only
/// under `helper` must not inherit that `demo` evidence merely by citing
/// the parent's own `FindingRaised` event through `Journal` — the same
/// direct citation of that `demo` coordinate is already refused
/// (`journal_reference_to_an_unrelated_work_is_inadmissible`'s own
/// sibling shape); wrapping it in the parent's journal event must not
/// launder it into `Admitted`.
#[test]
fn journal_reference_does_not_inherit_a_parents_broader_source_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    route_fixture::install_route_fixture(&estate, "wa_container_child_role");
    route_fixture::install_route_fixture(&estate, "wa_simple_leaf");
    let (wirkd_child, _pointer) = start_wirkd(&estate);

    let demo_repo = dir.path().join("demo-repo");
    init_repo(&demo_repo);
    write_file(&demo_repo, "secret.md", "parentonlyevidence marker\n");
    let git = |args: &[&str]| {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(&demo_repo)
                .status()
                .unwrap()
                .success()
        );
    };
    git(&["add", "."]);
    git(&[
        "-c",
        "user.email=wb@example.test",
        "-c",
        "user.name=wb",
        "commit",
        "-q",
        "-m",
        "one",
    ]);
    let (ok, acquired, err) = atlas(
        &estate,
        &[
            "acquire",
            "--source",
            "demo",
            "--repository",
            demo_repo.to_str().unwrap(),
            "--revision",
            "HEAD",
        ],
    );
    assert!(ok, "{err}");
    let generation_1 = acquired["generation"]["generation"]
        .as_str()
        .unwrap()
        .to_string();
    let (ok, _, err) = atlas(
        &estate,
        &["publish", "--source", "demo", "--generation", &generation_1],
    );
    assert!(ok, "{err}");

    let parent_repo = dir.path().join("parent-repo");
    init_repo(&parent_repo);
    let parent = submit(
        &estate,
        "wa_container_child_role",
        &parent_repo,
        &["demo:read", "helper:write"],
        None,
    )
    .unwrap();

    let (ok, search, err) = atlas(
        &estate,
        &[
            "search",
            "--work",
            &parent.work_id,
            "--query",
            "parentonlyevidence",
        ],
    );
    assert!(ok, "{err}");
    let demo_coordinate = search["hits"][0]["coordinate"]
        .as_str()
        .unwrap()
        .to_string();
    let (code, parent_finding, stderr) = raise_cli(
        &estate,
        &parent.work_id,
        &parent.run_id,
        &[
            "--kind",
            "gap",
            "--scope",
            "estate_local",
            "--claim",
            "a real demo-only gap",
            "--evidence",
            &demo_coordinate,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let parent_finding_id = parent_finding["id"].as_str().unwrap().to_string();
    let parent_raise_event = journal_events(&estate, &parent.work_id)
        .into_iter()
        .find_map(|event| match &event.kind {
            EventKind::FindingRaised { finding } if finding.id.0 == parent_finding_id => {
                Some(event.id.0)
            }
            _ => None,
        })
        .expect("the parent's own FindingRaised event");

    let child_repo = dir.path().join("child-repo");
    init_repo(&child_repo);
    let child = submit(
        &estate,
        "wa_simple_leaf",
        &child_repo,
        &["helper:write"],
        Some(ParentRef {
            work: &parent.work_id,
            waypoint: "outer",
            run: &parent.run_id,
            role: "helper",
            attempt: None,
        }),
    )
    .unwrap();

    // Direct citation of the `demo` coordinate is already refused
    // (established sibling behavior) — the decisive case here is the
    // *indirect* citation through the parent's own journal event.
    let journal_reference = format!("work/{}/event/{parent_raise_event}", parent.work_id);
    let (code, child_finding, stderr) = raise_cli(
        &estate,
        &child.work_id,
        &child.run_id,
        &[
            "--kind",
            "gap",
            "--scope",
            "estate_local",
            "--claim",
            "laundering the parent's own demo evidence through journal kinship",
            "--evidence",
            &journal_reference,
        ],
    );
    assert_eq!(
        code,
        Some(3),
        "a narrowed child must not inherit the parent's own demo evidence via Journal kinship alone: {stderr} {child_finding}"
    );

    stop_wirkd(&estate, wirkd_child);
}

/// W-B-CORRECT.md defect 2 ("journal disclosure"): naming a `--work` id
/// is a selection, never a general evidence grant. An unrelated Work's
/// own findings must not be disclosed to a requester whose own lineage
/// does not include it, while the same Work can always list its own.
/// `--admin` remains the one, explicit, separately named path that
/// still sees everything.
#[test]
fn finding_list_scoped_to_the_requesting_works_own_lineage() {
    let dir = tempfile::tempdir().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    route_fixture::install_route_fixture(&estate, "wa_simple_leaf");
    let (wirkd_child, _pointer) = start_wirkd(&estate);

    let repo_a = dir.path().join("repo-a");
    init_repo(&repo_a);
    let work_a = submit(&estate, "wa_simple_leaf", &repo_a, &["demo:write"], None).unwrap();
    let repo_b = dir.path().join("repo-b");
    init_repo(&repo_b);
    let work_b = submit(&estate, "wa_simple_leaf", &repo_b, &["demo:write"], None).unwrap();

    let evidence_b = format!(
        "work/{}/event/{}",
        work_b.work_id,
        submitted_event_id(&estate, &work_b.work_id)
    );
    let (code, raised, stderr) = raise_cli(
        &estate,
        &work_b.work_id,
        &work_b.run_id,
        &[
            "--kind",
            "gap",
            "--scope",
            "work_local",
            "--claim",
            "a gap only work_b should be able to disclose",
            "--evidence",
            &evidence_b,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let finding_b = raised["id"].as_str().unwrap().to_string();

    // Negative: work_a is not in work_b's own lineage — a plain
    // `--work work_b` selection under work_a's own requesting identity
    // must not disclose work_b's finding.
    let (code, _reply, stderr) = finding_cli(
        &estate,
        &[
            "list",
            "--requesting-work",
            &work_a.work_id,
            "--work",
            &work_b.work_id,
        ],
    );
    assert_eq!(
        code,
        Some(2),
        "an unrelated work's own findings must be refused, not disclosed: {stderr}"
    );

    // Positive control: work_b listing its own lineage sees its own
    // finding.
    let (code, listed, stderr) = finding_cli(
        &estate,
        &[
            "list",
            "--requesting-work",
            &work_b.work_id,
            "--work",
            &work_b.work_id,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        listed["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["id"] == finding_b)
    );

    // Explicit administrative inspection remains a separate, working
    // path: unscoped by lineage, but only when named explicitly.
    let (code, listed, stderr) =
        finding_cli(&estate, &["list", "--admin", "--work", &work_b.work_id]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        listed["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["id"] == finding_b)
    );

    stop_wirkd(&estate, wirkd_child);
}

// ---- 7. estate-local finding with only unavailable evidence -----------

#[test]
fn estate_local_finding_with_only_unavailable_evidence_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    route_fixture::install_route_fixture(&estate, "wa_simple_leaf");
    let (wirkd_child, _pointer) = start_wirkd(&estate);

    let repo = dir.path().join("repo");
    init_repo(&repo);
    let work = submit(&estate, "wa_simple_leaf", &repo, &["demo:write"], None).unwrap();

    // A well-formed journal reference naming a real event on the raising
    // Work's own lineage, but for an event that does not exist —
    // resolvable admission scope, unresolvable content: `Unavailable`,
    // never promoted to `Admitted`.
    let evidence = format!("work/{}/event/does-not-exist", work.work_id);
    let (code, reply, stderr) = raise_cli(
        &estate,
        &work.work_id,
        &work.run_id,
        &[
            "--kind",
            "verified_outcome",
            "--scope",
            "estate_local",
            "--claim",
            "nothing to point at",
            "--evidence",
            &evidence,
        ],
    );
    assert_eq!(
        code,
        Some(3),
        "expected NoAdmittedEvidence, got: {reply} {stderr}"
    );

    stop_wirkd(&estate, wirkd_child);
}

// ---- 8. Application: mechanical proof, then the asserted judgement ----

#[test]
fn application_refuses_unchanged_coordinates_and_succeeds_on_a_real_change() {
    let dir = tempfile::tempdir().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    route_fixture::install_route_fixture(&estate, "wa_simple_leaf");
    let (wirkd_child, _pointer) = start_wirkd(&estate);

    let source_repo = dir.path().join("source-repo");
    fs::create_dir_all(&source_repo).unwrap();
    let git = |args: &[&str]| {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(&source_repo)
                .status()
                .unwrap()
                .success()
        );
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "wb@example.test"]);
    git(&["config", "user.name", "wb"]);
    fs::write(
        source_repo.join("bind.rs"),
        "fn bind_socket() { /* mode 0775 */ }\n",
    )
    .unwrap();
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "one"]);

    let (ok, acquired, err) = atlas(
        &estate,
        &[
            "acquire",
            "--source",
            "demo",
            "--repository",
            source_repo.to_str().unwrap(),
            "--revision",
            "HEAD",
        ],
    );
    assert!(ok, "{err}");
    let generation_1 = acquired["generation"]["generation"]
        .as_str()
        .unwrap()
        .to_string();
    let (ok, _, err) = atlas(
        &estate,
        &["publish", "--source", "demo", "--generation", &generation_1],
    );
    assert!(ok, "{err}");

    let work_repo = dir.path().join("work-repo");
    init_repo(&work_repo);
    let work = submit(&estate, "wa_simple_leaf", &work_repo, &["demo:write"], None).unwrap();

    let (ok, search, err) = atlas(
        &estate,
        &["search", "--work", &work.work_id, "--query", "bind_socket"],
    );
    assert!(ok, "{err}");
    let coordinate = search["hits"][0]["coordinate"]
        .as_str()
        .unwrap()
        .to_string();

    let (code, raised, stderr) = raise_cli(
        &estate,
        &work.work_id,
        &work.run_id,
        &[
            "--kind",
            "contradicted_assumption",
            "--scope",
            "estate_local",
            "--claim",
            "bind_socket sets no explicit mode",
            "--evidence",
            &coordinate,
            "--applies-to",
            &coordinate,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let finding_id = raised["id"].as_str().unwrap().to_string();

    // Same revision published again: a republication is not an application.
    let (code, _reply, stderr) = applied_cli(
        &estate,
        &work.work_id,
        &work.run_id,
        &[
            "--finding",
            &finding_id,
            "--source",
            "demo",
            "--revision",
            "HEAD",
            "--by",
            "root",
        ],
    );
    assert_eq!(code, Some(3), "expected a refusal, got: {stderr}");

    // A real, unrelated change published as generation 2 (a different
    // file, `bind.rs` untouched): the finding's own coordinates carry
    // the identical object id, so this must still refuse.
    fs::write(source_repo.join("unrelated.md"), "unrelated\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "two"]);
    let (ok, acquired2, err) = atlas(
        &estate,
        &["refresh", "--source", "demo", "--revision", "HEAD"],
    );
    assert!(ok, "{err}");
    let generation_2 = acquired2["generation"]["generation"]
        .as_str()
        .unwrap()
        .to_string();
    let (ok, _, err) = atlas(
        &estate,
        &["publish", "--source", "demo", "--generation", &generation_2],
    );
    assert!(ok, "{err}");
    let rev_2 = Command::new("git")
        .args(["-C", source_repo.to_str().unwrap(), "rev-parse", "HEAD"])
        .output()
        .unwrap();
    let rev_2 = String::from_utf8_lossy(&rev_2.stdout).trim().to_string();
    let (code, _reply, stderr) = applied_cli(
        &estate,
        &work.work_id,
        &work.run_id,
        &[
            "--finding",
            &finding_id,
            "--source",
            "demo",
            "--revision",
            &rev_2,
            "--by",
            "root",
        ],
    );
    assert_eq!(
        code,
        Some(3),
        "expected CoordinatesUnchanged, got: {stderr}"
    );

    // Now a real edit to `bind.rs` itself: the exact coordinate changed.
    fs::write(
        source_repo.join("bind.rs"),
        "fn bind_socket() { set_permissions(0o600); }\n",
    )
    .unwrap();
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "three"]);
    let (ok, acquired3, err) = atlas(
        &estate,
        &["refresh", "--source", "demo", "--revision", "HEAD"],
    );
    assert!(ok, "{err}");
    let generation_3 = acquired3["generation"]["generation"]
        .as_str()
        .unwrap()
        .to_string();
    let (ok, _, err) = atlas(
        &estate,
        &["publish", "--source", "demo", "--generation", &generation_3],
    );
    assert!(ok, "{err}");
    let rev_3 = Command::new("git")
        .args(["-C", source_repo.to_str().unwrap(), "rev-parse", "HEAD"])
        .output()
        .unwrap();
    let rev_3 = String::from_utf8_lossy(&rev_3.stdout).trim().to_string();
    let (code, applied, stderr) = applied_cli(
        &estate,
        &work.work_id,
        &work.run_id,
        &[
            "--finding",
            &finding_id,
            "--source",
            "demo",
            "--revision",
            &rev_3,
            "--by",
            "root",
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let record = applied["applied"].as_array().unwrap().last().unwrap();
    assert_ne!(record["before"]["object_id"], record["after"]["object_id"]);
    assert_eq!(record["attribution"]["attribution"], "asserted");
    assert_eq!(record["attribution"]["verified"], serde_json::json!(false));
    assert_eq!(record["attribution"]["producer"]["work"], work.work_id);
    assert_eq!(record["attribution"]["producer"]["run"], work.run_id);

    stop_wirkd(&estate, wirkd_child);
}

/// W-B-APPLICATION-REPAIR.md, "Mutation credit requires the exact Write
/// execution/source authority and complete after-resource bytes matching
/// the validated artifact receipt".
///
/// The cited Claim is *history*: a Validated Done Claim that closed its
/// own Run — and, in the ordinary shape, its own Work — still proves
/// exactly which bytes it produced. What the candidate did was reach
/// that shape by weakening the caller instead: one triple served as both
/// the current producer and the historical receipt, so terminality had
/// to be allowed for either to work. Here they are two identities.
/// `--claim-run` names the historical Run; the caller's own triple stays
/// a real current producing action.
///
/// Every refusal below is a distinct real authority failure executed
/// against a real daemon, real Git repositories and real Atlas
/// generations: a receipt naming a different artifact, a receipt whose
/// digest no longer matches the after generation's *whole* bytes (the
/// only byte that moved sits outside every span the search ever
/// returned), a Work that really holds `Write` on the alias but really
/// executed in a different repository, and a Read-only membership with a
/// genuine, valid Write Claim of its own elsewhere.
#[test]
fn claim_attribution_binds_the_exact_execution_source_and_whole_after_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    route_fixture::write_route(
        &estate,
        "bind_three",
        r#"{"id":"bind-three","waypoints":[
            {"id":"wp-1","kind":"Deterministic","command":["true"],"declared_outputs":[{"name":"bind.rs","required":true}]},
            {"id":"wp-2","kind":"Deterministic","command":["true"],"declared_outputs":[{"name":"notes.md","required":true}]},
            {"id":"wp-3","kind":"Deterministic","command":["true"],"declared_outputs":[{"name":"done.md","required":true}]}
        ]}"#,
    );
    let (wirkd_child, pointer) = start_wirkd(&estate);

    // One repository plays both roles — the Work's own execution
    // checkout and the Atlas source acquired from it — so a real Claim
    // produces bytes Atlas then republishes unchanged.
    let repo = publish_source(dir.path(), &estate, "demo", "repo");

    let work = submit(&estate, "bind_three", &repo, &["demo:write"], None).unwrap();
    let coordinate = bind_coordinate(&estate, &work.work_id);
    let finding = raise_bind_finding(&estate, &work.work_id, &work.run_id, &coordinate);
    let wp1_run = work.run_id.clone();

    // Two more Works, both raising their own Finding against *this*
    // generation, before anything is fixed — so each one's own
    // `applies_to` is the same before-state the owner's is.
    let clone_repo = dir.path().join("clone-repo");
    init_repo(&clone_repo);
    let clone_work = submit(&estate, "bind_three", &clone_repo, &["demo:write"], None).unwrap();
    let clone_finding = raise_bind_finding(
        &estate,
        &clone_work.work_id,
        &clone_work.run_id,
        &bind_coordinate(&estate, &clone_work.work_id),
    );
    let ro_repo = dir.path().join("ro-repo");
    init_repo(&ro_repo);
    let work_ro = submit(
        &estate,
        "bind_three",
        &ro_repo,
        &["other:write", "demo:read"],
        None,
    )
    .unwrap();
    let ro_finding = raise_bind_finding(
        &estate,
        &work_ro.work_id,
        &work_ro.run_id,
        &bind_coordinate(&estate, &work_ro.work_id),
    );

    // The real fix, claimed by `wp-1` itself, then committed and
    // republished unchanged.
    let fixed = "fn bind_socket() { set_permissions(0o600); }\nfn untouched_tail() { /* outside every hit span */ }\n";
    write_file(&repo, "bind.rs", fixed);
    claim_ok(&estate, &work.work_id, &wp1_run, "bind.rs=bind.rs");
    write_file(&clone_repo, "bind.rs", fixed);
    claim_ok(
        &estate,
        &clone_work.work_id,
        &clone_work.run_id,
        "bind.rs=bind.rs",
    );
    write_file(&ro_repo, "bind.rs", fixed);
    claim_ok(
        &estate,
        &work_ro.work_id,
        &work_ro.run_id,
        "bind.rs=bind.rs",
    );
    let rev_2 = republish(&estate, &repo, "demo", "two");

    // `wp-2` claims a different artifact; `wp-3` is the caller's own
    // current producing action for every call below.
    let wp2_run = status(&pointer.socket, &work.work_id)["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    write_file(&repo, "notes.md", "notes\n");
    claim_ok(&estate, &work.work_id, &wp2_run, "notes.md=notes.md");
    fs::remove_file(repo.join("notes.md")).unwrap();
    let wp3_run = status(&pointer.socket, &work.work_id)["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(wp3_run, wp2_run);

    let cite = |claim_run: &str, revision: &str| -> Vec<String> {
        vec![
            "--finding".into(),
            finding.clone(),
            "--source".into(),
            "demo".into(),
            "--revision".into(),
            revision.to_string(),
            "--by".into(),
            "root".into(),
            "--claim-run".into(),
            claim_run.to_string(),
        ]
    };
    let as_argv = |v: &Vec<String>| -> Vec<String> { v.clone() };

    // The receipt names `notes.md`, not this finding's own path.
    let args = as_argv(&cite(&wp2_run, &rev_2));
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let refusal = applied_refusal(&estate, &work.work_id, &wp3_run, &argv);
    assert!(
        refusal.contains("ChangedClaimedArtifact"),
        "a receipt at another path is not this finding's own production: {refusal}"
    );

    // A Run that never carried a Validated Done Claim at all — the
    // caller's own current, open one.
    let args = as_argv(&cite(&wp3_run, &rev_2));
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let refusal = applied_refusal(&estate, &work.work_id, &wp3_run, &argv);
    assert!(refusal.contains("WrongClaim"), "{refusal}");

    // A Work that really holds `Write` on the alias `demo` and really
    // made a Validated Done Claim over `bind.rs` with byte-identical
    // content — but executed in an entirely different repository. Alias
    // equality alone credited it on the candidate.
    let clone_wp2 = status(&pointer.socket, &clone_work.work_id)["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    let refusal = applied_refusal(
        &estate,
        &clone_work.work_id,
        &clone_wp2,
        &[
            "--finding",
            &clone_finding,
            "--source",
            "demo",
            "--revision",
            &rev_2,
            "--by",
            "root",
            "--claim-run",
            &clone_work.run_id,
        ],
    );
    assert!(
        refusal.contains("DifferentExecutionSource"),
        "a shared --repo alias is not the same checkout: {refusal}"
    );

    // A Read-only membership, with a genuine valid Write Claim of its
    // own on another source, never acquires mutation credit here
    // (ruling 0077's allowance is for `Attribution::Asserted` alone).
    let ro_wp2 = status(&pointer.socket, &work_ro.work_id)["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    let refusal = applied_refusal(
        &estate,
        &work_ro.work_id,
        &ro_wp2,
        &[
            "--finding",
            &ro_finding,
            "--source",
            "demo",
            "--revision",
            &rev_2,
            "--by",
            "root",
            "--claim-run",
            &work_ro.run_id,
        ],
    );
    assert!(
        refusal.contains("ReadOnlyMutationCredit"),
        "a Read-only membership confers no mutation credit: {refusal}"
    );
    // Its unverified assertion, by contrast, is still perfectly legal.
    let (code, asserted, stderr) = applied_cli(
        &estate,
        &work_ro.work_id,
        &ro_wp2,
        &[
            "--finding",
            &ro_finding,
            "--source",
            "demo",
            "--revision",
            &rev_2,
            "--by",
            "root",
            "--json",
        ],
    );
    assert_eq!(
        code,
        Some(0),
        "ruling 0077: a Read source still asserts: {stderr}"
    );
    assert_eq!(
        asserted["applied"].as_array().unwrap().last().unwrap()["attribution"]["attribution"],
        "asserted"
    );

    // Green: the historical closing Claim of `wp-1`, cited from the
    // caller's own current `wp-3` action.
    let args = as_argv(&cite(&wp1_run, &rev_2));
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let (code, applied, stderr) = applied_cli(&estate, &work.work_id, &wp3_run, &argv);
    assert_eq!(code, Some(0), "{stderr}");
    let record = applied["applied"].as_array().unwrap().last().unwrap();
    assert_eq!(record["attribution"]["attribution"], "claim");
    assert_eq!(record["attribution"]["work"], work.work_id);
    assert_eq!(record["attribution"]["run"], wp1_run);
    assert!(record["attribution"]["claim"].is_string());
    assert!(record["attribution"]["claim_event"].is_string());
    assert_eq!(record["after"]["resource"], "present");

    // Whole resource, not a retrieved span: the *only* byte that moves
    // now is in `untouched_tail`, outside every span the Atlas search
    // ever returned for `bind_socket`. The receipt digest is over the
    // complete file, so the same Claim must no longer attest these
    // bytes — a historical receipt never silently becomes later bytes.
    write_file(
        &repo,
        "bind.rs",
        "fn bind_socket() { set_permissions(0o600); }\nfn untouched_tail() { /* moved */ }\n",
    );
    let rev_3 = republish(&estate, &repo, "demo", "three");
    let args = as_argv(&cite(&wp1_run, &rev_3));
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let refusal = applied_refusal(&estate, &work.work_id, &wp3_run, &argv);
    assert!(
        refusal.contains("DifferentAfterBytes"),
        "the receipt attests whole-resource bytes, span or no span: {refusal}"
    );
    // The same change is still a perfectly recordable *asserted*
    // Application — mechanical observed change is not the receipt.
    let (code, _reply, stderr) = applied_cli(
        &estate,
        &work.work_id,
        &wp3_run,
        &[
            "--finding",
            &finding,
            "--source",
            "demo",
            "--revision",
            &rev_3,
            "--by",
            "root",
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");

    stop_wirkd(&estate, wirkd_child);
}

// ---- 9. malformed index blocks reads until rebuild --------------------

#[test]
fn malformed_index_row_is_a_hard_error_and_rebuild_repairs_it() {
    let dir = tempfile::tempdir().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    write_two_leaf_route(&estate);
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let repo = dir.path().join("repo");
    init_repo(&repo);
    let work = submit(&estate, "two_leaf", &repo, &["demo:write"], None).unwrap();
    write_file(&repo, "out1.md", "one\n");
    claim_ok(&estate, &work.work_id, &work.run_id, "out1.md=out1.md");
    write_policy_admitting(
        &estate,
        "deterministic_verified",
        "verified_outcome",
        &[(
            "out1-produced",
            "1",
            &obligation_basis_for(&estate, &work.work_id, "wp-1"),
        )],
    );
    let claim_id = claim_event_id(&estate, &work.work_id);
    let evidence = format!("work/{}/event/{claim_id}", work.work_id);
    let wp2_run = status(&pointer.socket, &work.work_id)["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    let (code, _raised, stderr) = raise_cli(
        &estate,
        &work.work_id,
        &wp2_run,
        &[
            "--kind",
            "verified_outcome",
            "--scope",
            "estate_local",
            "--claim",
            "the deterministic leaf ran",
            "--evidence",
            &evidence,
            "--obligation",
            "out1-produced@1",
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");

    let (ok, _index, err) = atlas(&estate, &["findings", "--rebuild", "--admin"]);
    assert!(ok, "{err}");

    fs::write(
        estate.join("atlas").join("findings.ndjson"),
        b"not valid json at all\n",
    )
    .unwrap();
    let (ok, _reply, _err) = atlas(&estate, &["findings", "--admin"]);
    assert!(!ok, "a malformed index row must block plain reads");

    let (ok, index, err) = atlas(&estate, &["findings", "--rebuild", "--admin"]);
    assert!(ok, "rebuild must repair a malformed index: {err}");
    assert_eq!(index["rows"].as_array().unwrap().len(), 1);

    stop_wirkd(&estate, wirkd_child);
}

// ---- 10. restart repairs a terminal Work's missing settlement ---------

#[test]
fn restart_settles_and_indexes_a_terminal_works_missing_finding() {
    let dir = tempfile::tempdir().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    // Hand-built journal: `WorkSubmitted` (one Deterministic leaf, no
    // declared outputs so no artifact needs to exist on disk),
    // `RunOpened`, a Validated Done `ClaimRecorded` (completing the
    // Work), and a `FindingRaised` naming that exact Claim — with no
    // `FindingSettled` at all. This is what a crash between the Claim
    // and the settlement leaves behind; no daemon ever wrote this file.
    let work_id = WorkId("work-manual-1".to_string());
    let run_id = RunId("run-manual-1".to_string());
    let leaf_obligation = wirk_core::VerificationObligation {
        id: "manual-check".to_string(),
        edition: "1".to_string(),
        proves: "the manual leaf's command ran".to_string(),
        outputs: Vec::new(),
        requires: None,
        review: None,
    };
    let leaf = wirk_core::WaypointDefinition {
        id: WaypointId("wp-1".to_string()),
        kind: wirk_core::WaypointKind::Deterministic,
        declared_outputs: Vec::new(),
        intent: None,
        command: Some(vec!["true".to_string()]),
        boundary: wirk_core::Boundary(Vec::new()),
        leaves: Vec::new(),
        required_child_outcomes: Vec::new(),
        selection: None,
        // The hand-built leaf declares the obligation the Finding below
        // names; the policy admitting its basis is written after this
        // journal exists, exactly as an estate operator admits a check.
        verifies: Some(leaf_obligation),
        orient: None,
    };
    let manual_basis = wirk_core::obligation_basis(&leaf, Some(&WorldHash("manual".to_string())))
        .expect("the hand-built leaf's own admitted basis");
    raw_append(
        &estate,
        &work_id.0,
        None,
        EventKind::WorkSubmitted {
            route: wirk_core::RouteId("manual".to_string()),
            repositories: vec![wirk_core::RepositoryBinding {
                name: "demo".to_string(),
                access: wirk_core::Access::Write,
            }],
            intent: "manual".to_string(),
            waypoints: vec![leaf.id.clone()],
            waypoint_defs: vec![leaf],
            parent: None,
            execution_repo: None,
            execution_identity: None,
        },
    );
    raw_append(
        &estate,
        &work_id.0,
        Some(&run_id.0),
        EventKind::RunOpened {
            run: run_id.clone(),
            waypoint: WaypointId("wp-1".to_string()),
            attempt: 1,
            world_hash: WorldHash("manual".to_string()),
        },
    );
    raw_append(
        &estate,
        &work_id.0,
        Some(&run_id.0),
        EventKind::ClaimRecorded {
            claim: ClaimId("claim-manual-1".to_string()),
            claim_kind: ClaimKind::Done,
            verdict: ClaimVerdict::Validated,
            artifacts: Vec::new(),
        },
    );
    let claim_event_id = journal_events(&estate, &work_id.0)
        .into_iter()
        .find_map(|event| {
            matches!(&event.kind, EventKind::ClaimRecorded { .. }).then_some(event.id)
        })
        .expect("the ClaimRecorded event we just appended");
    let finding = Finding {
        id: FindingId("finding-manual-1".to_string()),
        work: work_id.clone(),
        run: run_id.clone(),
        waypoint: WaypointId("wp-1".to_string()),
        kind: FindingKind::VerifiedOutcome,
        scope: FindingScope::EstateLocal,
        claim: "the deterministic leaf ran".to_string(),
        evidence: vec![AdmittedEvidence {
            reference: EvidenceRef::Journal {
                work: work_id.clone(),
                event: claim_event_id.clone(),
            },
            outcome: EvidenceOutcome::Admitted {
                generation: work_id.0.clone(),
                object_id: claim_event_id.0.clone(),
            },
        }],
        contradicts: Vec::new(),
        applies_to: Vec::new(),
        supersedes: None,
        proposed_change: None,
        obligation: Some(wirk_core::ObligationRef {
            id: "manual-check".to_string(),
            edition: "1".to_string(),
        }),
        confirmed_by: None,
    };
    raw_append(
        &estate,
        &work_id.0,
        Some(&run_id.0),
        EventKind::FindingRaised { finding },
    );

    // The estate now admits exactly that leaf's own obligation, at the
    // basis its hand-built World really content-addresses to.
    write_policy_admitting(
        &estate,
        "deterministic_verified",
        "verified_outcome",
        &[("manual-check", "1", &manual_basis)],
    );

    // Confirmed pre-condition: the Work is terminal, and there is no
    // settlement in the journal at all.
    let events_before = journal_events(&estate, &work_id.0);
    assert!(wirk_core::fold(&events_before).state.is_terminal());
    assert!(
        !events_before
            .iter()
            .any(|event| matches!(event.kind, EventKind::FindingSettled { .. }))
    );

    let (wirkd_child, pointer) = start_wirkd(&estate);
    let _ = status(&pointer.socket, &work_id.0);

    let events_after = journal_events(&estate, &work_id.0);
    assert!(
        events_after
            .iter()
            .any(|event| matches!(event.kind, EventKind::FindingSettled { .. })),
        "startup must settle a terminal Work's missing finding"
    );

    let (ok, index, err) = atlas(&estate, &["findings", "--admin"]);
    assert!(ok, "{err}");
    assert_eq!(index["rows"].as_array().unwrap().len(), 1);
    assert_eq!(
        index["rows"][0]["settlement"]["minted_at_startup"],
        serde_json::json!(true)
    );

    stop_wirkd(&estate, wirkd_child);
}

// ---- 10b. a settlement journaled by the previous revision -------------

/// The independent review's executed C3, closed. `SettlementCheck`'s
/// obligation-proof fields arrived without `#[serde(default)]`, so a
/// `FindingSettled` written by the previous revision (`0634657` and
/// before) failed to deserialize: the Work's canonical journal went
/// `JournalError malformed line`, the estate findings index went
/// `AtlasError … is malformed`, and `--rebuild` silently produced an
/// empty index. A canonical journal a newer binary cannot read is worse
/// than a refusal.
///
/// This drives the byte-for-byte shape the old binary really wrote
/// (`loop-b-obligation-correct/raw/02-red3-legacy-settlement-unreadable.txt`
/// records it from a real base build):
/// `{"ValidatedClaim":{"work":…,"claim":…,"claim_event":…}}` — no
/// `waypoint`, no `attempt`, no `world_hash`, no obligation, no proof.
///
/// The decisive real-binary version of this check is
/// `probes/g3-legacy-settlement-readable.sh`; this test is its
/// regression guard, so the shape cannot silently break again.
#[test]
fn a_settlement_journaled_by_the_previous_revision_stays_readable_and_reads_as_historical() {
    let dir = tempfile::tempdir().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();

    let work_id = WorkId("work-legacy-1".to_string());
    let run_id = RunId("run-legacy-1".to_string());
    let leaf = wirk_core::WaypointDefinition {
        id: WaypointId("wp-1".to_string()),
        kind: wirk_core::WaypointKind::Deterministic,
        declared_outputs: Vec::new(),
        intent: None,
        command: Some(vec!["true".to_string()]),
        boundary: wirk_core::Boundary(Vec::new()),
        leaves: Vec::new(),
        required_child_outcomes: Vec::new(),
        selection: None,
        // A base-era Route declared no obligation, because the concept
        // did not exist yet.
        verifies: None,
        orient: None,
    };
    raw_append(
        &estate,
        &work_id.0,
        None,
        EventKind::WorkSubmitted {
            route: wirk_core::RouteId("legacy".to_string()),
            repositories: vec![wirk_core::RepositoryBinding {
                name: "demo".to_string(),
                access: wirk_core::Access::Write,
            }],
            intent: "legacy".to_string(),
            waypoints: vec![leaf.id.clone()],
            waypoint_defs: vec![leaf],
            parent: None,
            execution_repo: None,
            execution_identity: None,
        },
    );
    raw_append(
        &estate,
        &work_id.0,
        Some(&run_id.0),
        EventKind::RunOpened {
            run: run_id.clone(),
            waypoint: WaypointId("wp-1".to_string()),
            attempt: 1,
            world_hash: WorldHash("legacy".to_string()),
        },
    );
    raw_append(
        &estate,
        &work_id.0,
        Some(&run_id.0),
        EventKind::ClaimRecorded {
            claim: ClaimId("claim-legacy-1".to_string()),
            claim_kind: ClaimKind::Done,
            verdict: ClaimVerdict::Validated,
            artifacts: Vec::new(),
        },
    );
    let claim_event_id = journal_events(&estate, &work_id.0)
        .into_iter()
        .find_map(|event| {
            matches!(&event.kind, EventKind::ClaimRecorded { .. }).then_some(event.id)
        })
        .expect("the ClaimRecorded event just appended");
    let finding_id = FindingId("finding-legacy-1".to_string());
    let finding = Finding {
        id: finding_id.clone(),
        work: work_id.clone(),
        run: run_id.clone(),
        waypoint: WaypointId("wp-1".to_string()),
        kind: FindingKind::VerifiedOutcome,
        scope: FindingScope::EstateLocal,
        claim: "the deterministic leaf ran".to_string(),
        evidence: vec![AdmittedEvidence {
            reference: EvidenceRef::Journal {
                work: work_id.clone(),
                event: claim_event_id.clone(),
            },
            outcome: EvidenceOutcome::Admitted {
                generation: work_id.0.clone(),
                object_id: claim_event_id.0.clone(),
            },
        }],
        contradicts: Vec::new(),
        applies_to: Vec::new(),
        supersedes: None,
        proposed_change: None,
        obligation: None,
        confirmed_by: None,
    };
    raw_append(
        &estate,
        &work_id.0,
        Some(&run_id.0),
        EventKind::FindingRaised { finding },
    );

    // The settlement, written as raw JSON in exactly the shape the
    // previous revision emitted — never through today's types, which
    // could not produce it.
    let journal_path = estate.join("works").join(&work_id.0).join("journal.ndjson");
    let existing = fs::read_to_string(&journal_path).unwrap();
    let next_seq = existing.lines().count() as u64 + 1;
    let legacy_line = format!(
        r#"{{"seq":{next_seq},"event":{{"id":"01LEGACYSETTLEDEVENT00000000","work":"{}","run":null,"at":1,"kind":{{"kind":"FindingSettled","finding":"{}","settlement":{{"authority":{{"class":"deterministic_verified","policy_version":1,"policy_digest":"6660a354469c47fd14812d5326b242ea560046ed26679730e7dc9a1e000ef16a"}},"check":{{"ValidatedClaim":{{"work":"{}","claim":"claim-legacy-1","claim_event":"{}"}}}},"settled_by":"{}","at":1,"minted_at_startup":false}}}}}}}}"#,
        work_id.0, finding_id.0, work_id.0, claim_event_id.0, claim_event_id.0
    );
    fs::write(&journal_path, format!("{existing}{legacy_line}\n")).unwrap();

    // It deserializes, it folds, and the Work is readable.
    let events = journal_events(&estate, &work_id.0);
    assert!(
        events
            .iter()
            .any(|event| matches!(event.kind, EventKind::FindingSettled { .. })),
        "the previous revision's own FindingSettled must still replay"
    );
    let folded = wirk_core::fold(&events);
    let record = folded
        .findings
        .get(&finding_id)
        .expect("the historical finding folds");
    let wirk_core::FindingState::Settled(settlement) = &record.state else {
        panic!("the historical settlement must still fold as Settled");
    };
    assert_eq!(settlement.authority.policy_version, 1);
    assert_eq!(
        settlement.authority.policy_digest,
        "6660a354469c47fd14812d5326b242ea560046ed26679730e7dc9a1e000ef16a"
    );
    let wirk_core::SettlementCheck::ValidatedClaim { proof, claim, .. } = &settlement.check else {
        panic!("expected the historical ValidatedClaim check");
    };
    assert_eq!(claim, &ClaimId("claim-legacy-1".to_string()));
    assert!(
        proof.is_none(),
        "a settlement the previous revision wrote carries no obligation proof, \
         and none is invented for it"
    );

    // The daemon reads that estate: status, list and the index all work,
    // and the historical settlement renders as historical — never as a
    // zero-valued obligation and never as newly verified.
    let (wirkd_child, pointer) = start_wirkd(&estate);
    let reply = status(&pointer.socket, &work_id.0);
    assert!(
        reply["state"].is_string(),
        "the base-era Work's own status is readable again: {reply}"
    );

    let (code, listed, stderr) = finding_cli(&estate, &["list", "--admin"]);
    assert_eq!(code, Some(0), "{stderr}");
    let historical = listed["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == finding_id.0)
        .expect("the historical finding is listed");
    let settled = &historical["settled"];
    assert!(settled.is_object(), "still settled: {historical}");
    assert_eq!(settled["authority"]["policy"]["policy_version"], 1);
    assert!(!settled["proves"]["recorded"].as_bool().unwrap());
    assert!(settled["proves"]["statement"].is_null());
    assert!(settled["proves"]["obligation"].is_null());
    assert!(
        settled["proves"]["historical"]
            .as_str()
            .unwrap()
            .contains("carries no obligation-proof fields"),
        "the reader states what this record holds, never what the past failed to record: {settled}"
    );
    assert!(
        settled["proves"]["unread_by_this_revision"].is_null(),
        "a base-era record really does carry nothing unread: {settled}"
    );
    assert_eq!(
        settled["check"]["claim"].as_str().unwrap(),
        "claim-legacy-1",
        "the facts the previous revision did record are still readable"
    );
    assert_eq!(settled["check"]["obligation"]["recorded"], false);

    // The estate index rebuilds from that journal and keeps the row —
    // it does not silently drop it.
    let (ok, index, err) = atlas(&estate, &["findings", "--rebuild", "--admin"]);
    assert!(ok, "{err}");
    let rows = index["rows"].as_array().unwrap();
    assert_eq!(
        rows.len(),
        1,
        "the historical row survives a rebuild: {index}"
    );
    assert_eq!(rows[0]["finding"]["id"].as_str().unwrap(), finding_id.0);
    assert_eq!(rows[0]["settlement"]["proves"]["recorded"], false);

    // And a plain read of the freshly written index works too.
    let (ok, index, err) = atlas(&estate, &["findings", "--admin"]);
    assert!(ok, "{err}");
    assert_eq!(index["rows"].as_array().unwrap().len(), 1);

    stop_wirkd(&estate, wirkd_child);
}

// ---- 11. restart under a changed policy never rewrites history --------

/// W-B-CORRECT.md defect 5 ("recovery and currentness"): changing the
/// admitted policy across a restart must not rewrite an
/// already-`FindingSettled` decision's own recorded `policy_digest`, and
/// a *new* settlement minted after that restart must honestly carry the
/// policy actually in force at *that* decision — never the old one, and
/// never silently. Two Works, two policy files, one restart between
/// them: Work 1 settles under policy A before the restart; Work 2's own
/// trigger already exists but its Finding is raised only after the
/// restart, under policy B.
#[test]
fn restart_under_a_changed_policy_never_rewrites_an_existing_settlement() {
    let dir = tempfile::tempdir().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    write_two_leaf_route(&estate);
    let (wirkd_child, pointer) = start_wirkd(&estate);

    // Work 1: settles inline, under policy A, before any restart.
    let repo_1 = dir.path().join("repo-1");
    init_repo(&repo_1);
    let work_1 = submit(&estate, "two_leaf", &repo_1, &["demo:write"], None).unwrap();
    write_file(&repo_1, "out1.md", "one\n");
    claim_ok(&estate, &work_1.work_id, &work_1.run_id, "out1.md=out1.md");
    // Policy A admits Work 1's own obligation basis. Each Work here has
    // its own repository and so its own real `base_sha`, which the World
    // hash — and therefore the admitted basis — genuinely covers.
    let basis_1 = obligation_basis_for(&estate, &work_1.work_id, "wp-1");
    write_policy_admitting(
        &estate,
        "deterministic_verified",
        "verified_outcome",
        &[("out1-produced", "1", &basis_1)],
    );
    let policy_a = fs::read_to_string(estate.join("policy").join("settlement.json")).unwrap();
    let claim_1 = claim_event_id(&estate, &work_1.work_id);
    let evidence_1 = format!("work/{}/event/{claim_1}", work_1.work_id);
    let wp2_run_1 = status(&pointer.socket, &work_1.work_id)["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    let (code, raised_1, stderr) = raise_cli(
        &estate,
        &work_1.work_id,
        &wp2_run_1,
        &[
            "--kind",
            "verified_outcome",
            "--scope",
            "estate_local",
            "--claim",
            "work 1's deterministic leaf ran",
            "--evidence",
            &evidence_1,
            "--obligation",
            "out1-produced@1",
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        raised_1["settled"].is_object(),
        "expected inline settlement under policy A: {raised_1}"
    );
    let digest_a = raised_1["settled"]["authority"]["policy"]["policy_digest"]
        .as_str()
        .unwrap()
        .to_string();
    let finding_1 = raised_1["id"].as_str().unwrap().to_string();

    // Work 2: its own trigger (a real Validated Done Claim) already
    // exists before the restart, but no Finding names it yet.
    let repo_2 = dir.path().join("repo-2");
    init_repo(&repo_2);
    let work_2 = submit(&estate, "two_leaf", &repo_2, &["demo:write"], None).unwrap();
    write_file(&repo_2, "out1.md", "one\n");
    claim_ok(&estate, &work_2.work_id, &work_2.run_id, "out1.md=out1.md");
    let claim_2 = claim_event_id(&estate, &work_2.work_id);
    let wp2_run_2 = status(&pointer.socket, &work_2.work_id)["run_id"]
        .as_str()
        .unwrap()
        .to_string();

    stop_wirkd(&estate, wirkd_child);

    // A different, real policy file: same class stays enabled (so Work
    // 2's Finding can still settle), but the bytes — and so the digest —
    // genuinely differ.
    // The obligation basis is content-addressed, never location-
    // addressed: two throwaway repositories whose base commits happen to
    // hash identically give the same basis, and both Works then discharge
    // the *same* admitted check. That is correct — and it is why this
    // policy admits the basis set rather than asserting the two differ
    // (an earlier draft did assert that, and it was flaky exactly
    // because `git commit --allow-empty` in the same second produces the
    // same commit id).
    let basis_2 = obligation_basis_for(&estate, &work_2.work_id, "wp-1");
    let policy_b = format!(
        r#"{{"version":2,"classes":[{{"class":"deterministic_verified","scope":"estate_local","kinds":["verified_outcome"],"obligations":[{{"id":"out1-produced","edition":"1","basis":"{basis_1}"}},{{"id":"out1-produced","edition":"1","basis":"{basis_2}"}}]}},{{"class":"superseded_in_origin","scope":"estate_local","kinds":["gap"],"obligations":[]}}]}}"#
    );
    assert_ne!(policy_a, policy_b);
    write_policy(&estate, &policy_b);
    let (wirkd_child, _pointer) = start_wirkd(&estate);

    // The restart's own startup sweep must not touch Work 1's already-
    // settled finding at all.
    let (code, listed_1, stderr) = finding_cli(
        &estate,
        &[
            "list",
            "--requesting-work",
            &work_1.work_id,
            "--work",
            &work_1.work_id,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let after_restart = listed_1["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["id"] == finding_1)
        .expect("work 1's finding survives the restart");
    assert_eq!(
        after_restart["settled"]["authority"]["policy"]["policy_digest"],
        serde_json::json!(digest_a),
        "an existing settlement's own recorded policy digest must never be rewritten by a later restart"
    );

    // Now, after the restart, Work 2's Finding is raised for the first
    // time — its trigger predates the restart, but the decision itself
    // happens under policy B, and must say so honestly.
    let evidence_2 = format!("work/{}/event/{claim_2}", work_2.work_id);
    let (code, raised_2, stderr) = raise_cli(
        &estate,
        &work_2.work_id,
        &wp2_run_2,
        &[
            "--kind",
            "verified_outcome",
            "--scope",
            "estate_local",
            "--claim",
            "work 2's deterministic leaf ran",
            "--evidence",
            &evidence_2,
            "--obligation",
            "out1-produced@1",
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        raised_2["settled"].is_object(),
        "expected inline settlement under policy B: {raised_2}"
    );
    let digest_b = raised_2["settled"]["authority"]["policy"]["policy_digest"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(
        digest_a, digest_b,
        "a decision made under the new policy must record the new policy's own digest, never the old one"
    );

    stop_wirkd(&estate, wirkd_child);
}

// ---- 8b. Application: the caller's own current admitted authority -------

fn source_git(repo: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .args(args)
            .current_dir(repo)
            .status()
            .unwrap()
            .success(),
        "git {args:?} in {}",
        repo.display()
    );
}

/// A real Git source with one interesting resource, acquired and
/// published as generation 1 — the shape every Application case below
/// starts from. `untouched_tail` sits outside any span a search hit
/// returns, so a whole-resource digest and a snippet digest differ.
fn publish_source(dir: &Path, estate: &Path, alias: &str, name: &str) -> std::path::PathBuf {
    let source_repo = dir.join(name);
    fs::create_dir_all(&source_repo).unwrap();
    source_git(&source_repo, &["init", "-q"]);
    source_git(&source_repo, &["config", "user.email", "wb@example.test"]);
    source_git(&source_repo, &["config", "user.name", "wb"]);
    fs::write(
        source_repo.join("bind.rs"),
        "fn bind_socket() { /* mode 0775 */ }\nfn untouched_tail() { /* outside every hit span */ }\n",
    )
    .unwrap();
    source_git(&source_repo, &["add", "."]);
    source_git(&source_repo, &["commit", "-q", "-m", "one"]);
    let (ok, acquired, err) = atlas(
        estate,
        &[
            "acquire",
            "--source",
            alias,
            "--repository",
            source_repo.to_str().unwrap(),
            "--revision",
            "HEAD",
        ],
    );
    assert!(ok, "{err}");
    let generation = acquired["generation"]["generation"]
        .as_str()
        .unwrap()
        .to_string();
    let (ok, _, err) = atlas(
        estate,
        &["publish", "--source", alias, "--generation", &generation],
    );
    assert!(ok, "{err}");
    source_repo
}

/// Commits `repo`'s current worktree and republishes it as the source's
/// next generation, returning the new revision.
fn republish(estate: &Path, repo: &Path, alias: &str, message: &str) -> String {
    source_git(repo, &["add", "-A", "."]);
    source_git(repo, &["commit", "-q", "-m", message]);
    let (ok, acquired, err) = atlas(
        estate,
        &["refresh", "--source", alias, "--revision", "HEAD"],
    );
    assert!(ok, "{err}");
    let generation = acquired["generation"]["generation"]
        .as_str()
        .unwrap()
        .to_string();
    let (ok, _, err) = atlas(
        estate,
        &["publish", "--source", alias, "--generation", &generation],
    );
    assert!(ok, "{err}");
    let rev = Command::new("git")
        .args(["-C", repo.to_str().unwrap(), "rev-parse", "HEAD"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&rev.stdout).trim().to_string()
}

/// The `applies_to`/`evidence` coordinate for `bind_socket`, as this
/// Work's own admitted Atlas search resolves it.
fn bind_coordinate(estate: &Path, work_id: &str) -> String {
    let (ok, search, err) = atlas(
        estate,
        &["search", "--work", work_id, "--query", "bind_socket"],
    );
    assert!(ok, "{err}");
    search["hits"][0]["coordinate"]
        .as_str()
        .expect("the Work's own admitted search resolves bind_socket")
        .to_string()
}

fn raise_bind_finding(estate: &Path, work_id: &str, run_id: &str, coordinate: &str) -> String {
    let (code, raised, stderr) = raise_cli(
        estate,
        work_id,
        run_id,
        &[
            "--kind",
            "contradicted_assumption",
            "--scope",
            "estate_local",
            "--claim",
            "bind_socket sets no explicit mode",
            "--evidence",
            coordinate,
            "--applies-to",
            coordinate,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    raised["id"].as_str().unwrap().to_string()
}

/// `applied_cli`, asserting the call was refused and returning the
/// daemon's own public refusal line (`Refused: <Code> <message>`) — a
/// negative here names the exact check that fired, never "some exit 3".
fn applied_refusal(estate: &Path, work: &str, run: &str, args: &[&str]) -> String {
    let mut full = vec!["finding", "applied"];
    full.extend_from_slice(args);
    let output = Command::new(wirk_bin())
        .args(&full)
        .env("WIRK_ESTATE_ROOT", estate)
        .env("WIRK_WORK_ID", work)
        .env("WIRK_RUN_ID", run)
        .output()
        .expect("wirk finding applied runs");
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    assert_eq!(
        output.status.code(),
        Some(3),
        "expected a refusal; stdout={stdout} stderr={stderr}"
    );
    stdout
}

/// The number of Applications this finding's own record currently
/// carries, read back through the public scoped list.
fn applications_of(
    estate: &Path,
    requester: &str,
    work: &str,
    finding: &str,
) -> Vec<serde_json::Value> {
    let (code, listed, stderr) = finding_cli(
        estate,
        &["list", "--requesting-work", requester, "--work", work],
    );
    assert_eq!(code, Some(0), "{stderr}");
    listed["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["id"] == finding)
        .expect("the finding is listed")["applied"]
        .as_array()
        .unwrap()
        .clone()
}

/// A container with two leaves and one *optional* `helper` role: the
/// smallest Route that gives a parent Work a still-open producing Run
/// after its first leaf is claimed, and a role a child can legally be
/// spawned under. No obligation is declared — this fixture exists for
/// lineage, not settlement.
fn write_lineage_container_route(estate: &Path) {
    route_fixture::write_route(
        estate,
        "lineage_container",
        r#"{"id":"lineage-container","waypoints":[
            {"id":"outer","kind":"Container",
             "declared_outputs":[{"name":"a.md","required":true}],
             "required_child_outcomes":[{"role":"helper","required":false}],
             "leaves":[
               {"id":"outer/leaf-a","kind":"Deterministic","command":["sh","-c","echo a > a.md"],
                "declared_outputs":[{"name":"a.md","required":true}]},
               {"id":"outer/leaf-b","kind":"Deterministic","command":["sh","-c","echo b > b.md"],
                "declared_outputs":[{"name":"b.md","required":true}]}
             ]}
        ]}"#,
    );
}

/// W-B-APPLICATION-REPAIR.md, "A new actor-attributed implementation
/// assertion has an actual current admitted producing Work/Run/World"
/// and "The producer must itself admit the referenced source/evidence;
/// an origin Work's broader admission is not the caller's grant".
///
/// Five executed reds against the frozen candidate f685b4b, every one
/// of which wrote a durable `FindingApplied` there: a *spent* Run (its
/// own Claim already recorded, and still the latest attempt for its
/// Waypoint, which is exactly why the candidate accepted it), a failed
/// Run, a canceled Work, a caller holding no grant of its own on the
/// changed source, and — the journal-write boundary ruling 0101 already
/// imposed on `finding assert` but never on this verb — an off-lineage
/// caller appending to another Work's journal.
#[test]
fn application_requires_a_current_admitted_producer_and_the_callers_own_grants() {
    let dir = tempfile::tempdir().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    write_two_leaf_route(&estate);
    write_lineage_container_route(&estate);
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let source_repo = publish_source(dir.path(), &estate, "demo", "source-repo");

    let work_repo = dir.path().join("work-repo");
    init_repo(&work_repo);
    // The owner runs the container Route so that claiming its first
    // leaf leaves a *second*, still-open producing Run behind — the
    // spent-Run red and the green positive then differ only in which
    // Run the caller names. It binds `other` as well as `demo` purely
    // so a child can legally be narrowed to `other` alone below (a
    // child's bindings are a subset of its parent's, `ChildExceeds    // ParentBinding`).
    let owner = submit(
        &estate,
        "lineage_container",
        &work_repo,
        &["demo:write", "other:write"],
        None,
    )
    .unwrap();
    assert_eq!(owner.waypoint, "outer/leaf-a");
    let coordinate = bind_coordinate(&estate, &owner.work_id);
    let finding = raise_bind_finding(&estate, &owner.work_id, &owner.run_id, &coordinate);

    // The real change an Application would describe.
    fs::write(
        source_repo.join("bind.rs"),
        "fn bind_socket() { set_permissions(0o600); }\nfn untouched_tail() { /* outside every hit span */ }\n",
    )
    .unwrap();
    let rev_2 = republish(&estate, &source_repo, "demo", "two");

    let args: Vec<String> = vec![
        "--finding".into(),
        finding.clone(),
        "--source".into(),
        "demo".into(),
        "--revision".into(),
        rev_2.clone(),
        "--by".into(),
        "root".into(),
    ];
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();

    // RED 1: `wp-1`'s Run has recorded its own Validated Done Claim and
    // is spent. It is still `latest_run_for_waypoint(wp-1)` — the only
    // currency the candidate checked — but ruling 0095 already decided
    // that a spent action produces no new assertion.
    write_file(&work_repo, "a.md", "a\n");
    claim_ok(&estate, &owner.work_id, &owner.run_id, "a.md=a.md");
    let wp2_run = status(&pointer.socket, &owner.work_id)["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(wp2_run, owner.run_id);
    let refusal = applied_refusal(&estate, &owner.work_id, &owner.run_id, &argv);
    assert!(
        refusal.contains("ProducingActionMismatch"),
        "a spent Run must not produce a new assertion: {refusal}"
    );

    // RED 2: a failed Run — and then the superseded attempt after a
    // retry, which the candidate already refused (preserved).
    let failed = submit(&estate, "two_leaf", &work_repo, &["demo:write"], None).unwrap();
    fail_via_socket(&pointer.socket, &estate, &failed.work_id, &failed.run_id);
    let refusal = applied_refusal(&estate, &failed.work_id, &failed.run_id, &argv);
    assert!(
        refusal.contains("NoAdmittedProducingAction"),
        "a failed Run must not produce a new assertion: {refusal}"
    );
    let (code, out) = retry_run_cli(&estate, &failed.work_id, &failed.run_id);
    assert_eq!(code, Some(0), "{out}");
    let retried = status(&pointer.socket, &failed.work_id)["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(retried, failed.run_id);
    let refusal = applied_refusal(&estate, &failed.work_id, &failed.run_id, &argv);
    assert!(
        refusal.contains("ProducingActionMismatch"),
        "a superseded attempt must not produce a new assertion: {refusal}"
    );

    // RED 3: a canceled Work. Its Run id still exists; nothing is being
    // produced by it.
    let canceled = submit(&estate, "two_leaf", &work_repo, &["demo:write"], None).unwrap();
    let (code, out) = cancel_cli(&estate, &canceled.work_id, false);
    assert_eq!(code, Some(0), "{out}");
    let refusal = applied_refusal(&estate, &canceled.work_id, &canceled.run_id, &argv);
    assert!(
        refusal.contains("NoAdmittedProducingAction"),
        "a canceled Work's Run must not produce a new assertion: {refusal}"
    );

    // RED 4: a real, current producing action that *is* on the
    // finding's own lineage — so the journal boundary below admits it —
    // holding no grant of its own on `demo`. The candidate resolved the
    // membership against the *finding owner's* bindings alone, so this
    // child borrowed an authority it never held.
    let child_repo = dir.path().join("child-repo");
    init_repo(&child_repo);
    let ungranted = submit(
        &estate,
        "two_leaf",
        &child_repo,
        &["other:write"],
        Some(ParentRef {
            work: &owner.work_id,
            waypoint: "outer",
            run: &wp2_run,
            role: "helper",
            attempt: None,
        }),
    )
    .unwrap();
    let refusal = applied_refusal(&estate, &ungranted.work_id, &ungranted.run_id, &argv);
    assert!(
        refusal.contains("not admitted by this producing work's own bindings"),
        "a caller with no grant on the changed source must not record an Application: {refusal}"
    );

    // RED 5: a real, current, `demo:write`-bound producing action
    // writing into a journal it holds no reference permission on.
    let stranger = submit(&estate, "two_leaf", &work_repo, &["demo:write"], None).unwrap();
    let refusal = applied_refusal(&estate, &stranger.work_id, &stranger.run_id, &argv);
    assert!(
        refusal.contains("is not this producing work's own journal or its parent/child lineage"),
        "an off-lineage caller must not append to another Work's journal: {refusal}"
    );

    // A forged triple with no Work or Run at all stays refused.
    let refusal = applied_refusal(&estate, "work-never-existed", "run-never-existed", &argv);
    assert!(refusal.contains("NotFound"), "{refusal}");

    // Green: the finding's own Work, on its own current open Run.
    let (code, applied, stderr) = applied_cli(&estate, &owner.work_id, &wp2_run, &argv);
    assert_eq!(code, Some(0), "{stderr}");
    let record = applied["applied"].as_array().unwrap().last().unwrap();
    assert_eq!(record["attribution"]["attribution"], "asserted");
    assert_eq!(record["attribution"]["verified"], serde_json::json!(false));
    assert_eq!(record["attribution"]["producer"]["work"], owner.work_id);
    assert_eq!(record["attribution"]["producer"]["run"], wp2_run);
    // The mechanical half names both resource identities explicitly.
    assert_eq!(record["before"]["resource"], "present");
    assert_eq!(record["after"]["resource"], "present");
    assert_ne!(record["before"]["object_id"], record["after"]["object_id"]);
    // The judgement stays an unverified, attributed assertion: no bare
    // `applied: true` anywhere in the reply.
    assert_eq!(
        record["implements_finding"]["verified"],
        serde_json::json!(false)
    );

    // Exactly one Application survived: no refusal wrote a record.
    assert_eq!(
        applications_of(&estate, &owner.work_id, &owner.work_id, &finding).len(),
        1,
        "every refusal above must have left the journal unchanged"
    );

    stop_wirkd(&estate, wirkd_child);
}

/// W-B-APPLICATION-REPAIR.md, "Show exact before/after resource
/// identity, deletion as explicit absence and ordered durable
/// Application history ... Immediate journal-first derived index
/// visibility and idempotent crash/restart reconciliation are required".
///
/// Four sequential real Atlas generations of one resource: fixed,
/// emptied, deleted, then a same-file edit that has nothing to do with
/// the finding. Every one is a separate durable Application; none
/// replaces an earlier one; an emptied file keeps a real zero-byte
/// object id and reads `present`, while a deletion reads `absent` with
/// no fabricated id; the estate index carries every row the moment the
/// journal does, with no rebuild; and a restart re-derives exactly the
/// same rows rather than duplicating them.
#[test]
fn ordered_applications_record_deletion_absence_and_reach_the_index_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    write_two_leaf_route(&estate);
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let source_repo = publish_source(dir.path(), &estate, "demo", "source-repo");
    let work_repo = dir.path().join("work-repo");
    init_repo(&work_repo);
    let work = submit(&estate, "two_leaf", &work_repo, &["demo:write"], None).unwrap();
    let coordinate = bind_coordinate(&estate, &work.work_id);
    let finding = raise_bind_finding(&estate, &work.work_id, &work.run_id, &coordinate);

    let apply = |revision: &str| {
        applied_cli(
            &estate,
            &work.work_id,
            &work.run_id,
            &[
                "--finding",
                &finding,
                "--source",
                "demo",
                "--revision",
                revision,
                "--by",
                "root",
            ],
        )
    };
    let index_rows = |requester: &str| -> Vec<serde_json::Value> {
        let (ok, listed, err) = atlas(&estate, &["findings", "--requesting-work", requester]);
        assert!(ok, "{err}");
        listed["rows"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["kind"] == "applied")
            .cloned()
            .collect()
    };

    // 1. The real fix.
    fs::write(
        source_repo.join("bind.rs"),
        "fn bind_socket() { set_permissions(0o600); }\nfn untouched_tail() { /* outside every hit span */ }\n",
    )
    .unwrap();
    let rev_fixed = republish(&estate, &source_repo, "demo", "fix");
    let (code, applied, stderr) = apply(&rev_fixed);
    assert_eq!(code, Some(0), "{stderr}");
    let fixed_object = applied["applied"].as_array().unwrap()[0]["after"]["object_id"]
        .as_str()
        .unwrap()
        .to_string();
    // Journal first, index second — and *now*, with no `--rebuild`.
    assert_eq!(
        index_rows(&work.work_id).len(),
        1,
        "an applied row must reach the estate index without a rebuild"
    );

    // 2. Emptied, not deleted: a real, zero-byte Git object.
    fs::write(source_repo.join("bind.rs"), "").unwrap();
    let rev_empty = republish(&estate, &source_repo, "demo", "empty");
    let (code, applied, stderr) = apply(&rev_empty);
    assert_eq!(code, Some(0), "{stderr}");
    let records = applied["applied"].as_array().unwrap();
    assert_eq!(records.len(), 2, "history is appended, never replaced");
    assert_eq!(records[1]["after"]["resource"], "present");
    assert_eq!(
        records[1]["after"]["object_id"], "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391",
        "an empty file has a real object id"
    );

    // 3. Deleted: explicit absence, never a fabricated id.
    fs::remove_file(source_repo.join("bind.rs")).unwrap();
    let rev_gone = republish(&estate, &source_repo, "demo", "delete");
    let (code, applied, stderr) = apply(&rev_gone);
    assert_eq!(code, Some(0), "{stderr}");
    let records = applied["applied"].as_array().unwrap();
    assert_eq!(records.len(), 3);
    assert_eq!(records[2]["after"]["resource"], "absent");
    assert_eq!(records[2]["after"]["object_id"], serde_json::Value::Null);
    assert_ne!(
        records[2]["after"]["object_id"], records[1]["after"]["object_id"],
        "a deletion and an empty file are different facts"
    );
    // The earlier Applications still say what they always said.
    assert_eq!(records[0]["after"]["object_id"], fixed_object);
    assert_eq!(records[0]["after"]["resource"], "present");

    // A Claim attribution cannot attest a deletion: there are no bytes
    // to digest, and saying so is not "the bytes differ".
    let refusal = applied_refusal(
        &estate,
        &work.work_id,
        &work.run_id,
        &[
            "--finding",
            &finding,
            "--source",
            "demo",
            "--revision",
            &rev_gone,
            "--by",
            "root",
            "--claim-run",
            &work.run_id,
        ],
    );
    assert!(
        refusal.contains("WrongClaim") || refusal.contains("DeletedResource"),
        "{refusal}"
    );

    // 4. A same-file edit with nothing to do with the finding. It is
    // recorded as exactly what it is: observed change plus an
    // unverified, attributed judgement. No bare `applied: true`, and
    // nothing anywhere in the reply claims the judgement is verified.
    fs::write(
        source_repo.join("bind.rs"),
        "fn bind_socket() { /* mode 0775 */ }\nfn untouched_tail() { /* something else entirely */ }\n",
    )
    .unwrap();
    let rev_unrelated = republish(&estate, &source_repo, "demo", "unrelated");
    let (code, applied, stderr) = apply(&rev_unrelated);
    assert_eq!(code, Some(0), "{stderr}");
    let records = applied["applied"].as_array().unwrap();
    assert_eq!(records.len(), 4);
    for record in records {
        assert_eq!(
            record["implements_finding"]["verified"],
            serde_json::json!(false)
        );
        assert_eq!(record["attribution"]["verified"], serde_json::json!(false));
    }
    let rendered = serde_json::to_string(&applied).unwrap();
    assert!(
        !rendered.contains("\"applied\":true"),
        "no bare applied=true anywhere in the reply: {rendered}"
    );

    // Every Application is a distinct, immediately visible index row.
    let before_restart = index_rows(&work.work_id);
    assert_eq!(
        before_restart.len(),
        4,
        "four applied rows, no rebuild, no restart"
    );
    let ids: std::collections::BTreeSet<String> = before_restart
        .iter()
        .map(|row| row["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids.len(), 4, "each Application is its own row");

    // Restart: the same rows, re-derived from the journals, idempotent.
    stop_wirkd(&estate, wirkd_child);
    let (wirkd_child, _pointer) = start_wirkd(&estate);
    let after_restart = index_rows(&work.work_id);
    assert_eq!(
        after_restart, before_restart,
        "restart reconciliation must be idempotent, never duplicating rows"
    );

    // And the administrative destructive rebuild agrees with both.
    let (ok, rebuilt, err) = atlas(&estate, &["findings", "--admin", "--rebuild"]);
    assert!(ok, "{err}");
    let rebuilt: Vec<serde_json::Value> = rebuilt["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["kind"] == "applied")
        .cloned()
        .collect();
    assert_eq!(rebuilt.len(), 4);
    assert_eq!(
        rebuilt
            .iter()
            .map(|row| row["id"].as_str().unwrap().to_string())
            .collect::<std::collections::BTreeSet<_>>(),
        ids,
        "a rebuild from every journal derives exactly the rows already there"
    );

    let _ = &pointer;
    stop_wirkd(&estate, wirkd_child);
}

/// W-B-APPLICATION-REPAIR.md, "Historical Claim evidence remains usable
/// as history: a validated closing Claim may complete its Work and still
/// prove exact recorded artifact production", and ruling 0101's
/// requirement that a later admitted Work be able to use prior learning
/// under scoped reference rules rather than through arbitrary
/// cross-journal writes.
///
/// The whole shape in one run: a container Work raises the Finding and
/// stays open; its own child really does the fix and closes with a
/// Validated Done Claim, completing itself; the container — a real,
/// current, admitted producing action of its own — then records the
/// Application citing that completed child's Claim across journals. The
/// caller and the cited receipt are two different identities, which is
/// exactly what lets the cited one be terminal. A sibling Work with an
/// identical, equally valid Claim is refused: kinship is a reference
/// rule, not a wildcard.
#[test]
fn a_later_admitted_work_cites_a_completed_childs_closing_claim() {
    let dir = tempfile::tempdir().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    write_lineage_container_route(&estate);
    route_fixture::write_route(
        &estate,
        "fix_leaf",
        r#"{"id":"fix-leaf","waypoints":[
            {"id":"fix","kind":"Deterministic","command":["true"],"declared_outputs":[{"name":"bind.rs","required":true}]}
        ]}"#,
    );
    let (wirkd_child, pointer) = start_wirkd(&estate);

    // The container executes in the source repository itself, so its own
    // child can legally declare the same execution binding.
    let repo = publish_source(dir.path(), &estate, "demo", "repo");
    let container = submit(&estate, "lineage_container", &repo, &["demo:write"], None).unwrap();
    assert_eq!(container.waypoint, "outer/leaf-a");
    let coordinate = bind_coordinate(&estate, &container.work_id);
    let finding = raise_bind_finding(&estate, &container.work_id, &container.run_id, &coordinate);

    // The child really does the fix and really closes.
    let fixer = submit(
        &estate,
        "fix_leaf",
        &repo,
        &["demo:write"],
        Some(ParentRef {
            work: &container.work_id,
            waypoint: "outer",
            run: &container.run_id,
            role: "helper",
            attempt: None,
        }),
    )
    .unwrap();
    let fixed = "fn bind_socket() { set_permissions(0o600); }\nfn untouched_tail() { /* outside every hit span */ }\n";
    write_file(&repo, "bind.rs", fixed);
    claim_ok(&estate, &fixer.work_id, &fixer.run_id, "bind.rs=bind.rs");
    assert_eq!(
        state_of(&pointer.socket, &fixer.work_id),
        "completed",
        "the child's Claim closed its own Work: that is the history being cited"
    );
    let rev_2 = republish(&estate, &repo, "demo", "fix");

    let cite = |work: &str, run: &str, claim_work: &str, claim_run: &str| {
        applied_cli(
            &estate,
            work,
            run,
            &[
                "--finding",
                &finding,
                "--source",
                "demo",
                "--revision",
                &rev_2,
                "--by",
                "the-container",
                "--claim-run",
                claim_run,
                "--claim-work",
                claim_work,
            ],
        )
    };

    // The completed child's own triple cannot make the assertion: it is
    // history now, not a producing action.
    let refusal = applied_refusal(
        &estate,
        &fixer.work_id,
        &fixer.run_id,
        &[
            "--finding",
            &finding,
            "--source",
            "demo",
            "--revision",
            &rev_2,
            "--by",
            "the-child",
        ],
    );
    assert!(
        refusal.contains("NoAdmittedProducingAction") && refusal.contains("terminal"),
        "a completed Work's past Claims are history, not a producer: {refusal}"
    );

    // A sibling Work with an equally real, equally valid Claim — over
    // the same path, in the same checkout, with the same digest, so
    // every mechanical check it faces passes — is still not on this
    // caller's lineage, and cannot be cited.
    let sibling = submit(&estate, "fix_leaf", &repo, &["demo:write"], None).unwrap();
    claim_ok(
        &estate,
        &sibling.work_id,
        &sibling.run_id,
        "bind.rs=bind.rs",
    );
    let refusal = applied_refusal(
        &estate,
        &container.work_id,
        &container.run_id,
        &[
            "--finding",
            &finding,
            "--source",
            "demo",
            "--revision",
            &rev_2,
            "--by",
            "the-container",
            "--claim-run",
            &sibling.run_id,
            "--claim-work",
            &sibling.work_id,
        ],
    );
    assert!(
        refusal.contains("the cited claim's work is not this producing work's own journal or its parent/child lineage"),
        "kinship is a reference rule, not a wildcard: {refusal}"
    );

    // Green: the container's own current action, citing its completed
    // child's closing Claim across journals.
    let (code, applied, stderr) = cite(
        &container.work_id,
        &container.run_id,
        &fixer.work_id,
        &fixer.run_id,
    );
    assert_eq!(code, Some(0), "{stderr}");
    let record = applied["applied"].as_array().unwrap().last().unwrap();
    assert_eq!(record["attribution"]["attribution"], "claim");
    assert_eq!(record["attribution"]["work"], fixer.work_id);
    assert_eq!(record["attribution"]["run"], fixer.run_id);
    // The judgement is the *caller's*, and stays unverified; the cited
    // Claim is the mechanical half and names a different Work.
    assert_eq!(
        record["implements_finding"]["by"].as_str().unwrap(),
        "recorded name: the-container, unverified"
    );
    assert_eq!(
        record["implements_finding"]["verified"],
        serde_json::json!(false)
    );

    stop_wirkd(&estate, wirkd_child);
}

/// The independent Application verification's F-1
/// (`loop-b-application-verify/APPLICATION-VERDICT.md`), reproduced as a
/// standing guard rather than a one-off probe.
///
/// The frozen candidate read `current_producing_action` from an unlocked
/// `replay_events` at the top of `handle_finding_applied` and appended
/// under a *different* Work's lock much later, with a real Git object
/// read and an Atlas resolution in between. One thread per connection
/// meant a concurrent transition on the producer's own Work landed
/// inside that window: the reviewer's `race2.sh` put a `FindingApplied`
/// after its own producer's `RunFailed` in 15 of 39 journals.
///
/// This sweeps the window instead of hoping to land in it. Every racer
/// raises its finding against generation 1 *before* the single
/// republish, so every one of them really reaches the append path — the
/// reviewer's own voided `raw/09`/`raw/10` refused for
/// `GenerationUnchanged` and never got there, which is why the green
/// control below is asserted first and separately. Each trial then
/// starts the real `wirk finding applied` process and fires the
/// concurrent transition after a different delay, walking the whole
/// window a millisecond at a time.
///
/// The assertion is the linearization property itself, and it is
/// deterministic on a correct daemon whoever wins: a journal may record
/// the Application, or the transition that superseded its producer, or
/// both — but never the Application *after* the transition. On the
/// pre-fix tree it is red.
fn assert_no_application_after_its_producers_transition(
    estate: &Path,
    work_id: &str,
    transition: &str,
) {
    let events = journal_events(estate, work_id);
    let applied_at = events
        .iter()
        .position(|event| matches!(event.kind, EventKind::FindingApplied { .. }));
    let transition_at = events.iter().position(|event| match &event.kind {
        EventKind::RunFailed { .. } => transition == "fail",
        EventKind::WorkCanceled { .. } => transition == "cancel",
        EventKind::ClaimRecorded {
            verdict: ClaimVerdict::Validated,
            ..
        } => transition == "claim",
        _ => false,
    });
    if let (Some(applied_at), Some(transition_at)) = (applied_at, transition_at) {
        assert!(
            applied_at < transition_at,
            "work {work_id}: a FindingApplied was appended at seq {applied_at} after the \
             {transition} at seq {transition_at} that superseded its own producing Run — the \
             assertion this verb refuses outright when the same call arrives a millisecond \
             later. Journal: {:?}",
            events
                .iter()
                .map(|event| format!("{:?}", std::mem::discriminant(&event.kind)))
                .collect::<Vec<_>>()
        );
    }
}

/// Waits for a spawned child, failing loudly rather than hanging forever
/// if the daemon deadlocked: this file's races hold more than one Work
/// journal lock at once, and a lock-order inversion's only symptom is a
/// test run that never ends.
fn wait_within(mut child: std::process::Child, secs: u64, what: &str) -> Option<i32> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    loop {
        match child.try_wait().expect("try_wait") {
            Some(status) => return status.code(),
            None if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                panic!("{what} did not finish within {secs}s: the daemon is deadlocked");
            }
            None => std::thread::sleep(std::time::Duration::from_millis(20)),
        }
    }
}

fn spawn_applied(estate: &Path, work_id: &str, run_id: &str, args: &[&str]) -> std::process::Child {
    let mut full = vec!["finding", "applied"];
    full.extend_from_slice(args);
    Command::new(wirk_bin())
        .args(&full)
        .env("WIRK_ESTATE_ROOT", estate)
        .env("WIRK_WORK_ID", work_id)
        .env("WIRK_RUN_ID", run_id)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("wirk finding applied spawns")
}

/// `spawn_applied`, keeping the process's own output. The deterministic
/// races below assert the *named* refusal a parked-then-released call
/// returns, never "some non-zero exit" — `applied_refusal`'s own
/// discipline, on a call that has to be started before it can be
/// answered.
fn spawn_applied_capturing(
    estate: &Path,
    work_id: &str,
    run_id: &str,
    args: &[&str],
) -> std::process::Child {
    let mut full = vec!["finding", "applied"];
    full.extend_from_slice(args);
    Command::new(wirk_bin())
        .args(&full)
        .env("WIRK_ESTATE_ROOT", estate)
        .env("WIRK_WORK_ID", work_id)
        .env("WIRK_RUN_ID", run_id)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("wirk finding applied spawns")
}

/// Waits for a spawned `finding applied` and returns its exit code and
/// its own public output, failing loudly rather than hanging if the
/// daemon deadlocked — `wait_within`'s reason, on a piped child.
fn finish_applied(child: std::process::Child, secs: u64, what: &str) -> (Option<i32>, String) {
    let output = wait_output_within(child, secs, what);
    let mut text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if !stderr.is_empty() {
        text.push(' ');
        text.push_str(&stderr);
    }
    (output.status.code(), text)
}

fn wait_output_within(
    mut child: std::process::Child,
    secs: u64,
    what: &str,
) -> std::process::Output {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    loop {
        match child.try_wait().expect("try_wait") {
            Some(_) => return child.wait_with_output().expect("collect output"),
            None if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                panic!("{what} did not finish within {secs}s: the daemon is deadlocked");
            }
            None => std::thread::sleep(std::time::Duration::from_millis(20)),
        }
    }
}

/// The argv fragment of the `git` call `wirkd` makes **after** it has
/// read the producer's authority out of an unlocked replay and
/// **before** it takes a single journal lock: `atlas.resolve_exact`'s
/// own re-verification of the finding's before-state blob
/// (`wirk-atlas/src/git.rs::blob`). Parking here puts a request exactly
/// in the window `APPLICATION-VERDICT.md` F-1 measured 15 stale appends
/// through.
const GIT_INSIDE_THE_UNLOCKED_WINDOW: &str = "cat-file blob";

/// The argv fragment of the one `git` call `wirkd` makes with every
/// journal guard for this append already held, between re-deriving the
/// producer's authority and appending: `server.rs::read_blob`, reached
/// only on the `--claim-run` attribution path. Parking here holds the
/// producer's journal lock open, which is what a concurrent failure
/// must then queue behind.
const GIT_INSIDE_THE_HELD_LOCKS: &str = "cat-file -p";

#[test]
fn an_application_is_never_minted_from_a_producer_a_concurrent_request_superseded() {
    let dir = tempfile::tempdir().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    write_two_leaf_route(&estate);
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let source_repo = publish_source(dir.path(), &estate, "demo", "source-repo");
    let work_repo = dir.path().join("work-repo");
    init_repo(&work_repo);

    // Every racer, and its finding, against generation 1 — before the
    // republish below. `race2.sh`'s own correction: a finding raised
    // against the *current* generation is refused `GenerationUnchanged`
    // and never reaches the append path at all.
    const TRIALS: usize = 14;
    let mut racers = Vec::new();
    for _ in 0..=TRIALS {
        let racer = submit(&estate, "two_leaf", &work_repo, &["demo:write"], None).unwrap();
        let coordinate = bind_coordinate(&estate, &racer.work_id);
        let finding = raise_bind_finding(&estate, &racer.work_id, &racer.run_id, &coordinate);
        racers.push((racer, finding));
    }

    fs::write(
        source_repo.join("bind.rs"),
        "fn bind_socket() { set_permissions(0o600); }\nfn untouched_tail() { /* outside every hit span */ }\n",
    )
    .unwrap();
    let rev_2 = republish(&estate, &source_repo, "demo", "two");

    let argv_for = |finding: &str| -> Vec<String> {
        vec![
            "--finding".into(),
            finding.to_string(),
            "--source".into(),
            "demo".into(),
            "--revision".into(),
            rev_2.clone(),
            "--by".into(),
            "racer".into(),
        ]
    };

    // The green control, first and alone: with no concurrent
    // transition this exact call succeeds and records exactly one
    // Application. Without it, every "refused" below would prove
    // nothing about a race — the reviewer's voided probe's own lesson.
    let (control, control_finding) = &racers[0];
    let args = argv_for(control_finding);
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let (code, _, stderr) = applied_cli(&estate, &control.work_id, &control.run_id, &argv);
    assert_eq!(code, Some(0), "the no-race control must succeed: {stderr}");
    assert_eq!(
        applications_of(&estate, &control.work_id, &control.work_id, control_finding).len(),
        1,
        "the control records exactly one Application"
    );

    // Now sweep the window. `fail`, `cancel` and a closing `claim` are
    // three different ways the producing action stops being current,
    // and all three go through the same unlocked-read window the
    // candidate left open.
    let mut succeeded = 0usize;
    let mut refused = 0usize;
    for (index, (racer, finding)) in racers.iter().enumerate().skip(1) {
        let transition = ["fail", "cancel", "claim"][index % 3];
        let args = argv_for(finding);
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        let child = spawn_applied(&estate, &racer.work_id, &racer.run_id, &argv);
        std::thread::sleep(std::time::Duration::from_millis((index as u64 % 7) * 3));
        match transition {
            "fail" => fail_via_socket(&pointer.socket, &estate, &racer.work_id, &racer.run_id),
            "cancel" => {
                let _ = cancel_cli(&estate, &racer.work_id, false);
            }
            _ => {
                write_file(&work_repo, "out1.md", "one\n");
                let _ = claim(
                    &estate,
                    &racer.work_id,
                    &racer.run_id,
                    &["--artifact", "out1.md=out1.md"],
                );
            }
        }
        match wait_within(child, 30, "wirk finding applied") {
            Some(0) => succeeded += 1,
            _ => refused += 1,
        }
        assert_no_application_after_its_producers_transition(&estate, &racer.work_id, transition);
    }

    // Liveness, so a daemon that simply refused everything could never
    // pass this test by refusing: the window really was straddled from
    // both sides.
    assert!(
        succeeded + refused == TRIALS,
        "every racer returned: {succeeded} + {refused} != {TRIALS}"
    );
    assert!(
        succeeded > 0,
        "no racer ever won: the sweep never reached the append path, so it proves nothing"
    );

    stop_wirkd(&estate, wirkd_child);
}

/// The same boundary where the caller and the finding's owner are
/// *different* Works, which is where the fix has to take two journal
/// locks at once rather than one.
///
/// Two things are asserted. The linearization property still holds when
/// the producer's journal is not the journal being appended to. And the
/// daemon does not deadlock doing it: `journal_lock_order` puts an
/// ancestor before its descendant, the same direction `settle_ready`
/// and `close_cascade` already take, and both directions are driven
/// here concurrently — a parent applying to its child's finding and a
/// child applying to its parent's — with a bounded wait, because a
/// lock-order inversion's only symptom is a run that never ends.
///
/// The currency half is then executed twice, *ordered* rather than
/// raced (0121; `git_gate`): once with nothing contending, which must
/// succeed, and once with the producer's failure landing — issued,
/// answered and journaled — entirely inside the window between this
/// verb's unlocked authority read and its first journal lock, which
/// must refuse and leave the child's journal with no `FindingApplied`
/// at all. Its companion,
/// `a_failure_that_arrives_mid_append_is_ordered_behind_the_application_it_races`,
/// covers the other side: a failure arriving after the re-derivation,
/// with the append's own locks held.
#[test]
fn a_cross_journal_application_holds_its_producers_currency_without_deadlocking() {
    let dir = tempfile::tempdir().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    write_two_leaf_route(&estate);
    write_lineage_container_route(&estate);
    let gate = GitGate::install(&dir.path().join("gate"));
    let (wirkd_child, pointer) = start_wirkd_with_path(&estate, Some(&gate.path_env()));

    let source_repo = publish_source(dir.path(), &estate, "demo", "source-repo");
    let work_repo = dir.path().join("work-repo");
    init_repo(&work_repo);

    // A parent with a still-open producing Run, and a real spawned
    // child holding its own `demo:write` grant — the same repository
    // the parent bound it to, since a child's bindings must resolve to
    // its parent's own (`ChildExceedsParentBinding`). Both raise a
    // finding of their own against generation 1.
    let parent = submit(
        &estate,
        "lineage_container",
        &work_repo,
        &["demo:write"],
        None,
    )
    .unwrap();
    let child = submit(
        &estate,
        "two_leaf",
        &work_repo,
        &["demo:write"],
        Some(ParentRef {
            work: &parent.work_id,
            waypoint: "outer",
            run: &parent.run_id,
            role: "helper",
            attempt: None,
        }),
    )
    .unwrap();

    let parent_coordinate = bind_coordinate(&estate, &parent.work_id);
    let parent_finding =
        raise_bind_finding(&estate, &parent.work_id, &parent.run_id, &parent_coordinate);
    let child_coordinate = bind_coordinate(&estate, &child.work_id);
    let child_finding =
        raise_bind_finding(&estate, &child.work_id, &child.run_id, &child_coordinate);

    fs::write(
        source_repo.join("bind.rs"),
        "fn bind_socket() { set_permissions(0o600); }\nfn untouched_tail() { /* outside every hit span */ }\n",
    )
    .unwrap();
    let rev_2 = republish(&estate, &source_repo, "demo", "two");

    let argv_for = |finding: &str| -> Vec<String> {
        vec![
            "--finding".into(),
            finding.to_string(),
            "--source".into(),
            "demo".into(),
            "--revision".into(),
            rev_2.clone(),
            "--by".into(),
            "cross".into(),
        ]
    };

    // Both directions at once, each naming the *other* Work's finding:
    // the parent is the ancestor in one and the descendant journal
    // being written in the other. If the two requests took their two
    // locks in opposite orders, this is where they would meet.
    let up = argv_for(&child_finding);
    let up_argv: Vec<&str> = up.iter().map(String::as_str).collect();
    let down = argv_for(&parent_finding);
    let down_argv: Vec<&str> = down.iter().map(String::as_str).collect();
    let a = spawn_applied(&estate, &parent.work_id, &parent.run_id, &up_argv);
    let b = spawn_applied(&estate, &child.work_id, &child.run_id, &down_argv);
    let a_code = wait_within(a, 30, "parent applying to its child's finding");
    let b_code = wait_within(b, 30, "child applying to its parent's finding");
    assert_eq!(
        a_code,
        Some(0),
        "a parent on its own current Run may apply to its child's finding"
    );
    assert_eq!(
        b_code,
        Some(0),
        "a child on its own current Run may apply to its parent's finding"
    );
    assert_eq!(
        applications_of(&estate, &child.work_id, &child.work_id, &child_finding).len(),
        1
    );
    assert_eq!(
        applications_of(&estate, &parent.work_id, &parent.work_id, &parent_finding).len(),
        1
    );

    // ---- The cross-journal currency itself, ordered rather than raced.
    //
    // What replaced what, and why. This used to be six pairs, each
    // started and then chased by a `fail` after a `(index % 6) * 3`ms
    // sleep, scored by `won > 0` and asserting `applied_at <= failed_at`
    // only `if let (Some, Some)`. That is not a test: under load every
    // racer can lose (a spurious red 0119 already recorded as the
    // builder's own named flake), and a run in which exactly one racer
    // wins satisfies `won > 0` while the safety assertion is *skipped*
    // on the other five. 0121 requires the same contract proved by
    // deterministic coordination across the real in-flight window, with
    // both events mandatory.
    //
    // The coordination is `git_gate` (see that module for the R2-R5
    // reasoning): no product instrumentation, no new flag, no env var
    // wirk reads. The daemon already shells out to the platform `git`
    // *inside* the window — `atlas.resolve_exact` re-verifies the
    // finding's before-state blob after the unlocked producer read and
    // before the first journal lock — so a wrapper on the daemon's own
    // `PATH` parks the request exactly there, and the test decides when
    // it resumes. Every byte the daemon reads is real Git's.
    // Two more parent/child pairs, every child's finding raised against
    // generation 1 before the republish below — the same fixture the
    // sweep built, sized to the two ordered cases that replace it: one
    // uncontested control and one contested run.
    let mut pairs = Vec::new();
    for _ in 0..2 {
        let p = submit(
            &estate,
            "lineage_container",
            &work_repo,
            &["demo:write"],
            None,
        )
        .unwrap();
        let c = submit(
            &estate,
            "two_leaf",
            &work_repo,
            &["demo:write"],
            Some(ParentRef {
                work: &p.work_id,
                waypoint: "outer",
                run: &p.run_id,
                role: "helper",
                attempt: None,
            }),
        )
        .unwrap();
        let coordinate = bind_coordinate(&estate, &c.work_id);
        let f = raise_bind_finding(&estate, &c.work_id, &c.run_id, &coordinate);
        pairs.push((p, c, f));
    }
    const THIRD: &str = "fn bind_socket() { set_permissions(0o600); /* third */ }\nfn untouched_tail() { /* outside every hit span */ }\n";
    fs::write(source_repo.join("bind.rs"), THIRD).unwrap();
    let rev_3 = republish(&estate, &source_repo, "demo", "three");

    let cross_argv = |finding: &str| -> Vec<String> {
        vec![
            "--finding".into(),
            finding.to_string(),
            "--source".into(),
            "demo".into(),
            "--revision".into(),
            rev_3.clone(),
            "--by".into(),
            "cross".into(),
        ]
    };

    // The uncontested control, first and alone. The identical call is
    // parked at the identical point and released with nothing racing
    // it: it must succeed and record exactly one Application. Without
    // it, the refusal below would prove nothing about a race — it could
    // be the gate itself refusing the call.
    let (control_parent, control_child, control_finding) = &pairs[0];
    let args = cross_argv(control_finding);
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    gate.arm(GIT_INSIDE_THE_UNLOCKED_WINDOW);
    let control = spawn_applied_capturing(
        &estate,
        &control_parent.work_id,
        &control_parent.run_id,
        &argv,
    );
    gate.wait_until_parked("the uncontested control sits in the pre-lock window");
    gate.release();
    let (code, output) = finish_applied(control, 60, "the uncontested cross-journal control");
    assert_eq!(
        code,
        Some(0),
        "the parked-but-uncontested control must still succeed: {output}"
    );
    assert_eq!(
        applications_of(
            &estate,
            &control_child.work_id,
            &control_child.work_id,
            control_finding
        )
        .len(),
        1,
        "the control records exactly one Application in the child's journal"
    );

    // And the contested one. The producer's failure lands *entirely
    // inside* the window — issued, answered and journaled while the
    // Application is provably parked before its first journal lock —
    // and only then is the Application let go. On the pre-fix shape
    // (authority read once, unlocked, at the top and never re-derived)
    // this is the exact interleaving that minted a `FindingApplied`
    // into a child's journal after its own producer had been failed in
    // another journal. Nothing here is a wall-clock guess: the ordering
    // is program order across the gate.
    let (parent, child, finding) = &pairs[1];
    let args = cross_argv(finding);
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    gate.arm(GIT_INSIDE_THE_UNLOCKED_WINDOW);
    let racer = spawn_applied_capturing(&estate, &parent.work_id, &parent.run_id, &argv);
    gate.wait_until_parked("the cross-journal racer sits in the pre-lock window");
    fail_via_socket(&pointer.socket, &estate, &parent.work_id, &parent.run_id);
    let failed_at = journal_events(&estate, &parent.work_id)
        .into_iter()
        .find(|event| matches!(event.kind, EventKind::RunFailed { .. }))
        .map(|event| event.at.0)
        .expect(
            "the producer's failure must be durable in its own journal before the Application \
             is released: without that this proves nothing",
        );
    assert!(
        gate.is_parked(),
        "the Application must still be inside the window the failure just crossed"
    );
    gate.release();
    let (code, output) = finish_applied(racer, 60, "the cross-journal racer");
    assert_eq!(
        code,
        Some(3),
        "an Application whose producer was failed inside the window must be refused: {output}"
    );
    assert!(
        output.contains("NoAdmittedProducingAction"),
        "the refusal must be the sequential path's own authority refusal, not a race-only code: \
         {output}"
    );
    let applied: Vec<Timestamp> = journal_events(&estate, &child.work_id)
        .into_iter()
        .filter(|event| matches!(event.kind, EventKind::FindingApplied { .. }))
        .map(|event| event.at)
        .collect();
    assert!(
        applied.is_empty(),
        "child {} carries a FindingApplied minted at {applied:?} from a producer {} that was \
         failed at {failed_at} in a different journal, before this append could take a lock",
        child.work_id,
        parent.work_id
    );
    assert!(
        applications_of(&estate, &child.work_id, &child.work_id, finding).is_empty(),
        "and the public record shows no Application either"
    );

    stop_wirkd(&estate, wirkd_child);
}

/// The other side of the same boundary, and the half no sequential
/// control can reach: a producer failure that arrives while the
/// Application is **already past** its own authority re-derivation and
/// inside the locked section, on its way to the append.
///
/// "Apply, then fail" and "fail, then apply" run one after the other
/// and prove only that each sequential answer is right. The contract
/// 0121 names is stronger — the producer's authority is *held*, not
/// merely re-read: a failure that arrives after the re-check cannot
/// interleave between it and the append. That needs a real concurrent
/// failure and a real in-flight append, which is what this stages.
///
/// The stage. `wirkd` makes exactly one `git` call with every journal
/// guard for this append already held — `read_blob`, resolving the
/// cited Claim's own after-bytes, reached on the `--claim-run`
/// attribution path — and it sits between the re-derivation of the
/// producer's current producing action and the `FindingApplied` append
/// itself. Parking there (`git_gate`, no product instrumentation) holds
/// the producer's journal lock open. A `fail` issued at that moment is
/// genuinely in flight against a genuinely in-flight Application, and
/// the daemon has to serialize it — the whole property, executed.
///
/// Both events are mandatory here, and the ordering is asserted from
/// observations rather than guessed from two clocks: the failure is
/// issued while the Application is parked, and it is proved to have
/// completed only *after* the gate released — so it cannot have
/// preceded the append it is ordered behind. The journal timestamps are
/// then checked too, as a second, weaker witness.
#[test]
fn a_failure_that_arrives_mid_append_is_ordered_behind_the_application_it_races() {
    let dir = tempfile::tempdir().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    write_lineage_container_route(&estate);
    route_fixture::write_route(
        &estate,
        "fix_leaf",
        r#"{"id":"fix-leaf","waypoints":[
            {"id":"fix","kind":"Deterministic","command":["true"],"declared_outputs":[{"name":"bind.rs","required":true}]}
        ]}"#,
    );
    let gate = GitGate::install(&dir.path().join("gate"));
    let (wirkd_child, pointer) = start_wirkd_with_path(&estate, Some(&gate.path_env()));

    // The container executes in the source repository itself, so its own
    // child can legally declare the same execution binding and its
    // Claim really is a receipt over the published source
    // (`a_later_admitted_work_cites_a_completed_childs_closing_claim`'s
    // own fixture, R2).
    let repo = publish_source(dir.path(), &estate, "demo", "repo");
    let container = submit(&estate, "lineage_container", &repo, &["demo:write"], None).unwrap();
    let fixer = submit(
        &estate,
        "fix_leaf",
        &repo,
        &["demo:write"],
        Some(ParentRef {
            work: &container.work_id,
            waypoint: "outer",
            run: &container.run_id,
            role: "helper",
            attempt: None,
        }),
    )
    .unwrap();

    // The finding is the *child's*, so the append below writes into a
    // different journal than the producer's: two journals locked at
    // once, the case `journal_lock_order` exists for.
    let coordinate = bind_coordinate(&estate, &fixer.work_id);
    let finding = raise_bind_finding(&estate, &fixer.work_id, &fixer.run_id, &coordinate);

    let fixed = "fn bind_socket() { set_permissions(0o600); }\nfn untouched_tail() { /* outside every hit span */ }\n";
    write_file(&repo, "bind.rs", fixed);
    claim_ok(&estate, &fixer.work_id, &fixer.run_id, "bind.rs=bind.rs");
    assert_eq!(
        state_of(&pointer.socket, &fixer.work_id),
        "completed",
        "the child's Claim closed its own Work: that is the receipt being cited"
    );
    let rev_2 = republish(&estate, &repo, "demo", "fix");

    let args = [
        "--finding".to_string(),
        finding.clone(),
        "--source".to_string(),
        "demo".to_string(),
        "--revision".to_string(),
        rev_2.clone(),
        "--by".to_string(),
        "the-container".to_string(),
        "--claim-run".to_string(),
        fixer.run_id.clone(),
        "--claim-work".to_string(),
        fixer.work_id.clone(),
    ];
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();

    gate.arm(GIT_INSIDE_THE_HELD_LOCKS);
    let applying = spawn_applied_capturing(&estate, &container.work_id, &container.run_id, &argv);
    gate.wait_until_parked("the Application is inside its own locked section");

    // Issued now, against a producer whose journal this append is
    // holding. A daemon that did not hold that authority through the
    // append would answer it immediately.
    let socket = pointer.socket.clone();
    let estate_for_fail = estate.clone();
    let work = container.work_id.clone();
    let run = container.run_id.clone();
    let failing = std::thread::spawn(move || {
        fail_via_socket(&socket, &estate_for_fail, &work, &run);
        std::time::Instant::now()
    });

    // The causal observation, made before anything is released: while
    // this append's own locks are held, that failure cannot be
    // answered. A daemon that re-derived the producer's authority and
    // then let its journal go before appending answers it inside this
    // window; one that holds it cannot answer until `release` below.
    // The window is bounded and negative — it orders nothing, it only
    // watches — and the mandatory event checks further down do not
    // depend on it.
    let watch_until = std::time::Instant::now() + std::time::Duration::from_millis(1500);
    while std::time::Instant::now() < watch_until {
        assert!(
            !failing.is_finished(),
            "the concurrent failure was answered while the Application was still inside its \
             own locked section: the producer's authority was checked but not held"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    let released_at = gate.release();
    let (code, output) = finish_applied(applying, 60, "the mid-append Application");
    let failed_returned_at = failing
        .join()
        .expect("the concurrent failure returns rather than deadlocking");
    assert!(
        failed_returned_at > released_at,
        "the concurrent failure was answered before the Application's own locked section \
         ended: the producer's authority was not held through the append"
    );

    assert_eq!(
        code,
        Some(0),
        "the Application itself must succeed: {output}"
    );
    let applied: Vec<Timestamp> = journal_events(&estate, &fixer.work_id)
        .into_iter()
        .filter(|event| matches!(event.kind, EventKind::FindingApplied { .. }))
        .map(|event| event.at)
        .collect();
    assert_eq!(
        applied.len(),
        1,
        "exactly one FindingApplied must be minted in the finding's own journal"
    );
    let failed: Vec<Timestamp> = journal_events(&estate, &container.work_id)
        .into_iter()
        .filter(|event| matches!(event.kind, EventKind::RunFailed { .. }))
        .map(|event| event.at)
        .collect();
    assert_eq!(
        failed.len(),
        1,
        "and the concurrent failure must really have been journaled: both events are required"
    );
    assert!(
        applied[0].0 <= failed[0].0,
        "the Application at {} is stamped after the failure at {} that it was ordered ahead of",
        applied[0].0,
        failed[0].0
    );

    // The record is the cited receipt's, exactly as the sequential path
    // writes it — a raced append is not a different, weaker Application.
    let record = applications_of(&estate, &container.work_id, &fixer.work_id, &finding);
    assert_eq!(record.len(), 1);
    assert_eq!(record[0]["attribution"]["attribution"], "claim");
    assert_eq!(record[0]["attribution"]["work"], fixer.work_id);
    assert_eq!(record[0]["attribution"]["run"], fixer.run_id);

    stop_wirkd(&estate, wirkd_child);
}

/// F-2, the retired flag. `--claim` was meaningful on this verb before
/// the repair split the caller from the Claim it cites; the CLI's
/// tolerate-unknown-arguments convention then turned an explicit
/// request for *checked Claim attribution* into a silent exit-0
/// recording of the strictly weaker unverified `asserted` attribution.
/// A caller who asked for the stronger record must not be handed the
/// weaker one without being told.
#[test]
fn the_retired_claim_flag_is_refused_with_its_migration_rather_than_downgraded() {
    let dir = tempfile::tempdir().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();

    let output = Command::new(wirk_bin())
        .args([
            "finding",
            "applied",
            "--finding",
            "finding-x",
            "--source",
            "demo",
            "--revision",
            "deadbeef",
            "--by",
            "root",
            "--claim",
            "run-1",
        ])
        .env("WIRK_ESTATE_ROOT", &estate)
        .env("WIRK_WORK_ID", "work-x")
        .env("WIRK_RUN_ID", "run-x")
        .output()
        .expect("wirk finding applied runs");
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert_eq!(
        output.status.code(),
        Some(1),
        "an explicit retired --claim is refused, not silently downgraded: {stderr}"
    );
    assert!(
        stderr.contains("--claim-run"),
        "the refusal must name the migration: {stderr}"
    );
    assert!(
        stderr.contains("retired"),
        "the refusal must say the flag is retired: {stderr}"
    );
    // And it refuses before the daemon is ever contacted: no `wirkd` is
    // running in this estate at all, and this is still exit 1, not the
    // exit 2 an unreachable daemon gives.

    // The convention it deliberately does not change: an unrelated
    // unknown flag is still tolerated, exactly as everywhere else in
    // this binary. This one reaches the daemon and fails there instead.
    let output = Command::new(wirk_bin())
        .args([
            "finding",
            "applied",
            "--finding",
            "finding-x",
            "--source",
            "demo",
            "--revision",
            "deadbeef",
            "--by",
            "root",
            "--totally-bogus-flag",
        ])
        .env("WIRK_ESTATE_ROOT", &estate)
        .env("WIRK_WORK_ID", "work-x")
        .env("WIRK_RUN_ID", "run-x")
        .output()
        .expect("wirk finding applied runs");
    assert_ne!(
        output.status.code(),
        Some(1),
        "no general unknown-flag rejector was introduced: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// ---- 12. Supported public access to the basis a policy must admit ------
//
// `loop-b-basis-access`, from the integrated review's §5.1: the
// settlement mechanism worked, and configuring it did not. An operator
// writing `policy/settlement.json` must admit an obligation's content
// `basis`; for an `Actor` (and a `Deterministic`) Waypoint that value
// binds the **reserved World hash**, which exists only after submit.
// It was rendered only inside an already-settled record, and the
// `pending` reply named `obligation-basis-not-admitted` without ever
// naming the value — so the reviewer had to reimplement
// `obligation_basis` out of band in Python to write a policy the daemon
// would accept. These tests pin the supported read-only path that
// closes that, and pin just as hard that it stays *inspection*.

/// `wirk work obligations` through the real CLI. Returns the exit code,
/// the parsed JSON reply (`Null` when the call was refused) and stderr,
/// the same triple every other CLI helper in this file returns.
fn obligations_cli(estate: &Path, args: &[&str]) -> (Option<i32>, serde_json::Value, String) {
    let mut full = vec!["work", "obligations", "--estate"];
    let estate_str = estate.to_str().unwrap();
    full.push(estate_str);
    full.extend_from_slice(args);
    full.push("--json");
    let output = Command::new(wirk_bin())
        .args(&full)
        .output()
        .expect("wirk work obligations runs");
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    (
        output.status.code(),
        serde_json::from_str(&stdout).unwrap_or(serde_json::Value::Null),
        stderr,
    )
}

/// The one entry for `waypoint` in a `work obligations` reply.
fn obligation_entry<'a>(reply: &'a serde_json::Value, waypoint: &str) -> &'a serde_json::Value {
    reply["obligations"]
        .as_array()
        .unwrap_or_else(|| panic!("the reply carries an obligations array: {reply}"))
        .iter()
        .find(|entry| entry["waypoint"].as_str() == Some(waypoint))
        .unwrap_or_else(|| panic!("no obligation entry for {waypoint}: {reply}"))
}

/// Every byte of estate state this verb must not move: each Work's own
/// journal, the derived findings index, and the settlement policy file.
fn estate_fingerprint(estate: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut walk = vec![estate.to_path_buf()];
    while let Some(dir) = walk.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk.push(path);
            } else if let Ok(bytes) = fs::read(&path) {
                out.push((path.to_string_lossy().to_string(), bytes));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// An Actor review Route with a caller-chosen obligation statement, so
/// two Routes can differ in exactly one authored field and nothing else.
fn write_basis_route(estate: &Path, name: &str, proves: &str) {
    route_fixture::write_route(
        estate,
        name,
        &format!(
            r#"{{"id":"{name}","waypoints":[
            {{"id":"review","kind":"Actor","intent":"Independently review whether bind_socket restricts the wirkd socket mode, and record the decision",
             "declared_outputs":[{{"name":"review.md","required":true}}],
             "boundary":["**"],
             "verifies":{{"id":"socket-mode-reviewed","edition":"1",
               "proves":"{proves}",
               "outputs":["review.md"],
               {REVIEW_CONTRACT}}}}},
            {{"id":"file","kind":"Actor","intent":"file the reviewer's own finding",
             "declared_outputs":[{{"name":"done.md","required":true}}],"boundary":["**"]}}
        ]}}"#
        ),
    );
}

/// **The decisive green.** The exact sequence the integrated review
/// could not perform: an operator, holding nothing but the public CLI,
/// reads the canonical basis for a really reserved obligation, admits
/// *that value* in `policy/settlement.json` by hand, and a genuine
/// validated review receipt then settles.
///
/// The value is deliberately never transcribed from the in-process
/// `obligation_basis_for` helper into the policy: the policy is written
/// from **what the CLI printed**, and the helper is used only to assert
/// afterwards that the CLI printed the canonical value. That ordering is
/// the whole point — a surface that agreed with the helper but not with
/// the daemon would settle nothing.
///
/// Four states are asserted apart, because conflating any two of them is
/// how a digest becomes a proof: the authored obligation, the reserved
/// basis, the policy admission, and the check's readiness.
#[test]
fn work_obligations_discloses_the_reserved_basis_an_operator_admits_by_hand() {
    let dir = tempfile::tempdir().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    let intent = "Independently review whether bind_socket restricts the wirkd socket mode, and record the decision";
    write_actor_review_route(&estate, "socket_review", intent, r#"["review.md"]"#);
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let repo = dir.path().join("demo-repo");
    init_repo(&repo);
    write_file(&repo, "socket.rs", "fn bind_socket() { socketmarker }\n");
    let target_coordinate = publish_and_locate(&estate, &repo, "socketmarker");

    let work = submit_kind(
        &estate,
        "socket_review",
        &repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .unwrap();

    // (a) Before anything is reserved further, the authored obligation
    // and the reserved basis are already separable, and no policy
    // exists.
    let (code, reply, stderr) = obligations_cli(&estate, &["--work", &work.work_id, "--admin"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(reply["scope"].as_str().unwrap(), "administrative");
    assert_eq!(reply["policy"]["state"].as_str().unwrap(), "absent");
    let entry = obligation_entry(&reply, "review");
    assert_eq!(entry["waypoint_kind"].as_str().unwrap(), "actor");
    assert_eq!(
        entry["obligation"]["id"].as_str().unwrap(),
        "socket-mode-reviewed"
    );
    assert_eq!(entry["obligation"]["edition"].as_str().unwrap(), "1");
    assert_eq!(
        entry["obligation"]["review"]["recipe"].as_str().unwrap(),
        "socket-boundary-review@3",
        "the authored contract is rendered as authored content, never as authority"
    );
    assert_eq!(entry["mechanism"]["kind"].as_str().unwrap(), "actor_review");
    assert!(entry["mechanism"]["present"].as_bool().unwrap());
    assert_eq!(
        entry["mechanism"]["review_targets"]["declared"]
            .as_u64()
            .unwrap(),
        1
    );
    assert_eq!(
        entry["mechanism"]["review_targets"]["frozen"]
            .as_u64()
            .unwrap(),
        1,
        "the selector really resolved and froze into the reserved World"
    );
    assert!(
        entry["mechanism"]["review_targets"]["resolved"]
            .as_bool()
            .unwrap()
    );
    assert_eq!(entry["reservation"]["state"].as_str().unwrap(), "reserved");
    assert_eq!(entry["basis"]["state"].as_str().unwrap(), "available");
    assert_eq!(
        entry["admission"]["state"].as_str().unwrap(),
        "no-policy-file",
        "with no policy file the admission state says exactly that, and nothing reads as admitted"
    );
    assert!(
        entry["admission"]["admitted_by"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        entry["findings"].as_array().unwrap().is_empty(),
        "no finding names this obligation yet, so there is no readiness to report"
    );

    // (b) The operator's own value, read off the public reply.
    let disclosed = entry["basis"]["basis"].as_str().unwrap().to_string();
    assert_eq!(
        disclosed,
        obligation_basis_for(&estate, &work.work_id, "review"),
        "the disclosed value is the canonical `wirk_core::obligation_basis`, not a second digest"
    );
    assert_eq!(
        entry["reservation"]["world_hash"].as_str().unwrap(),
        journal_events(&estate, &work.work_id)
            .into_iter()
            .rev()
            .find_map(|event| match event.kind {
                EventKind::WaypointReserved {
                    waypoint,
                    world_hash,
                    ..
                } if waypoint.0 == "review" => Some(world_hash.0),
                _ => None,
            })
            .unwrap(),
        "the execution basis the digest binds is named beside it"
    );

    // The review really runs.
    let worktree = materialize_actor(&pointer.socket, &estate, &work.work_id, &work.run_id);
    fs::write(
        worktree.join("review.md"),
        "recipe socket-boundary-review@3\ndecision: contradicted_assumption\nbind_socket never narrows the socket mode.\n",
    )
    .unwrap();
    claim_ok(&estate, &work.work_id, &work.run_id, "review.md=review.md");

    // (c) Inspection admits nothing on its own: the review has run and
    // the obligation is still not admitted.
    let (_, reply, _) = obligations_cli(&estate, &["--work", &work.work_id, "--admin"]);
    assert_eq!(
        obligation_entry(&reply, "review")["admission"]["state"]
            .as_str()
            .unwrap(),
        "no-policy-file",
        "reading the basis never admits it"
    );

    // (d) The operator admits *the value the CLI printed*, by hand.
    write_review_policy(
        &estate,
        r#"["contradicted_assumption"]"#,
        "socket-mode-reviewed",
        "1",
        &disclosed,
    );
    let (_, reply, _) = obligations_cli(&estate, &["--work", &work.work_id, "--admin"]);
    let entry = obligation_entry(&reply, "review");
    assert_eq!(reply["policy"]["state"].as_str().unwrap(), "loaded");
    assert_eq!(reply["policy"]["version"].as_u64().unwrap(), 2);
    assert_eq!(entry["admission"]["state"].as_str().unwrap(), "admitted");
    let admitted_by = &entry["admission"]["admitted_by"][0];
    assert_eq!(admitted_by["class"].as_str().unwrap(), "actor_reviewed");
    assert_eq!(admitted_by["scope"].as_str().unwrap(), "work_local");
    assert_eq!(
        admitted_by["kinds"][0].as_str().unwrap(),
        "contradicted_assumption",
        "the entry names the scope and kinds settlement will also match, so a wrongly scoped admission is visible here"
    );

    // (e) Admission is still not readiness, and readiness is not
    // settlement: nothing has settled because no Finding exists yet.
    assert!(
        entry["findings"].as_array().unwrap().is_empty(),
        "an admitted basis with no finding settles nothing: {entry}"
    );

    // (f) The genuine receipt now settles, through the ordinary verbs.
    let claim_event = claim_event_id(&estate, &work.work_id);
    let evidence = format!("work/{}/event/{claim_event}", work.work_id);
    let file_run = status(&pointer.socket, &work.work_id)["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    let (code, settled, stderr) = raise_cli(
        &estate,
        &work.work_id,
        &file_run,
        &[
            "--scope",
            "work_local",
            "--evidence",
            &evidence,
            "--kind",
            "contradicted_assumption",
            "--claim",
            "the socket is bound with a wider mode than the trust boundary states",
            "--applies-to",
            &target_coordinate,
            "--obligation",
            "socket-mode-reviewed@1",
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        settled["settled"].is_object(),
        "the basis read from the public CLI is the value that actually settles: {settled}"
    );
    assert_eq!(
        settled["settled"]["check"]["obligation"]["basis"]
            .as_str()
            .unwrap(),
        disclosed,
        "the settled receipt carries exactly the basis the operator was shown"
    );

    // (g) The fifth distinction: the settled receipt, reported as such
    // and separate from readiness.
    let (_, reply, _) = obligations_cli(&estate, &["--work", &work.work_id, "--admin"]);
    let entry = obligation_entry(&reply, "review");
    let finding = &entry["findings"][0];
    assert_eq!(
        finding["settled"]["state"].as_str().unwrap(),
        "settled",
        "{entry}"
    );
    assert_eq!(
        finding["settled"]["class"].as_str().unwrap(),
        "actor_reviewed"
    );
    assert_eq!(
        finding["ready"]["state"].as_str().unwrap(),
        "already-settled",
        "a settled finding reports itself as settled rather than as `not-ready`, which would read as a contradiction beside it: {finding}"
    );

    stop_wirkd(&estate, wirkd_child);
}

/// `work obligations`'s `findings[].ready.reason` for a `not-ready`
/// Finding, and `finding settle`'s own `pending.reason` for the same
/// Finding, must name the identical cause — this is what
/// `diagnose/investigate`'s finding-18d39153def38ae4-6 established was
/// missing (`handle_work_obligations` sent a bare `{"state":
/// "not-ready"}` with no reason at all) and what `not_ready_reason`
/// (`server.rs`) now computes once for both verbs.
#[test]
fn work_obligations_not_ready_reason_matches_finding_settle_pending_reason() {
    let dir = tempfile::tempdir().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    write_two_leaf_route(&estate);
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let repo = dir.path().join("repo");
    init_repo(&repo);
    let work = submit(&estate, "two_leaf", &repo, &["demo:write"], None).unwrap();
    write_file(&repo, "out1.md", "one\n");
    claim_ok(&estate, &work.work_id, &work.run_id, "out1.md=out1.md");

    // Evidence naming the `WorkSubmitted` event, not the `ClaimRecorded`
    // one wp-1's own Claim produced: `deterministic_verified_readiness`
    // (`wirk-core/src/lib.rs`) requires the cited event to be that
    // Waypoint's own Validated `Done` Claim, so citing a different event
    // kind leaves the check genuinely unheld — a true `not-ready`, never
    // a `ready`-but-unadmitted case that would land in the other arm.
    let submitted_id = submitted_event_id(&estate, &work.work_id);
    let evidence = format!("work/{}/event/{submitted_id}", work.work_id);
    let wp2_run = status(&pointer.socket, &work.work_id)["run_id"]
        .as_str()
        .unwrap()
        .to_string();

    // No `policy/settlement.json` exists at all, so `finding settle`
    // would report `no-policy-file` — the plainest of the pending
    // reasons and enough to pin that `work obligations` now carries the
    // same value rather than a bare `not-ready` with nothing beside it.
    let (code, raised, stderr) = raise_cli(
        &estate,
        &work.work_id,
        &wp2_run,
        &[
            "--kind",
            "verified_outcome",
            "--scope",
            "estate_local",
            "--evidence",
            &evidence,
            "--claim",
            "out1.md was produced as wp-1 declared",
            "--obligation",
            "out1-produced@1",
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        raised["settled"].is_null(),
        "no policy admits anything yet: {raised}"
    );
    let finding_id = raised["id"].as_str().unwrap().to_string();

    let (_, settle_pending, _) =
        finding_cli(&estate, &["settle", "--finding", &finding_id, "--admin"]);
    let settle_reason = settle_pending["pending"]["reason"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(settle_reason, "no-policy-file", "{settle_pending}");

    let (code, reply, stderr) = obligations_cli(&estate, &["--work", &work.work_id, "--admin"]);
    assert_eq!(code, Some(0), "{stderr}");
    let entry = obligation_entry(&reply, "wp-1");
    let finding = entry["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["finding"].as_str() == Some(finding_id.as_str()))
        .unwrap_or_else(|| panic!("no finding entry for {finding_id}: {entry}"));
    assert_eq!(finding["ready"]["state"].as_str().unwrap(), "not-ready");
    assert_eq!(
        finding["ready"]["reason"].as_str().unwrap(),
        settle_reason,
        "`work obligations` must name the identical reason `finding settle` computes for the same finding: {finding}"
    );

    // The human renderer prints the reason beside the `not-ready` state,
    // the same pattern the adjacent `basis unavailable` line already
    // uses for `entry["basis"]["reason"]`.
    let output = Command::new(wirk_bin())
        .args([
            "work",
            "obligations",
            "--estate",
            estate.to_str().unwrap(),
            "--work",
            &work.work_id,
            "--admin",
        ])
        .output()
        .expect("wirk work obligations runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("not-ready") && stdout.contains("reason: no-policy-file"),
        "human output should print the not-ready reason: {stdout}"
    );

    stop_wirkd(&estate, wirkd_child);
}

/// Inspection is inspection. Nothing this verb does appends an event,
/// writes the derived findings index, or edits the settlement policy —
/// asserted over every byte of the estate, before and after, in the
/// three states where a mutation would be easiest to hide: no policy at
/// all, an admitted policy with a ready check that has *not* been
/// settled through `finding settle`, and after settlement.
#[test]
fn work_obligations_never_mutates_journal_catalog_or_policy() {
    let dir = tempfile::tempdir().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    let intent = "Independently review whether bind_socket restricts the wirkd socket mode, and record the decision";
    write_actor_review_route(&estate, "socket_review", intent, r#"["review.md"]"#);
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let repo = dir.path().join("demo-repo");
    init_repo(&repo);
    write_file(&repo, "socket.rs", "fn bind_socket() { socketmarker }\n");
    let target_coordinate = publish_and_locate(&estate, &repo, "socketmarker");
    let work = submit_kind(
        &estate,
        "socket_review",
        &repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .unwrap();

    let unchanged = |estate: &Path, label: &str| {
        let before = estate_fingerprint(estate);
        for _ in 0..3 {
            let (code, _, stderr) = obligations_cli(estate, &["--work", &work.work_id, "--admin"]);
            assert_eq!(code, Some(0), "{stderr}");
        }
        assert!(
            estate_fingerprint(estate) == before,
            "work obligations changed estate state ({label})"
        );
    };

    unchanged(&estate, "no policy file");

    let worktree = materialize_actor(&pointer.socket, &estate, &work.work_id, &work.run_id);
    fs::write(
        worktree.join("review.md"),
        "decision: contradicted_assumption\n",
    )
    .unwrap();
    claim_ok(&estate, &work.work_id, &work.run_id, "review.md=review.md");
    let basis = obligation_basis_for(&estate, &work.work_id, "review");
    write_review_policy(
        &estate,
        r#"["contradicted_assumption"]"#,
        "socket-mode-reviewed",
        "1",
        &basis,
    );

    // The dangerous state: the check is genuinely ready and the policy
    // genuinely admits it, so anything that evaluated *and appended*
    // would mint a settlement here. Inspection must not.
    let claim_event = claim_event_id(&estate, &work.work_id);
    let evidence = format!("work/{}/event/{claim_event}", work.work_id);
    let file_run = status(&pointer.socket, &work.work_id)["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    let (code, raised, stderr) = raise_cli(
        &estate,
        &work.work_id,
        &file_run,
        &[
            "--scope",
            "work_local",
            "--evidence",
            &evidence,
            "--kind",
            "contradicted_assumption",
            "--claim",
            "the socket is bound with a wider mode than the trust boundary states",
            "--applies-to",
            &target_coordinate,
            "--obligation",
            "socket-mode-reviewed@1",
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    // `finding raise` itself settles when the check already holds, which
    // is the landed behaviour; the point here is that *inspection after
    // it* changes nothing further.
    assert!(raised["settled"].is_object(), "{raised}");
    unchanged(&estate, "admitted policy, settled finding");

    stop_wirkd(&estate, wirkd_child);
}

/// `wirk work obligations` without `--json`, as an operator runs it.
fn obligations_human(estate: &Path, args: &[&str]) -> (Option<i32>, String, String) {
    let mut full = vec!["work", "obligations", "--estate"];
    let estate_str = estate.to_str().unwrap();
    full.push(estate_str);
    full.extend_from_slice(args);
    let output = Command::new(wirk_bin())
        .args(&full)
        .output()
        .expect("wirk work obligations runs");
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

/// **The basis review's F2 and F3, both red before this correction, on
/// the default surface an operator actually reads.**
///
/// F2: the verb exists so an operator can write
/// `policy/settlement.json`. A **container** obligation's entry must
/// also list the child obligation's own basis under `mechanisms`, and
/// that instruction lived in a `note` visible only under `--json`; a
/// container declaring no `requires` obliges nothing at all, and its
/// human line was an ordinary admittable-looking `basis <digest>` with
/// nothing to say so — while the Actor-with-no-`review` case *is*
/// visible, because its basis line reads `unavailable (no-mechanism)`.
/// The asymmetry is honest in the data and was dropped on the way to
/// the operator.
///
/// F3: a correctly-spelled `--waypoint` selecting a Waypoint that
/// happens to declare nothing printed "no waypoint on this work's route
/// declares a verification obligation" — a falsehood about the Route
/// whenever another Waypoint does, and the identical sentence a
/// genuinely obligation-free Route gets. The design already refuses a
/// *mistyped* `--waypoint` so a typo cannot read that way; this is the
/// same trap by the other door.
#[test]
fn work_obligations_human_output_names_the_mechanism_and_the_narrowing() {
    let dir = tempfile::tempdir().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    // One Route carrying all four cases at once: a container obligation
    // with a `requires` mechanism, a container obligation without one,
    // an Actor obligation with no `review` contract, and a Waypoint
    // declaring no obligation at all.
    route_fixture::write_route(
        &estate,
        "mechanism_shapes",
        r#"{"id":"mechanism-shapes","waypoints":[
        {"id":"outer","kind":"Container",
         "declared_outputs":[{"name":"a.md","required":true}],
         "required_child_outcomes":[{"role":"auditor","required":true}],
         "verifies":{"id":"audit-performed","edition":"1",
           "proves":"the auditor child Work ran the admitted audit check and closed the auditor role",
           "outputs":["auditor"],
           "requires":{"id":"socket-mode-checked","edition":"1"}},
         "leaves":[{"id":"outer/leaf-a","kind":"Deterministic","command":["sh","-c","echo a > a.md"],
           "declared_outputs":[{"name":"a.md","required":true}]}]},
        {"id":"bare","kind":"Container",
         "declared_outputs":[{"name":"b.md","required":true}],
         "required_child_outcomes":[{"role":"scribe","required":true}],
         "verifies":{"id":"nothing-obliged","edition":"1",
           "proves":"a container obligation that names no required child obligation",
           "outputs":["scribe"]},
         "leaves":[{"id":"bare/leaf-b","kind":"Deterministic","command":["sh","-c","echo b > b.md"],
           "declared_outputs":[{"name":"b.md","required":true}]}]},
        {"id":"unmechanised","kind":"Actor","intent":"record a decision with no review contract",
         "declared_outputs":[{"name":"note.md","required":true}],"boundary":["**"],
         "verifies":{"id":"decision-recorded","edition":"1",
           "proves":"a decision was recorded","outputs":["note.md"]}},
        {"id":"plain","kind":"Deterministic","command":["sh","-c","echo c > c.md"],
         "declared_outputs":[{"name":"c.md","required":true}]}
    ]}"#,
    );
    let (wirkd_child, _pointer) = start_wirkd(&estate);

    let repo = dir.path().join("demo-repo");
    init_repo(&repo);
    let work = submit(
        &estate,
        "mechanism_shapes",
        &repo,
        &["demo:write", "auditor:write", "scribe:write"],
        None,
    )
    .unwrap();

    let (code, human, stderr) = obligations_human(&estate, &["--work", &work.work_id, "--admin"]);
    assert_eq!(code, Some(0), "{stderr}");

    // F2, the container that obliges something: the second policy field
    // an operator must write is named on the surface that tells them
    // what to write.
    assert!(
        human.contains("mechanism required_child_obligation requires socket-mode-checked@1"),
        "the required child obligation is named in the human output: {human}"
    );
    assert!(
        human.contains("mechanisms"),
        "the container's own admission instruction — the `mechanisms` field of the policy entry — reaches the operator: {human}"
    );

    // F2, the two mechanisms that are absent: each says so, beside its
    // own basis line, rather than looking like an ordinary admittable
    // obligation.
    assert!(
        human.contains("mechanism required_child_obligation absent:"),
        "a container naming no `requires` obliges nothing, and the human output says so: {human}"
    );
    assert!(
        human.contains("mechanism actor_review absent:"),
        "an Actor obligation with no `review` contract obliges nothing, and the human output says so: {human}"
    );

    // The JSON reply is unchanged: this is a rendering repair.
    let (_, reply, _) = obligations_cli(&estate, &["--work", &work.work_id, "--admin"]);
    assert_eq!(
        obligation_entry(&reply, "bare")["mechanism"]["present"].as_bool(),
        Some(false)
    );
    assert!(
        obligation_entry(&reply, "outer")["mechanism"]["note"]
            .as_str()
            .unwrap()
            .contains("mechanisms")
    );

    // F3: `plain` really is on this Route and really declares nothing,
    // while three other Waypoints do declare an obligation.
    let (code, narrowed, stderr) = obligations_human(
        &estate,
        &["--work", &work.work_id, "--admin", "--waypoint", "plain"],
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        !narrowed.contains("no waypoint on this work's route declares a verification obligation"),
        "a narrowed answer must not be reported as a fact about the whole Route: {narrowed}"
    );
    assert!(
        narrowed.contains("waypoint plain declares no verification obligation"),
        "the selected Waypoint is named as the subject of the empty answer: {narrowed}"
    );
    assert!(
        narrowed.contains("3 of 6 waypoint(s) on this route declare one"),
        "the Route's own count is reported beside it, from the reply's `declaring_obligation`: {narrowed}"
    );
    assert!(
        narrowed.contains("narrowed to waypoint plain"),
        "a narrowed read says so on its header line: {narrowed}"
    );

    // A narrowed read that *does* select an obligation still says it was
    // narrowed, so a single entry never reads as the whole Route.
    let (code, one, stderr) = obligations_human(
        &estate,
        &["--work", &work.work_id, "--admin", "--waypoint", "outer"],
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert!(one.contains("narrowed to waypoint outer"), "{one}");
    assert!(!one.contains("waypoint bare "), "{one}");

    // The genuinely obligation-free Route keeps its own sentence, so the
    // two facts stay distinguishable in both directions.
    route_fixture::install_route_fixture(&estate, "wa_simple_leaf");
    let plain_repo = dir.path().join("plain-repo");
    init_repo(&plain_repo);
    let plain = submit(
        &estate,
        "wa_simple_leaf",
        &plain_repo,
        &["demo:write"],
        None,
    )
    .unwrap();
    let (code, none, stderr) = obligations_human(&estate, &["--work", &plain.work_id, "--admin"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        none.contains("no waypoint on this work's route declares a verification obligation"),
        "{none}"
    );
    assert!(!none.contains("narrowed to waypoint"), "{none}");

    stop_wirkd(&estate, wirkd_child);
}

/// A changed World or a changed authored obligation is a different
/// basis, and the policy that admitted the old one refuses — visible in
/// the inspection surface itself, not only as a settlement that quietly
/// does not happen.
///
/// Three Works: the admitted one; the same Route against a **different
/// repository** (a different Actor World, so a different reserved World
/// hash); and a Route differing in exactly one authored field, the
/// `proves` sentence. All three declare `socket-mode-reviewed@1` — the
/// same *name* — which is exactly why admitting by name alone would be
/// forgeable and admitting by basis is not.
#[test]
fn a_changed_world_or_obligation_moves_the_basis_and_the_old_policy_refuses() {
    let dir = tempfile::tempdir().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    write_basis_route(
        &estate,
        "as_admitted",
        "an admitted independent review of the socket boundary was performed under recipe socket-boundary-review@3 and recorded a declared decision",
    );
    write_basis_route(
        &estate,
        "widened",
        "an admitted independent review proved the socket boundary is correct in every respect",
    );
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let repo = dir.path().join("demo-repo");
    init_repo(&repo);
    write_file(&repo, "socket.rs", "fn bind_socket() { socketmarker }\n");
    let target_coordinate = publish_and_locate(&estate, &repo, "socketmarker");

    let other_repo = dir.path().join("other-repo");
    init_repo(&other_repo);
    write_file(&other_repo, "socket.rs", "fn bind_socket() { elsewhere }\n");

    let admitted_work = submit_kind(
        &estate,
        "as_admitted",
        &repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .unwrap();
    let other_world = submit_kind(
        &estate,
        "as_admitted",
        &other_repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .unwrap();
    let widened = submit_kind(
        &estate,
        "widened",
        &repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .unwrap();

    let basis_of = |work_id: &str| {
        let (code, reply, stderr) = obligations_cli(&estate, &["--work", work_id, "--admin"]);
        assert_eq!(code, Some(0), "{stderr}");
        obligation_entry(&reply, "review")["basis"]["basis"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let admitted_basis = basis_of(&admitted_work.work_id);
    let other_world_basis = basis_of(&other_world.work_id);
    let widened_basis = basis_of(&widened.work_id);
    assert_ne!(
        admitted_basis, other_world_basis,
        "a different reviewing World is a different basis"
    );
    assert_ne!(
        admitted_basis, widened_basis,
        "a widened `proves` sentence is a different basis"
    );

    write_review_policy(
        &estate,
        r#"["contradicted_assumption"]"#,
        "socket-mode-reviewed",
        "1",
        &admitted_basis,
    );

    let admission_of = |work_id: &str| {
        let (_, reply, _) = obligations_cli(&estate, &["--work", work_id, "--admin"]);
        obligation_entry(&reply, "review")["admission"]["state"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(admission_of(&admitted_work.work_id), "admitted");
    assert_eq!(
        admission_of(&other_world.work_id),
        "basis-not-admitted",
        "the policy names this obligation id and edition, at another basis — and says so"
    );
    assert_eq!(admission_of(&widened.work_id), "basis-not-admitted");

    // And the refusal is real, not only reported: the widened Route's
    // own genuine review receipt settles nothing under the old policy.
    let worktree = materialize_actor(&pointer.socket, &estate, &widened.work_id, &widened.run_id);
    fs::write(
        worktree.join("review.md"),
        "decision: contradicted_assumption\n",
    )
    .unwrap();
    claim_ok(
        &estate,
        &widened.work_id,
        &widened.run_id,
        "review.md=review.md",
    );
    let claim_event = claim_event_id(&estate, &widened.work_id);
    let evidence = format!("work/{}/event/{claim_event}", widened.work_id);
    let file_run = status(&pointer.socket, &widened.work_id)["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    let (code, raised, stderr) = raise_cli(
        &estate,
        &widened.work_id,
        &file_run,
        &[
            "--scope",
            "work_local",
            "--evidence",
            &evidence,
            "--kind",
            "contradicted_assumption",
            "--claim",
            "the socket is bound with a wider mode than the trust boundary states",
            "--applies-to",
            &target_coordinate,
            "--obligation",
            "socket-mode-reviewed@1",
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert!(raised["settled"].is_null(), "{raised}");
    let finding_id = raised["id"].as_str().unwrap().to_string();
    let (code, settle, err) =
        finding_cli(&estate, &["settle", "--finding", &finding_id, "--admin"]);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(
        settle["pending"]["reason"].as_str().unwrap(),
        "obligation-basis-not-admitted"
    );

    // The check is READY and the basis is NOT admitted — the two states
    // the surface must never conflate. Inspection says exactly that.
    let (_, reply, _) = obligations_cli(&estate, &["--work", &widened.work_id, "--admin"]);
    let entry = obligation_entry(&reply, "review");
    assert_eq!(
        entry["admission"]["state"].as_str().unwrap(),
        "basis-not-admitted"
    );
    let finding = &entry["findings"][0];
    assert_eq!(finding["ready"]["state"].as_str().unwrap(), "ready");
    assert_eq!(
        finding["ready"]["basis"].as_str().unwrap(),
        widened_basis,
        "the readiness names the basis the check would need admitted"
    );
    assert_eq!(finding["settled"]["state"].as_str().unwrap(), "not-settled");

    stop_wirkd(&estate, wirkd_child);
}

/// Absent, unreserved and unresolved states are reported as themselves.
/// None of them may read as admitted or ready, and a mistyped Waypoint
/// or Work must never come back as a confident "nothing here".
#[test]
fn absent_unreserved_and_unresolved_obligations_stay_honest() {
    let dir = tempfile::tempdir().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    // A Route whose *second* Waypoint carries the obligation, so it is
    // real, authored and not yet reserved; and whose review selector
    // names a path the source does not carry, so nothing freezes.
    route_fixture::write_route(
        &estate,
        "later_obligation",
        r#"{"id":"later_obligation","waypoints":[
            {"id":"first","kind":"Actor","intent":"do the work first",
             "declared_outputs":[{"name":"first.md","required":true}],"boundary":["**"]},
            {"id":"review","kind":"Actor","intent":"review it afterwards",
             "declared_outputs":[{"name":"review.md","required":true}],"boundary":["**"],
             "verifies":{"id":"later-reviewed","edition":"1",
               "proves":"an admitted independent review was performed under recipe later-review@1 and recorded a declared decision",
               "outputs":["review.md"],
               "review":{"recipe":"later-review@1","targets":[{"source":"demo","path":"absent.rs"}],"decisions":["gap"]}}}
        ]}"#,
    );
    // A Route whose reviewing Waypoint IS reserved but whose declared
    // selector names a path the source does not carry, so nothing froze
    // into the reserved World and the check can never hold.
    route_fixture::write_route(
        &estate,
        "unresolved_target",
        r#"{"id":"unresolved_target","waypoints":[
            {"id":"review","kind":"Actor","intent":"review a target that does not exist",
             "declared_outputs":[{"name":"review.md","required":true}],"boundary":["**"],
             "verifies":{"id":"phantom-reviewed","edition":"1",
               "proves":"an admitted independent review was performed under recipe phantom-review@1 and recorded a declared decision",
               "outputs":["review.md"],
               "review":{"recipe":"phantom-review@1","targets":[{"source":"demo","path":"absent.rs"}],"decisions":["gap"]}}}
        ]}"#,
    );
    // A Route whose Actor obligation declares no `review` contract at
    // all: well-formed, and it obliges nothing.
    route_fixture::write_route(
        &estate,
        "no_mechanism",
        r#"{"id":"no_mechanism","waypoints":[
            {"id":"review","kind":"Actor","intent":"review with no declared contract",
             "declared_outputs":[{"name":"review.md","required":true}],"boundary":["**"],
             "verifies":{"id":"mechanismless","edition":"1",
               "proves":"nothing is obliged","outputs":["review.md"]}}
        ]}"#,
    );
    route_fixture::install_route_fixture(&estate, "wa_simple_leaf");
    let (wirkd_child, _pointer) = start_wirkd(&estate);

    let repo = dir.path().join("demo-repo");
    init_repo(&repo);
    write_file(&repo, "socket.rs", "fn bind_socket() { socketmarker }\n");
    publish_and_locate(&estate, &repo, "socketmarker");

    // Not reserved: the obligation is on a Waypoint the Work has not
    // reached.
    let later = submit_kind(
        &estate,
        "later_obligation",
        &repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .unwrap();
    let (code, reply, stderr) = obligations_cli(&estate, &["--work", &later.work_id, "--admin"]);
    assert_eq!(code, Some(0), "{stderr}");
    let entry = obligation_entry(&reply, "review");
    assert_eq!(
        entry["reservation"]["state"].as_str().unwrap(),
        "not-reserved"
    );
    assert!(entry["reservation"]["world_hash"].is_null());
    assert_eq!(entry["basis"]["state"].as_str().unwrap(), "not-reserved");
    assert!(
        entry["basis"]["basis"].is_null(),
        "an unreserved obligation has no basis to admit: {entry}"
    );
    assert!(
        entry["basis"]["reason"]
            .as_str()
            .unwrap()
            .contains("reserved"),
        "the reason says why, rather than leaving a bare null: {entry}"
    );
    assert_eq!(
        entry["admission"]["state"].as_str().unwrap(),
        "unknown-basis",
        "with no basis there is no admission question, and nothing may read as admitted"
    );
    assert!(entry["mechanism"]["review_targets"]["frozen"].is_null());
    assert!(
        !entry["mechanism"]["review_targets"]["resolved"]
            .as_bool()
            .unwrap()
    );
    assert_eq!(
        reply["route"]["waypoints"].as_u64().unwrap(),
        2,
        "the whole Route is accounted for, including the Waypoint declaring nothing"
    );
    assert_eq!(reply["route"]["declaring_obligation"].as_u64().unwrap(), 1);

    // Unresolved: the reviewing World IS reserved — so a basis really
    // exists and could be admitted — but no declared target froze, so
    // the check can never hold. Admissible and never ready are
    // different facts and both are said.
    let unresolved = submit_kind(
        &estate,
        "unresolved_target",
        &repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .unwrap();
    let (_, reply, _) = obligations_cli(&estate, &["--work", &unresolved.work_id, "--admin"]);
    let entry = obligation_entry(&reply, "review");
    assert_eq!(entry["reservation"]["state"].as_str().unwrap(), "reserved");
    assert_eq!(entry["basis"]["state"].as_str().unwrap(), "available");
    assert_eq!(
        entry["mechanism"]["review_targets"]["declared"]
            .as_u64()
            .unwrap(),
        1
    );
    assert_eq!(
        entry["mechanism"]["review_targets"]["frozen"]
            .as_u64()
            .unwrap(),
        0,
        "the selector resolved to nothing and the reply says so: {entry}"
    );
    assert!(
        !entry["mechanism"]["review_targets"]["resolved"]
            .as_bool()
            .unwrap()
    );
    assert_eq!(
        entry["admission"]["state"].as_str().unwrap(),
        "no-policy-file"
    );

    // No mechanism at all: an Actor obligation with no review contract.
    let no_target = submit_kind(
        &estate,
        "no_mechanism",
        &repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .unwrap();
    let (_, reply, _) = obligations_cli(&estate, &["--work", &no_target.work_id, "--admin"]);
    let entry = obligation_entry(&reply, "review");
    assert_eq!(entry["mechanism"]["kind"].as_str().unwrap(), "actor_review");
    assert!(
        !entry["mechanism"]["present"].as_bool().unwrap(),
        "an Actor obligation with no review contract obliges nothing: {entry}"
    );
    assert_eq!(entry["basis"]["state"].as_str().unwrap(), "no-mechanism");
    assert!(entry["basis"]["basis"].is_null());
    assert_eq!(
        entry["admission"]["state"].as_str().unwrap(),
        "unknown-basis"
    );

    // A Route with no obligation anywhere is an empty list plus an
    // honest count, never a refusal.
    let plain_repo = dir.path().join("plain-repo");
    init_repo(&plain_repo);
    let plain = submit(
        &estate,
        "wa_simple_leaf",
        &plain_repo,
        &["demo:write"],
        None,
    )
    .unwrap();
    let (code, reply, stderr) = obligations_cli(&estate, &["--work", &plain.work_id, "--admin"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(reply["obligations"].as_array().unwrap().is_empty());
    assert_eq!(reply["route"]["declaring_obligation"].as_u64().unwrap(), 0);

    // Invalid references stay refusals.
    let (code, _, stderr) = obligations_cli(
        &estate,
        &[
            "--work",
            &later.work_id,
            "--admin",
            "--waypoint",
            "no-such-waypoint",
        ],
    );
    assert_eq!(code, Some(2), "{stderr}");
    assert!(stderr.contains("no such waypoint"), "{stderr}");
    let (code, _, stderr) = obligations_cli(&estate, &["--work", "work-does-not-exist", "--admin"]);
    assert_eq!(code, Some(2), "{stderr}");
    assert!(stderr.contains("no such work"), "{stderr}");
    // Naming both scopes, or neither, is refused rather than defaulting.
    let (code, _, _) = obligations_cli(&estate, &["--work", &later.work_id]);
    assert_eq!(code, Some(2));
    let (code, _, _) = obligations_cli(
        &estate,
        &[
            "--work",
            &later.work_id,
            "--admin",
            "--requesting-work",
            &later.work_id,
        ],
    );
    assert_eq!(code, Some(2));

    stop_wirkd(&estate, wirkd_child);
}

// ---- 13. Estate publication: discovery is admission, not kinship ------
//
// `LATER-DISCOVERY-ADJUDICATION.md`, from `later-discovery-probe`'s
// executed counterexample: the estate findings index gated every row on
// the requester's lineage *before* the source view ran. A later Work
// admitted to exactly the sources the settling Work held — able to
// resolve the reviewed bytes itself — received the byte-identical empty
// reply a Work denied those sources received, while a weaker child on
// the lineage saw the row. Genuinely settled estate knowledge was
// undiscoverable to every Work outside one family.
//
// The correction is not "drop the gate": a published row is admitted
// whole, to a requester whose own bindings cover the producing Work's
// own, or it is not returned at all.

/// A settlement policy admitting two `actor_reviewed` obligations at
/// **estate** scope — the scope the findings index indexes at all.
fn write_estate_review_policy(estate: &Path, entries: &[(&str, &str, &str)]) {
    let obligations = entries
        .iter()
        .map(|(id, edition, basis)| {
            format!(r#"{{"id":"{id}","edition":"{edition}","basis":"{basis}"}}"#)
        })
        .collect::<Vec<_>>()
        .join(",");
    write_policy(
        estate,
        &format!(
            r#"{{"version":2,"classes":[{{"class":"actor_reviewed","scope":"estate_local","kinds":["contradicted_assumption"],"obligations":[{obligations}]}}]}}"#
        ),
    );
}

/// `atlas findings --requesting-work <id>`, parsed.
fn index_for(estate: &Path, requester: &str) -> serde_json::Value {
    let (ok, reply, err) = atlas(estate, &["findings", "--requesting-work", requester]);
    assert!(ok, "{err}");
    reply
}

/// Every string anywhere in `value` — so a leak assertion inspects the
/// whole answer rather than the one field a fix happened to remember.
fn all_strings_of(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(text) => out.push(text.clone()),
        serde_json::Value::Array(items) => items.iter().for_each(|item| all_strings_of(item, out)),
        serde_json::Value::Object(map) => {
            for (key, item) in map {
                out.push(key.clone());
                all_strings_of(item, out);
            }
        }
        _ => {}
    }
}

fn assert_absent(what: &str, value: &serde_json::Value, needles: &[&str]) {
    let mut strings = Vec::new();
    all_strings_of(value, &mut strings);
    for needle in needles {
        assert!(
            !strings.iter().any(|text| text.contains(needle)),
            "{what} disclosed {needle:?}: {value}"
        );
    }
}

/// **The decisive contract.** Two settled `actor_reviewed` publications
/// exist, over the *same* reviewed target in the same source. Their
/// producers differ in one thing only: the second also reads `attic`,
/// and its authored prose, its intent, its recipe and its report name
/// all quote `attic`'s own sentinel. Nothing in the estate reviews
/// `attic`.
///
/// - The **independent** Work `wide`, holding exactly what the first
///   producer held and no parent relationship to anything, discovers
///   the first row with no id and no transcript supplied, and reads its
///   whole attributable receipt.
/// - It does **not** see the second row. It admits every source the
///   *review* named, and that is deliberately not enough: the row's
///   free prose can quote any source its author could read, so the gate
///   is the producer's own binding set.
/// - `wider`, which also holds `attic`, sees both.
/// - `denied`, holding neither reviewed source, sees neither, and its
///   reply is what it always was.
///
/// Nothing crosses but a policy receipt: an `estate_local` finding that
/// was only *asserted* stays lineage-bound, a `work_local` one is
/// nowhere, and an administrative `--rebuild` moves none of it.
#[test]
fn an_independent_source_admitted_work_discovers_a_settled_estate_publication() {
    let dir = tempfile::tempdir().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    let intent = "Independently review whether bind_socket restricts the wirkd socket mode, and record the decision";
    write_actor_review_route(&estate, "socket_review", intent, r#"["review.md"]"#);
    // The same reviewed target, authored by a producer that also reads
    // `attic`: its intent, its `proves` and its report name carry the
    // attic sentinel. The review itself still names only `demo`.
    route_fixture::write_route(
        &estate,
        "attic_review",
        &format!(
            r#"{{"id":"attic_review","waypoints":[
            {{"id":"review","kind":"Actor","intent":"Cross-check the socket boundary against the atticmarker roster and record the decision",
             "declared_outputs":[{{"name":"roster-atticmarker.md","required":true}}],
             "boundary":["**"],
             "verifies":{{"id":"attic-mode-reviewed","edition":"1",
               "proves":"an admitted review of the socket boundary against the atticmarker roster was performed and recorded a declared decision",
               "outputs":["roster-atticmarker.md"],
               {REVIEW_CONTRACT}}}}},
            {{"id":"file","kind":"Actor","intent":"file the reviewer's own finding",
             "declared_outputs":[{{"name":"done.md","required":true}}],"boundary":["**"]}}
        ]}}"#
        ),
    );
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let repo = dir.path().join("demo-repo");
    init_repo(&repo);
    write_file(&repo, "socket.rs", "fn bind_socket() { socketmarker }\n");
    let target_coordinate = publish_and_locate(&estate, &repo, "socketmarker");
    let attic = dir.path().join("attic-repo");
    init_repo(&attic);
    write_file(&attic, "roster.md", "atticmarker: the standby custodian\n");
    publish_and_locate_as(&estate, &attic, "attic", "atticmarker");

    // Two producers over the same target, differing only in what else
    // they may read — and two later Works with no relation to either.
    let narrow = submit_kind(
        &estate,
        "socket_review",
        &repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .unwrap();
    let broad_repo = dir.path().join("broad-repo");
    init_repo(&broad_repo);
    write_file(
        &broad_repo,
        "socket.rs",
        "fn bind_socket() { socketmarker }\n",
    );
    let broad = submit_kind(
        &estate,
        "attic_review",
        &broad_repo,
        &["demo:write", "attic:read"],
        None,
        Some("actor"),
    )
    .unwrap();
    let wide = submit_kind(
        &estate,
        "socket_review",
        &repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .unwrap();
    let wider = submit_kind(
        &estate,
        "attic_review",
        &broad_repo,
        &["demo:write", "attic:read"],
        None,
        Some("actor"),
    )
    .unwrap();
    let denied = submit_kind(
        &estate,
        "attic_review",
        &attic,
        &["attic:read"],
        None,
        Some("actor"),
    )
    .unwrap();

    // Both reviews really run, and both really settle.
    let settle = |work: &Submitted, obligation: &str, report: &str, claim: &str| -> String {
        let worktree = materialize_actor(&pointer.socket, &estate, &work.work_id, &work.run_id);
        fs::write(
            worktree.join(report),
            "recipe socket-boundary-review@3\ndecision: contradicted_assumption\n",
        )
        .unwrap();
        claim_ok(
            &estate,
            &work.work_id,
            &work.run_id,
            &format!("{report}={report}"),
        );
        let claim_event = claim_event_id(&estate, &work.work_id);
        let evidence = format!("work/{}/event/{claim_event}", work.work_id);
        let file_run = status(&pointer.socket, &work.work_id)["run_id"]
            .as_str()
            .unwrap()
            .to_string();
        let (code, raised, stderr) = raise_cli(
            &estate,
            &work.work_id,
            &file_run,
            &[
                "--scope",
                "estate_local",
                "--evidence",
                &evidence,
                "--kind",
                "contradicted_assumption",
                "--claim",
                claim,
                "--applies-to",
                &target_coordinate,
                "--obligation",
                obligation,
            ],
        );
        assert_eq!(code, Some(0), "{stderr}");
        assert!(
            raised["settled"].is_object(),
            "the publication must really carry a policy receipt: {raised}"
        );
        raised["id"].as_str().unwrap().to_string()
    };

    // The policy admits both obligations, at their own reserved bases.
    let narrow_basis = obligation_basis_for(&estate, &narrow.work_id, "review");
    let broad_basis = obligation_basis_for(&estate, &broad.work_id, "review");
    assert_ne!(narrow_basis, broad_basis);
    write_estate_review_policy(
        &estate,
        &[
            ("socket-mode-reviewed", "1", &narrow_basis),
            ("attic-mode-reviewed", "1", &broad_basis),
        ],
    );
    let narrow_finding = settle(
        &narrow,
        "socket-mode-reviewed@1",
        "review.md",
        "the socket is bound with a wider mode than the trust boundary states",
    );
    let broad_finding = settle(
        &broad,
        "attic-mode-reviewed@1",
        "roster-atticmarker.md",
        "the socket boundary contradicts the atticmarker standby roster",
    );

    // (a) The independent Work, given no id and no transcript, finds the
    //     publication whose producer it fully admits — and only that one.
    let reply = index_for(&estate, &wide.work_id);
    let rows = reply["rows"].as_array().unwrap();
    assert_eq!(
        rows.len(),
        1,
        "an independent Work admitted to the producer's own sources must discover the settled publication: {reply}"
    );
    assert_eq!(
        reply["disclosure"]["off_lineage"].as_u64().unwrap(),
        1,
        "the row it may not have is still only a number: {reply}"
    );
    assert_eq!(reply["disclosure"]["withheld"].as_u64().unwrap(), 0);
    let row = &rows[0];
    assert_eq!(row["kind"], "settled");
    assert_eq!(row["finding"]["id"].as_str().unwrap(), narrow_finding);
    assert_eq!(row["origin"]["work"].as_str().unwrap(), narrow.work_id);

    // (b) The exact attributable evidence, whole — this is what makes
    //     the row worth discovering rather than merely visible.
    let check = &row["settlement"]["check"];
    assert_eq!(check["check"], "actor_review");
    assert_eq!(check["obligation"]["id"], "socket-mode-reviewed");
    assert_eq!(check["obligation"]["basis"].as_str().unwrap(), narrow_basis);
    assert_eq!(check["recipe"], "socket-boundary-review@3");
    assert_eq!(check["decision"], "contradicted_assumption");
    assert!(check["world_hash"].is_string(), "{row}");
    assert_eq!(check["targets"][0]["selector"]["source"], "demo");
    assert_eq!(check["targets"][0]["selector"]["path"], "socket.rs");
    assert!(check["targets"][0]["generation"].is_string(), "{row}");
    assert!(check["targets"][0]["object_id"].is_string(), "{row}");
    assert!(check["report"][0]["digest"].is_string(), "{row}");
    assert_eq!(
        row["settlement"]["authority"]["policy"]["class"],
        "actor_reviewed"
    );
    assert!(
        row["settlement"]["authority"]["policy"]["policy_digest"].is_string(),
        "{row}"
    );

    // (c) The epistemic states survive publication exactly as they are.
    assert_eq!(row["finding"]["claim_verified"], serde_json::json!(false));
    assert!(
        row["settlement"]["proves"]["standing"]
            .as_str()
            .unwrap()
            .contains("judgement"),
        "the receipt still says its conclusion is judgement: {row}"
    );

    // (d) The negative that makes this a boundary rather than an
    //     opening: `wide` admits every source the *review* named, and
    //     still may not have the row whose producer also read `attic`.
    assert_absent(
        "the independent work's index",
        &reply,
        &[
            "atticmarker",
            "attic",
            "roster",
            &broad_finding,
            &broad.work_id,
            &broad_basis,
        ],
    );

    // (e) A requester that also admits `attic` receives both.
    let both = index_for(&estate, &wider.work_id);
    assert_eq!(both["rows"].as_array().unwrap().len(), 2, "{both}");
    assert_eq!(both["disclosure"]["off_lineage"].as_u64().unwrap(), 0);
    assert_eq!(both["disclosure"]["withheld"].as_u64().unwrap(), 0);

    // (f) A requester denied the reviewed source learns exactly what it
    //     always learned: a count, and nothing else.
    let refused = index_for(&estate, &denied.work_id);
    assert!(refused["rows"].as_array().unwrap().is_empty(), "{refused}");
    assert_eq!(refused["disclosure"]["off_lineage"].as_u64().unwrap(), 2);
    assert_absent(
        "the denied work's index",
        &refused,
        &[
            "socketmarker",
            "atticmarker",
            "socket.rs",
            "demo",
            &narrow_finding,
            &broad_finding,
        ],
    );

    // (g) The producers' own lineage answers are unchanged.
    let own = index_for(&estate, &narrow.work_id);
    assert!(
        own["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["finding"]["id"].as_str() == Some(narrow_finding.as_str())),
        "a Work still sees its own row: {own}"
    );

    // (h) Only a receipt crosses. An `estate_local` finding that was
    //     merely asserted is an opinion; a `work_local` one is nowhere;
    //     and a forced administrative rebuild moves neither.
    let file_run = status(&pointer.socket, &narrow.work_id)["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    let (code, opinion, stderr) = raise_cli(
        &estate,
        &narrow.work_id,
        &file_run,
        &[
            "--scope",
            "estate_local",
            "--evidence",
            &target_coordinate,
            "--kind",
            "gap",
            "--claim",
            "an opinion about socketmarker, never reviewed",
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let opinion_id = opinion["id"].as_str().unwrap().to_string();
    let (code, _, stderr) = finding_cli(
        &estate,
        &[
            "assert",
            "--finding",
            &opinion_id,
            "--decision",
            "accepted",
            "--by",
            "an operator",
            "--admin",
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let (ok, rebuilt, err) = atlas(&estate, &["findings", "--rebuild", "--admin"]);
    assert!(ok, "{err}");
    assert_eq!(
        rebuilt["rows"].as_array().unwrap().len(),
        3,
        "the administrative index holds both receipts and the opinion: {rebuilt}"
    );
    let after = index_for(&estate, &wide.work_id);
    assert_eq!(
        after["rows"].as_array().unwrap().len(),
        1,
        "an asserted opinion is not an estate publication: {after}"
    );
    assert_absent(
        "the independent index after a rebuild",
        &after,
        &[&opinion_id],
    );

    // (i) And none of this granted one byte of journal authority.
    let (code, _, stderr) = finding_cli(
        &estate,
        &[
            "list",
            "--requesting-work",
            &wide.work_id,
            "--work",
            &narrow.work_id,
        ],
    );
    assert_eq!(
        code,
        Some(2),
        "discovering a publication never opens the producer's journal: {stderr}"
    );

    // (j) And reading the index changed nothing.
    let before = estate_fingerprint(&estate);
    for _ in 0..3 {
        index_for(&estate, &wide.work_id);
        index_for(&estate, &denied.work_id);
    }
    assert!(
        estate_fingerprint(&estate) == before,
        "a scoped index read must not move a byte of the estate"
    );

    stop_wirkd(&estate, wirkd_child);
}

// `NATIVE-CHAIN-ADJUDICATION.md` G3, from the real native consumer's own
// transcript (`loop-b-native-chain-verify/raw/13-raise-and-escape.txt`):
// a later independent Work had legitimately discovered an earlier
// settled EstateLocal finding, disagreed with it on its own evidence,
// and had no way to say so. `--contradicts <finding-id>` fell through
// coordinate parsing and came back "invalid coordinate hex"; the same
// target spelled `work/<id>/event/<id>` was refused off lineage; and
// `finding assert` writes into the other Work's journal, which is not
// this Work's own authority and is lineage-gated anyway. The consumer
// deleted the sentence and recorded nothing structural.
//
// The relation is `EvidenceRef::Finding`, admitted by the *same*
// publication gate the estate findings index already uses — so what a
// Work may name is exactly what it may already discover, and a
// disagreement is one more record in its own journal.

/// Raises a Finding and keeps the raw stdout beside the exit code.
///
/// A refusal is printed to stdout, not stderr (`finding.rs` prints
/// `Refused: <code> <message>` with `println!`), so a test that asserts
/// on *which* refusal a caller got has to read the stream the caller
/// actually reads.
fn raise_raw(estate: &Path, work_id: &str, run_id: &str, args: &[&str]) -> (Option<i32>, String) {
    let mut full = vec!["finding", "raise"];
    full.extend_from_slice(args);
    full.push("--json");
    let output = Command::new(wirk_bin())
        .args(&full)
        .env("WIRK_ESTATE_ROOT", estate)
        .env("WIRK_WORK_ID", work_id)
        .env("WIRK_RUN_ID", run_id)
        .output()
        .expect("wirk finding raise runs");
    (
        output.status.code(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

/// **The decisive contract.** A settled `actor_reviewed` publication
/// exists in a Work with no relation whatever to the later one.
///
/// - The later Work discovers it with no id and no transcript, reads it,
///   and records its **own** evidence-backed disagreement naming that
///   exact record — under its own Work/Run authority, in its own
///   journal.
/// - The readback separates four things a reader must never have to
///   guess between: the *claimed* relation (which list the entry is in
///   and the pair the caller named), the *resolved* exact target (Work
///   and origin event), the *actual admission* (which of the two routes
///   admitted it), and the target's own *settlement standing*.
/// - The named record is untouched: no assertion, no supersession, no
///   settlement, not one byte appended to its journal. And the
///   disagreement is not published by naming a settled thing — it is
///   `work_local`, and an `estate_local` one still needs admitted source
///   evidence of its own.
/// - Every way of naming a record this Work may not have — absent,
///   wrong pair, absent Work, an id from nowhere, and the real record
///   asked for by a Work denied its sources — returns the identical
///   refusal, so a refusal never says whether the thing exists.
/// - A malformed token is refused as a *form*, naming the three forms.
///   That is the exact diagnostic whose absence sent the real consumer
///   hunting a coordinate it had never written.
/// - It survives a restart and an administrative rebuild frozen: the
///   route and standing recorded are what was true at raise time.
#[test]
fn a_later_independent_work_records_its_own_disagreement_with_a_settled_publication() {
    let dir = tempfile::tempdir().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    write_actor_review_route(
        &estate,
        "socket_review",
        "Independently review whether bind_socket restricts the wirkd socket mode, and record the decision",
        r#"["review.md"]"#,
    );
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let repo = dir.path().join("demo-repo");
    init_repo(&repo);
    write_file(&repo, "socket.rs", "fn bind_socket() { socketmarker }\n");
    let target_coordinate = publish_and_locate(&estate, &repo, "socketmarker");
    let attic = dir.path().join("attic-repo");
    init_repo(&attic);
    write_file(&attic, "roster.md", "atticmarker: the standby custodian\n");
    publish_and_locate_as(&estate, &attic, "attic", "atticmarker");

    // The producer, and two later Works with no relation to it: one
    // holding exactly what it holds, one holding nothing of it.
    let producer = submit_kind(
        &estate,
        "socket_review",
        &repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .unwrap();
    let later = submit_kind(
        &estate,
        "socket_review",
        &repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .unwrap();
    let denied = submit_kind(
        &estate,
        "socket_review",
        &attic,
        &["attic:read"],
        None,
        Some("actor"),
    )
    .unwrap();

    // A real review, really settled under an admitted policy.
    let worktree = materialize_actor(
        &pointer.socket,
        &estate,
        &producer.work_id,
        &producer.run_id,
    );
    fs::write(
        worktree.join("review.md"),
        "recipe socket-boundary-review@3\ndecision: contradicted_assumption\n",
    )
    .unwrap();
    claim_ok(
        &estate,
        &producer.work_id,
        &producer.run_id,
        "review.md=review.md",
    );
    let claim_event = claim_event_id(&estate, &producer.work_id);
    let producer_basis = obligation_basis_for(&estate, &producer.work_id, "review");
    write_estate_review_policy(&estate, &[("socket-mode-reviewed", "1", &producer_basis)]);
    let producer_run = status(&pointer.socket, &producer.work_id)["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    let (code, settled, stderr) = raise_cli(
        &estate,
        &producer.work_id,
        &producer_run,
        &[
            "--scope",
            "estate_local",
            "--kind",
            "contradicted_assumption",
            "--claim",
            "the socket is bound with a wider mode than the trust boundary states",
            "--evidence",
            &format!("work/{}/event/{claim_event}", producer.work_id),
            "--applies-to",
            &target_coordinate,
            "--obligation",
            "socket-mode-reviewed@1",
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert!(settled["settled"].is_object(), "{settled}");
    let settled_id = settled["id"].as_str().unwrap().to_string();

    // (a) Discovery, with no id and no transcript supplied.
    let discovered = index_for(&estate, &later.work_id);
    let row = &discovered["rows"].as_array().unwrap()[0];
    assert_eq!(row["finding"]["id"].as_str().unwrap(), settled_id);
    let origin_event = row["origin"]["raised_event"].as_str().unwrap().to_string();

    // (b) The disagreement, recorded under the later Work's own triple.
    let later_run = status(&pointer.socket, &later.work_id)["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    let relation = format!("work/{}/finding/{settled_id}", producer.work_id);
    let producer_journal_before = fs::read(
        estate
            .join("works")
            .join(&producer.work_id)
            .join("journal.ndjson"),
    )
    .unwrap();
    let (code, disagreement, stderr) = raise_cli(
        &estate,
        &later.work_id,
        &later_run,
        &[
            "--scope",
            "work_local",
            "--kind",
            "contradicted_assumption",
            "--claim",
            "0775 is the mode of the containing directory; the reviewed line binds the socket 0700",
            "--contradicts",
            &relation,
            "--evidence",
            &target_coordinate,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let disagreement_id = disagreement["id"].as_str().unwrap().to_string();

    // (c) Four separable facts in the readback.
    let entry = &disagreement["contradicts"][0];
    assert_eq!(
        entry["reference"], "finding",
        "the claimed relation: {entry}"
    );
    assert_eq!(entry["work"].as_str().unwrap(), producer.work_id);
    assert_eq!(entry["finding"].as_str().unwrap(), settled_id);
    assert_eq!(
        entry["resolved"]["work"].as_str().unwrap(),
        producer.work_id,
        "the resolved exact target: {entry}"
    );
    assert_eq!(
        entry["resolved"]["origin_event"].as_str().unwrap(),
        origin_event,
        "the relation resolves to the very event the index published: {entry}"
    );
    assert_eq!(
        entry["admitted_by"], "settled_estate_publication",
        "the actual admission route: {entry}"
    );
    assert_eq!(
        entry["target_standing"], "settled",
        "the target's own standing, which is not the claim's: {entry}"
    );
    assert_eq!(disagreement["work"].as_str().unwrap(), later.work_id);
    assert_eq!(disagreement["run"].as_str().unwrap(), later_run);
    assert_eq!(disagreement["claim_verified"], serde_json::json!(false));

    // (d) The prior record is intact, and nothing was promoted.
    assert_eq!(
        fs::read(
            estate
                .join("works")
                .join(&producer.work_id)
                .join("journal.ndjson")
        )
        .unwrap(),
        producer_journal_before,
        "naming a record appends nothing to the journal that holds it"
    );
    let (code, reread, stderr) =
        finding_cli(&estate, &["settle", "--finding", &settled_id, "--admin"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(reread["settled"].is_object(), "{reread}");
    assert!(
        reread["assertions"].as_array().unwrap().is_empty(),
        "{reread}"
    );
    assert!(
        reread["contradicts"].as_array().unwrap().is_empty(),
        "{reread}"
    );
    assert!(reread["supersedes"].is_null(), "{reread}");
    assert!(
        disagreement["settled"].is_null(),
        "a disagreement settles nothing of its own either: {disagreement}"
    );
    let after = index_for(&estate, &later.work_id);
    assert_eq!(
        after["rows"].as_array().unwrap().len(),
        1,
        "a work_local disagreement is not published by naming a published thing: {after}"
    );

    // (e) An estate-scoped disagreement still needs source evidence of
    //     its own: a relation is a claim about a record, never the
    //     admitted evidence that earns a publication.
    let (code, refused) = raise_raw(
        &estate,
        &later.work_id,
        &later_run,
        &[
            "--scope",
            "estate_local",
            "--kind",
            "contradicted_assumption",
            "--claim",
            "estate-scoped on a relation alone",
            "--contradicts",
            &relation,
        ],
    );
    assert_eq!(code, Some(3), "{refused}");
    assert!(refused.contains("NoAdmittedEvidence"), "{refused}");

    // (f) Its own record is nameable by itself, on the own-journal
    //     route, while its own journal lock is held for the raise.
    let (code, own, stderr) = raise_cli(
        &estate,
        &later.work_id,
        &later_run,
        &[
            "--scope",
            "work_local",
            "--kind",
            "relationship",
            "--claim",
            "restating my own disagreement",
            "--evidence",
            &format!("work/{}/finding/{disagreement_id}", later.work_id),
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(own["evidence"][0]["admitted_by"], "own_journal");
    assert_eq!(own["evidence"][0]["target_standing"], "unsettled");

    // (g) One refusal for every way of naming a record this Work may not
    //     have — including the real one, asked for by a denied Work.
    let denied_run = status(&pointer.socket, &denied.work_id)["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    let uniform = "no such finding is admitted to this work";
    let cases: Vec<(&str, String, &str, &str)> = vec![
        (
            "an absent finding in a real work",
            format!("work/{}/finding/finding-00000000000000-0", producer.work_id),
            &later.work_id,
            &later_run,
        ),
        (
            "a real finding under the wrong work",
            format!("work/{}/finding/{settled_id}", later.work_id),
            &later.work_id,
            &later_run,
        ),
        (
            "an absent work",
            format!("work/work-00000000000000-0/finding/{settled_id}"),
            &later.work_id,
            &later_run,
        ),
        (
            "an id from another estate",
            "work/work-ffffffffffffff-9/finding/finding-ffffffffffffff-9".to_string(),
            &later.work_id,
            &later_run,
        ),
        (
            "the real record, asked for by a denied work",
            relation.clone(),
            &denied.work_id,
            &denied_run,
        ),
    ];
    for (what, token, work, run) in cases {
        let (code, out) = raise_raw(
            &estate,
            work,
            run,
            &[
                "--scope",
                "work_local",
                "--kind",
                "contradicted_assumption",
                "--claim",
                "a negative control",
                "--contradicts",
                &token,
            ],
        );
        assert_eq!(code, Some(3), "{what}: {out}");
        assert!(
            out.contains("InadmissibleEvidence") && out.contains(uniform),
            "{what} must be refused in the one message every other failure gets: {out}"
        );
    }
    // And the denied Work learned nothing about the record it named
    // beyond the token it typed itself.
    let denied_index = index_for(&estate, &denied.work_id);
    assert!(
        denied_index["rows"].as_array().unwrap().is_empty(),
        "{denied_index}"
    );

    // (h) A malformed token is a *form* refusal that names the forms —
    //     the diagnostic whose absence produced "invalid coordinate hex".
    for token in [
        settled_id.as_str(),
        "work/x/finding/",
        "work//finding/y",
        "work/x/nonsense/y",
    ] {
        let (code, out) = raise_raw(
            &estate,
            &later.work_id,
            &later_run,
            &[
                "--scope",
                "work_local",
                "--kind",
                "gap",
                "--claim",
                "a form control",
                "--contradicts",
                token,
            ],
        );
        assert_eq!(code, Some(3), "{token}: {out}");
        assert!(out.contains("BadRequest"), "{token}: {out}");
        assert!(
            out.contains("work/<work-id>/finding/<finding-id>")
                && out.contains("work/<work-id>/event/<event-id>"),
            "a form refusal must name the forms: {out}"
        );
        assert!(
            !out.contains("coordinate hex"),
            "and must not send the caller after a coordinate it never wrote: {out}"
        );
    }

    // (i) Reading changed nothing, and the record survives a restart and
    //     an administrative rebuild exactly as it was frozen.
    let fingerprint = estate_fingerprint(&estate);
    for _ in 0..3 {
        index_for(&estate, &later.work_id);
        finding_cli(
            &estate,
            &[
                "settle",
                "--finding",
                &settled_id,
                "--requesting-work",
                &later.work_id,
            ],
        );
    }
    assert!(
        estate_fingerprint(&estate) == fingerprint,
        "a read must not move a byte"
    );
    stop_wirkd(&estate, wirkd_child);
    let (wirkd_child, _pointer) = start_wirkd(&estate);
    let (ok, _, err) = atlas(&estate, &["findings", "--rebuild", "--admin"]);
    assert!(ok, "{err}");
    let (code, recovered, stderr) = finding_cli(
        &estate,
        &[
            "list",
            "--work",
            &later.work_id,
            "--requesting-work",
            &later.work_id,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let recovered = recovered["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|record| record["id"].as_str() == Some(disagreement_id.as_str()))
        .expect("the disagreement survives a restart")
        .clone();
    assert_eq!(
        recovered["contradicts"][0], *entry,
        "frozen, not re-derived: {recovered}"
    );
    stop_wirkd(&estate, wirkd_child);
}

// ==== The journal lock discipline (ruling 0119) ============================
//
// `verify_*` here are the independent reviewer's own tests from
// `loop-b-finding-disagreement-verify/raw/50-verify-tests-findings.patch`,
// adopted as supplied except where noted; the rest are this correction's
// own, covering what the supplied pair does not: a real parent/child
// pair citing each other's events (the older half of the same defect),
// self-reference under contention, refused pairs, and the authority a
// raise must still hold at the moment it appends now that admission no
// longer runs under the journal guard.

/// Builds an estate holding two mutually-discoverable settled EstateLocal
/// publications produced by two Works with no kinship, and returns
/// (dir, estate, socket, [(work, run, finding_id) x 2], coordinate).
#[allow(clippy::type_complexity)]
fn verify_two_settled_publishers(
    dir: &tempfile::TempDir,
) -> (
    std::path::PathBuf,
    std::path::PathBuf,
    Vec<(String, String, String)>,
    String,
    KillOnDrop,
) {
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    write_actor_review_route(
        &estate,
        "socket_review",
        "Independently review whether bind_socket restricts the wirkd socket mode, and record the decision",
        r#"["review.md"]"#,
    );
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let repo = dir.path().join("demo-repo");
    init_repo(&repo);
    write_file(&repo, "socket.rs", "fn bind_socket() { socketmarker }\n");
    let coordinate = publish_and_locate(&estate, &repo, "socketmarker");

    let mut submitted = Vec::new();
    for _ in 0..2 {
        submitted.push(
            submit_kind(
                &estate,
                "socket_review",
                &repo,
                &["demo:write"],
                None,
                Some("actor"),
            )
            .unwrap(),
        );
    }
    let mut prepared = Vec::new();
    for s in &submitted {
        let worktree = materialize_actor(&pointer.socket, &estate, &s.work_id, &s.run_id);
        fs::write(
            worktree.join("review.md"),
            "recipe socket-boundary-review@3\ndecision: contradicted_assumption\n",
        )
        .unwrap();
        claim_ok(&estate, &s.work_id, &s.run_id, "review.md=review.md");
        let claim_event = claim_event_id(&estate, &s.work_id);
        let basis = obligation_basis_for(&estate, &s.work_id, "review");
        prepared.push((s.work_id.clone(), claim_event, basis));
    }
    let entries: Vec<(&str, &str, &str)> = prepared
        .iter()
        .map(|(_, _, basis)| ("socket-mode-reviewed", "1", basis.as_str()))
        .collect();
    write_estate_review_policy(&estate, &entries);

    let mut out = Vec::new();
    for (work_id, claim_event, _) in &prepared {
        let run = status(&pointer.socket, work_id)["run_id"]
            .as_str()
            .unwrap()
            .to_string();
        let (code, settled, stderr) = raise_cli(
            &estate,
            work_id,
            &run,
            &[
                "--scope",
                "estate_local",
                "--kind",
                "contradicted_assumption",
                "--claim",
                "the socket is bound with a wider mode than the trust boundary states",
                "--evidence",
                &format!("work/{work_id}/event/{claim_event}"),
                "--applies-to",
                &coordinate,
                "--obligation",
                "socket-mode-reviewed@1",
            ],
        );
        assert_eq!(code, Some(0), "{stderr}");
        assert!(settled["settled"].is_object(), "{settled}");
        out.push((
            work_id.clone(),
            run,
            settled["id"].as_str().unwrap().to_string(),
        ));
    }
    (estate, pointer.socket, out, coordinate, wirkd_child)
}

/// One `wirk finding raise`, spawned but not waited on.
fn spawn_raise(estate: &Path, work: &str, run: &str, args: &[&str]) -> std::process::Child {
    let mut full = vec!["finding", "raise"];
    full.extend_from_slice(args);
    full.push("--json");
    Command::new(wirk_bin())
        .args(&full)
        .env("WIRK_ESTATE_ROOT", estate)
        .env("WIRK_WORK_ID", work)
        .env("WIRK_RUN_ID", run)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn finding raise")
}

/// `wirk finding raise` on the human path, so a refusal's own sentence
/// is readable: the `--json` form the other helper uses prints nothing
/// parseable when the daemon refuses.
fn raise_text(estate: &Path, work: &str, run: &str, args: &[&str]) -> (Option<i32>, String) {
    let mut full = vec!["finding", "raise"];
    full.extend_from_slice(args);
    let output = Command::new(wirk_bin())
        .args(&full)
        .env("WIRK_ESTATE_ROOT", estate)
        .env("WIRK_WORK_ID", work)
        .env("WIRK_RUN_ID", run)
        .output()
        .expect("wirk finding raise runs");
    (
        output.status.code(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

/// Waits for every spawned client, or reports the ones still running at
/// `seconds`. Polling on `try_wait`, never a fixed sleep: a passing run
/// finishes in the time the raises actually take, and a hung one is
/// reported as hung rather than as slow.
fn join_or_report_hung(children: &mut [std::process::Child], seconds: u64) -> Vec<Option<i32>> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
    let mut codes: Vec<Option<i32>> = vec![None; children.len()];
    let mut done = vec![false; children.len()];
    loop {
        let mut all_done = true;
        for (index, child) in children.iter_mut().enumerate() {
            if done[index] {
                continue;
            }
            match child.try_wait().unwrap() {
                Some(status) => {
                    codes[index] = status.code();
                    done[index] = true;
                }
                None => all_done = false,
            }
        }
        if all_done {
            return codes;
        }
        if std::time::Instant::now() > deadline {
            let hung: Vec<usize> = done
                .iter()
                .enumerate()
                .filter_map(|(index, finished)| (!finished).then_some(index))
                .collect();
            for child in children.iter_mut() {
                let _ = child.kill();
                let _ = child.wait();
            }
            panic!(
                "{}/{} clients were still running after {seconds}s (indexes {hung:?}): \
                 a journal lock inversion wedges them permanently, and every later read of \
                 either journal with it (ruling 0119)",
                hung.len(),
                children.len()
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// Runs one read-only CLI call with its own wall clock, so a wedged
/// daemon is reported rather than hanging the test.
fn answers_within(label: &str, seconds: u64, args: &[&str]) -> bool {
    let mut child = Command::new(wirk_bin())
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            eprintln!("VERIFY: {label} -> exit {:?}", status.code());
            return true;
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            eprintln!("VERIFY: {label} -> HUNG");
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

fn finding_raised_ids(estate: &Path, work_id: &str) -> Vec<String> {
    journal_events(estate, work_id)
        .into_iter()
        .filter_map(|event| match event.kind {
            EventKind::FindingRaised { finding } => Some(finding.id.0),
            _ => None,
        })
        .collect()
}

/// **The lock-order question the builder's 12-way concurrency did not
/// ask.** Its twelve simultaneous raises all ran under *one* raising
/// Work, so they contended for one journal lock in one order. Two
/// distinct Works each naming the other's record take two locks in
/// opposite orders: `handle_finding_raise` held the raising Work's own
/// journal `Mutex` across `admit_evidence`, and the off-lineage branch
/// of `finding_reference_admitted` calls `replay_events` on the *named*
/// Work, which locks that Work's journal while the first is still held.
#[test]
fn verify_cross_work_relation_raises_do_not_deadlock() {
    let dir = tempfile::tempdir().unwrap();
    let (estate, _socket, works, coordinate, wirkd_child) = verify_two_settled_publishers(&dir);
    let (a_work, a_run, a_finding) = works[0].clone();
    let (b_work, b_run, b_finding) = works[1].clone();

    // Each direction alone must work first: this is a positive control,
    // not a hang detector on a route that never worked.
    for (w, r, target_work, target_finding) in [
        (&a_work, &a_run, &b_work, &b_finding),
        (&b_work, &b_run, &a_work, &a_finding),
    ] {
        let (code, out, stderr) = raise_cli(
            &estate,
            w,
            r,
            &[
                "--scope",
                "work_local",
                "--kind",
                "contradicted_assumption",
                "--claim",
                "sequential control",
                "--contradicts",
                &format!("work/{target_work}/finding/{target_finding}"),
            ],
        );
        assert_eq!(code, Some(0), "{stderr}");
        assert_eq!(
            out["contradicts"][0]["admitted_by"], "settled_estate_publication",
            "{out}"
        );
    }

    let a_token = format!("work/{a_work}/finding/{a_finding}");
    let b_token = format!("work/{b_work}/finding/{b_finding}");
    let before_a = finding_raised_ids(&estate, &a_work).len();
    let before_b = finding_raised_ids(&estate, &b_work).len();
    let rounds = 12;
    for round in 0..rounds {
        let claim = format!("AB/BA round {round}");
        let mut children = vec![
            spawn_raise(
                &estate,
                &a_work,
                &a_run,
                &[
                    "--scope",
                    "work_local",
                    "--kind",
                    "contradicted_assumption",
                    "--claim",
                    &claim,
                    "--contradicts",
                    &b_token,
                    "--evidence",
                    &coordinate,
                ],
            ),
            spawn_raise(
                &estate,
                &b_work,
                &b_run,
                &[
                    "--scope",
                    "work_local",
                    "--kind",
                    "contradicted_assumption",
                    "--claim",
                    &claim,
                    "--contradicts",
                    &a_token,
                    "--evidence",
                    &coordinate,
                ],
            ),
        ];
        let codes = join_or_report_hung(&mut children, 20);
        assert_eq!(codes, vec![Some(0), Some(0)], "round {round}");
    }

    // Not merely "nobody hung": every raise is on its own journal, once.
    // An observe/re-check loop that lost an append would show up here.
    assert_eq!(
        finding_raised_ids(&estate, &a_work).len(),
        before_a + rounds,
        "every accepted raise on A is journaled exactly once"
    );
    assert_eq!(
        finding_raised_ids(&estate, &b_work).len(),
        before_b + rounds,
        "every accepted raise on B is journaled exactly once"
    );

    stop_wirkd(&estate, wirkd_child);
}

/// The same question one noun over, on the route that predates this
/// change: two Works citing each other's journal events. Off lineage a
/// `Journal` reference is refused by the lineage check *before* the
/// other journal is replayed, so this pair takes no second lock — which
/// is exactly why it is here as a control, and why
/// `verify_parent_and_child_citing_each_others_events_do_not_deadlock`
/// below is the one that reaches the older half of the defect.
#[test]
fn verify_cross_work_journal_evidence_does_not_deadlock() {
    let dir = tempfile::tempdir().unwrap();
    let (estate, _socket, works, coordinate, wirkd_child) = verify_two_settled_publishers(&dir);
    let (a_work, a_run, _) = works[0].clone();
    let (b_work, b_run, _) = works[1].clone();
    let a_event = claim_event_id(&estate, &a_work);
    let b_event = claim_event_id(&estate, &b_work);

    for round in 0..8 {
        let claim = format!("journal AB/BA round {round}");
        let mut children = vec![
            spawn_raise(
                &estate,
                &a_work,
                &a_run,
                &[
                    "--scope",
                    "work_local",
                    "--kind",
                    "gap",
                    "--claim",
                    &claim,
                    "--evidence",
                    &format!("work/{b_work}/event/{b_event}"),
                ],
            ),
            spawn_raise(
                &estate,
                &b_work,
                &b_run,
                &[
                    "--scope",
                    "work_local",
                    "--kind",
                    "gap",
                    "--claim",
                    &claim,
                    "--evidence",
                    &format!("work/{a_work}/event/{a_event}"),
                ],
            ),
        ];
        join_or_report_hung(&mut children, 20);
    }
    let _ = coordinate;

    stop_wirkd(&estate, wirkd_child);
}

/// What the wedge does to everything else, asked while it is happening.
/// Adapted from the reviewer's characterisation test: the fixed sleep
/// that measured how permanent the deadlock was is replaced by polling,
/// and the three read surfaces it sampled are joined by a cross-Work
/// `finding assert` — so this is the brief's mixed simultaneous reads,
/// asserts and raises rather than a timing measurement of a known hang.
#[test]
fn verify_mixed_reads_asserts_and_raises_stay_answerable() {
    let dir = tempfile::tempdir().unwrap();
    let (estate, _socket, works, coordinate, wirkd_child) = verify_two_settled_publishers(&dir);
    let (a_work, a_run, a_finding) = works[0].clone();
    let (b_work, b_run, b_finding) = works[1].clone();
    let a_token = format!("work/{a_work}/finding/{a_finding}");
    let b_token = format!("work/{b_work}/finding/{b_finding}");

    let mut children = Vec::new();
    for round in 0..4 {
        let claim = format!("wedge {round}");
        children.push(spawn_raise(
            &estate,
            &a_work,
            &a_run,
            &[
                "--scope",
                "work_local",
                "--kind",
                "gap",
                "--claim",
                &claim,
                "--contradicts",
                &b_token,
                "--evidence",
                &coordinate,
            ],
        ));
        children.push(spawn_raise(
            &estate,
            &b_work,
            &b_run,
            &[
                "--scope",
                "work_local",
                "--kind",
                "gap",
                "--claim",
                &claim,
                "--contradicts",
                &a_token,
                "--evidence",
                &coordinate,
            ],
        ));
    }

    // Asked while the eight raises are in flight. Each of these reads a
    // journal the raises are writing: on the wedged daemon the whole
    // estate index still answered while both named Works went dark.
    let estate_arg = estate.to_str().unwrap();
    let index_ok = answers_within(
        "atlas findings --admin (whole estate)",
        20,
        &[
            "atlas", "findings", "--estate", estate_arg, "--admin", "--json",
        ],
    );
    let list_a_ok = answers_within(
        "finding list on A",
        20,
        &[
            "finding", "list", "--estate", estate_arg, "--work", &a_work, "--admin", "--json",
        ],
    );
    let settle_a_ok = answers_within(
        "finding settle read of A's settled publication",
        20,
        &[
            "finding",
            "settle",
            "--estate",
            estate_arg,
            "--finding",
            &a_finding,
            "--admin",
            "--json",
        ],
    );
    let assert_b_ok = answers_within(
        "finding assert on B's settled publication",
        20,
        &[
            "finding",
            "assert",
            "--estate",
            estate_arg,
            "--finding",
            &b_finding,
            "--decision",
            "acknowledged",
            "--by",
            "operator",
            "--reason",
            "read during simultaneous raises",
            "--admin",
            "--json",
        ],
    );

    let codes = join_or_report_hung(&mut children, 30);
    assert!(
        codes.iter().all(|code| *code == Some(0)),
        "every raise completed: {codes:?}"
    );
    assert!(index_ok, "the estate index answered during the raises");
    assert!(list_a_ok, "a named Work's own findings stayed readable");
    assert!(
        settle_a_ok,
        "a settled estate publication stayed readable while its Work was being written"
    );
    assert!(assert_b_ok, "an assertion landed on the other named Work");

    stop_wirkd(&estate, wirkd_child);
}

/// The older half of the same defect, which is not reachable through two
/// unrelated Works: a **real parent and child**, each citing the other's
/// journal event. Both references are on lineage, so both admissions
/// replay the other Work's journal — the two locks, opposite orders, on
/// a route that has nothing to do with the off-lineage relation added
/// later. Reproduced on the parent daemon by the independent review
/// (`raw/48-lineage-ab-ba-parent-cli.txt`).
#[test]
fn verify_parent_and_child_citing_each_others_events_do_not_deadlock() {
    let dir = tempfile::tempdir().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    write_container_route(&estate, "obliged_container", "outer", "echo a > a.md");
    write_child_check_route(&estate, "helper_check", "echo 0775 > socket-mode.txt");
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let parent_repo = dir.path().join("parent-repo");
    init_repo(&parent_repo);
    let parent = submit(
        &estate,
        "obliged_container",
        &parent_repo,
        &["demo:write", "helper:write"],
        None,
    )
    .unwrap();

    // The parent keeps its leaf Run open, so both Works can raise.
    let helper_repo = dir.path().join("helper-repo");
    init_repo(&helper_repo);
    let child = submit(
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
    .unwrap();

    let parent_event = submitted_event_id(&estate, &parent.work_id);
    let child_event = submitted_event_id(&estate, &child.work_id);
    let child_run = status(&pointer.socket, &child.work_id)["run_id"]
        .as_str()
        .unwrap()
        .to_string();

    // Sequential controls: each direction alone is a real lineage
    // reference, accepted, before any of it is done at the same time.
    // The parent's own reference to its child resolves `admitted`; the
    // child's reference upward is recorded `Unavailable`, because the
    // parent's `WorkSubmitted` names a binding the narrower child does
    // not hold — an established disclosure outcome, unchanged here, and
    // reached only *after* the parent's journal has been replayed, which
    // is what makes it take the same second lock an admission does.
    for (work, run, token, expected) in [
        (
            &parent.work_id,
            &parent.run_id,
            format!("work/{}/event/{child_event}", child.work_id),
            "admitted",
        ),
        (
            &child.work_id,
            &child_run,
            format!("work/{}/event/{parent_event}", parent.work_id),
            "unavailable",
        ),
    ] {
        let (code, out, stderr) = raise_cli(
            &estate,
            work,
            run,
            &[
                "--scope",
                "work_local",
                "--kind",
                "gap",
                "--claim",
                "lineage sequential control",
                "--evidence",
                &token,
            ],
        );
        assert_eq!(code, Some(0), "{stderr}");
        assert_eq!(out["evidence"][0]["outcome"], expected, "{out}");
    }

    for round in 0..12 {
        let claim = format!("lineage AB/BA round {round}");
        let mut children = vec![
            spawn_raise(
                &estate,
                &parent.work_id,
                &parent.run_id,
                &[
                    "--scope",
                    "work_local",
                    "--kind",
                    "gap",
                    "--claim",
                    &claim,
                    "--evidence",
                    &format!("work/{}/event/{child_event}", child.work_id),
                ],
            ),
            spawn_raise(
                &estate,
                &child.work_id,
                &child_run,
                &[
                    "--scope",
                    "work_local",
                    "--kind",
                    "gap",
                    "--claim",
                    &claim,
                    "--evidence",
                    &format!("work/{}/event/{parent_event}", parent.work_id),
                ],
            ),
        ];
        let codes = join_or_report_hung(&mut children, 20);
        assert_eq!(codes, vec![Some(0), Some(0)], "round {round}");
    }

    // Both journals are still readable, and the lineage relationship is
    // intact rather than merely unlocked.
    let (code, listed, stderr) =
        finding_cli(&estate, &["list", "--work", &parent.work_id, "--admin"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(listed["findings"].as_array().unwrap().len(), 13, "{listed}");

    stop_wirkd(&estate, wirkd_child);
}

/// The case the `Mutex` non-reentrancy comment was written for, now that
/// the guard is taken later: a Work naming *its own* record, many times
/// at once. Every raise must be journaled exactly once — an
/// observe/re-check loop that dropped a loser's append instead of
/// re-reading would lose findings here, and one that re-locked its own
/// journal during admission would hang on the first token.
#[test]
fn verify_simultaneous_self_referencing_raises_are_all_journaled_once() {
    let dir = tempfile::tempdir().unwrap();
    let (estate, _socket, works, coordinate, wirkd_child) = verify_two_settled_publishers(&dir);
    let (a_work, a_run, a_finding) = works[0].clone();
    let own = format!("work/{a_work}/finding/{a_finding}");
    let before = finding_raised_ids(&estate, &a_work).len();

    let concurrent = 8;
    let mut children = Vec::new();
    for round in 0..concurrent {
        let claim = format!("own reference {round}");
        children.push(spawn_raise(
            &estate,
            &a_work,
            &a_run,
            &[
                "--scope",
                "work_local",
                "--kind",
                "gap",
                "--claim",
                &claim,
                "--contradicts",
                &own,
                "--evidence",
                &coordinate,
            ],
        ));
    }
    let codes = join_or_report_hung(&mut children, 40);
    assert!(
        codes.iter().all(|code| *code == Some(0)),
        "a Work may always name its own record: {codes:?}"
    );
    let raised = finding_raised_ids(&estate, &a_work);
    assert_eq!(
        raised.len(),
        before + concurrent,
        "no update was lost to the re-check loop"
    );
    let unique: std::collections::HashSet<&String> = raised.iter().collect();
    assert_eq!(unique.len(), raised.len(), "no id was journaled twice");

    // The route is still the own-journal one, not the publication one.
    let (code, out, stderr) = raise_cli(
        &estate,
        &a_work,
        &a_run,
        &[
            "--scope",
            "work_local",
            "--kind",
            "gap",
            "--claim",
            "own route control",
            "--contradicts",
            &own,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(out["contradicts"][0]["admitted_by"], "own_journal", "{out}");

    stop_wirkd(&estate, wirkd_child);
}

/// Refusals take the same locks admissions do — `finding_reference_admitted`
/// replays the named Work's journal *before* it discovers the record is
/// not published — so two Works simultaneously naming each other's
/// never-published `WorkLocal` records wedge exactly the same way. The
/// refusal must also stay the single undifferentiated one.
#[test]
fn verify_simultaneous_refused_cross_work_raises_do_not_deadlock() {
    let dir = tempfile::tempdir().unwrap();
    let (estate, _socket, works, coordinate, wirkd_child) = verify_two_settled_publishers(&dir);
    let (a_work, a_run, _) = works[0].clone();
    let (b_work, b_run, _) = works[1].clone();

    // A never-published record in each: `WorkLocal` is never indexed and
    // never nameable off lineage.
    let mut private = Vec::new();
    for (work, run) in [(&a_work, &a_run), (&b_work, &b_run)] {
        let (code, out, stderr) = raise_cli(
            &estate,
            work,
            run,
            &[
                "--scope",
                "work_local",
                "--kind",
                "gap",
                "--claim",
                "not for publication",
                "--evidence",
                &coordinate,
            ],
        );
        assert_eq!(code, Some(0), "{stderr}");
        private.push(out["id"].as_str().unwrap().to_string());
    }

    for round in 0..8 {
        let claim = format!("refused AB/BA round {round}");
        let mut children = vec![
            spawn_raise(
                &estate,
                &a_work,
                &a_run,
                &[
                    "--scope",
                    "work_local",
                    "--kind",
                    "gap",
                    "--claim",
                    &claim,
                    "--contradicts",
                    &format!("work/{b_work}/finding/{}", private[1]),
                ],
            ),
            spawn_raise(
                &estate,
                &b_work,
                &b_run,
                &[
                    "--scope",
                    "work_local",
                    "--kind",
                    "gap",
                    "--claim",
                    &claim,
                    "--contradicts",
                    &format!("work/{a_work}/finding/{}", private[0]),
                ],
            ),
        ];
        let codes = join_or_report_hung(&mut children, 20);
        assert!(
            codes.iter().all(|code| *code == Some(3)),
            "a never-published record is not nameable off lineage: {codes:?}"
        );
    }

    // One refusal for every failure, still: the same message a wholly
    // absent finding gets.
    let (code, refused) = raise_text(
        &estate,
        &a_work,
        &a_run,
        &[
            "--scope",
            "work_local",
            "--kind",
            "gap",
            "--claim",
            "refusal wording",
            "--contradicts",
            &format!("work/{b_work}/finding/{}", private[1]),
        ],
    );
    assert_eq!(code, Some(3), "{refused}");
    assert!(
        refused.contains("no such finding is admitted to this work"),
        "{refused}"
    );

    stop_wirkd(&estate, wirkd_child);
}

/// Admission no longer runs under the raising Work's journal guard, so
/// the authority a raise rests on is re-checked when the guard is
/// re-taken. These are the two ways that authority actually moves: the
/// Run is superseded by a retry, and the Work goes terminal. Neither may
/// produce a Finding appended after the fact.
#[test]
fn verify_authority_is_rechecked_when_the_journal_guard_is_retaken() {
    let dir = tempfile::tempdir().unwrap();
    let (estate, socket, works, coordinate, wirkd_child) = verify_two_settled_publishers(&dir);
    let (a_work, a_run, _) = works[0].clone();
    let (b_work, b_run, b_finding) = works[1].clone();
    let b_token = format!("work/{b_work}/finding/{b_finding}");

    // Superseded Run, sequentially: the refusal is the currency check,
    // and it appends nothing at all.
    fail_via_socket(&socket, &estate, &a_work, &a_run);
    let (code, out) = retry_run_cli(&estate, &a_work, &a_run);
    assert_eq!(code, Some(0), "{out}");
    let current_a = status(&socket, &a_work)["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(current_a, a_run);
    let before = journal_events(&estate, &a_work).len();
    let (code, refused) = raise_text(
        &estate,
        &a_work,
        &a_run,
        &[
            "--scope",
            "work_local",
            "--kind",
            "gap",
            "--claim",
            "from a superseded run",
            "--contradicts",
            &b_token,
            "--evidence",
            &coordinate,
        ],
    );
    assert_eq!(code, Some(3), "{refused}");
    assert!(
        refused.contains("not current for its waypoint"),
        "{refused}"
    );
    assert_eq!(
        journal_events(&estate, &a_work).len(),
        before,
        "a refused raise writes nothing"
    );

    // Terminal Work, concurrently: raises and a cancel at the same
    // moment. Whatever the interleaving, the journal must never carry a
    // Finding raised after the Work was canceled — that is the check
    // being re-asked under the guard rather than trusted from the
    // observation admission was decided on.
    let mut children = Vec::new();
    for round in 0..8 {
        let claim = format!("racing the cancel {round}");
        children.push(spawn_raise(
            &estate,
            &b_work,
            &b_run,
            &[
                "--scope",
                "work_local",
                "--kind",
                "gap",
                "--claim",
                &claim,
                "--contradicts",
                &format!("work/{a_work}/finding/{}", works[0].2),
                "--evidence",
                &coordinate,
            ],
        ));
    }
    let (code, out) = cancel_cli(&estate, &b_work, false);
    assert_eq!(code, Some(0), "{out}");
    join_or_report_hung(&mut children, 40);

    let events = journal_events(&estate, &b_work);
    let canceled_at = events
        .iter()
        .position(|event| matches!(event.kind, EventKind::WorkCanceled { .. }))
        .expect("the Work was canceled");
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.kind, EventKind::WorkCanceled { .. }))
            .count(),
        1,
        "exactly one cancellation"
    );
    assert!(
        !events[canceled_at + 1..]
            .iter()
            .any(|event| matches!(event.kind, EventKind::FindingRaised { .. })),
        "no Finding is appended to a Work that was already terminal"
    );
    // And the uninvolved Work is untouched by any of it.
    let (code, listed, stderr) = finding_cli(&estate, &["list", "--work", &a_work, "--admin"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        !listed["findings"].as_array().unwrap().is_empty(),
        "{listed}"
    );

    stop_wirkd(&estate, wirkd_child);
}

/// `applies_to` accepts a relation token now, and `applies_to` is what
/// `reviewed_targets` matches a frozen review target against. A relation
/// must cover no target: naming a record is not reviewing a source.
#[test]
fn verify_a_relation_token_in_applies_to_discharges_no_review_target() {
    let dir = tempfile::tempdir().unwrap();
    let (estate, socket, works, coordinate, wirkd_child) = verify_two_settled_publishers(&dir);
    let (a_work, a_run, a_finding) = works[0].clone();
    let relation = format!("work/{a_work}/finding/{a_finding}");
    let claim_event = claim_event_id(&estate, &a_work);

    // Same Work, same obligation, same everything — except the frozen
    // review target is offered a relation instead of the coordinate.
    let (code, out, stderr) = raise_cli(
        &estate,
        &a_work,
        &a_run,
        &[
            "--scope",
            "estate_local",
            "--kind",
            "contradicted_assumption",
            "--claim",
            "a relation offered where a reviewed target belongs",
            "--evidence",
            &format!("work/{a_work}/event/{claim_event}"),
            "--applies-to",
            &relation,
            "--obligation",
            "socket-mode-reviewed@1",
        ],
    );
    eprintln!("VERIFY relation-in-applies-to: code={code:?} stderr={stderr}");
    if code == Some(0) {
        assert!(
            out["settled"].is_null(),
            "a relation must not discharge a frozen review target: {out}"
        );
        let entry = &out["applies_to"][0];
        assert_eq!(entry["reference"], "finding", "{out}");
    }
    // And the coordinate still does, so this is the token and not the call.
    let (code2, out2, stderr2) = raise_cli(
        &estate,
        &a_work,
        &a_run,
        &[
            "--scope",
            "estate_local",
            "--kind",
            "contradicted_assumption",
            "--claim",
            "the same call with the coordinate it should have had",
            "--evidence",
            &format!("work/{a_work}/event/{claim_event}"),
            "--applies-to",
            &coordinate,
            "--obligation",
            "socket-mode-reviewed@1",
        ],
    );
    assert_eq!(code2, Some(0), "{stderr2}");
    assert!(out2["settled"].is_object(), "positive control: {out2}");
    let _ = socket;
    stop_wirkd(&estate, wirkd_child);
}

// ---- F5: the finding-coordinate namespace, reached at the gate -------

/// A managed output's recorded path is `claims/<claim>/<name>`. Nothing
/// stops a *repository* from containing a file at that literal path, so
/// the two namespaces can collide by string equality alone — and
/// `resolve_claim_attribution`'s answer, "only a `Worktree` receipt
/// names a source coordinate", is the only thing between that collision
/// and a Work being credited with mutating a source it never touched
/// (ruling 0145; F5 of the independent native-foundation review, which
/// recorded the control as present, correct and **untested**).
///
/// **This reaches the gate, it does not stop short of it.** The
/// independent review's own reason for not reproducing this live was
/// that everything before the store filter — a current Run, a Validated
/// Done Claim, a `Write` binding on the named source, a matching
/// execution identity — has to hold first, and that
/// `ReadOnlyMutationCredit` refusing earlier proves nothing about the
/// filter. So every one of those gates is satisfied here, twice, by one
/// Work with a real `Write` binding on a real published Atlas source:
///
/// * `wp-1` files a real Claim of a **managed** output, minting claim
///   `C1` and a receipt `store=work_outputs path=claims/C1/REPORT.md`;
/// * the repository is then given a real file at that same literal path,
///   `claims/C1/REPORT.md`, committed and published;
/// * `wp-2` files a real Claim of **that repository file**, minting a
///   receipt `store=worktree path=claims/C1/REPORT.md`.
///
/// The two receipts carry the **same path string and the same digest**
/// — the staged bytes and the committed bytes are byte-identical on
/// purpose — and differ in exactly one field, `store`. So the positive
/// control (cite `wp-2`) and the denied control (cite `wp-1`) differ in
/// nothing the earlier gates can see, and the only check that can
/// separate them is the store discriminator. A synthetic pair of
/// constructed receipts could not make that argument.
#[test]
fn a_managed_receipt_never_attests_a_source_coordinate_that_collides_with_its_path() {
    let dir = tempfile::tempdir().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    route_fixture::write_route(
        &estate,
        "collide_three",
        r#"{"id":"collide-three","waypoints":[
            {"id":"wp-1","kind":"Deterministic","command":["true"],"declared_outputs":[{"name":"REPORT.md","required":true}]},
            {"id":"wp-2","kind":"Deterministic","command":["true"],"declared_outputs":[{"name":"REPORT.md","required":true}]},
            {"id":"wp-3","kind":"Deterministic","command":["true"],"declared_outputs":[{"name":"done.md","required":true}]}
        ]}"#,
    );
    let (wirkd_child, pointer) = start_wirkd(&estate);
    let repo = publish_source(dir.path(), &estate, "demo", "repo");
    let work = submit(&estate, "collide_three", &repo, &["demo:write"], None).unwrap();
    let wp1_run = work.run_id.clone();

    // The bytes both receipts will attest. Distinctive so the Work's own
    // admitted Atlas search resolves this file and no other.
    const REPORT: &str = "namespacecollisionprobe: the reviewed report\n";

    // ---- wp-1: a real managed-output Claim -------------------------
    let staging = {
        let out = Command::new(wirk_bin())
            .args(["output", "dir"])
            .env("WIRK_ESTATE_ROOT", &estate)
            .env("WIRK_WORK_ID", &work.work_id)
            .env("WIRK_RUN_ID", &wp1_run)
            .output()
            .expect("wirk output dir runs");
        assert_eq!(
            out.status.code(),
            Some(0),
            "wirk output dir: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        std::path::PathBuf::from(String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    fs::write(staging.join("REPORT.md"), REPORT).expect("stage the managed output");
    let (code, stdout) = claim(&estate, &work.work_id, &wp1_run, &["--output", "REPORT.md"]);
    assert_eq!(code, Some(0), "the managed Claim must validate: {stdout}");

    // The receipt wirkd actually recorded, read back from the daemon —
    // its `claims/<claim>/<name>` path is the collision this test then
    // builds in the repository.
    let managed = receipt_with_store(&pointer.socket, &work.work_id, "work_outputs")
        .expect("wp-1's managed receipt is recorded");
    let managed_path = managed["path"].as_str().expect("managed path").to_string();
    let managed_digest = managed["digest"]
        .as_str()
        .expect("managed digest")
        .to_string();
    assert!(
        managed_path.starts_with("claims/") && managed_path.ends_with("/REPORT.md"),
        "the managed namespace is claims/<claim>/<name>: {managed_path}"
    );

    // ---- the repository really does hold a file at that path --------
    fs::create_dir_all(repo.join(&managed_path).parent().unwrap())
        .expect("the repository's own claims/<claim>/ directory");
    // Published first at *other* bytes, so the Finding's own
    // `applies_to` is a real before-state at a real generation.
    fs::write(
        repo.join(&managed_path),
        "namespacecollisionprobe: before\n",
    )
    .unwrap();
    let _rev_before = republish(&estate, &repo, "demo", "collide-before");

    let coordinate = {
        let (ok, search, err) = atlas(
            &estate,
            &[
                "search",
                "--work",
                &work.work_id,
                "--query",
                "namespacecollisionprobe",
            ],
        );
        assert!(ok, "{err}");
        search["hits"][0]["coordinate"]
            .as_str()
            .expect("the Work's own admitted search resolves the colliding path")
            .to_string()
    };
    let (code, raised, stderr) = raise_cli(
        &estate,
        &work.work_id,
        &wp1_run,
        &[
            "--kind",
            "contradicted_assumption",
            "--scope",
            "estate_local",
            "--claim",
            "the report at the colliding path is stale",
            "--evidence",
            &coordinate,
            "--applies-to",
            &coordinate,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let finding = raised["id"].as_str().unwrap().to_string();

    // ---- wp-2: a real worktree Claim at the identical path ----------
    let wp2_run = status(&pointer.socket, &work.work_id)["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(wp2_run, wp1_run);
    fs::write(repo.join(&managed_path), REPORT).unwrap();
    let (code, stdout) = claim(
        &estate,
        &work.work_id,
        &wp2_run,
        &["--artifact", &format!("REPORT.md={managed_path}")],
    );
    assert_eq!(code, Some(0), "the worktree Claim must validate: {stdout}");
    let rev_after = republish(&estate, &repo, "demo", "collide-after");

    let worktree = receipt_with_store(&pointer.socket, &work.work_id, "worktree")
        .expect("wp-2's worktree receipt is recorded");
    // The premise of the whole test, asserted rather than assumed: the
    // two receipts are indistinguishable except by `store`.
    assert_eq!(worktree["path"].as_str().unwrap(), managed_path);
    assert_eq!(worktree["digest"].as_str().unwrap(), managed_digest);

    let wp3_run = status(&pointer.socket, &work.work_id)["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(wp3_run, wp2_run);

    // ---- denied control: the managed receipt alone ------------------
    // Every earlier gate holds for this Run — it is current for `wp-1`,
    // it carries a Validated Done Claim, its Work holds `Write` on
    // `demo`, and its execution identity is this very repository — so
    // the refusal below is the store discriminator firing and nothing
    // else.
    let refusal = applied_refusal(
        &estate,
        &work.work_id,
        &wp3_run,
        &[
            "--finding",
            &finding,
            "--source",
            "demo",
            "--revision",
            &rev_after,
            "--by",
            "root",
            "--claim-run",
            &wp1_run,
        ],
    );
    assert!(
        refusal.contains("ChangedClaimedArtifact"),
        "a managed receipt whose path collides with a repository path must not attest it: {refusal}"
    );

    // ---- positive control: the worktree receipt ---------------------
    let (code, applied, stderr) = applied_cli(
        &estate,
        &work.work_id,
        &wp3_run,
        &[
            "--finding",
            &finding,
            "--source",
            "demo",
            "--revision",
            &rev_after,
            "--by",
            "root",
            "--claim-run",
            &wp2_run,
            "--json",
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let record = applied["applied"].as_array().unwrap().last().unwrap();
    assert_eq!(record["attribution"]["attribution"], "claim");
    assert_eq!(
        record["attribution"]["run"], wp2_run,
        "the Worktree receipt at the same path is what attests: {record}"
    );

    stop_wirkd(&estate, wirkd_child);
}

/// The first artifact receipt this Work recorded in the named store, as
/// `wirk work status --admin` reports it.
fn receipt_with_store(socket: &Path, work_id: &str, store: &str) -> Option<serde_json::Value> {
    let status = status(socket, work_id);
    for entry in status["evidence"].as_array().cloned().unwrap_or_default() {
        for artifact in entry["artifacts"].as_array().cloned().unwrap_or_default() {
            if artifact["store"].as_str() == Some(store) {
                return Some(artifact);
            }
        }
    }
    None
}

// ---- F4: the readiness ladder, every reachable rung, both verbs ------

/// The `ready.reason` for one Finding, as `work obligations` reports it.
fn obligations_reason(estate: &Path, work_id: &str, waypoint: &str, finding: &str) -> String {
    let (code, reply, stderr) = obligations_cli(estate, &["--work", work_id, "--admin"]);
    assert_eq!(code, Some(0), "{stderr}");
    let entry = obligation_entry(&reply, waypoint);
    let row = entry["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["finding"].as_str() == Some(finding))
        .unwrap_or_else(|| panic!("no finding row for {finding}: {entry}"));
    assert_eq!(row["ready"]["state"].as_str(), Some("not-ready"), "{row}");
    row["ready"]["reason"].as_str().unwrap().to_string()
}

/// The `pending.reason` for one Finding, as `finding settle` reports it.
fn settle_reason(estate: &Path, finding: &str) -> String {
    let (code, pending, stderr) = finding_cli(estate, &["settle", "--finding", finding, "--admin"]);
    assert_eq!(code, Some(0), "{stderr}");
    pending["pending"]["reason"]
        .as_str()
        .unwrap_or_else(|| panic!("no pending reason: {pending}"))
        .to_string()
}

/// Every readiness reason the ladder can reach, walked as a real
/// operator walks it — by editing this estate's own
/// `policy/settlement.json` between calls and asking both verbs the same
/// question about the same Finding each time.
///
/// **What this replaces.** The change shipped one test, for
/// `no-policy-file`, which is also the one branch the manual CLI probe
/// ruling 0146 recorded had already covered. Six values were untested
/// (F4 of the independent native-foundation review).
///
/// **The defect the untested branches hid.** `obligation-basis-not-admitted`
/// derived its basis *only* from `settlement_candidates`, and a Finding
/// has a settlement candidate only when its check already holds. So in
/// the very situation the value names — the estate admitted the
/// obligation at some basis, but not the one this Waypoint derives, which
/// is *why* no check holds — the arm could not fire, and the operator was
/// handed the vaguer `no-admitted-check-holds-yet` while
/// `admission.state` in the same reply already said `basis-not-admitted`.
/// Row 5 below is that case; it asserts the two halves of one reply agree,
/// and it is watched red against the pre-correction candidate, which
/// answers `no-admitted-check-holds-yet` there.
///
/// **The seventh value.** `no-obligation-named` is reachable through
/// `finding settle` and *structurally unreachable* through `work
/// obligations`, because that verb lists a Finding only under the
/// obligation the Finding itself names — a Finding naming none is in no
/// obligation's list. That is asserted below as the truthful behaviour it
/// is, rather than manufactured into a row.
#[test]
fn the_readiness_reason_ladder_is_walked_end_to_end_and_both_verbs_agree() {
    let dir = tempfile::tempdir().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    write_two_leaf_route(&estate);
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let repo = dir.path().join("repo");
    init_repo(&repo);
    let work = submit(&estate, "two_leaf", &repo, &["demo:write"], None).unwrap();
    write_file(&repo, "out1.md", "one\n");
    claim_ok(&estate, &work.work_id, &work.run_id, "out1.md=out1.md");
    let wp2_run = status(&pointer.socket, &work.work_id)["run_id"]
        .as_str()
        .unwrap()
        .to_string();

    // Evidence naming `WorkSubmitted`, not wp-1's own `ClaimRecorded`:
    // `deterministic_verified_readiness` requires the cited event to be
    // that Waypoint's own Validated Done Claim, so the check is
    // genuinely unheld and every rung below is a real not-ready.
    let evidence = format!(
        "work/{}/event/{}",
        work.work_id,
        submitted_event_id(&estate, &work.work_id)
    );
    let raise_obliged = |claim: &str| -> String {
        let (code, raised, stderr) = raise_cli(
            &estate,
            &work.work_id,
            &wp2_run,
            &[
                "--kind",
                "verified_outcome",
                "--scope",
                "estate_local",
                "--evidence",
                &evidence,
                "--claim",
                claim,
                "--obligation",
                "out1-produced@1",
            ],
        );
        assert_eq!(code, Some(0), "{stderr}");
        assert!(raised["settled"].is_null(), "{raised}");
        raised["id"].as_str().unwrap().to_string()
    };
    let finding = raise_obliged("out1.md was produced as wp-1 declared");
    let real_basis = obligation_basis_for(&estate, &work.work_id, "wp-1");

    // ---- 1. no policy file at all ----------------------------------
    let mut walked: Vec<&str> = Vec::new();
    let both = |expected: &str, walked: &mut Vec<&'static str>, tag: &'static str| {
        let settle = settle_reason(&estate, &finding);
        let obligations = obligations_reason(&estate, &work.work_id, "wp-1", &finding);
        assert_eq!(settle, expected, "finding settle at rung {tag}");
        assert_eq!(
            obligations, expected,
            "work obligations must name the identical reason at rung {tag}"
        );
        walked.push(tag);
    };
    assert!(!estate.join("policy").join("settlement.json").exists());
    both("no-policy-file", &mut walked, "no-policy-file");

    // ---- 2. a policy file that cannot be read ----------------------
    write_policy(&estate, "{not json at all");
    both("policy-unreadable", &mut walked, "policy-unreadable");

    // ---- 3. a policy admitting some *other* obligation -------------
    write_policy_admitting(
        &estate,
        "deterministic_verified",
        "verified_outcome",
        &[("some-other-obligation", "1", &real_basis)],
    );
    both(
        "obligation-not-admitted",
        &mut walked,
        "obligation-not-admitted",
    );

    // ---- 4. the name admitted, at a basis this estate never derives -
    // The rung the correction makes reachable.
    write_policy_admitting(
        &estate,
        "deterministic_verified",
        "verified_outcome",
        &[(
            "out1-produced",
            "1",
            "0000000000000000000000000000000000000000000000000000000000000000",
        )],
    );
    both(
        "obligation-basis-not-admitted",
        &mut walked,
        "obligation-basis-not-admitted",
    );
    // …and the *same reply* agrees with itself: the admission object
    // eight lines above the finding row says the same thing, which is
    // exactly what the vaguer answer used to contradict.
    let (_, reply, _) = obligations_cli(&estate, &["--work", &work.work_id, "--admin"]);
    let entry = obligation_entry(&reply, "wp-1");
    assert_eq!(
        entry["admission"]["state"].as_str(),
        Some("basis-not-admitted"),
        "the reply's own admission state: {entry}"
    );

    // ---- 5. the real derived basis admitted, the check still unheld -
    write_policy_admitting(
        &estate,
        "deterministic_verified",
        "verified_outcome",
        &[("out1-produced", "1", &real_basis)],
    );
    both(
        "no-admitted-check-holds-yet",
        &mut walked,
        "no-admitted-check-holds-yet",
    );
    let (_, reply, _) = obligations_cli(&estate, &["--work", &work.work_id, "--admin"]);
    assert_eq!(
        obligation_entry(&reply, "wp-1")["admission"]["state"].as_str(),
        Some("admitted"),
        "the basis really is admitted now, so the reason is about the check"
    );

    // ---- 6. a Finding naming no obligation at all ------------------
    // Reachable through `finding settle`; structurally absent from
    // `work obligations`, which lists a Finding only under the
    // obligation that Finding names.
    let (code, unobliged, stderr) = raise_cli(
        &estate,
        &work.work_id,
        &wp2_run,
        &[
            "--kind",
            "verified_outcome",
            "--scope",
            "estate_local",
            "--evidence",
            &evidence,
            "--claim",
            "an outcome this Work claims without naming an obligation",
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let unobliged = unobliged["id"].as_str().unwrap().to_string();
    assert_eq!(settle_reason(&estate, &unobliged), "no-obligation-named");
    let (_, reply, _) = obligations_cli(&estate, &["--work", &work.work_id, "--admin"]);
    assert!(
        !obligation_entry(&reply, "wp-1")["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["finding"].as_str() == Some(unobliged.as_str())),
        "a Finding naming no obligation is under no obligation's list, so this \
         value is honestly unreachable here rather than reported as something else"
    );
    walked.push("no-obligation-named");

    // Six of the seven, plus `review-targets-unresolved`, which needs an
    // Actor obligation with a `review` contract and is walked by
    // `an_actor_review_whose_selector_never_resolved_says_so`.
    assert_eq!(walked.len(), 6, "walked: {walked:?}");

    // ---- F3: one coherent request, many findings -------------------
    // The reason is a property of the Finding and the policy, never of
    // how many Findings share the reply. Sixty more on the same
    // obligation must not change any answer — the shared snapshot the
    // correction reuses is the same snapshot the single-Finding reply
    // was computed from.
    for i in 0..60 {
        raise_obliged(&format!("bulk probe {i}"));
    }
    let (code, reply, stderr) = obligations_cli(&estate, &["--work", &work.work_id, "--admin"]);
    assert_eq!(code, Some(0), "{stderr}");
    let rows = obligation_entry(&reply, "wp-1")["findings"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(rows.len(), 61, "every obliged finding is listed");
    for row in &rows {
        assert_eq!(
            row["ready"]["reason"].as_str(),
            Some("no-admitted-check-holds-yet"),
            "the reason must not depend on how many findings share the reply: {row}"
        );
    }
    assert_eq!(
        settle_reason(&estate, &finding),
        "no-admitted-check-holds-yet",
        "and `finding settle` still agrees after the bulk"
    );

    // ---- read stays read -------------------------------------------
    // Neither verb settled anything and neither appended: the correction
    // moves *where* the inputs are read, never whether reading mutates.
    let before = estate_fingerprint(&estate);
    let _ = obligations_cli(&estate, &["--work", &work.work_id, "--admin"]);
    let _ = settle_reason(&estate, &finding);
    assert_eq!(
        before,
        estate_fingerprint(&estate),
        "reading why a Finding is not ready must move no byte of the estate"
    );

    // ---- a same-lineage requester with narrower bindings ------------
    // The scoped view withholds the authored half and keeps readiness:
    // the reason is a closed vocabulary with no alias, path, digest or
    // sentence in it.
    let (code, scoped, stderr) = obligations_cli(
        &estate,
        &["--work", &work.work_id, "--requesting-work", &work.work_id],
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(scoped["scope"].as_str(), Some("requester"));
    let scoped_entry = obligation_entry(&scoped, "wp-1");
    let scoped_row = scoped_entry["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["finding"].as_str() == Some(finding.as_str()))
        .unwrap();
    assert_eq!(
        scoped_row["ready"]["reason"].as_str(),
        Some("no-admitted-check-holds-yet"),
        "readiness is Kept in the scoped view: {scoped_row}"
    );

    stop_wirkd(&estate, wirkd_child);
}
