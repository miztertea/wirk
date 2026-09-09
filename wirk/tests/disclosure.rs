//! Real-daemon, real-Git, real-Atlas proof of the W-B **disclosure**
//! contract (`knowledge/work/p3-world-loop/W-B-DISCLOSURE-REPAIR.md`;
//! `W-B-CONSTRUCTION-REVIEW.md` "journal kinship is not universal
//! evidence access"; `W-B-AUTHORITY-ADJUDICATION.md` "source-scoped
//! nested journal evidence ... scoped actor consultation").
//!
//! The boundary under test is one sentence: **lineage grants permission
//! to reference another Work's journal; it never grants disclosure of
//! that journal's sources.** A Work sees a referenced record's *journal
//! identities* (Work, Run, Claim, Event, Finding, Waypoint, role) on
//! lineage permission alone, and its *source content* (alias,
//! generation, object id, coordinate, checkout path, argv, digest) only
//! when its own bindings independently admit that source.
//!
//! Every case here drives the built `wirk` binary against a real
//! `wirkd`, real Git repositories and a real published Atlas — the
//! discipline `findings.rs` already uses. Two sources exist throughout:
//! `open`, which the whole family holds, and `closed`, which only the
//! parent does. `closed` is what must never travel downward.

#[path = "support/nested_harness.rs"]
mod harness;
#[path = "support/route_fixture.rs"]
mod route_fixture;
#[path = "../src/wirkd/mod.rs"]
mod wirkd;

use std::fs;
use std::path::Path;
use std::process::Command;

use harness::*;

use wirk_core::{EventKind, WaypointId};
use wirkd::WirkdPointer;

fn wirk_bin() -> &'static str {
    env!("CARGO_BIN_EXE_wirk")
}

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

/// Commit, acquire and publish `repo` under `source`, and return the
/// exact coordinate of the unit carrying `marker` (`findings.rs`'s own
/// `publish_and_locate_as`, reused verbatim).
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

/// The `work/<id>/event/<id>` token naming the `FindingRaised` event of
/// `finding_id` in `work_id`'s own journal.
fn raise_event_ref(estate: &Path, work_id: &str, finding_id: &str) -> String {
    let event = journal_events(estate, work_id)
        .into_iter()
        .find_map(|event| match &event.kind {
            EventKind::FindingRaised { finding } if finding.id.0 == finding_id => Some(event.id.0),
            _ => None,
        })
        .expect("the named finding's own FindingRaised event");
    format!("work/{work_id}/event/{event}")
}

/// The `work/<id>/event/<id>` token naming the first event of `kind` in
/// `work_id`'s own journal, by the event's serde tag.
fn event_ref_of_kind(estate: &Path, work_id: &str, kind: &str) -> String {
    let event = journal_events(estate, work_id)
        .into_iter()
        .find(|event| serde_json::to_value(&event.kind).unwrap()["kind"].as_str() == Some(kind))
        .unwrap_or_else(|| panic!("no {kind} event in {work_id}"))
        .id
        .0;
    format!("work/{work_id}/event/{event}")
}

/// Every string anywhere in `value`, flattened — so a leak assertion
/// inspects the *whole* answer, not the one field a fix happened to
/// remember.
fn all_strings(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(text) => out.push(text.clone()),
        serde_json::Value::Array(items) => items.iter().for_each(|item| all_strings(item, out)),
        serde_json::Value::Object(map) => {
            for (key, item) in map {
                out.push(key.clone());
                all_strings(item, out);
            }
        }
        _ => {}
    }
}

/// Asserts no needle appears anywhere in `value` or in `stderr` — the
/// non-disclosure half of every negative below. A denial that names the
/// alias, the path, the generation, the object id or the encoded token
/// it denied has disclosed exactly what it refused.
fn assert_discloses_nothing(what: &str, value: &serde_json::Value, stderr: &str, needles: &[&str]) {
    let mut strings = Vec::new();
    all_strings(value, &mut strings);
    strings.push(stderr.to_string());
    for needle in needles {
        assert!(
            !strings.iter().any(|text| text.contains(needle)),
            "{what} disclosed {needle:?}: {value} {stderr}"
        );
    }
}

/// The `obligation_basis` the estate's settlement policy must admit for
/// `waypoint` in `work_id` — re-derived from the Work's own journal
/// exactly as the daemon derives it (`findings.rs`'s own helper of the
/// same name, reused verbatim).
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

/// One parent bound to both sources, one child narrowed to `open`, one
/// grandchild narrowed the same, and one unrelated Work — the family
/// every case below is built on.
struct Family {
    #[allow(dead_code)]
    dir: tempfile::TempDir,
    estate: std::path::PathBuf,
    wirkd: KillOnDrop,
    pointer: WirkdPointer,
    parent: Submitted,
    child: Submitted,
    unrelated: Submitted,
    /// The exact coordinate of the `closed` source unit the parent may
    /// cite and nobody below it may learn.
    closed_coordinate: String,
    /// The same for `open`, which the whole family holds.
    open_coordinate: String,
    /// Values that must never reach a narrowed requester.
    closed_secrets: Vec<String>,
}

fn build_family(narrowed_grants: &[&str]) -> Family {
    let dir = tempfile::tempdir().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    route_fixture::install_route_fixture(&estate, "wa_simple_leaf");
    // A container declaring the `helper` child role — so a real child
    // and grandchild can attach — whose own Deterministic leaf declares
    // a verification obligation, so a settlement with a real
    // `DeterministicProof` (artifact path and digest read out of the
    // producing Work's checkout) exists to scope.
    route_fixture::write_route(
        &estate,
        "disclosure_container",
        r#"{"id":"disclosure-container","waypoints":[
            {"id":"outer","kind":"Container",
             "declared_outputs":[{"name":"a.md","required":true}],
             "required_child_outcomes":[{"role":"helper","required":true}],
             "leaves":[
               {"id":"outer/leaf-a","kind":"Deterministic",
                "command":["sh","-c","echo a > a.md"],
                "declared_outputs":[{"name":"a.md","required":true}],
                "verifies":{"id":"a-produced","edition":"1",
                            "proves":"outer/leaf-a ran its declared command and produced a.md",
                            "outputs":["a.md"]}}
             ]}
        ]}"#,
    );
    let (wirkd, pointer) = start_wirkd(&estate);

    let closed_repo = dir.path().join("embargo-repo");
    init_repo(&closed_repo);
    write_file(
        &closed_repo,
        "embargoed.md",
        "embargomarker: the embargoed finding basis\n",
    );
    let closed_coordinate =
        publish_and_locate_as(&estate, &closed_repo, "embargo", "embargomarker");

    let open_repo = dir.path().join("open-repo");
    init_repo(&open_repo);
    write_file(&open_repo, "shared.md", "openmarker: the shared basis\n");
    let open_coordinate = publish_and_locate_as(&estate, &open_repo, "open", "openmarker");

    let parent_repo = dir.path().join("parent-repo");
    init_repo(&parent_repo);
    let parent = submit(
        &estate,
        "disclosure_container",
        &parent_repo,
        &[
            "embargo:write",
            "open:read",
            "helper:write",
            "scratch:write",
        ],
        None,
    )
    .unwrap();

    let child_repo = dir.path().join("child-repo");
    init_repo(&child_repo);
    let child = submit(
        &estate,
        "disclosure_container",
        &child_repo,
        narrowed_grants,
        Some(ParentRef {
            work: &parent.work_id,
            waypoint: "outer",
            run: &parent.run_id,
            role: "helper",
            attempt: None,
        }),
    )
    .unwrap();

    let unrelated_repo = dir.path().join("unrelated-repo");
    init_repo(&unrelated_repo);
    let unrelated = submit(
        &estate,
        "wa_simple_leaf",
        &unrelated_repo,
        &["open:read", "helper:write"],
        None,
    )
    .unwrap();

    let closed_secrets = vec![
        "embargo".to_string(),
        "embargoed.md".to_string(),
        "embargomarker".to_string(),
        closed_coordinate.clone(),
    ];

    Family {
        dir,
        estate,
        wirkd,
        pointer,
        parent,
        child,
        unrelated,
        closed_coordinate,
        open_coordinate,
        closed_secrets,
    }
}

// ---- 1. Consultation: a broad World event is not disclosure-free ---------

/// The frozen base checks embedded `Source` coordinates on a
/// `FindingRaised` event and returns success for **every other event
/// kind**. A `WorkSubmitted` names the producing Work's whole binding
/// set; a `WaypointReserved` carries the compiled World — repository,
/// checkout path, branch, base SHA, argv and frozen review targets. A
/// child narrowed to `open` must not acquire the parent's `closed`
/// binding by citing either one.
#[test]
fn a_narrowed_child_cannot_cite_its_parents_broad_world_events() {
    let family = build_family(&["scratch:write", "open:read", "helper:write"]);
    let estate = &family.estate;

    for kind in ["WorkSubmitted", "WaypointReserved"] {
        let reference = event_ref_of_kind(estate, &family.parent.work_id, kind);
        let (code, reply, stderr) = raise_cli(
            estate,
            &family.child.work_id,
            &family.child.run_id,
            &[
                "--kind",
                "gap",
                "--scope",
                "estate_local",
                "--claim",
                "reading the parent's own compiled world through kinship",
                "--evidence",
                &reference,
            ],
        );
        assert_eq!(
            code,
            Some(3),
            "a narrowed child must not admit its parent's own {kind}, which names sources it does not hold: {reply} {stderr}"
        );
        assert_discloses_nothing(
            &format!("the refusal of {kind}"),
            &reply,
            &stderr,
            &family.closed_secrets_slice(),
        );
    }

    stop_wirkd(estate, family.wirkd);
}

// ---- 2. Consultation: the wrapper is followed, not stopped at -----------

/// One hop defeats the frozen base entirely: `embedded_sources_admitted`
/// filters the referenced event's own `Journal` entries out
/// (`EvidenceRef::Journal { .. } => None`) and checks only its direct
/// `Source` ones. So a parent raises F1 on `closed` (legitimately),
/// wraps it in F2 whose only evidence is `work/<parent>/event/<F1>`, and
/// the narrowed child cites F2 — laundering `closed` through a single
/// indirection that the direct citation already refuses.
#[test]
fn a_journal_wrapper_does_not_launder_a_denied_source() {
    let family = build_family(&["scratch:write", "open:read", "helper:write"]);
    let estate = &family.estate;

    let (code, f1, stderr) = raise_cli(
        estate,
        &family.parent.work_id,
        &family.parent.run_id,
        &[
            "--kind",
            "gap",
            "--scope",
            "estate_local",
            "--claim",
            "a real closed-source gap the parent may legitimately raise",
            "--evidence",
            &family.closed_coordinate,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let f1_id = f1["id"].as_str().unwrap().to_string();
    let f1_ref = raise_event_ref(estate, &family.parent.work_id, &f1_id);

    // The wrapper: legitimate for the parent, which holds `closed`.
    let (code, f2, stderr) = raise_cli(
        estate,
        &family.parent.work_id,
        &family.parent.run_id,
        &[
            "--kind",
            "relationship",
            "--scope",
            "estate_local",
            "--claim",
            "a wrapper whose only evidence is the parent's own earlier finding",
            "--evidence",
            &f1_ref,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let f2_id = f2["id"].as_str().unwrap().to_string();
    let f2_ref = raise_event_ref(estate, &family.parent.work_id, &f2_id);

    // Direct citation of F1 is already refused on the frozen base. The
    // decisive case is F2, one hop further out.
    let (code, reply, stderr) = raise_cli(
        estate,
        &family.child.work_id,
        &family.child.run_id,
        &[
            "--kind",
            "gap",
            "--scope",
            "estate_local",
            "--claim",
            "laundering closed evidence through a one-hop journal wrapper",
            "--evidence",
            &f2_ref,
        ],
    );
    assert_eq!(
        code,
        Some(3),
        "a wrapper event must be followed to the closed source it transitively names: {reply} {stderr}"
    );
    assert_discloses_nothing(
        "the wrapper refusal",
        &reply,
        &stderr,
        &family.closed_secrets_slice(),
    );

    stop_wirkd(estate, family.wirkd);
}

// ---- 3. Consultation: the useful positives must survive -----------------

/// The repair must not become "no cross-Work evidence". A narrowed child
/// citing a parent finding whose evidence is entirely `open` — a source
/// the child independently holds — is exactly the settled EstateLocal
/// learning the estate exists to reuse, and it must be admitted through
/// the intended public surface. So must a wrapper over it, and so must a
/// journal reference to an event that names no source at all.
#[test]
fn admitted_cross_work_learning_stays_usable_through_the_public_surface() {
    let family = build_family(&["scratch:write", "open:read", "helper:write"]);
    let estate = &family.estate;

    let (code, open_finding, stderr) = raise_cli(
        estate,
        &family.parent.work_id,
        &family.parent.run_id,
        &[
            "--kind",
            "gap",
            "--scope",
            "estate_local",
            "--claim",
            "an open-source gap later works should be able to build on",
            "--evidence",
            &family.open_coordinate,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let open_id = open_finding["id"].as_str().unwrap().to_string();
    let open_ref = raise_event_ref(estate, &family.parent.work_id, &open_id);

    let (code, wrapper, stderr) = raise_cli(
        estate,
        &family.parent.work_id,
        &family.parent.run_id,
        &[
            "--kind",
            "relationship",
            "--scope",
            "estate_local",
            "--claim",
            "a wrapper over the open finding",
            "--evidence",
            &open_ref,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let wrapper_id = wrapper["id"].as_str().unwrap().to_string();
    let wrapper_ref = raise_event_ref(estate, &family.parent.work_id, &wrapper_id);

    // Direct: the child holds `open`, so the parent's open-only finding
    // is admissible evidence for it.
    let (code, reply, stderr) = raise_cli(
        estate,
        &family.child.work_id,
        &family.child.run_id,
        &[
            "--kind",
            "gap",
            "--scope",
            "estate_local",
            "--claim",
            "building on the parent's admitted open finding",
            "--evidence",
            &open_ref,
        ],
    );
    assert_eq!(
        code,
        Some(0),
        "admitted cross-Work learning must stay reusable: {reply} {stderr}"
    );
    assert_eq!(reply["evidence"][0]["outcome"], "admitted", "{reply}");

    // Transitive: following the wrapper must *admit* as readily as it
    // denies — the recursion is a check, not a ban.
    let (code, reply, stderr) = raise_cli(
        estate,
        &family.child.work_id,
        &family.child.run_id,
        &[
            "--kind",
            "gap",
            "--scope",
            "estate_local",
            "--claim",
            "building on the parent's admitted open wrapper",
            "--evidence",
            &wrapper_ref,
        ],
    );
    assert_eq!(
        code,
        Some(0),
        "a wrapper over an admitted source must itself be admitted: {reply} {stderr}"
    );

    stop_wirkd(estate, family.wirkd);
}

// ---- 4. Presentation: `finding list` must scope sources -----------------

/// `handle_finding_list` scopes its *target Works* to the requester's
/// lineage and then renders `finding_json` in full — every evidence
/// coordinate, every proof artifact path, every Application source and
/// generation. Lineage is permission to reference the parent's journal;
/// it is not permission to read `closed`.
#[test]
fn finding_list_withholds_sources_the_requester_does_not_hold() {
    let family = build_family(&["scratch:write", "open:read", "helper:write"]);
    let estate = &family.estate;

    let (code, closed_finding, stderr) = raise_cli(
        estate,
        &family.parent.work_id,
        &family.parent.run_id,
        &[
            "--kind",
            "gap",
            "--scope",
            "estate_local",
            "--claim",
            "a closed-source gap",
            "--evidence",
            &family.closed_coordinate,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let closed_id = closed_finding["id"].as_str().unwrap().to_string();

    let (code, open_finding, stderr) = raise_cli(
        estate,
        &family.parent.work_id,
        &family.parent.run_id,
        &[
            "--kind",
            "gap",
            "--scope",
            "estate_local",
            "--claim",
            "an open-source gap",
            "--evidence",
            &family.open_coordinate,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let open_id = open_finding["id"].as_str().unwrap().to_string();

    let (code, listed, stderr) = finding_cli(
        estate,
        &[
            "list",
            "--requesting-work",
            &family.child.work_id,
            "--work",
            &family.parent.work_id,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");

    assert_discloses_nothing(
        "the child's scoped finding list",
        &listed,
        &stderr,
        &family.closed_secrets_slice(),
    );

    // Useful positive in the same answer: the open finding's own
    // coordinate is still disclosed in full, because the child holds it.
    let findings = listed["findings"].as_array().unwrap();
    let open_row = findings
        .iter()
        .find(|f| f["id"] == open_id.as_str())
        .unwrap_or_else(|| panic!("the open finding must still be listed: {listed}"));
    assert_eq!(
        open_row["evidence"][0]["outcome"], "admitted",
        "an admitted source must stay fully disclosed: {open_row}"
    );

    // The closed finding's journal identity may still be present — that
    // is a journal reference, not a source — but nothing about `closed`
    // may be. The `assert_discloses_nothing` above already pins that;
    // this pins that the withholding is *stated*, never silent.
    let closed_row = findings.iter().find(|f| f["id"] == closed_id.as_str());
    if let Some(closed_row) = closed_row {
        assert_eq!(
            closed_row["evidence"][0]["withheld"],
            serde_json::json!(true),
            "a withheld evidence entry must say so: {closed_row}"
        );
    }
    assert!(
        listed["disclosure"]["withheld"].as_u64().unwrap_or(0) >= 1,
        "the answer must carry an honest withheld count: {listed}"
    );

    // Explicit administration is preserved, unchanged and still total.
    let (code, admin, stderr) = finding_cli(
        estate,
        &["list", "--admin", "--work", &family.parent.work_id],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let mut admin_strings = Vec::new();
    all_strings(&admin, &mut admin_strings);
    assert!(
        admin_strings
            .iter()
            .any(|s| s.contains("embargomarker") || s.contains(&family.closed_coordinate)),
        "explicit administrative inspection must remain total: {admin}"
    );

    stop_wirkd(estate, family.wirkd);
}

// ---- 5. Presentation: the estate index is a disclosure surface ----------

/// `wirk atlas findings` takes no requester at all on the frozen base:
/// any caller reaching the estate root receives every settled row,
/// every proof target and every Application source in the estate. It
/// must offer a requester-scoped answer, and its unscoped form must be
/// named administrative explicitly rather than being the default.
#[test]
fn the_estate_findings_index_is_requester_scoped_with_explicit_administration() {
    let family = build_family(&["scratch:write", "open:read", "helper:write"]);
    let estate = &family.estate;

    let (code, closed_finding, stderr) = raise_cli(
        estate,
        &family.parent.work_id,
        &family.parent.run_id,
        &[
            "--kind",
            "gap",
            "--scope",
            "estate_local",
            "--claim",
            "a closed-source gap",
            "--evidence",
            &family.closed_coordinate,
            "--applies-to",
            &family.closed_coordinate,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let closed_id = closed_finding["id"].as_str().unwrap().to_string();

    // An Application against `closed`: source alias, both generation
    // points and the revision all land in the index row.
    let closed_repo = family.dir.path().join("embargo-repo");
    write_file(
        &closed_repo,
        "embargoed.md",
        "embargomarker: the embargoed finding basis, revised\n",
    );
    let _ = publish_and_locate_as(estate, &closed_repo, "embargo", "embargomarker");
    let (ok, status, err) = atlas(estate, &["status", "--source", "embargo"]);
    assert!(ok, "{err}");
    let revision = status["sources"][0]["published_generation"]["revision"]
        .as_str()
        .unwrap_or_else(|| panic!("a published embargo revision: {status}"))
        .to_string();
    let (code, _applied, stderr) = applied_cli(
        estate,
        &family.parent.work_id,
        &family.parent.run_id,
        &[
            "--finding",
            &closed_id,
            "--source",
            "embargo",
            "--revision",
            &revision,
            "--by",
            "the parent",
        ],
    );
    assert_eq!(code, Some(0), "{_applied} {stderr}");

    // A second, `open`-based estate record with an Assertion against it
    // — the useful positive that must survive inside the same scoped
    // answer the embargo row is withheld from.
    let (code, open_finding, stderr) = raise_cli(
        estate,
        &family.parent.work_id,
        &family.parent.run_id,
        &[
            "--kind",
            "gap",
            "--scope",
            "estate_local",
            "--claim",
            "an open-source gap worth indexing",
            "--evidence",
            &family.open_coordinate,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let open_id = open_finding["id"].as_str().unwrap().to_string();
    let (code, _asserted, stderr) = finding_cli(
        estate,
        &[
            "assert",
            "--finding",
            &open_id,
            "--decision",
            "accepted",
            "--by",
            "the operator",
            "--admin",
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");

    // The index is populated by the established explicit administrative
    // rebuild — `finding applied`/`assert` do not reconcile inline on
    // this base, which is an Application-contract gap this disclosure
    // repair deliberately does not reach into.
    let (ok, _rebuilt, err) = atlas(estate, &["findings", "--rebuild", "--admin"]);
    assert!(ok, "{err}");

    // Unscoped, unnamed: must no longer be the default answer.
    let (ok, unscoped, err) = atlas(estate, &["findings"]);
    assert!(
        !ok,
        "an unscoped estate index must require an explicit requester or an explicit --admin: {unscoped} {err}"
    );

    // Requester-scoped: the child sees its lineage's rows without
    // learning anything about `embargo`.
    let (ok, scoped, err) = atlas(
        estate,
        &["findings", "--requesting-work", &family.child.work_id],
    );
    assert!(ok, "{err}");
    assert_discloses_nothing(
        "the child's scoped index",
        &scoped,
        &err,
        &family.closed_secrets_slice(),
    );
    let mut scoped_strings = Vec::new();
    all_strings(&scoped, &mut scoped_strings);
    assert!(
        scoped_strings.iter().any(|s| s.contains(&open_id)),
        "the admitted open row must still reach the child: {scoped}"
    );

    // An unrelated Work's own scoped index sees no lineage rows at all.
    let (ok, foreign, err) = atlas(
        estate,
        &["findings", "--requesting-work", &family.unrelated.work_id],
    );
    assert!(ok, "{err}");
    assert_discloses_nothing(
        "an unrelated work's index",
        &foreign,
        &err,
        &family.closed_secrets_slice(),
    );
    let mut foreign_strings = Vec::new();
    all_strings(&foreign, &mut foreign_strings);
    assert!(
        !foreign_strings.iter().any(|s| s.contains(&open_id)),
        "an unrelated work reaches none of the parent's lineage rows: {foreign}"
    );

    // Explicit administration remains total.
    let (ok, admin, err) = atlas(estate, &["findings", "--admin"]);
    assert!(ok, "{err}");
    let mut admin_strings = Vec::new();
    all_strings(&admin, &mut admin_strings);
    assert!(
        admin_strings.iter().any(|s| s.contains("embargo")),
        "explicit administrative index inspection must remain total: {admin}"
    );

    stop_wirkd(estate, family.wirkd);
}

// ---- 6. A grandchild is narrowed by its own bindings, not its depth -----

/// Depth is not scope. A grandchild that holds `open` reaches its
/// grandparent's open evidence (lineage walks upward through every
/// ancestor); one that holds neither reaches nothing, including its own
/// parent's — the check is always the *requesting* Work's own bindings.
#[test]
fn a_grandchild_is_scoped_by_its_own_bindings_at_every_depth() {
    let family = build_family(&["scratch:write", "open:read", "helper:write"]);
    let estate = &family.estate;

    let (code, open_finding, stderr) = raise_cli(
        estate,
        &family.parent.work_id,
        &family.parent.run_id,
        &[
            "--kind",
            "gap",
            "--scope",
            "estate_local",
            "--claim",
            "an open-source gap at the top of the family",
            "--evidence",
            &family.open_coordinate,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let open_ref = raise_event_ref(
        estate,
        &family.parent.work_id,
        open_finding["id"].as_str().unwrap(),
    );

    let (code, closed_finding, stderr) = raise_cli(
        estate,
        &family.parent.work_id,
        &family.parent.run_id,
        &[
            "--kind",
            "gap",
            "--scope",
            "estate_local",
            "--claim",
            "a closed-source gap at the top of the family",
            "--evidence",
            &family.closed_coordinate,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let closed_ref = raise_event_ref(
        estate,
        &family.parent.work_id,
        closed_finding["id"].as_str().unwrap(),
    );

    // A grandchild under the child, holding `open` only.
    let grandchild_repo = family.dir.path().join("grandchild-repo");
    init_repo(&grandchild_repo);
    let grandchild = submit(
        estate,
        "wa_simple_leaf",
        &grandchild_repo,
        &["open:read", "helper:write"],
        Some(ParentRef {
            work: &family.child.work_id,
            waypoint: "outer",
            run: &family.child.run_id,
            role: "helper",
            attempt: None,
        }),
    );

    if let Ok(grandchild) = grandchild {
        let (code, reply, stderr) = raise_cli(
            estate,
            &grandchild.work_id,
            &grandchild.run_id,
            &[
                "--kind",
                "gap",
                "--scope",
                "estate_local",
                "--claim",
                "a grandchild reaching two levels up for open evidence",
                "--evidence",
                &open_ref,
            ],
        );
        assert_eq!(
            code,
            Some(0),
            "a grandchild holding the source reaches its grandparent's admitted evidence: {reply} {stderr}"
        );

        let (code, reply, stderr) = raise_cli(
            estate,
            &grandchild.work_id,
            &grandchild.run_id,
            &[
                "--kind",
                "gap",
                "--scope",
                "estate_local",
                "--claim",
                "a grandchild reaching two levels up for closed evidence",
                "--evidence",
                &closed_ref,
            ],
        );
        assert_eq!(
            code,
            Some(3),
            "a grandchild is scoped by its own bindings, however deep the kinship: {reply} {stderr}"
        );
        assert_discloses_nothing(
            "the grandchild refusal",
            &reply,
            &stderr,
            &family.closed_secrets_slice(),
        );
    }

    stop_wirkd(estate, family.wirkd);
}

// ---- 7. Unrelated and sibling controls stay refused ---------------------

/// The established lineage refusal is not weakened by any of the above:
/// an unrelated Work citing the parent's *open* evidence — a source it
/// genuinely holds — is still refused, because the reference route
/// itself was never granted. Source admission is a second gate, never a
/// replacement for the first.
#[test]
fn source_admission_never_replaces_the_lineage_gate() {
    let family = build_family(&["scratch:write", "open:read", "helper:write"]);
    let estate = &family.estate;

    let (code, open_finding, stderr) = raise_cli(
        estate,
        &family.parent.work_id,
        &family.parent.run_id,
        &[
            "--kind",
            "gap",
            "--scope",
            "estate_local",
            "--claim",
            "an open-source gap",
            "--evidence",
            &family.open_coordinate,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let open_ref = raise_event_ref(
        estate,
        &family.parent.work_id,
        open_finding["id"].as_str().unwrap(),
    );

    let (code, reply, stderr) = raise_cli(
        estate,
        &family.unrelated.work_id,
        &family.unrelated.run_id,
        &[
            "--kind",
            "gap",
            "--scope",
            "estate_local",
            "--claim",
            "an unrelated work reaching for a source it does hold, by a route it does not",
            "--evidence",
            &open_ref,
        ],
    );
    assert_eq!(
        code,
        Some(3),
        "holding the source never grants the journal route: {reply} {stderr}"
    );

    stop_wirkd(estate, family.wirkd);
}

// ---- 8. Proof metadata: the receipt survives, its sources do not --------

/// The sharpest form of the whole contract. A parent settles a real
/// `DeterministicVerified` finding: the proof binds the admitted
/// obligation's content basis, the reserved World hash and the exact
/// artifact receipts — name, resolved path and digest — read out of the
/// parent's own checkout, which the narrowed child has no binding for.
///
/// The child must still be told the **outcome**: that this finding is
/// settled, under which policy class, version and digest, by which
/// Claim, Claim event and settlement event. That is the admitted
/// cross-Work receipt a later Work legitimately builds on. What it must
/// not be told is the proof's own source half.
#[test]
fn a_settled_proofs_receipt_reaches_a_narrowed_child_without_its_sources() {
    let family = build_family(&["scratch:write", "open:read", "helper:write"]);
    let estate = &family.estate;

    // P3 execution-recovery item 1: the parent's own Deterministic
    // leaf now runs in this Work's own worktree, not the caller's
    // shared --repo-path checkout.
    write_file(
        &family.estate.join("worktrees").join(&family.parent.work_id),
        "a.md",
        "a\n",
    );
    claim_ok(
        estate,
        &family.parent.work_id,
        &family.parent.run_id,
        "a.md=a.md",
    );
    let basis = obligation_basis_for(estate, &family.parent.work_id, "outer/leaf-a");
    write_policy_admitting(estate, "a-produced", "1", &basis);

    let claim_event = journal_events(estate, &family.parent.work_id)
        .into_iter()
        .find_map(|event| {
            matches!(&event.kind, EventKind::ClaimRecorded { .. }).then_some(event.id.0)
        })
        .expect("the parent's own ClaimRecorded event");
    let evidence = format!("work/{}/event/{claim_event}", family.parent.work_id);
    let (code, raised, stderr) = raise_cli(
        estate,
        &family.parent.work_id,
        &family.parent.run_id,
        &[
            "--kind",
            "verified_outcome",
            "--scope",
            "estate_local",
            "--claim",
            "outer/leaf-a produced a.md",
            "--evidence",
            &evidence,
            "--obligation",
            "a-produced@1",
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let settled_id = raised["id"].as_str().unwrap().to_string();
    let (code, settled, stderr) =
        finding_cli(estate, &["settle", "--finding", &settled_id, "--admin"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        settled["settled"].is_object(),
        "the parent's own finding must really settle before this scopes anything: {settled}"
    );

    // The parent, holding everything, sees the whole proof.
    let (code, own, stderr) = finding_cli(
        estate,
        &[
            "list",
            "--requesting-work",
            &family.parent.work_id,
            "--work",
            &family.parent.work_id,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let own_row = own["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["id"] == settled_id.as_str())
        .unwrap()
        .clone();
    assert_eq!(
        own_row["settled"]["proves"]["recorded"],
        serde_json::json!(true)
    );
    assert!(
        !own_row["settled"]["check"]["artifacts"]
            .as_array()
            .expect("the settling work sees its own artifact receipts")
            .is_empty(),
        "{own_row}"
    );

    // The child, narrowed, sees the receipt and not the sources.
    let (code, listed, stderr) = finding_cli(
        estate,
        &[
            "list",
            "--requesting-work",
            &family.child.work_id,
            "--work",
            &family.parent.work_id,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let row = listed["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["id"] == settled_id.as_str())
        .unwrap_or_else(|| panic!("the settled outcome must still reach the child: {listed}"))
        .clone();

    // The useful positive: the outcome receipt, in full.
    assert_eq!(
        row["settled"]["authority"]["policy"]["class"], "deterministic_verified",
        "{row}"
    );
    assert!(row["settled"]["settled_by_event"].is_string(), "{row}");
    assert_eq!(row["settled"]["check"]["check"], "validated_claim", "{row}");
    assert!(
        row["settled"]["check"]["claim_event"].is_string(),
        "the journal identity that settled it is a reference, not a source: {row}"
    );

    // The withheld half: the proof, whole, and the statement derived
    // from it.
    assert_eq!(
        row["settled"]["check"]["proof"],
        serde_json::json!({"withheld": true}),
        "{row}"
    );
    assert_eq!(
        row["settled"]["proves"],
        serde_json::json!({"withheld": true}),
        "{row}"
    );
    assert!(row["settled"]["check"]["artifacts"].is_null(), "{row}");
    assert!(
        listed["disclosure"]["withheld"].as_u64().unwrap_or(0) >= 1,
        "{listed}"
    );

    // Nothing anywhere in the answer carries the obligation basis the
    // artifact digests went into, nor the receipts themselves.
    assert_discloses_nothing(
        "the child's view of a settled proof",
        &listed,
        &stderr,
        &[&basis],
    );

    // The limit this test recorded under `0110` — "`a.md` still appears
    // inside the proposer's own free-text claim, and no part of this
    // repair inspects prose to stop that" — is the gap
    // `CHILD-PRODUCER-DISCLOSURE-ADJUDICATION.md` closed. Nothing here
    // inspects prose still: the sentence is withheld whole because its
    // *author* holds bindings this requester does not, which is the same
    // conservative rule the off-lineage publication route already
    // applied to a whole row. The structured record — the coordinate,
    // the generation, the object id, the resolved path, the digest and
    // the basis — is still what the repair guarantees never travels.
    assert_eq!(
        row["claim_text"],
        serde_json::json!({"withheld": true}),
        "a broader author's own prose is bounded by that author's admission: {row}"
    );
    assert_eq!(row["claim"], serde_json::json!({"withheld": true}), "{row}");

    let _ = &family.pointer;
    stop_wirkd(estate, family.wirkd);
}

/// `CHILD-PRODUCER-DISCLOSURE-ADJUDICATION.md`, on the executed
/// counterexample in `loop-b-child-disclosure-control/raw/scen/41-H.json`:
/// a child narrowed to `ledger+public` was handed its parent's authored
/// claim sentence naming a `vaultx`-only sentinel, while an unrelated
/// Work holding the *identical* bindings was refused the same row whole.
/// Kinship permits consultation of the family's journal; it never widens
/// what an authored sentence may quote.
///
/// Here the parent's prose — the claim sentence, the proposed change and
/// an asserted rejection's reason — quotes the embargoed marker while
/// every *structured* part of the record stays inside the child's own
/// grants. The narrowed child must be shown the record and not the
/// prose; the producer and `--admin` must be shown all of it.
#[test]
fn a_narrowed_lineage_requester_is_not_shown_the_producers_authored_prose() {
    let family = build_family(&["scratch:write", "open:read", "helper:write"]);
    let estate = &family.estate;

    let (code, raised, stderr) = raise_cli(
        estate,
        &family.parent.work_id,
        &family.parent.run_id,
        &[
            "--kind",
            "gap",
            "--scope",
            "estate_local",
            // Prose the parent wrote itself, quoting a source only the
            // parent holds. The *evidence* is the open coordinate the
            // child holds too, so nothing but the prose is in question.
            "--claim",
            "the shared basis is stale; cross-checked against embargomarker, which only the producer reads",
            "--proposed-change",
            "rotate embargomarker in embargoed.md",
            "--evidence",
            &family.open_coordinate,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let finding_id = raised["id"].as_str().unwrap().to_string();

    let (code, _asserted, stderr) = finding_cli(
        estate,
        &[
            "assert",
            "--finding",
            &finding_id,
            "--decision",
            "rejected",
            "--by",
            "reviewer",
            "--reason",
            "rejected: the embargomarker roster says otherwise",
            "--admin",
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");

    // ---- the narrowed child, on the lineage, through `finding list` ----
    let (code, listed, stderr) = finding_cli(
        estate,
        &[
            "list",
            "--requesting-work",
            &family.child.work_id,
            "--work",
            &family.parent.work_id,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert_discloses_nothing(
        "the narrowed child's view of its parent's authored prose",
        &listed,
        &stderr,
        &family.closed_secrets_slice(),
    );

    let row = listed["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["id"] == finding_id.as_str())
        .unwrap_or_else(|| panic!("the record itself must still reach the child: {listed}"))
        .clone();
    let marker = serde_json::json!({"withheld": true});
    assert_eq!(row["claim"], marker, "the claim sentence is prose: {row}");
    assert_eq!(row["claim_text"], marker, "so is its raw form: {row}");
    assert_eq!(
        row["proposed_change"], marker,
        "so is the proposed change: {row}"
    );
    assert_eq!(
        row["assertions"][0]["decision"]["reason"], marker,
        "so is an assertion's rejection reason: {row}"
    );
    assert!(
        listed["disclosure"]["withheld"].as_u64().unwrap_or(0) >= 1,
        "the withholding must be counted, never silent: {listed}"
    );

    // The narrowed structural read the child legitimately keeps: the
    // record's own identity, kind and scope, and the evidence entry it
    // independently holds, in full.
    assert_eq!(row["kind"], "gap", "{row}");
    assert_eq!(row["scope"], "estate_local", "{row}");
    assert!(row["work"].is_string(), "{row}");
    assert_eq!(
        row["evidence"][0]["outcome"], "admitted",
        "an admitted source stays fully disclosed: {row}"
    );
    assert_eq!(
        row["assertions"][0]["decision"]["decision"], "rejected",
        "the recorded decision itself is not prose: {row}"
    );

    // ---- the same boundary on the estate index ----
    // The index reconciles on settle and on Application; an assertion
    // reaches it at the next startup or through the administrative
    // sweep, so the sweep is run explicitly rather than waited for.
    let (ok, _rebuilt, stderr) = atlas(estate, &["findings", "--admin", "--rebuild"]);
    assert!(ok, "{stderr}");
    let (ok, index, stderr) = atlas(
        estate,
        &["findings", "--requesting-work", &family.child.work_id],
    );
    assert!(ok, "{stderr}");
    assert_discloses_nothing(
        "the narrowed child's view of the estate index",
        &index,
        &stderr,
        &family.closed_secrets_slice(),
    );
    let indexed = index["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["finding"]["id"] == finding_id.as_str())
        .unwrap_or_else(|| panic!("the asserted row must still be indexed: {index}"))
        .clone();
    assert_eq!(indexed["finding"]["claim"], marker, "{indexed}");
    assert_eq!(indexed["finding"]["claim_text"], marker, "{indexed}");
    assert_eq!(
        indexed["assertion"]["decision"]["reason"], marker,
        "{indexed}"
    );

    // ---- the producer's own read is unchanged ----
    let (code, own, stderr) = finding_cli(
        estate,
        &[
            "list",
            "--requesting-work",
            &family.parent.work_id,
            "--work",
            &family.parent.work_id,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let own_row = own["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["id"] == finding_id.as_str())
        .unwrap()
        .clone();
    assert!(
        own_row["claim_text"]
            .as_str()
            .unwrap()
            .contains("embargomarker"),
        "the author reads its own prose in full: {own_row}"
    );
    assert!(
        own_row["proposed_change"]
            .as_str()
            .unwrap()
            .contains("embargomarker"),
        "{own_row}"
    );

    // ---- explicit administration stays total ----
    let (code, admin, stderr) = finding_cli(
        estate,
        &["list", "--admin", "--work", &family.parent.work_id],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let mut admin_strings = Vec::new();
    all_strings(&admin, &mut admin_strings);
    assert!(
        admin_strings.iter().any(|s| s.contains("embargomarker")),
        "explicit administrative inspection must remain total: {admin}"
    );

    let _ = &family.pointer;
    stop_wirkd(estate, family.wirkd);
}

/// `ASSERTION-AUTHOR-ADJUDICATION.md`, on the executed counterexample in
/// `loop-b-lineage-prose-verify/raw/32-matrix-green.txt` section C. The
/// authored-prose gate keyed on the Work whose journal *holds* a record,
/// which is the author of a claim sentence and a proposed change but
/// never necessarily the author of an assertion: `finding assert` admits
/// any requester on the target Finding's lineage, so a broad parent
/// writes its own sentence into a narrow child's journal, and the child
/// — which of course admits its own checkout whole — was handed that
/// sentence in clear on every scoped surface.
///
/// Here the parent asserts a rejection on the *child's* own finding,
/// quoting the embargoed marker only the parent holds. The child must
/// keep the record, the recorded decision, the recorded name and the
/// fact of the rejection, and lose only the sentence; the parent that
/// wrote it, and `--admin`, must read it whole.
#[test]
fn a_narrowed_holder_is_not_shown_the_reason_a_broader_work_asserted_on_its_record() {
    let family = build_family(&["scratch:write", "open:read", "helper:write"]);
    let estate = &family.estate;

    // The child raises on its own journal, in its own words, quoting
    // only what it holds — so nothing but the parent's later sentence is
    // ever in question here.
    let (code, raised, stderr) = raise_cli(
        estate,
        &family.child.work_id,
        &family.child.run_id,
        &[
            "--kind",
            "gap",
            "--scope",
            "estate_local",
            "--claim",
            "the child's own statement, quoting only openmarker, which the child holds",
            "--evidence",
            &family.open_coordinate,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let finding_id = raised["id"].as_str().unwrap().to_string();

    // The broader parent, on the lineage, writes its own reason into the
    // child's journal. This is a supported assertion and stays one.
    let (code, _asserted, stderr) = finding_cli(
        estate,
        &[
            "assert",
            "--finding",
            &finding_id,
            "--decision",
            "rejected",
            "--by",
            "parent reviewer",
            "--reason",
            "rejected: the embargomarker roster says otherwise",
            "--requesting-work",
            &family.parent.work_id,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");

    let marker = serde_json::json!({"withheld": true});

    // ---- the holder reading its OWN journal ----
    let (code, listed, stderr) = finding_cli(
        estate,
        &[
            "list",
            "--requesting-work",
            &family.child.work_id,
            "--work",
            &family.child.work_id,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert_discloses_nothing(
        "the narrowed holder's view of a broader Work's assertion on its own record",
        &listed,
        &stderr,
        &family.closed_secrets_slice(),
    );
    let row = listed["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["id"] == finding_id.as_str())
        .unwrap_or_else(|| panic!("the holder must still read its own record: {listed}"))
        .clone();
    assert_eq!(
        row["assertions"][0]["decision"]["reason"], marker,
        "the reason is the broader parent's prose: {row}"
    );
    assert!(
        listed["disclosure"]["withheld"].as_u64().unwrap_or(0) >= 1,
        "the withholding must be counted, never silent: {listed}"
    );

    // The holder's own prose, and the assertion's journal-side half,
    // are untouched: this narrows one sentence, not the record.
    assert!(
        row["claim_text"].as_str().unwrap().contains("openmarker"),
        "the holder still reads the prose it wrote itself: {row}"
    );
    assert_eq!(
        row["assertions"][0]["decision"]["decision"], "rejected",
        "the recorded decision is not prose: {row}"
    );
    assert!(
        row["assertions"][0]["by"]
            .as_str()
            .unwrap()
            .contains("parent reviewer"),
        "the recorded name stays, unverified as ever: {row}"
    );

    // ---- the same boundary through `finding assert`'s own reply ----
    let (code, reply, stderr) = finding_cli(
        estate,
        &[
            "assert",
            "--finding",
            &finding_id,
            "--decision",
            "deferred",
            "--by",
            "the child itself",
            "--requesting-work",
            &family.child.work_id,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert_discloses_nothing(
        "the scoped reply `finding assert` hands back",
        &reply,
        &stderr,
        &family.closed_secrets_slice(),
    );

    // ---- and through `finding settle`'s own reply ----
    let (code, settled, stderr) = finding_cli(
        estate,
        &[
            "settle",
            "--finding",
            &finding_id,
            "--requesting-work",
            &family.child.work_id,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert_discloses_nothing(
        "the scoped reply `finding settle` hands back",
        &settled,
        &stderr,
        &family.closed_secrets_slice(),
    );

    // ---- and on the estate index, rebuilt from the journals alone ----
    let (ok, _rebuilt, stderr) = atlas(estate, &["findings", "--admin", "--rebuild"]);
    assert!(ok, "{stderr}");
    let (ok, index, stderr) = atlas(
        estate,
        &["findings", "--requesting-work", &family.child.work_id],
    );
    assert!(ok, "{stderr}");
    assert_discloses_nothing(
        "the narrowed holder's view of the estate index",
        &index,
        &stderr,
        &family.closed_secrets_slice(),
    );
    let indexed = index["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| {
            r["finding"]["id"] == finding_id.as_str()
                && r["assertion"]["decision"]["decision"] == "rejected"
        })
        .unwrap_or_else(|| panic!("the asserted row must still be indexed: {index}"))
        .clone();
    assert_eq!(
        indexed["assertion"]["decision"]["reason"], marker,
        "the index must answer exactly as the list did: {indexed}"
    );
    assert!(
        indexed["finding"]["claim_text"]
            .as_str()
            .unwrap()
            .contains("openmarker"),
        "the holder's own claim is not narrowed by someone else's sentence: {indexed}"
    );

    // ---- the Work that actually wrote it reads it in full ----
    let (code, author_view, stderr) = finding_cli(
        estate,
        &[
            "list",
            "--requesting-work",
            &family.parent.work_id,
            "--work",
            &family.child.work_id,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let author_row = author_view["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["id"] == finding_id.as_str())
        .unwrap()
        .clone();
    assert!(
        author_row["assertions"][0]["decision"]["reason"]
            .as_str()
            .unwrap()
            .contains("embargomarker"),
        "the author reads its own sentence in full: {author_row}"
    );

    // ---- explicit administration stays total ----
    let (code, admin, stderr) = finding_cli(
        estate,
        &["list", "--admin", "--work", &family.child.work_id],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let mut admin_strings = Vec::new();
    all_strings(&admin, &mut admin_strings);
    assert!(
        admin_strings.iter().any(|s| s.contains("embargomarker")),
        "explicit administrative inspection must remain total: {admin}"
    );

    let _ = &family.pointer;
    stop_wirkd(estate, family.wirkd);
}

/// Ruling 0114's first carried gap, on the executed observation in
/// `loop-b-assertion-author-verify/VERDICT.md` V2: an `Assertion`
/// carries its operator's sentence in `Assertion.reason`, and that
/// field was rendered nowhere. `Decision` carries a `reason` only on
/// `Rejected`, so a sentence supplied with `--decision deferred`,
/// `accepted` or `partially_accepted` was accepted, journaled and kept
/// durably — and then shown to *nobody*, the author and `--admin`
/// included. The recorded prose is the record; a decision is not a
/// reason to drop it.
///
/// Rendering it is disclosure, so it is rendered *through the same
/// author gate* the rejected reason already runs on, and the pair
/// counts once: `Decision::Rejected` repeats the identical sentence, so
/// a rejected assertion withholds two renderings of one authored thing
/// and adds exactly one to the count, exactly as `claim`/`claim_text`
/// already do.
#[test]
fn a_recorded_assertion_reason_is_rendered_for_every_decision_and_withheld_once() {
    let family = build_family(&["scratch:write", "open:read", "helper:write"]);
    let estate = &family.estate;

    let (code, raised, stderr) = raise_cli(
        estate,
        &family.child.work_id,
        &family.child.run_id,
        &[
            "--kind",
            "gap",
            "--scope",
            "estate_local",
            "--claim",
            "the child's own statement, quoting only openmarker, which the child holds",
            "--evidence",
            &family.open_coordinate,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let finding_id = raised["id"].as_str().unwrap().to_string();

    // The narrowed holder's own view of its own record, before anybody
    // else writes a sentence into it: the baseline every count below is
    // measured against, so the numbers are this change's and not the
    // record's.
    let holder_list = || {
        let (code, listed, stderr) = finding_cli(
            estate,
            &[
                "list",
                "--requesting-work",
                &family.child.work_id,
                "--work",
                &family.child.work_id,
            ],
        );
        assert_eq!(code, Some(0), "{stderr}");
        assert_discloses_nothing(
            "the narrowed holder's view of a broader Work's assertions on its own record",
            &listed,
            &stderr,
            &family.closed_secrets_slice(),
        );
        let row = listed["findings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["id"] == finding_id.as_str())
            .unwrap_or_else(|| panic!("the holder must still read its own record: {listed}"))
            .clone();
        (listed["disclosure"]["withheld"].as_u64().unwrap(), row)
    };
    let (baseline, _) = holder_list();

    // Every decision this daemon compiles in that can carry a free
    // sentence, each asserted by the *broader parent* into the narrowed
    // child's own journal.
    let assert_as = |work: &str, decision: &str, by: &str, reason: &str| {
        let (code, reply, stderr) = finding_cli(
            estate,
            &[
                "assert",
                "--finding",
                &finding_id,
                "--decision",
                decision,
                "--by",
                by,
                "--reason",
                reason,
                "--requesting-work",
                work,
            ],
        );
        assert_eq!(code, Some(0), "{stderr}");
        (reply, stderr)
    };

    let marker = serde_json::json!({"withheld": true});
    let reasons = [
        (
            "deferred",
            "deferred: the embargomarker roster review is pending",
        ),
        (
            "accepted",
            "accepted: the embargomarker roster already carries this",
        ),
        (
            "partially_accepted",
            "partially accepted: only the embargomarker half holds",
        ),
        (
            "rejected",
            "rejected: the embargomarker roster says otherwise",
        ),
    ];
    for (index, (decision, reason)) in reasons.iter().enumerate() {
        let (reply, stderr) =
            assert_as(&family.parent.work_id, decision, "parent reviewer", reason);
        // `finding assert`'s own scoped reply is one of the surfaces
        // that must not hand the sentence back to the narrowed holder;
        // here the requester *is* the author, so it reads it — what
        // must never appear is the reply to somebody else, checked
        // through the holder's list below.
        let _ = (&reply, &stderr);

        let (withheld, row) = holder_list();
        let assertion = &row["assertions"][index];
        assert_eq!(
            assertion["decision"]["decision"],
            serde_json::Value::String((*decision).to_string()),
            "the recorded decision is journal-side and stays: {row}"
        );
        assert_eq!(
            assertion["reason"], marker,
            "a `{decision}` assertion's recorded reason must be rendered, and gated on its author: {row}"
        );
        if *decision == "rejected" {
            assert_eq!(
                assertion["decision"]["reason"], marker,
                "the same sentence repeated inside the decision is withheld too: {row}"
            );
        }
        assert_eq!(
            withheld,
            baseline + index as u64 + 1,
            "one authored sentence is one withholding, however many times the record repeats it: {row}"
        );
    }

    // The Work that actually wrote them reads every one in full — a
    // deferred or accepted sentence is not a second-class record.
    let (code, author_view, stderr) = finding_cli(
        estate,
        &[
            "list",
            "--requesting-work",
            &family.parent.work_id,
            "--work",
            &family.child.work_id,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let author_row = author_view["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["id"] == finding_id.as_str())
        .unwrap()
        .clone();
    for (index, (decision, reason)) in reasons.iter().enumerate() {
        assert_eq!(
            author_row["assertions"][index]["reason"].as_str(),
            Some(*reason),
            "the author reads its own `{decision}` sentence in full: {author_row}"
        );
    }

    // Explicit administration stays total, which is exactly where the
    // gap was loudest: an administrator was shown `reason: None` for a
    // sentence it had itself accepted and journaled.
    let (code, admin, stderr) = finding_cli(
        estate,
        &["list", "--admin", "--work", &family.child.work_id],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let admin_row = admin["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["id"] == finding_id.as_str())
        .unwrap()
        .clone();
    for (index, (_, reason)) in reasons.iter().enumerate() {
        assert_eq!(
            admin_row["assertions"][index]["reason"].as_str(),
            Some(*reason),
            "explicit administrative inspection must remain total: {admin_row}"
        );
    }

    // The holder's own sentence, on its own record, is its own to read:
    // this narrows by authorship, not by decision, and adds no
    // withholding of its own.
    assert_as(
        &family.child.work_id,
        "accepted",
        "the child itself",
        "accepted: openmarker is enough for this",
    );
    let (withheld, row) = holder_list();
    assert_eq!(
        row["assertions"][4]["reason"].as_str(),
        Some("accepted: openmarker is enough for this"),
        "the holder reads the sentence it wrote itself: {row}"
    );
    assert_eq!(
        withheld,
        baseline + reasons.len() as u64,
        "its own sentence withholds nothing: {row}"
    );

    // An assertion with no reason at all renders a null, never a
    // withholding: nothing was hidden because nothing was recorded.
    let (code, _reply, stderr) = finding_cli(
        estate,
        &[
            "assert",
            "--finding",
            &finding_id,
            "--decision",
            "deferred",
            "--by",
            "parent reviewer",
            "--requesting-work",
            &family.parent.work_id,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let (withheld, row) = holder_list();
    assert!(
        row["assertions"][5]["reason"].is_null(),
        "an assertion that recorded no reason renders a null: {row}"
    );
    assert_eq!(
        withheld,
        baseline + reasons.len() as u64,
        "an absent sentence is not a withheld one: {row}"
    );

    let _ = &family.pointer;
    stop_wirkd(estate, family.wirkd);
}

/// Ruling 0114's second carried gap, on the same review's own observed
/// residue: `reconcile_findings_index` runs at daemon start and on the
/// administrative `--rebuild`, and `settle_ready`/`finding applied`
/// already re-run it the moment they append. `finding assert` never
/// did — so a just-asserted row was absent from `atlas findings` until
/// somebody restarted the daemon or ran a destructive rebuild, and the
/// estate index disagreed with `finding list` about a record that was
/// already durable in the journal.
///
/// Journal first, index second, through the same idempotent
/// content-addressed sweep: no second append protocol, no rewriting of
/// history, and repeated assertions on one Finding accumulate as the
/// distinct rows they are rather than duplicating.
#[test]
fn a_newly_asserted_row_reaches_the_estate_index_with_no_restart_and_no_rebuild() {
    let family = build_family(&["scratch:write", "open:read", "helper:write"]);
    let estate = &family.estate;

    let (code, raised, stderr) = raise_cli(
        estate,
        &family.child.work_id,
        &family.child.run_id,
        &[
            "--kind",
            "gap",
            "--scope",
            "estate_local",
            "--claim",
            "the child's own statement, quoting only openmarker, which the child holds",
            "--evidence",
            &family.open_coordinate,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let finding_id = raised["id"].as_str().unwrap().to_string();

    let indexed_rows = || {
        let (ok, index, stderr) = atlas(estate, &["findings", "--admin"]);
        assert!(ok, "{stderr}");
        index["rows"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["finding"]["id"] == finding_id.as_str())
            .cloned()
            .collect::<Vec<_>>()
    };

    // Raising is not indexing: the row this test is about does not
    // exist yet, so its later presence is this assertion's doing.
    assert!(
        indexed_rows().is_empty(),
        "a raised finding carries no asserted row yet"
    );

    let (code, _reply, stderr) = finding_cli(
        estate,
        &[
            "assert",
            "--finding",
            &finding_id,
            "--decision",
            "rejected",
            "--by",
            "parent reviewer",
            "--reason",
            "rejected: the embargomarker roster says otherwise",
            "--requesting-work",
            &family.parent.work_id,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");

    // No restart. No `--rebuild`. The same daemon, queried immediately.
    let rows = indexed_rows();
    assert_eq!(
        rows.len(),
        1,
        "the just-asserted row must be in the estate index without a restart or a rebuild: {rows:?}"
    );
    assert_eq!(rows[0]["kind"], "asserted");
    assert_eq!(rows[0]["assertion"]["decision"]["decision"], "rejected");
    let first_row_id = rows[0]["id"].as_str().unwrap().to_string();

    // A scoped reader reaches the same new row through its own view,
    // and the index agrees with the list about what it may see: the
    // narrowed holder keeps the row and the decision, and loses only
    // the broader author's sentence.
    let (ok, scoped, stderr) = atlas(
        estate,
        &["findings", "--requesting-work", &family.child.work_id],
    );
    assert!(ok, "{stderr}");
    assert_discloses_nothing(
        "the narrowed holder's immediate view of the estate index",
        &scoped,
        &stderr,
        &family.closed_secrets_slice(),
    );
    let scoped_row = scoped["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == first_row_id.as_str())
        .unwrap_or_else(|| panic!("the scoped reader reaches the same new row: {scoped}"))
        .clone();
    assert_eq!(
        scoped_row["assertion"]["decision"]["reason"],
        serde_json::json!({"withheld": true}),
        "the index answers exactly as the list does: {scoped_row}"
    );

    // A second assertion on the same Finding is a second journal event
    // and therefore a second row, not a rewrite and not a duplicate of
    // the first: the row id is content-addressed on the event that
    // minted it.
    let (code, _reply, stderr) = finding_cli(
        estate,
        &[
            "assert",
            "--finding",
            &finding_id,
            "--decision",
            "deferred",
            "--by",
            "the child itself",
            "--requesting-work",
            &family.child.work_id,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let rows = indexed_rows();
    assert_eq!(
        rows.len(),
        2,
        "repeated assertions accumulate as the distinct rows they are: {rows:?}"
    );
    let mut ids = rows
        .iter()
        .map(|row| row["id"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 2, "no row is indexed twice: {rows:?}");
    assert!(ids.contains(&first_row_id), "the first row is unchanged");

    // And the journals remain the whole truth: a destructive rebuild
    // from them alone reproduces exactly the same rows, which is what
    // makes the immediate sweep a reconciliation rather than a second
    // source of record.
    let before = indexed_rows();
    let (ok, _rebuilt, stderr) = atlas(estate, &["findings", "--admin", "--rebuild"]);
    assert!(ok, "{stderr}");
    assert_eq!(
        indexed_rows(),
        before,
        "the rebuild from journals alone reproduces the immediately-swept rows exactly"
    );

    let _ = &family.pointer;
    stop_wirkd(estate, family.wirkd);
}

// ---- 9. Response repair: settle/assert/applied scope their own replies --

/// `loop-b-disclosure-verify/VERDICT.md` C1/C2: `finding settle` and
/// `finding assert` used to resolve a finding by an estate-wide scan
/// with no requester at all and hand back the unscoped record — the
/// exact identities `finding list` had just withheld. Neither verb has
/// a default any more; `--admin` stays total.
#[test]
fn finding_settle_scopes_its_reply_and_admin_stays_unscoped() {
    let family = build_family(&["scratch:write", "open:read", "helper:write"]);
    let estate = &family.estate;

    let (code, raised, stderr) = raise_cli(
        estate,
        &family.parent.work_id,
        &family.parent.run_id,
        &[
            "--kind",
            "gap",
            "--scope",
            "estate_local",
            "--claim",
            "the parent's own finding on a source only it holds",
            "--evidence",
            &family.closed_coordinate,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let f1 = raised["id"].as_str().unwrap().to_string();

    // Neither `--requesting-work` nor `--admin`: refused before any
    // request even reaches wirkd — there is no silent unscoped default.
    let (code, refused, stderr) = finding_cli(estate, &["settle", "--finding", &f1]);
    assert_eq!(
        code,
        Some(2),
        "settle with no requester and no --admin must be refused: {refused} {stderr}"
    );

    // The bypass this closes: the narrowed child hands the same finding
    // id it just learned from the repaired `finding list` to `finding
    // settle`, and must not get back what `list` withheld.
    let (code, scoped, stderr) = finding_cli(
        estate,
        &[
            "settle",
            "--finding",
            &f1,
            "--requesting-work",
            &family.child.work_id,
        ],
    );
    assert_eq!(code, Some(0), "{scoped} {stderr}");
    assert_discloses_nothing(
        "`finding settle` for the narrowed child",
        &scoped,
        &stderr,
        &family.closed_secrets_slice(),
    );
    assert_eq!(scoped["evidence"][0]["withheld"], serde_json::json!(true));

    // Explicit administration is a distinct, named surface and stays
    // total — the real encoded coordinate, not a withheld marker.
    let (code, admin, stderr) = finding_cli(estate, &["settle", "--finding", &f1, "--admin"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        admin["evidence"][0]["coordinate"].is_string(),
        "explicit administrative settle must remain total: {admin}"
    );

    stop_wirkd(estate, family.wirkd);
}

/// The same repair for `finding assert`, plus the write-side half
/// `settle` does not need: `assert` appends a `FindingAsserted` event to
/// the *target* finding's own Work, so a non-admin requester off that
/// Work's lineage is refused outright — the journal is never written
/// and then merely hidden in the reply.
#[test]
fn finding_assert_scopes_its_reply_and_refuses_off_lineage_writes() {
    let family = build_family(&["scratch:write", "open:read", "helper:write"]);
    let estate = &family.estate;

    let (code, raised, stderr) = raise_cli(
        estate,
        &family.parent.work_id,
        &family.parent.run_id,
        &[
            "--kind",
            "gap",
            "--scope",
            "estate_local",
            "--claim",
            "the parent's own finding on a source only it holds",
            "--evidence",
            &family.closed_coordinate,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let f1 = raised["id"].as_str().unwrap().to_string();
    let before = journal_events(estate, &family.parent.work_id).len();

    // Neither `--requesting-work` nor `--admin`: refused at the CLI.
    let (code, refused, stderr) = finding_cli(
        estate,
        &[
            "assert",
            "--finding",
            &f1,
            "--decision",
            "rejected",
            "--by",
            "nobody",
        ],
    );
    assert_eq!(
        code,
        Some(2),
        "assert with no requester and no --admin must be refused: {refused} {stderr}"
    );

    // Off-lineage: a genuinely unrelated Work names itself as requester.
    // The write is refused outright, not merely hidden afterward.
    let (code, refused, stderr) = finding_cli(
        estate,
        &[
            "assert",
            "--finding",
            &f1,
            "--decision",
            "rejected",
            "--by",
            "outsider",
            "--requesting-work",
            &family.unrelated.work_id,
        ],
    );
    assert_eq!(
        code,
        Some(2),
        "an unrelated requester must not be able to write an assertion onto the parent's own journal: {refused} {stderr}"
    );
    assert_discloses_nothing(
        "the off-lineage assert refusal",
        &refused,
        &stderr,
        &family.closed_secrets_slice(),
    );
    let after_refused = journal_events(estate, &family.parent.work_id).len();
    assert_eq!(
        before, after_refused,
        "a refused off-lineage assert must leave the target journal exactly as it was"
    );

    // On lineage — the child is the parent's own descendant, exactly the
    // "legitimate admitted later-Work EstateLocal use" the authority
    // adjudication preserves: the write succeeds and the reply is
    // scoped exactly as `finding list` already is.
    let (code, accepted, stderr) = finding_cli(
        estate,
        &[
            "assert",
            "--finding",
            &f1,
            "--decision",
            "rejected",
            "--by",
            "the-narrowed-child",
            "--requesting-work",
            &family.child.work_id,
        ],
    );
    assert_eq!(code, Some(0), "{accepted} {stderr}");
    assert_discloses_nothing(
        "the child's own scoped assert reply",
        &accepted,
        &stderr,
        &family.closed_secrets_slice(),
    );
    assert_eq!(accepted["evidence"][0]["withheld"], serde_json::json!(true));
    assert_eq!(
        accepted["assertions"][0]["by"].as_str().unwrap(),
        "recorded name: the-narrowed-child, unverified"
    );
    let after_accepted = journal_events(estate, &family.parent.work_id).len();
    assert_eq!(
        after_accepted,
        after_refused + 1,
        "the on-lineage assert must append exactly the one FindingAsserted event: {accepted}"
    );

    // Explicit administration remains total — the real encoded
    // coordinate, not a withheld marker.
    let (code, admin, stderr) = finding_cli(
        estate,
        &[
            "assert",
            "--finding",
            &f1,
            "--decision",
            "accepted",
            "--by",
            "root",
            "--admin",
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        admin["evidence"][0]["coordinate"].is_string(),
        "explicit administrative assert must remain total: {admin}"
    );

    stop_wirkd(estate, family.wirkd);
}

/// `loop-b-disclosure-verify/VERDICT.md` C3, read but not executed
/// there: `finding applied` already checks the *caller's* own triple for
/// currency, but resolved the *target* finding through the same
/// unscoped path as C1/C2. `applied` gains no new flag — the view is
/// built from the caller's own already-checked producer Work, so a
/// narrowed child applying against a source it *does* hold still
/// receives its own reply scoped by its own bindings, exactly like
/// every other consultation surface.
///
/// W-B Application repair: the case this test previously used to
/// demonstrate C3 — a narrowed child applying against `embargo`, a
/// source it holds no binding on at all, and being handed a withheld
/// record — is now refused outright. That was the repair's own RED 4:
/// the membership was admitted against the *finding owner's* bindings,
/// so the child recorded a durable Application against a source it
/// never held and then had it hidden from itself. The scoped-reply
/// property C3 established is preserved here, demonstrated on the
/// evidence the child may not see rather than on a record it should
/// never have been able to write.
#[test]
fn finding_applied_scopes_its_own_reply_by_the_producers_own_bindings() {
    let family = build_family(&["scratch:write", "open:read", "helper:write"]);
    let estate = &family.estate;
    let closed_repo = family.dir.path().join("embargo-repo");
    let open_repo = family.dir.path().join("open-repo");

    // A finding whose *evidence* is the embargoed coordinate and whose
    // *applies_to* is the shared one: the child may apply against
    // `open`, and must still never learn the embargoed basis.
    let (code, mixed, stderr) = raise_cli(
        estate,
        &family.parent.work_id,
        &family.parent.run_id,
        &[
            "--kind",
            "gap",
            "--scope",
            "estate_local",
            "--claim",
            "a shared-source gap whose evidence is closed",
            "--evidence",
            &family.closed_coordinate,
            "--applies-to",
            &family.open_coordinate,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let mixed_id = mixed["id"].as_str().unwrap().to_string();

    write_file(
        &open_repo,
        "shared.md",
        "openmarker: revised shared basis\n",
    );
    let _ = publish_and_locate_as(estate, &open_repo, "open", "openmarker");
    let (ok, status, err) = atlas(estate, &["status", "--source", "open"]);
    assert!(ok, "{err}");
    let open_revision = status["sources"][0]["published_generation"]["revision"]
        .as_str()
        .unwrap()
        .to_string();

    // A `Read` grant is a grant: ruling 0077 admits the unverified
    // assertion, and the caller-grant rule is satisfied by it.
    let (code, applied, stderr) = applied_cli(
        estate,
        &family.child.work_id,
        &family.child.run_id,
        &[
            "--finding",
            &mixed_id,
            "--source",
            "open",
            "--revision",
            &open_revision,
            "--by",
            "the-narrowed-child",
        ],
    );
    assert_eq!(code, Some(0), "{applied} {stderr}");
    assert_discloses_nothing(
        "`finding applied`'s own reply to a narrowed producer",
        &applied,
        &stderr,
        &family.closed_secrets_slice(),
    );
    let application = applied["applied"].as_array().unwrap().last().unwrap();
    assert_eq!(
        application["source"],
        serde_json::json!("open"),
        "the source the child does hold is carried in full: {application}"
    );
    assert_eq!(
        applied["evidence"][0]["withheld"],
        serde_json::json!(true),
        "the embargoed evidence stays withheld from the producer: {applied}"
    );

    // The stronger outcome the repair adds: a source the child holds no
    // binding on at all is refused before anything is written, rather
    // than written and then hidden.
    let (code, closed_gap, stderr) = raise_cli(
        estate,
        &family.parent.work_id,
        &family.parent.run_id,
        &[
            "--kind",
            "gap",
            "--scope",
            "estate_local",
            "--claim",
            "a closed-source gap for the response repair's own applied probe",
            "--evidence",
            &family.closed_coordinate,
            "--applies-to",
            &family.closed_coordinate,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let closed_id = closed_gap["id"].as_str().unwrap().to_string();

    write_file(
        &closed_repo,
        "embargoed.md",
        "embargomarker: revised for the narrowed producer\n",
    );
    let _ = publish_and_locate_as(estate, &closed_repo, "embargo", "embargomarker");
    let (ok, status, err) = atlas(estate, &["status", "--source", "embargo"]);
    assert!(ok, "{err}");
    let revision = status["sources"][0]["published_generation"]["revision"]
        .as_str()
        .unwrap()
        .to_string();
    let (code, refused, stderr) = applied_cli(
        estate,
        &family.child.work_id,
        &family.child.run_id,
        &[
            "--finding",
            &closed_id,
            "--source",
            "embargo",
            "--revision",
            &revision,
            "--by",
            "the-narrowed-child",
        ],
    );
    assert_eq!(
        code,
        Some(3),
        "a producer with no binding on the changed source records nothing: {refused} {stderr}"
    );

    // The parent, which holds `embargo`, applies cleanly and its own
    // reply carries the source in full. The repair scopes by the
    // producer's own bindings; it does not withhold universally.
    let (code, applied2, stderr) = applied_cli(
        estate,
        &family.parent.work_id,
        &family.parent.run_id,
        &[
            "--finding",
            &closed_id,
            "--source",
            "embargo",
            "--revision",
            &revision,
            "--by",
            "the-parent",
        ],
    );
    assert_eq!(code, Some(0), "{applied2} {stderr}");
    let application2 = applied2["applied"].as_array().unwrap().last().unwrap();
    assert_eq!(
        application2["source"],
        serde_json::json!("embargo"),
        "{application2}"
    );
    assert!(
        application2["after"]["object_id"].is_string(),
        "the parent's own applied reply must carry its own source in full: {application2}"
    );

    stop_wirkd(estate, family.wirkd);
}

impl Family {
    fn closed_secrets_slice(&self) -> Vec<&str> {
        self.closed_secrets.iter().map(String::as_str).collect()
    }
}

// ---- 8. P3 native launch metadata (V-2) ---------------------------------
//
// The independent currentness verification (`VERDICT.md` V-2) observed
// that the native launch wave placed `RunLaunchRequested` and
// `RunLaunchAttempted` — and, before the merge, `RunLaunched` — in
// `event_source_disclosure`'s no-disclosure arm on the reasoning that
// launch metadata is "launch mechanism ... never read out of a source
// checkout". The mechanism halves are: harness kind, model, effort,
// the holder's pid and start token. `selection.args`, `launch_argv` and
// `destination` are not — they are verbatim operator text, Herdr's own
// submitted argv, and a filesystem path. The cases below are the public
// counterexample the adjudication required before correcting it, and
// the useful positives the correction must not cost.

/// The family the launch cases need: the same parent-holds-`embargo`,
/// child-narrowed-to-`open` shape as `build_family`, but with an
/// **Actor** first leaf, because `RunLaunchRequested`/`RunLaunched` are
/// admitted only against a materialized Actor Run (`handle_record`).
struct LaunchFamily {
    #[allow(dead_code)]
    dir: tempfile::TempDir,
    estate: std::path::PathBuf,
    wirkd: KillOnDrop,
    pointer: WirkdPointer,
    parent: Submitted,
    child: Submitted,
    /// The real checkout path of the source the child does not hold —
    /// the thing an authored `--add-dir` would plausibly name.
    closed_checkout: String,
    closed_secrets: Vec<String>,
}

impl LaunchFamily {
    fn closed_secrets_slice(&self) -> Vec<&str> {
        self.closed_secrets.iter().map(String::as_str).collect()
    }
}

fn build_launch_family(narrowed_grants: &[&str]) -> LaunchFamily {
    let dir = tempfile::tempdir().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    // A container declaring the `helper` child role, whose own leaf is
    // an Actor Waypoint — the only kind of Run a launch is admitted
    // against at all.
    route_fixture::write_route(
        &estate,
        "disclosure_launch",
        r#"{"id":"disclosure-launch","waypoints":[
            {"id":"outer","kind":"Container",
             "declared_outputs":[{"name":"a.md","required":true}],
             "required_child_outcomes":[{"role":"helper","required":true}],
             "leaves":[
               {"id":"outer/leaf-a","kind":"Actor",
                "intent":"run the actor whose launch metadata is under test",
                "declared_outputs":[{"name":"a.md","required":true}],
                "boundary":["**"]}
             ]}
        ]}"#,
    );
    let (wirkd, pointer) = start_wirkd(&estate);

    let closed_repo = dir.path().join("embargo-repo");
    init_repo(&closed_repo);
    write_file(
        &closed_repo,
        "embargoed.md",
        "embargomarker: the embargoed finding basis\n",
    );
    let closed_coordinate =
        publish_and_locate_as(&estate, &closed_repo, "embargo", "embargomarker");

    let open_repo = dir.path().join("open-repo");
    init_repo(&open_repo);
    write_file(&open_repo, "shared.md", "openmarker: the shared basis\n");
    publish_and_locate_as(&estate, &open_repo, "open", "openmarker");

    let parent_repo = dir.path().join("parent-repo");
    init_repo(&parent_repo);
    let parent = submit_kind(
        &estate,
        "disclosure_launch",
        &parent_repo,
        &[
            "embargo:write",
            "open:read",
            "helper:write",
            "scratch:write",
        ],
        None,
        Some("actor"),
    )
    .unwrap();
    materialize_actor(&pointer.socket, &estate, &parent.work_id, &parent.run_id);

    let child_repo = dir.path().join("child-repo");
    init_repo(&child_repo);
    let child = submit(
        &estate,
        "disclosure_launch",
        &child_repo,
        narrowed_grants,
        Some(ParentRef {
            work: &parent.work_id,
            waypoint: "outer",
            run: &parent.run_id,
            role: "helper",
            attempt: None,
        }),
    )
    .unwrap();

    let closed_checkout = closed_repo.display().to_string();
    let closed_secrets = vec![
        "embargo".to_string(),
        "embargoed.md".to_string(),
        "embargomarker".to_string(),
        closed_coordinate,
        closed_checkout.clone(),
    ];

    LaunchFamily {
        dir,
        estate,
        wirkd,
        pointer,
        parent,
        child,
        closed_checkout,
        closed_secrets,
    }
}

/// The three P3 native launch events, appended through **wirkd's own
/// `record` verb** — the identical daemon boundary `RunLoop::launch`
/// writes them through, with the identical admission checks (one
/// request per Run, an attempt only after an admitted request, a
/// `RunLaunched` that must restate the bound request, and a `holder`
/// wirkd mints from the connection's own peer credentials rather than
/// from anything this caller says). No journal file is touched and no
/// Herdr transport is simulated.
fn record_launch_events(
    socket: &Path,
    work_id: &str,
    run_id: &str,
    selection: wirk_core::ActorSelection,
    destination: &str,
    launch_argv: &[&str],
) {
    use wirk_core::{ActorKind, AttemptHolder, RunId, WorkId};
    use wirkd::RecordPayload;

    let kind = ActorKind("claude".to_string());
    for event in [
        EventKind::RunLaunchRequested {
            run: RunId(run_id.to_string()),
            actor_kind: kind.clone(),
            selection: selection.clone(),
        },
        EventKind::RunLaunchAttempted {
            run: RunId(run_id.to_string()),
            destination: destination.to_string(),
            holder: AttemptHolder::default(),
        },
        EventKind::RunLaunched {
            run: RunId(run_id.to_string()),
            actor_kind: kind.clone(),
            selection: selection.clone(),
            launch_argv: launch_argv.iter().map(|arg| (*arg).to_string()).collect(),
        },
    ] {
        let reply = wirkd::client::call(
            socket,
            &wirkd::Request::record(RecordPayload {
                work_id: WorkId(work_id.to_string()),
                run: Some(RunId(run_id.to_string())),
                kind: event.clone(),
            }),
        )
        .expect("record launch event");
        assert!(
            matches!(reply, wirkd::Reply::Ok { .. }),
            "wirkd must admit {event:?} against a materialized Actor Run: {reply:?}"
        );
    }
}

const LAUNCH_KINDS: [&str; 3] = ["RunLaunchRequested", "RunLaunchAttempted", "RunLaunched"];

/// **Red on the pre-correction tree, all three kinds.** The parent's
/// launch carries a source-derived path in every place one can appear:
/// the authored `selection.args` the request binds, the Herdr
/// `destination` the attempt binds, and the `launch_argv` Herdr reports
/// back. A child narrowed to `open` may reference its parent's journal
/// — and must not thereby acquire the parent's `embargo` checkout.
#[test]
fn a_narrowed_child_cannot_cite_launch_metadata_naming_a_denied_checkout() {
    let family = build_launch_family(&["scratch:write", "open:read", "helper:write"]);
    let estate = &family.estate;
    let embargoed_path = format!("{}/embargoed.md", family.closed_checkout);

    record_launch_events(
        &family.pointer.socket,
        &family.parent.work_id,
        &family.parent.run_id,
        wirk_core::ActorSelection {
            model: Some("opus".to_string()),
            effort: Some("medium".to_string()),
            args: vec!["--add-dir".to_string(), embargoed_path.clone()],
        },
        &format!("{}/.herdr/embargo-session.sock", family.closed_checkout),
        &["claude", "--add-dir", &embargoed_path],
    );

    for kind in LAUNCH_KINDS {
        let reference = event_ref_of_kind(estate, &family.parent.work_id, kind);
        let (code, reply, stderr) = raise_cli(
            estate,
            &family.child.work_id,
            &family.child.run_id,
            &[
                "--kind",
                "gap",
                "--scope",
                "estate_local",
                "--claim",
                "reading the parent's own launch metadata through kinship",
                "--evidence",
                &reference,
            ],
        );
        assert_eq!(
            code,
            Some(3),
            "a narrowed child must not admit its parent's own {kind}, whose launch metadata \
             names a checkout it does not hold: {reply} {stderr}"
        );
        assert_discloses_nothing(
            &format!("the refusal of {kind}"),
            &reply,
            &stderr,
            &family.closed_secrets_slice(),
        );
    }

    // The useful positive, on the identical records: the producer's own
    // bindings do reach them, so the parent may cite all three.
    for kind in LAUNCH_KINDS {
        let reference = event_ref_of_kind(estate, &family.parent.work_id, kind);
        let (code, reply, stderr) = raise_cli(
            estate,
            &family.parent.work_id,
            &family.parent.run_id,
            &[
                "--kind",
                "gap",
                "--scope",
                "estate_local",
                "--claim",
                "the producer reads its own launch metadata",
                "--evidence",
                &reference,
            ],
        );
        assert_eq!(
            code,
            Some(0),
            "the producing Work's own bindings must still admit its own {kind}: {reply} {stderr}"
        );
    }

    stop_wirkd(estate, family.wirkd);
}

/// The other half of one classification, and the reason the fix is a
/// binding-set scoping rather than a ban: a launch record that carries
/// **no content at all** discloses nothing, so a narrowed child may
/// still read it for what it is — the mechanism identity of a launch,
/// which is the `actor_kind`, the Run and the attempt holder.
///
/// This is exactly the shape of a legacy record: `selection`,
/// `launch_argv`, `destination` and `holder` are all
/// `#[serde(default)]`, so a `RunLaunched` written before those fields
/// existed folds to precisely this. What that means is that the record
/// carries nothing to disclose — **not** that the Run launched bare
/// (the launch review's F-D). The estate's own pre-field launch path
/// passed `--model sonnet` and a `--settings <estate root>/…` pair for
/// every claude Run; the journal simply never wrote them down.
///
/// The launch review's F-A moved `model` and `effort` out of this case:
/// they are the same unvalidated operator-authored input `args` is, so
/// a selection carrying either is content. See
/// `a_launch_naming_a_denied_checkout_in_its_model_is_scoped_like_its_args`.
#[test]
fn a_launch_carrying_no_content_stays_readable_as_mechanism_identity() {
    let family = build_launch_family(&["scratch:write", "open:read", "helper:write"]);
    let estate = &family.estate;

    record_launch_events(
        &family.pointer.socket,
        &family.parent.work_id,
        &family.parent.run_id,
        wirk_core::ActorSelection::default(),
        "",
        &[],
    );

    for kind in LAUNCH_KINDS {
        let reference = event_ref_of_kind(estate, &family.parent.work_id, kind);
        let (code, reply, stderr) = raise_cli(
            estate,
            &family.child.work_id,
            &family.child.run_id,
            &[
                "--kind",
                "gap",
                "--scope",
                "estate_local",
                "--claim",
                "consulting the parent's bare launch identity",
                "--evidence",
                &reference,
            ],
        );
        assert_eq!(
            code,
            Some(0),
            "a {kind} carrying no selection, argv or destination discloses no source and must \
             stay citable by a narrowed child: {reply} {stderr}"
        );
    }

    stop_wirkd(estate, family.wirkd);
}

/// One hop must not launder it either. The parent legitimately raises a
/// finding whose only evidence is its own loaded `RunLaunched`; the
/// child cites *that finding's* raise event. The recursive walk
/// (`journal_reference_admitted`) has to follow the nested reference and
/// refuse, exactly as it already does for a wrapped `Source`. The same
/// record is then read back through `finding list`, where the parent's
/// evidence entry must come back as the contentless `withheld` marker
/// while the finding's own journal identities survive.
#[test]
fn a_wrapper_does_not_launder_launch_metadata_and_finding_list_withholds_it() {
    let family = build_launch_family(&["scratch:write", "open:read", "helper:write"]);
    let estate = &family.estate;
    let embargoed_path = format!("{}/embargoed.md", family.closed_checkout);

    record_launch_events(
        &family.pointer.socket,
        &family.parent.work_id,
        &family.parent.run_id,
        wirk_core::ActorSelection {
            model: None,
            effort: None,
            args: vec!["--add-dir".to_string(), embargoed_path.clone()],
        },
        &format!("{}/.herdr/embargo-session.sock", family.closed_checkout),
        &["claude", "--add-dir", &embargoed_path],
    );

    let launched = event_ref_of_kind(estate, &family.parent.work_id, "RunLaunched");
    let (code, f1, stderr) = raise_cli(
        estate,
        &family.parent.work_id,
        &family.parent.run_id,
        &[
            "--kind",
            "gap",
            "--scope",
            "estate_local",
            "--claim",
            "the parent's own note about its own launch",
            "--evidence",
            &launched,
        ],
    );
    assert_eq!(code, Some(0), "{f1} {stderr}");
    let f1_id = f1["id"].as_str().expect("finding id").to_string();
    let wrapper = raise_event_ref(estate, &family.parent.work_id, &f1_id);

    let (code, reply, stderr) = raise_cli(
        estate,
        &family.child.work_id,
        &family.child.run_id,
        &[
            "--kind",
            "gap",
            "--scope",
            "estate_local",
            "--claim",
            "wrapping the parent's launch metadata in one indirection",
            "--evidence",
            &wrapper,
        ],
    );
    assert_eq!(
        code,
        Some(3),
        "one wrapping finding must not launder launch metadata the direct citation refuses: \
         {reply} {stderr}"
    );
    assert_discloses_nothing(
        "the refusal of the wrapper",
        &reply,
        &stderr,
        &family.closed_secrets_slice(),
    );

    // Presentation: the child may list the estate's findings and must
    // see the parent's own entry withheld rather than rendered.
    let (code, listed, stderr) = finding_cli(
        estate,
        &[
            "list",
            "--requesting-work",
            &family.child.work_id,
            "--scope",
            "estate_local",
        ],
    );
    assert_eq!(code, Some(0), "{listed} {stderr}");
    assert_discloses_nothing(
        "the child's own finding list",
        &listed,
        &stderr,
        &family.closed_secrets_slice(),
    );
    let mut strings = Vec::new();
    all_strings(&listed, &mut strings);
    assert!(
        strings.iter().any(|text| text == "withheld"),
        "the parent's launch-metadata evidence must be marked withheld, not omitted: {listed}"
    );

    stop_wirkd(estate, family.wirkd);
}

// ---- The launch review's F-A, F-B and F-C ---------------------------------
//
// Three surfaces the first launch correction left open, each with an
// executed counterexample in `loop-b-launch-disclosure-verify/VERDICT.md`
// against the binary that correction produced. All three are the same
// contract as the launch events themselves: content present -> the
// producing Work's bindings, content absent -> nothing to disclose,
// decided structurally by which producer writes the field and never by
// reading the string.

/// Appends one arbitrary event through wirkd's own `record` verb — the
/// same daemon boundary and admission every other launch case here uses.
fn record_event(socket: &Path, work_id: &str, run_id: &str, kind: EventKind) {
    use wirk_core::{RunId, WorkId};
    use wirkd::RecordPayload;

    let reply = wirkd::client::call(
        socket,
        &wirkd::Request::record(RecordPayload {
            work_id: WorkId(work_id.to_string()),
            run: Some(RunId(run_id.to_string())),
            kind: kind.clone(),
        }),
    )
    .expect("record event");
    assert!(
        matches!(reply, wirkd::Reply::Ok { .. }),
        "wirkd must admit {kind:?}: {reply:?}"
    );
}

/// Reads wirkd's `status` verb off the wire in one of its two named
/// scopes, returning the reply envelope's error code (if any) and its
/// result object.
fn status_wire(
    socket: &Path,
    work_id: &str,
    requester: Option<&str>,
) -> (Option<String>, serde_json::Value) {
    use wirk_core::WorkId;
    use wirkd::StatusPayload;

    let payload = match requester {
        Some(requester) => {
            StatusPayload::scoped(WorkId(work_id.to_string()), WorkId(requester.to_string()))
        }
        None => StatusPayload::admin(WorkId(work_id.to_string())),
    };
    match wirkd::client::call(socket, &wirkd::Request::status(payload)).expect("status call") {
        wirkd::Reply::Ok { result, .. } => (None, result),
        wirkd::Reply::Err { error, .. } => (
            Some(error.code),
            serde_json::json!({"message": error.message}),
        ),
    }
}

/// **F-A, red on the pre-correction tree.** `selection.model` and
/// `selection.effort` are the same unvalidated operator-authored launch
/// input `selection.args` is — `wirk run` reads all three through the
/// identical `flag_value` with no vocabulary check — so a launch whose
/// only source-derived content is the *model* string must scope exactly
/// like one that put the path in `args`. The counterexample the review
/// executed is the whole test: `args`, `launch_argv` and `destination`
/// are all empty, and the denied checkout path is in `model` alone.
#[test]
fn a_launch_naming_a_denied_checkout_in_its_model_is_scoped_like_its_args() {
    let family = build_launch_family(&["scratch:write", "open:read", "helper:write"]);
    let estate = &family.estate;
    let embargoed_path = format!("{}/embargoed.md", family.closed_checkout);

    record_launch_events(
        &family.pointer.socket,
        &family.parent.work_id,
        &family.parent.run_id,
        wirk_core::ActorSelection {
            model: Some(embargoed_path.clone()),
            effort: Some("medium".to_string()),
            args: Vec::new(),
        },
        "",
        &[],
    );

    // `RunLaunchAttempted` is deliberately absent from this loop: its
    // `destination` is empty here, so it carries no content and stays
    // citable — the content-absent positive, on the same journal.
    for kind in ["RunLaunchRequested", "RunLaunched"] {
        let reference = event_ref_of_kind(estate, &family.parent.work_id, kind);
        let (code, reply, stderr) = raise_cli(
            estate,
            &family.child.work_id,
            &family.child.run_id,
            &[
                "--kind",
                "gap",
                "--scope",
                "estate_local",
                "--claim",
                "reading the parent's launch model through kinship",
                "--evidence",
                &reference,
            ],
        );
        assert_eq!(
            code,
            Some(3),
            "a narrowed child must not admit its parent's {kind} whose model names a checkout \
             it does not hold: {reply} {stderr}"
        );
        assert_discloses_nothing(
            &format!("the refusal of {kind}"),
            &reply,
            &stderr,
            &family.closed_secrets_slice(),
        );
    }

    let attempt = event_ref_of_kind(estate, &family.parent.work_id, "RunLaunchAttempted");
    let (code, reply, stderr) = raise_cli(
        estate,
        &family.child.work_id,
        &family.child.run_id,
        &[
            "--kind",
            "gap",
            "--scope",
            "estate_local",
            "--claim",
            "consulting an attempt that bound no destination",
            "--evidence",
            &attempt,
        ],
    );
    assert_eq!(
        code,
        Some(0),
        "an attempt whose destination is empty carries no content and must stay citable: \
         {reply} {stderr}"
    );

    // And the producer still reads all three of its own.
    for kind in LAUNCH_KINDS {
        let reference = event_ref_of_kind(estate, &family.parent.work_id, kind);
        let (code, reply, stderr) = raise_cli(
            estate,
            &family.parent.work_id,
            &family.parent.run_id,
            &[
                "--kind",
                "gap",
                "--scope",
                "estate_local",
                "--claim",
                "the producer reads its own launch metadata",
                "--evidence",
                &reference,
            ],
        );
        assert_eq!(code, Some(0), "the producer's own {kind}: {reply} {stderr}");
    }

    stop_wirkd(estate, family.wirkd);
}

/// **F-B, red on the pre-correction tree.** `LifecycleObserved.detail`
/// is the actor's captured pane screen (`RunLoop` calls `read_pane` on
/// the `Blocked` transition) and `RunFailed.cause.detail` is the launch
/// or transport diagnostic — both execution output read out of the
/// producing Work's checkout, neither authored prose. Both were in the
/// no-disclosure arm while the loaded `RunLaunched` in the same journal
/// was refused.
///
/// The content-absent positives are on the same journal and matter more
/// than the negatives: a lifecycle observation with no detail is the
/// overwhelming majority of the stream, and a narrowed child keeps it.
#[test]
fn execution_output_details_are_scoped_and_contentless_lifecycle_stays_readable() {
    use wirk_core::{FailureCause, Timestamp};

    let family = build_launch_family(&["scratch:write", "open:read", "helper:write"]);
    let estate = &family.estate;
    let embargoed_path = format!("{}/embargoed.md", family.closed_checkout);
    let socket = &family.pointer.socket;

    // wirkd refuses a lifecycle observation that precedes a launch, so
    // the Run is launched first — with a *content-absent* selection, so
    // the launch records themselves stay citable and every refusal
    // below is attributable to the detail under test and to nothing
    // else.
    record_launch_events(
        socket,
        &family.parent.work_id,
        &family.parent.run_id,
        wirk_core::ActorSelection::default(),
        "",
        &[],
    );

    // The useful, content-absent half first: the ordinary lifecycle
    // stream, and a failure that has only a status.
    record_event(
        socket,
        &family.parent.work_id,
        &family.parent.run_id,
        EventKind::LifecycleObserved {
            status: "working".to_string(),
            detail: None,
        },
    );
    // The pane capture, verbatim in the shape `read_pane` returns.
    record_event(
        socket,
        &family.parent.work_id,
        &family.parent.run_id,
        EventKind::LifecycleObserved {
            status: "blocked".to_string(),
            detail: Some(format!(
                "the actor is waiting on its pane %1:\ncat {embargoed_path}\nembargomarker: the embargoed basis"
            )),
        },
    );
    record_event(
        socket,
        &family.parent.work_id,
        &family.parent.run_id,
        EventKind::RunFailed {
            cause: FailureCause {
                status: Some("transport".to_string()),
                request_id: None,
                at: Timestamp(0),
                detail: Some(format!(
                    "connecting to {}/.herdr/herdr.sock: No such file or directory",
                    family.closed_checkout
                )),
            },
        },
    );

    let cite = |reference: &str, claim: &str| {
        raise_cli(
            estate,
            &family.child.work_id,
            &family.child.run_id,
            &[
                "--kind",
                "gap",
                "--scope",
                "estate_local",
                "--claim",
                claim,
                "--evidence",
                reference,
            ],
        )
    };

    // The contentless lifecycle observation is the FIRST of its kind in
    // the journal, so `event_ref_of_kind` names exactly it.
    let plain = event_ref_of_kind(estate, &family.parent.work_id, "LifecycleObserved");
    let (code, reply, stderr) = cite(&plain, "consulting the parent's plain lifecycle signal");
    assert_eq!(
        code,
        Some(0),
        "a LifecycleObserved with no detail carries no content and must stay citable by a \
         narrowed child: {reply} {stderr}"
    );

    // The pane capture is the second one; find it by its own detail
    // rather than by kind.
    let pane_ref = {
        let event = journal_events(estate, &family.parent.work_id)
            .into_iter()
            .find(|event| {
                matches!(
                    &event.kind,
                    EventKind::LifecycleObserved {
                        detail: Some(_),
                        ..
                    }
                )
            })
            .expect("the pane-detail LifecycleObserved");
        format!("work/{}/event/{}", family.parent.work_id, event.id.0)
    };
    let (code, reply, stderr) = cite(
        &pane_ref,
        "reading the parent's captured pane through kinship",
    );
    assert_eq!(
        code,
        Some(3),
        "a narrowed child must not admit a LifecycleObserved whose detail is the parent's own \
         pane screen: {reply} {stderr}"
    );
    assert_discloses_nothing(
        "the refusal of the pane observation",
        &reply,
        &stderr,
        &family.closed_secrets_slice(),
    );

    let failed = event_ref_of_kind(estate, &family.parent.work_id, "RunFailed");
    let (code, reply, stderr) = cite(&failed, "reading the parent's transport diagnostic");
    assert_eq!(
        code,
        Some(3),
        "a narrowed child must not admit a RunFailed whose cause detail names a checkout it \
         does not hold: {reply} {stderr}"
    );
    assert_discloses_nothing(
        "the refusal of the failure",
        &reply,
        &stderr,
        &family.closed_secrets_slice(),
    );

    // The producer reads both of its own.
    for reference in [&pane_ref, &failed] {
        let (code, reply, stderr) = raise_cli(
            estate,
            &family.parent.work_id,
            &family.parent.run_id,
            &[
                "--kind",
                "gap",
                "--scope",
                "estate_local",
                "--claim",
                "the producer reads its own execution output",
                "--evidence",
                reference,
            ],
        );
        assert_eq!(
            code,
            Some(0),
            "the producing Work's own bindings must still admit it: {reply} {stderr}"
        );
    }

    stop_wirkd(estate, family.wirkd);
}

/// **F-C, red on the pre-correction tree.** `status` had no requester at
/// all: it returned the resolved `selection`, Herdr's `launch_argv` and
/// the attempt's `destination` verbatim for any Work id, so the whole
/// launch correction could be read straight off a sibling verb. It now
/// answers one of two *named* scopes and refuses to answer unscoped.
///
/// The operator positive is as important as the negative: the named
/// administrative read still returns everything, because that is what an
/// operator at the estate root is for, and naming it claims nothing
/// about who is asking.
#[test]
fn status_answers_named_scopes_and_never_falls_through_to_the_unscoped_one() {
    let family = build_launch_family(&["scratch:write", "open:read", "helper:write"]);
    let estate = &family.estate;
    let socket = &family.pointer.socket;
    let embargoed_path = format!("{}/embargoed.md", family.closed_checkout);
    let destination = format!("{}/.herdr/embargo-session.sock", family.closed_checkout);

    record_launch_events(
        socket,
        &family.parent.work_id,
        &family.parent.run_id,
        wirk_core::ActorSelection {
            model: Some("opus".to_string()),
            effort: Some("medium".to_string()),
            args: vec!["--add-dir".to_string(), embargoed_path.clone()],
        },
        &destination,
        &["claude", "--add-dir", &embargoed_path],
    );

    // 1. The narrowed child asks about its parent. Lineage admits the
    //    journal; its own bindings do not admit the parent's checkout,
    //    so every content half comes back withheld and the journal
    //    identities survive.
    let (code, scoped) = status_wire(socket, &family.parent.work_id, Some(&family.child.work_id));
    assert_eq!(
        code, None,
        "the child may consult its parent's status: {scoped}"
    );
    assert_eq!(scoped["scope"], "requester", "{scoped}");
    assert!(
        scoped["disclosure"]["withheld"].as_u64().unwrap_or(0) > 0,
        "the parent's launch content must be reported withheld: {scoped}"
    );
    assert_discloses_nothing(
        "the child's scoped status read of its parent",
        &scoped,
        "",
        &family.closed_secrets_slice(),
    );
    assert_eq!(
        scoped["state"], "active",
        "the Work's own state is journal identity and survives: {scoped}"
    );

    // 2. The parent asks about itself: fully admitted, nothing withheld.
    let (code, own) = status_wire(socket, &family.parent.work_id, Some(&family.parent.work_id));
    assert_eq!(code, None, "{own}");
    assert_eq!(own["disclosure"]["withheld"], 0, "{own}");
    let mut strings = Vec::new();
    all_strings(&own, &mut strings);
    assert!(
        strings.iter().any(|text| text.contains(&embargoed_path)),
        "a Work reading its own status must still see its own launch metadata: {own}"
    );

    // 3. The named administrative read: everything, as before.
    let (code, admin) = status_wire(socket, &family.parent.work_id, None);
    assert_eq!(code, None, "{admin}");
    assert_eq!(admin["scope"], "administrative", "{admin}");
    let mut strings = Vec::new();
    all_strings(&admin, &mut strings);
    assert!(
        strings.iter().any(|text| text.contains(&destination)),
        "the named operator surface still returns the attempt destination: {admin}"
    );

    // 4. An unrelated Work is off the requester's lineage entirely, and
    //    the refusal names nothing.
    let outsider = {
        let repo = family.dir.path().join("outsider-repo");
        init_repo(&repo);
        submit(
            estate,
            "disclosure_launch",
            &repo,
            &["scratch:write", "open:read", "helper:write"],
            None,
        )
        .unwrap()
    };
    let (code, refused) = status_wire(socket, &family.parent.work_id, Some(&outsider.work_id));
    assert_eq!(
        code.as_deref(),
        Some("InadmissibleEvidence"),
        "an unrelated Work must not consult this Work's status: {refused}"
    );
    assert_discloses_nothing(
        "the off-lineage status refusal",
        &refused,
        "",
        &family.closed_secrets_slice(),
    );

    // 5. No silent unscoped default on the wire: a payload naming
    //    neither scope is refused rather than answered.
    let unscoped = wirkd::client::call(
        socket,
        &wirkd::Request::status(wirkd::StatusPayload {
            work_id: wirk_core::WorkId(family.parent.work_id.clone()),
            requester: None,
            admin: false,
        }),
    )
    .expect("status call");
    match unscoped {
        wirkd::Reply::Err { error, .. } => assert_eq!(
            error.code, "BadRequest",
            "an unscoped status must be refused, not answered: {}",
            error.message
        ),
        other => panic!("an unscoped status must be refused: {other:?}"),
    }

    stop_wirkd(estate, family.wirkd);
}

/// The public CLI half of F-C, through the real `wirk` binary: the
/// operator verb says which of the two surfaces answered, and
/// `--requesting-work` gets the narrowed one with its withheld count.
#[test]
fn the_status_cli_names_the_surface_that_answered() {
    let family = build_launch_family(&["scratch:write", "open:read", "helper:write"]);
    let estate = &family.estate;
    let embargoed_path = format!("{}/embargoed.md", family.closed_checkout);

    record_launch_events(
        &family.pointer.socket,
        &family.parent.work_id,
        &family.parent.run_id,
        wirk_core::ActorSelection {
            model: Some("opus".to_string()),
            effort: Some("medium".to_string()),
            args: vec!["--add-dir".to_string(), embargoed_path.clone()],
        },
        &format!("{}/.herdr/embargo-session.sock", family.closed_checkout),
        &["claude", "--add-dir", &embargoed_path],
    );

    let run_status = |args: &[&str]| {
        let mut full = vec!["work", "status", "--estate", estate.to_str().unwrap()];
        full.extend_from_slice(args);
        // The operator's own read (ruling 0117): asked for explicitly,
        // never inherited from whatever context runs the suite.
        let output = wirk_cli(&[])
            .args(&full)
            .output()
            .expect("wirk work status runs");
        (
            String::from_utf8_lossy(&output.stdout).trim().to_string(),
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        )
    };

    let (out, err) = run_status(&["--work", &family.parent.work_id]);
    assert!(
        out.contains("scope administrative"),
        "the operator verb must name the surface it used: {out} {err}"
    );

    let (out, err) = run_status(&[
        "--work",
        &family.parent.work_id,
        "--requesting-work",
        &family.child.work_id,
    ]);
    assert!(
        out.contains("scope requester withheld"),
        "the scoped verb must name its surface and its withheld count: {out} {err}"
    );
    assert!(
        !out.contains("embargo") && !err.contains("embargo"),
        "the scoped CLI answer must not disclose the parent's sources: {out} {err}"
    );

    stop_wirkd(estate, family.wirkd);
}

/// `status`'s sibling. `watch` streams a Work's **raw journal events** —
/// launch metadata and captured pane details included — so it reaches
/// strictly more than `status` does, and it had the identical absence
/// of a requester. It now answers the same two named scopes, admitting
/// or refusing the stream whole because a partially redacted `Event` is
/// not an `Event`.
#[test]
fn watch_answers_the_same_named_scopes_as_status() {
    use wirk_core::WorkId;

    let family = build_launch_family(&["scratch:write", "open:read", "helper:write"]);
    let estate = &family.estate;
    let socket = &family.pointer.socket;
    let embargoed_path = format!("{}/embargoed.md", family.closed_checkout);

    record_launch_events(
        socket,
        &family.parent.work_id,
        &family.parent.run_id,
        wirk_core::ActorSelection {
            model: Some("opus".to_string()),
            effort: Some("medium".to_string()),
            args: vec!["--add-dir".to_string(), embargoed_path.clone()],
        },
        &format!("{}/.herdr/embargo-session.sock", family.closed_checkout),
        &["claude", "--add-dir", &embargoed_path],
    );

    // The narrowed child may not stream its parent's raw journal.
    // `client::watch` always dials; a refusal arrives as the stream's
    // first and only item (`WatchLines`' own doc), so the refusal is
    // read off the stream rather than off the dial.
    let message = watch_refusal(
        socket,
        wirkd::WatchPayload::scoped(
            WorkId(family.parent.work_id.clone()),
            WorkId(family.child.work_id.clone()),
        ),
    );
    assert!(
        message.contains("InadmissibleEvidence"),
        "the refusal must be the scope one: {message}"
    );
    for needle in family.closed_secrets_slice() {
        assert!(
            !message.contains(needle),
            "the watch refusal disclosed {needle:?}: {message}"
        );
    }

    // A Work streaming its own journal is always admitted — this is
    // exactly what `RunLoop::drive` does, and it must be unchanged.
    let mut own = wirkd::client::watch(
        socket,
        wirkd::WatchPayload::scoped(
            WorkId(family.parent.work_id.clone()),
            WorkId(family.parent.work_id.clone()),
        ),
    )
    .expect("watch dials");
    assert!(
        own.next().expect("the replayed journal").is_ok(),
        "a Work must still stream its own journal"
    );
    drop(own);

    // And the named operator stream still sees everything.
    let mut admin = wirkd::client::watch(
        socket,
        wirkd::WatchPayload::admin(WorkId(family.parent.work_id.clone())),
    )
    .expect("watch dials");
    assert!(
        admin.next().expect("the replayed journal").is_ok(),
        "the named operator stream must still work"
    );
    drop(admin);

    // No silent unscoped default on the wire.
    let message = watch_refusal(
        socket,
        wirkd::WatchPayload {
            work_id: WorkId(family.parent.work_id.clone()),
            requester: None,
            admin: false,
        },
    );
    assert!(
        message.contains("BadRequest"),
        "an unscoped watch must be refused as a bad request: {message}"
    );

    stop_wirkd(estate, family.wirkd);
}

/// The refusal `watch` sends as its stream's first and only item.
fn watch_refusal(socket: &Path, payload: wirkd::WatchPayload) -> String {
    let mut events = wirkd::client::watch(socket, payload).expect("watch dials");
    match events.next() {
        Some(Err(err)) => err.to_string(),
        other => panic!("this watch must be refused, not streamed: {other:?}"),
    }
}

// ---- The scoped client: the requested scope, actually applied -------------
//
// The disclosure-integration review recorded four executed defects and
// one executed compatibility fact against the surface above. The guards
// below are their red: V-1 (`wirk wirkd watch --requesting-work`
// advertised and never read), V-2 (the duplicated usage token), V-4 (a
// withheld human detail rendered as an empty string) and V-5 (a scoped
// request answered by a daemon that never applied that scope, in full,
// silently).

/// **V-1, red on the pre-correction tree.** The `watch` verb's own
/// usage line offered `--requesting-work`, `wirkd_command` passed only
/// `--work`, and `wirkd_watch_command` sent `WatchPayload::admin`
/// unconditionally: an operator asking for a narrow stream of another
/// Work's journal got the whole raw journal instead. That is the same
/// "a named scope must not be silently ignored" failure the wire half
/// already refuses — and it is the half a human uses.
#[test]
fn wirkd_watch_cli_applies_the_named_requesting_work() {
    let family = build_launch_family(&["scratch:write", "open:read", "helper:write"]);
    let estate = &family.estate;
    let embargoed_path = format!("{}/embargoed.md", family.closed_checkout);

    record_launch_events(
        &family.pointer.socket,
        &family.parent.work_id,
        &family.parent.run_id,
        wirk_core::ActorSelection {
            model: Some("opus".to_string()),
            effort: Some("medium".to_string()),
            args: vec!["--add-dir".to_string(), embargoed_path.clone()],
        },
        &format!("{}/.herdr/embargo-session.sock", family.closed_checkout),
        &["claude", "--add-dir", &embargoed_path],
    );

    // `wirk wirkd watch` blocks by design (ruling 0044) — the whole
    // failure being guarded is that a refused scope keeps streaming
    // instead — so every case here is read exactly one line deep and
    // then killed. The first line *is* the answer: a refusal, or the
    // journal.
    let first_line = |args: &[&str]| -> String {
        use std::io::BufRead;
        let mut full = vec!["wirkd", "watch", "--estate", estate.to_str().unwrap()];
        full.extend_from_slice(args);
        // The operator's own stream (ruling 0117), asked for explicitly.
        let mut child = wirk_cli(&[])
            .args(&full)
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("wirk wirkd watch spawns");
        let mut line = String::new();
        let mut reader = std::io::BufReader::new(child.stdout.take().expect("piped stdout"));
        reader.read_line(&mut line).expect("one streamed line");
        let _ = child.kill();
        let _ = child.wait();
        line
    };

    // The narrowed child naming itself as the requester of its parent's
    // stream.
    let refused = first_line(&[
        "--work",
        &family.parent.work_id,
        "--requesting-work",
        &family.child.work_id,
    ]);
    assert!(
        refused.contains("refused") && refused.contains("InadmissibleEvidence"),
        "the named narrow scope must be applied, not ignored: {refused}"
    );
    assert!(
        !refused.contains("\"kind\""),
        "no journal event may be streamed to a requester the scope refuses: {refused}"
    );
    for needle in family.closed_secrets_slice() {
        assert!(
            !refused.contains(needle),
            "the refused CLI stream disclosed {needle:?}: {refused}"
        );
    }

    // The useful positives, both preserved: a Work streaming its own
    // journal under an explicit narrow scope, and the operator's
    // unscoped stream.
    let own = first_line(&[
        "--work",
        &family.parent.work_id,
        "--requesting-work",
        &family.parent.work_id,
    ]);
    assert!(
        own.contains(&family.parent.work_id) && own.contains("kind"),
        "a Work must still stream its own journal under its own scope: {own}"
    );
    let operator = first_line(&["--work", &family.parent.work_id]);
    assert!(
        operator.contains(&family.parent.work_id) && operator.contains("kind"),
        "the named operator stream must be unchanged: {operator}"
    );

    stop_wirkd(estate, family.wirkd);
}

/// **V-2, red on the pre-correction tree.** `wirk wirkd`'s usage line
/// printed `[--requesting-work <id>]` twice.
#[test]
fn wirkd_usage_names_the_scope_flag_once() {
    let output = wirk_cli(&[])
        .args(["wirkd"])
        .output()
        .expect("wirk wirkd runs");
    let usage = String::from_utf8_lossy(&output.stderr).to_string();
    assert_eq!(
        usage.matches("--requesting-work").count(),
        1,
        "the usage line must name the scope flag once: {usage}"
    );
}

/// **V-4, red on the pre-correction tree.** A withheld `needs_input`
/// detail reached the human line as `as_str().unwrap_or("")` and
/// printed as nothing at all after the colon — indistinguishable from a
/// Run whose detail was never recorded. That is exactly the
/// absent/unrecorded/withheld conflation F-D corrects, surviving in the
/// operator surface.
#[test]
fn the_scoped_human_status_says_withheld_rather_than_nothing() {
    let family = build_launch_family(&["scratch:write", "open:read", "helper:write"]);
    let estate = &family.estate;
    let socket = &family.pointer.socket;
    let embargoed_path = format!("{}/embargoed.md", family.closed_checkout);

    // Content-absent launch first (a lifecycle observation that
    // precedes a launch is refused), then the pane capture that puts
    // the Work in `NeedsInput` with a detail only the parent's own
    // bindings admit.
    record_launch_events(
        socket,
        &family.parent.work_id,
        &family.parent.run_id,
        wirk_core::ActorSelection::default(),
        "",
        &[],
    );
    record_event(
        socket,
        &family.parent.work_id,
        &family.parent.run_id,
        EventKind::LifecycleObserved {
            status: "Blocked".to_string(),
            detail: Some(format!(
                "the actor is waiting on its pane %1:\ncat {embargoed_path}\nembargomarker: the embargoed basis"
            )),
        },
    );
    let run_status = |args: &[&str]| {
        let mut full = vec!["work", "status", "--estate", estate.to_str().unwrap()];
        full.extend_from_slice(args);
        // The operator's own read (ruling 0117): asked for explicitly,
        // never inherited from whatever context runs the suite.
        let output = wirk_cli(&[])
            .args(&full)
            .output()
            .expect("wirk work status runs");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    };

    let scoped = run_status(&[
        "--work",
        &family.parent.work_id,
        "--requesting-work",
        &family.child.work_id,
    ]);
    assert!(
        scoped.contains("needs_input blocked: withheld"),
        "a withheld detail must read as withheld, not as nothing: {scoped}"
    );
    for needle in family.closed_secrets_slice() {
        assert!(
            !scoped.contains(needle),
            "the scoped human line disclosed {needle:?}: {scoped}"
        );
    }

    // The operator's own read is unchanged: it still shows the detail.
    let admin = run_status(&["--work", &family.parent.work_id]);
    assert!(
        admin.contains("needs_input blocked: the actor is waiting on its pane"),
        "the administrative line must still carry the recorded detail: {admin}"
    );

    // The same three facts on the JSON wire, which is what the human
    // line renders: withheld is the explicit marker object, never an
    // empty string, and a Work that has never been `NeedsInput` carries
    // no `needs_input` key at all — absent, not empty.
    let scoped_json = wirkd::client::status(
        socket,
        wirkd::StatusPayload::scoped(
            wirk_core::WorkId(family.parent.work_id.clone()),
            wirk_core::WorkId(family.child.work_id.clone()),
        ),
    )
    .expect("the scoped read is answered");
    match scoped_json {
        wirkd::Reply::Ok { result, .. } => {
            assert_eq!(
                result["needs_input"]["detail"],
                serde_json::json!({"withheld": true}),
                "the wire must mark the detail withheld, not blank it: {result}"
            );
            assert_eq!(
                result["needs_input"]["reason"].as_str(),
                Some("blocked"),
                "why the Work waits is journal identity and survives: {result}"
            );
        }
        other => panic!("the scoped read must be answered: {other:?}"),
    }
    let absent = run_status(&["--work", &family.child.work_id]);
    assert!(
        absent.contains("needs_input -"),
        "a Work that never needed input reads absent, not withheld: {absent}"
    );

    stop_wirkd(estate, family.wirkd);
}

/// **V-5 green, against the real daemon.** A scoped `status` names the
/// scope it applied, and a scoped `watch` acknowledges it before its
/// first event line — while the administrative stream stays exactly
/// what it was, first line an `Event`, so an older client reading this
/// daemon's operator stream is unaffected.
#[test]
fn the_daemon_states_the_scope_it_applied() {
    use wirk_core::WorkId;

    let family = build_launch_family(&["scratch:write", "open:read", "helper:write"]);
    let estate = &family.estate;
    let socket = &family.pointer.socket;

    let reply = wirkd::client::status(
        socket,
        wirkd::StatusPayload::scoped(
            WorkId(family.parent.work_id.clone()),
            WorkId(family.parent.work_id.clone()),
        ),
    )
    .expect("a scoped status the daemon applied is accepted");
    match reply {
        wirkd::Reply::Ok { result, .. } => assert_eq!(
            result["scope"].as_str(),
            Some("requester"),
            "the applied scope is the reply's own field"
        ),
        other => panic!("the scoped status must be answered: {other:?}"),
    }

    let mut own = wirkd::client::watch(
        socket,
        wirkd::WatchPayload::scoped(
            WorkId(family.parent.work_id.clone()),
            WorkId(family.parent.work_id.clone()),
        ),
    )
    .expect("watch dials");
    assert!(
        own.next().expect("the replayed journal").is_ok(),
        "the acknowledgment is consumed by the client, not handed on as an event"
    );
    drop(own);

    let mut admin = wirkd::client::watch(
        socket,
        wirkd::WatchPayload::admin(WorkId(family.parent.work_id.clone())),
    )
    .expect("watch dials");
    assert!(
        admin.next().expect("the replayed journal").is_ok(),
        "the administrative stream must carry no acknowledgment line at all"
    );
    drop(admin);

    stop_wirkd(estate, family.wirkd);
}

/// **V-5, red on the pre-correction tree, both verbs.** The scope gate
/// lives in the daemon; nothing bound the *client's* requested scope to
/// the answer it actually got. A daemon that predates the gate ignores
/// the unknown `requester` field and answers a narrowed `status` in
/// full, exit 0, and streams a narrowed `watch` as the whole raw
/// journal — silent scope loss, at the moment a consultation surface is
/// consumed.
///
/// The counterparty here is a real Unix socket speaking the real
/// protocol with the pre-gate reply shape, because that is the only way
/// to exercise the old *answer* inside the suite; it pins the contract,
/// and it is not the proof. The executed proof is the real old binary,
/// real old daemon, real new client control recorded in
/// `SCOPED-CLIENT-HANDOFF.md` (ruling 0040).
#[test]
fn a_scope_the_daemon_never_applied_is_refused_unread() {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use wirk_core::{Event, EventId, EventKind, RunId, Timestamp, WorkId};

    // One pre-gate answer per connection: an old daemon's `status`
    // reply carries every field and no `scope`, and an old daemon's
    // `watch` stream opens straight onto the journal.
    let secret = "embargomarker: the embargoed basis";
    let old_status = serde_json::json!({
        "ok": true,
        "result": {
            "state": "active",
            "current_waypoint": "outer/leaf-a",
            "needs_input": {"run": "run-1", "reason": "blocked", "detail": secret},
        }
    });
    let old_event = Event {
        id: EventId("event-1".to_string()),
        work: WorkId("work-1".to_string()),
        run: Some(RunId("run-1".to_string())),
        at: Timestamp(0),
        kind: EventKind::LifecycleObserved {
            status: "Blocked".to_string(),
            detail: Some(secret.to_string()),
        },
    };

    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("old.sock");
    let listener = UnixListener::bind(&socket_path).expect("stub socket binds");
    let old_status_line = serde_json::to_string(&old_status).unwrap();
    let old_event_line = serde_json::to_string(&old_event).unwrap();
    let server = std::thread::spawn(move || {
        for stream in listener.incoming().take(2) {
            let stream = stream.expect("stub connection");
            let mut request = String::new();
            BufReader::new(&stream)
                .read_line(&mut request)
                .expect("the request line");
            let mut writer = &stream;
            let line = if request.contains("\"watch\"") {
                &old_event_line
            } else {
                &old_status_line
            };
            let _ = writeln!(writer, "{line}");
            let _ = writer.flush();
            // The old `watch` connection stays open exactly as the old
            // daemon's does; the client must refuse on the first line
            // rather than wait for a marker that is never coming.
            if request.contains("\"watch\"") {
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
        }
    });

    let err = wirkd::client::status(
        &socket_path,
        wirkd::StatusPayload::scoped(WorkId("work-1".to_string()), WorkId("work-1".to_string())),
    )
    .expect_err("a status reply naming no applied scope must be refused");
    let message = err.to_string();
    assert!(
        message.contains("did not apply the requested scope"),
        "the refusal must name what actually failed: {message}"
    );
    assert!(
        !message.contains(secret) && !message.contains("blocked"),
        "the unscoped answer must not be presented, not even as a diagnostic: {message}"
    );

    let mut events = wirkd::client::watch(
        &socket_path,
        wirkd::WatchPayload::scoped(WorkId("work-1".to_string()), WorkId("work-1".to_string())),
    )
    .expect("watch dials");
    let first = events.next().expect("the stream answers");
    let message = match first {
        Err(err) => err.to_string(),
        Ok(event) => panic!("an unacknowledged stream must not yield an event: {event:?}"),
    };
    assert!(
        message.contains("did not apply the requested scope"),
        "the stream refusal must name what actually failed: {message}"
    );
    assert!(
        !message.contains(secret),
        "the refused stream disclosed the journal it refused: {message}"
    );
    assert!(
        events.next().is_none(),
        "nothing may be read off a stream whose scope was never established"
    );
    drop(events);
    let _ = server.join();
}

/// The other half of V-5, and the reason this is a *contract* check and
/// not a version ban: an explicitly administrative request asks for the
/// whole answer, so the same old daemon still serves it, and the client
/// still reads it. Ordinary operator use across versions is preserved;
/// only a request whose narrow scope was silently dropped is refused.
#[test]
fn an_administrative_request_is_not_a_scope_the_daemon_must_prove() {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use wirk_core::WorkId;

    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("old.sock");
    let listener = UnixListener::bind(&socket_path).expect("stub socket binds");
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().expect("stub connection");
        let mut request = String::new();
        BufReader::new(&stream)
            .read_line(&mut request)
            .expect("the request line");
        let mut writer = &stream;
        let old_reply = serde_json::json!({"ok": true, "result": {"state": "active"}});
        let _ = writeln!(writer, "{old_reply}");
        let _ = writer.flush();
    });

    let reply = wirkd::client::status(
        &socket_path,
        wirkd::StatusPayload::admin(WorkId("work-1".to_string())),
    )
    .expect("an administrative read is answered by either daemon");
    match reply {
        wirkd::Reply::Ok { result, .. } => {
            assert_eq!(result["state"].as_str(), Some("active"));
        }
        other => panic!("the administrative read must be answered: {other:?}"),
    }
    let _ = server.join();
}

// ---- The acknowledgment itself: established, and about this Work ---------
//
// The independent review of the correction above (`loop-b-scoped-
// client-verify/VERDICT.md`, F-1 and F-2) executed three states the
// scoped gate still accepted or still echoed:
//
//  - a scoped stream whose counterparty closed **without** acknowledging
//    exited 0 with no output — `WatchLines::next` propagated the EOF
//    through `?` as a clean end of stream, so a scripted consumer read
//    "this narrow stream ran and ended" for a scope that was never
//    established;
//  - the acknowledgment's `work_id` — the Work the answer is *about*,
//    which is not the requesting Work — was written by the daemon and
//    never compared to the Work the caller asked about, on either verb;
//  - a scoped `status` whose reply line is not a `Reply` at all came
//    back through `client::call` as `MalformedReply`, whose text
//    embeds the rejected line verbatim — the one path on which a
//    refused scoped answer could still be presented, in a diagnostic.
//
// The counterparties below are real Unix sockets speaking the real
// protocol, because that is the only way to hold one of those response
// shapes still inside the suite. They pin the shape; they are not the
// proof (ruling 0040). The executed proof is the real old daemon, real
// new daemon and real CLI record in `ACK-HANDOFF.md`.

/// **F-1, red before this correction.** A scoped stream whose
/// counterparty accepts the connection and closes it without
/// acknowledging: the client reported a clean end (`None`, exit 0),
/// indistinguishable from a narrow stream that really ran and ended.
/// The daemon reaches this state itself — `write_scope_ack` failing
/// makes `handle_watch_connection` return, closing the connection with
/// no line at all. The administrative control on the same counterparty
/// keeps its own meaning: an administrative stream that ends has ended.
#[test]
fn a_scoped_stream_that_ends_before_acknowledging_is_a_failure_not_a_clean_end() {
    use std::io::{BufRead, BufReader};
    use std::os::unix::net::UnixListener;
    use wirk_core::WorkId;

    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("eof.sock");
    let listener = UnixListener::bind(&socket_path).expect("stub socket binds");
    let server = std::thread::spawn(move || {
        for stream in listener.incoming().take(2) {
            let stream = stream.expect("stub connection");
            let mut request = String::new();
            BufReader::new(&stream)
                .read_line(&mut request)
                .expect("the request line");
            // Closed with no line written at all.
            drop(stream);
        }
    });

    let mut events = wirkd::client::watch(
        &socket_path,
        wirkd::WatchPayload::scoped(WorkId("work-1".to_string()), WorkId("work-1".to_string())),
    )
    .expect("watch dials");
    let message = match events
        .next()
        .expect("an unacknowledged stream must report a failure, not end silently")
    {
        Err(err) => err.to_string(),
        Ok(event) => panic!("an unacknowledged stream must not yield an event: {event:?}"),
    };
    assert!(
        message.contains("did not apply the requested scope")
            && message.contains("ended before acknowledging"),
        "the refusal must name what was actually observed: {message}"
    );
    assert!(
        events.next().is_none(),
        "nothing may be read off a stream whose scope was never established"
    );
    drop(events);

    let mut admin = wirkd::client::watch(
        &socket_path,
        wirkd::WatchPayload::admin(WorkId("work-1".to_string())),
    )
    .expect("watch dials");
    assert!(
        admin.next().is_none(),
        "an administrative stream that ends has ended, unchanged"
    );
    drop(admin);
    let _ = server.join();
}

/// **F-2, red before this correction, on `watch`.** The acknowledgment
/// names the Work the stream is about. A counterparty acknowledging
/// *another* Work was accepted and its stream consumed, so the answer a
/// caller read was never bound to the request it made. The target Work
/// is not the requesting Work: this compares the acknowledgment to
/// `--work`, the Work asked about, and a Work watching itself (where
/// the two coincide) keeps working.
#[test]
fn a_stream_acknowledged_for_another_work_is_refused_unread() {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use wirk_core::{Event, EventId, EventKind, RunId, Timestamp, WorkId};

    let secret = "embargomarker: the embargoed basis";
    let event_line = serde_json::to_string(&Event {
        id: EventId("event-1".to_string()),
        work: WorkId("work-1".to_string()),
        run: Some(RunId("run-1".to_string())),
        at: Timestamp(0),
        kind: EventKind::LifecycleObserved {
            status: "Blocked".to_string(),
            detail: Some(secret.to_string()),
        },
    })
    .unwrap();

    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("ack.sock");
    let listener = UnixListener::bind(&socket_path).expect("stub socket binds");
    let mistaken = serde_json::json!({"ok": true, "result": {"scope": "requester", "work_id": "work-somewhere-else"}}).to_string();
    let correct =
        serde_json::json!({"ok": true, "result": {"scope": "requester", "work_id": "work-1"}})
            .to_string();
    let streamed = event_line.clone();
    let server = std::thread::spawn(move || {
        for (index, stream) in listener.incoming().take(2).enumerate() {
            let stream = stream.expect("stub connection");
            let mut request = String::new();
            BufReader::new(&stream)
                .read_line(&mut request)
                .expect("the request line");
            let mut writer = &stream;
            let ack = if index == 0 { &mistaken } else { &correct };
            let _ = writeln!(writer, "{ack}");
            let _ = writeln!(writer, "{streamed}");
            let _ = writer.flush();
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    });

    let mut events = wirkd::client::watch(
        &socket_path,
        wirkd::WatchPayload::scoped(WorkId("work-1".to_string()), WorkId("work-1".to_string())),
    )
    .expect("watch dials");
    let message = match events.next().expect("the stream answers") {
        Err(err) => err.to_string(),
        Ok(event) => panic!("a stream acknowledged for another Work must not yield: {event:?}"),
    };
    assert!(
        message.contains("did not apply the requested scope") && message.contains("another work"),
        "the refusal must name what was actually observed: {message}"
    );
    assert!(
        !message.contains(secret) && !message.contains("work-somewhere-else"),
        "the rejected answer must not travel in the diagnostic: {message}"
    );
    assert!(
        events.next().is_none(),
        "nothing may be read off a stream acknowledged for another Work"
    );
    drop(events);

    // The positive on the identical path: the acknowledgment names the
    // Work that was asked about, so the stream is consumed normally.
    let mut good = wirkd::client::watch(
        &socket_path,
        wirkd::WatchPayload::scoped(WorkId("work-1".to_string()), WorkId("work-1".to_string())),
    )
    .expect("watch dials");
    assert!(
        good.next().expect("the stream answers").is_ok(),
        "an acknowledgment naming the requested Work must be accepted"
    );
    drop(good);
    let _ = server.join();
}

/// **F-2 on `status`, and the `MalformedReply` echo, both red before
/// this correction.** A scoped `status` reply is bound to the Work it
/// answers about as well as to the scope it applied; and a scoped reply
/// line that is not a `Reply` at all is refused without its content —
/// `client::call` formats the rejected line into `MalformedReply`, and
/// that path was reachable with a scoped request. The administrative
/// read is unchanged in both respects: it asked for the whole answer.
#[test]
fn a_scoped_status_is_bound_to_its_work_and_never_echoes_a_rejected_reply() {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use wirk_core::WorkId;

    let secret = "embargomarker: the embargoed basis";
    let mistaken = serde_json::json!({"ok": true, "result": {
        "scope": "requester",
        "work_id": "work-somewhere-else",
        "state": "active",
        "needs_input": {"run": "run-1", "reason": "blocked", "detail": secret},
    }})
    .to_string();
    let unnamed = serde_json::json!({"ok": true, "result": {
        "scope": "requester",
        "state": "active",
        "needs_input": {"run": "run-1", "reason": "blocked", "detail": secret},
    }})
    .to_string();
    // A sentinel malformed reply: a truncated line whose surviving
    // prefix carries the content the scoped read must not be shown.
    let truncated = format!(
        "{{\"ok\":true,\"result\":{{\"scope\":\"requester\",\"work_id\":\"work-1\",\"needs_input\":{{\"detail\":\"{secret}\"}}"
    );
    let good = serde_json::json!({"ok": true, "result": {
        "scope": "requester",
        "work_id": "work-1",
        "state": "active",
    }})
    .to_string();

    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("status.sock");
    let listener = UnixListener::bind(&socket_path).expect("stub socket binds");
    let replies = [mistaken, unnamed, truncated.clone(), truncated, good];
    let server = std::thread::spawn(move || {
        for (index, stream) in listener.incoming().take(replies.len()).enumerate() {
            let stream = stream.expect("stub connection");
            let mut request = String::new();
            BufReader::new(&stream)
                .read_line(&mut request)
                .expect("the request line");
            let mut writer = &stream;
            let _ = writeln!(writer, "{}", replies[index]);
            let _ = writer.flush();
        }
    });

    let scoped =
        || wirkd::StatusPayload::scoped(WorkId("work-1".to_string()), WorkId("work-1".to_string()));

    let message = wirkd::client::status(&socket_path, scoped())
        .expect_err("a reply about another Work must be refused")
        .to_string();
    assert!(
        message.contains("did not apply the requested scope") && message.contains("another work"),
        "the refusal must name what was actually observed: {message}"
    );
    assert!(
        !message.contains(secret) && !message.contains("work-somewhere-else"),
        "the rejected answer must not travel in the diagnostic: {message}"
    );

    let message = wirkd::client::status(&socket_path, scoped())
        .expect_err("a reply naming no Work at all must be refused")
        .to_string();
    assert!(
        message.contains("did not apply the requested scope") && message.contains("names no work"),
        "the refusal must name what was actually observed: {message}"
    );
    assert!(
        !message.contains(secret),
        "the rejected answer must not travel in the diagnostic: {message}"
    );

    let message = wirkd::client::status(&socket_path, scoped())
        .expect_err("a reply that is not a reply must be refused")
        .to_string();
    assert!(
        message.contains("malformed"),
        "a wire violation must still read as one: {message}"
    );
    assert!(
        !message.contains(secret),
        "a rejected scoped reply must not be echoed in its own diagnostic: {message}"
    );

    // The administrative control on the identical line: the operator
    // asked for the whole answer, and its diagnostic is unchanged.
    let message = wirkd::client::status(
        &socket_path,
        wirkd::StatusPayload::admin(WorkId("work-1".to_string())),
    )
    .expect_err("a malformed reply is a transport failure either way")
    .to_string();
    assert!(
        message.contains("malformed") && message.contains(secret),
        "the administrative diagnostic must be unchanged: {message}"
    );

    let reply = wirkd::client::status(&socket_path, scoped())
        .expect("a reply naming the requested Work and the applied scope is accepted");
    match reply {
        wirkd::Reply::Ok { result, .. } => {
            assert_eq!(result["state"].as_str(), Some("active"));
        }
        other => panic!("the scoped read must be answered: {other:?}"),
    }
    let _ = server.join();
}

/// The diagnostic says what was observed. Before this correction every
/// `ScopeNotApplied` ended "the daemon and this client are not the same
/// version" — a cause the client cannot know: a closed connection, a
/// counterparty answering about another Work and a broken same-version
/// daemon all reach it. What is established is that the requested scope
/// was not applied and the answer was not presented.
#[test]
fn the_scope_refusal_states_the_observation_not_an_unproved_cause() {
    use std::io::{BufRead, BufReader};
    use std::os::unix::net::UnixListener;
    use wirk_core::WorkId;

    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("silent.sock");
    let listener = UnixListener::bind(&socket_path).expect("stub socket binds");
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().expect("stub connection");
        let mut request = String::new();
        BufReader::new(&stream)
            .read_line(&mut request)
            .expect("the request line");
        drop(stream);
    });

    let mut events = wirkd::client::watch(
        &socket_path,
        wirkd::WatchPayload::scoped(WorkId("work-1".to_string()), WorkId("work-1".to_string())),
    )
    .expect("watch dials");
    let message = match events.next().expect("the stream answers") {
        Err(err) => err.to_string(),
        Ok(event) => panic!("an unacknowledged stream must not yield an event: {event:?}"),
    };
    assert!(
        !message.contains("not the same version"),
        "the diagnostic must not assert a cause it cannot know: {message}"
    );
    assert!(
        message.contains("discarded unread"),
        "what is established — the answer was not presented — must survive: {message}"
    );
    drop(events);
    let _ = server.join();
}

/// **Green against the real daemon.** The daemon names the Work its
/// scoped answer is about, on both verbs — `status` in its reply and
/// `watch` in its acknowledgment — and a Work reading another Work on
/// its own lineage (target and requester genuinely distinct) is bound
/// to the target it asked about, not to itself. The administrative
/// reply is unchanged.
#[test]
fn the_daemon_names_the_work_its_scoped_answer_is_about() {
    use wirk_core::WorkId;

    let family = build_launch_family(&["scratch:write", "open:read", "helper:write"]);
    let estate = &family.estate;
    let socket = &family.pointer.socket;

    // Target and requester distinct: the parent asking about its own
    // child, which its lineage and bindings admit.
    let reply = wirkd::client::status(
        socket,
        wirkd::StatusPayload::scoped(
            WorkId(family.child.work_id.clone()),
            WorkId(family.parent.work_id.clone()),
        ),
    )
    .expect("a scoped status about another Work on the lineage is accepted");
    match reply {
        wirkd::Reply::Ok { result, .. } => {
            assert_eq!(
                result["work_id"].as_str(),
                Some(family.child.work_id.as_str()),
                "the reply must name the Work it is about, not the requester: {result}"
            );
            assert_eq!(result["scope"].as_str(), Some("requester"));
        }
        other => panic!("the scoped status must be answered: {other:?}"),
    }

    // The administrative reply keeps its own shape: no scope binding is
    // asked for and none is imposed.
    let reply = wirkd::client::status(
        socket,
        wirkd::StatusPayload::admin(WorkId(family.child.work_id.clone())),
    )
    .expect("the administrative read is answered");
    match reply {
        wirkd::Reply::Ok { result, .. } => {
            assert_eq!(result["scope"].as_str(), Some("administrative"));
        }
        other => panic!("the administrative status must be answered: {other:?}"),
    }

    // The stream's acknowledgment, on the same distinct pair.
    let mut events = wirkd::client::watch(
        socket,
        wirkd::WatchPayload::scoped(
            WorkId(family.child.work_id.clone()),
            WorkId(family.parent.work_id.clone()),
        ),
    )
    .expect("watch dials");
    assert!(
        events.next().expect("the replayed journal").is_ok(),
        "the acknowledgment names the watched Work and is consumed, not handed on"
    );
    drop(events);

    stop_wirkd(estate, family.wirkd);
}

// ---- work obligations: the caller checks the answer it was given -------
//
// The basis review's F1. `handle_work_obligations` emits both halves of
// the scoped acknowledgment contract — the applied `scope` and the
// `work_id` the answer is about — and `client::status` has refused a
// reply missing either since the acknowledgment review. `work
// obligations` reproduced `status`'s reply shape and reused its
// server-side lineage gate, and then went through the generic
// `client::call`, which checks neither. A reply about another Work was
// rendered as the answer to this one; a scoped reply that was not a
// reply at all was echoed back in its own diagnostic.

/// **Red before this correction, on every case below.** The scoped
/// `work obligations` door refuses, unread, a reply that names another
/// Work, names no Work, names a Work that is not even a string, or
/// names no applied scope — and refuses a line that is not a `Reply` at
/// all without quoting it. The administrative door is unchanged: it
/// asked for the whole answer, from either daemon, and its diagnostic
/// still carries the rejected line.
#[test]
fn a_scoped_work_obligations_reply_is_bound_to_its_work_and_scope() {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use wirk_core::WorkId;

    // The two values a wrongly-bound reply would disclose as if they
    // were this Work's own: the basis an operator would then admit, and
    // the authored sentence behind it.
    let sentinel_basis = "basismarker0000000000000000000000000000000000000000000000000000";
    let sentinel_proves = "provesmarker: the other work's authored obligation";
    let answer = |work_id: serde_json::Value, scoped: bool| {
        let mut result = serde_json::json!({
            "work": "work-1",
            "policy": {"state": "loaded"},
            "route": {"waypoints": 1, "declaring_obligation": 1},
            "obligations": [{
                "waypoint": "review",
                "obligation": {"id": "socket-mode-reviewed", "edition": "1", "proves": sentinel_proves},
                "basis": {"state": "available", "basis": sentinel_basis},
            }],
        });
        if scoped {
            result["scope"] = serde_json::json!("requester");
        }
        if !work_id.is_null() {
            result["work_id"] = work_id;
        }
        serde_json::json!({"ok": true, "result": result}).to_string()
    };

    // A truncated line: still a `Reply` prefix, carrying the content a
    // scoped read must not be shown even in a parse diagnostic.
    let truncated = format!(
        "{{\"ok\":true,\"result\":{{\"scope\":\"requester\",\"work_id\":\"work-1\",\"obligations\":[{{\"basis\":{{\"basis\":\"{sentinel_basis}\"}}"
    );
    let replies = [
        // 1. about another Work
        answer(serde_json::json!("work-somewhere-else"), true),
        // 2. naming no Work at all
        answer(serde_json::Value::Null, true),
        // 3. a `work_id` that is not a string — a number reads as no
        //    named Work, never as a match
        answer(serde_json::json!(1), true),
        // 4. an older daemon: the whole answer, no applied scope named
        answer(serde_json::json!("work-1"), false),
        // 5. not a `Reply` at all, scoped
        truncated.clone(),
        // 6. the same line, administrative
        truncated,
        // 7. the reply this request actually asked for
        answer(serde_json::json!("work-1"), true),
    ];

    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("obligations.sock");
    let listener = UnixListener::bind(&socket_path).expect("stub socket binds");
    let count = replies.len();
    let server = std::thread::spawn(move || {
        for (index, stream) in listener.incoming().take(count).enumerate() {
            let stream = stream.expect("stub connection");
            let mut request = String::new();
            BufReader::new(&stream)
                .read_line(&mut request)
                .expect("the request line");
            let mut writer = &stream;
            let _ = writeln!(writer, "{}", replies[index]);
            let _ = writer.flush();
        }
    });

    let payload = |admin: bool| wirkd::WorkObligationsPayload {
        work_id: WorkId("work-1".to_string()),
        waypoint: None,
        requester: (!admin).then(|| WorkId("work-2".to_string())),
        admin,
    };
    let unread = |expected: &str| {
        let message = wirkd::client::work_obligations(&socket_path, payload(false))
            .expect_err("the scoped consultation must be refused")
            .to_string();
        assert!(
            message.contains("did not apply the requested scope") && message.contains(expected),
            "the refusal must name what was actually observed: {message}"
        );
        assert!(
            !message.contains(sentinel_basis) && !message.contains(sentinel_proves),
            "the rejected answer must not travel in the diagnostic: {message}"
        );
    };

    unread("another work");
    unread("names no work");
    unread("names no work");
    unread("names no applied scope");

    let message = wirkd::client::work_obligations(&socket_path, payload(false))
        .expect_err("a reply that is not a reply must be refused")
        .to_string();
    assert!(
        message.contains("malformed"),
        "a wire violation must still read as one: {message}"
    );
    assert!(
        !message.contains(sentinel_basis),
        "a rejected scoped reply must not be echoed in its own diagnostic: {message}"
    );

    // The administrative control on the identical line.
    let message = wirkd::client::work_obligations(&socket_path, payload(true))
        .expect_err("a malformed reply is a transport failure either way")
        .to_string();
    assert!(
        message.contains("malformed") && message.contains(sentinel_basis),
        "the administrative diagnostic must be unchanged: {message}"
    );

    // The positive on the identical path.
    match wirkd::client::work_obligations(&socket_path, payload(false))
        .expect("a reply naming the requested Work and the applied scope is accepted")
    {
        wirkd::Reply::Ok { result, .. } => {
            assert_eq!(result["work_id"].as_str(), Some("work-1"));
            assert_eq!(
                result["obligations"][0]["basis"]["basis"].as_str(),
                Some(sentinel_basis),
                "an answer that established its contract is delivered whole: {result}"
            );
        }
        other => panic!("the scoped read must be answered: {other:?}"),
    }
    let _ = server.join();
}

/// **Red before this correction: the CLI is the surface that renders,
/// and it went through the unchecked generic call.** Driven end to end
/// through the real `wirk` binary against a stub daemon that answers
/// like one predating the scope gate — the whole answer, no `scope`, no
/// `work_id`. The scoped read is refused, exit 2, with no part of the
/// answer printed in either output mode; the administrative read of the
/// identical reply is unchanged.
#[test]
fn the_cli_refuses_an_obligations_answer_that_never_named_its_scope() {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;

    let sentinel_basis = "oldbasismarker00000000000000000000000000000000000000000000000000";
    let unscoped = serde_json::json!({"ok": true, "result": {
        "work": "work-1",
        "policy": {"state": "loaded"},
        "route": {"waypoints": 1, "declaring_obligation": 1},
        "obligations": [{
            "waypoint": "review",
            "waypoint_kind": "actor",
            "obligation": {"id": "socket-mode-reviewed", "edition": "1"},
            "reservation": {"state": "reserved", "world_hash": "w-1"},
            "mechanism": {"kind": "actor_review", "present": true},
            "basis": {"state": "available", "basis": sentinel_basis},
            "admission": {"state": "admitted"},
            "findings": [],
        }],
    }})
    .to_string();

    let dir = tempfile::tempdir().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(estate.join(".wirk")).unwrap();
    let socket_path = dir.path().join("old.sock");
    let listener = UnixListener::bind(&socket_path).expect("stub socket binds");
    fs::write(
        estate.join(".wirk").join("wirkd.json"),
        serde_json::to_vec(&WirkdPointer {
            schema: "wirkd-pointer/1".to_string(),
            socket: socket_path.clone(),
            pid: std::process::id(),
            protocol_version: 1,
        })
        .unwrap(),
    )
    .unwrap();

    // Three connections: the two scoped reads below and the one
    // administrative control. The stub serves exactly those and exits,
    // so `join` cannot block.
    let server = std::thread::spawn(move || {
        for stream in listener.incoming().take(3) {
            let stream = stream.expect("stub connection");
            let mut request = String::new();
            BufReader::new(&stream)
                .read_line(&mut request)
                .expect("the request line");
            let mut writer = &stream;
            let _ = writeln!(writer, "{unscoped}");
            let _ = writer.flush();
        }
    });

    for json in [false, true] {
        let mut args = vec![
            "work",
            "obligations",
            "--estate",
            estate.to_str().unwrap(),
            "--work",
            "work-1",
            "--requesting-work",
            "work-2",
        ];
        if json {
            args.push("--json");
        }
        let output = Command::new(wirk_bin())
            .args(&args)
            .output()
            .expect("wirk work obligations runs");
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        assert_eq!(
            output.status.code(),
            Some(2),
            "a scoped read whose scope was never applied must fail (json={json}): {stdout} {stderr}"
        );
        assert!(
            stderr.contains("did not apply the requested scope"),
            "the operator is told what was observed (json={json}): {stderr}"
        );
        assert!(
            !stdout.contains(sentinel_basis) && !stderr.contains(sentinel_basis),
            "no part of the unscoped answer may be rendered (json={json}): {stdout} {stderr}"
        );
    }

    // The administrative read of the identical reply is unchanged: it
    // asked for the whole answer and gets it, from either daemon.
    let output = Command::new(wirk_bin())
        .args([
            "work",
            "obligations",
            "--estate",
            estate.to_str().unwrap(),
            "--work",
            "work-1",
            "--admin",
            "--json",
        ])
        .output()
        .expect("wirk work obligations runs");
    assert_eq!(output.status.code(), Some(0));
    assert!(
        String::from_utf8_lossy(&output.stdout).contains(sentinel_basis),
        "the administrative read is unchanged"
    );
    let _ = server.join();
}

/// **Green against the real daemon, on the three scopes an operator and
/// a Work actually use**: a Work consulting itself, a parent consulting
/// its child on its own lineage (target and requester genuinely
/// distinct), and the administrative read. Each answer establishes the
/// contract the door now checks, so each is delivered — the check is a
/// gate on a wrong answer, not a new refusal of right ones.
#[test]
fn the_daemon_names_the_work_each_scoped_obligations_answer_is_about() {
    let family = build_family(&["open:read", "helper:write"]);
    let estate = &family.estate;

    for (target, requester) in [
        (&family.parent.work_id, Some(&family.parent.work_id)),
        (&family.child.work_id, Some(&family.parent.work_id)),
        (&family.parent.work_id, None),
    ] {
        let args: Vec<&str> = match requester {
            Some(requester) => vec!["--work", target, "--requesting-work", requester],
            None => vec!["--work", target, "--admin"],
        };
        let (code, reply, stderr) = obligations_cli(estate, &args);
        assert_eq!(code, Some(0), "{args:?}: {stderr}");
        match requester {
            Some(_) => {
                assert_eq!(reply["scope"].as_str(), Some("requester"), "{args:?}");
                assert_eq!(
                    reply["work_id"].as_str(),
                    Some(target.as_str()),
                    "the reply names the Work it is about, and the caller checks it: {reply}"
                );
            }
            None => {
                assert_eq!(reply["scope"].as_str(), Some("administrative"), "{args:?}");
                assert!(reply.get("work_id").is_none(), "{reply}");
            }
        }
        assert!(reply["route"]["waypoints"].as_u64().unwrap() > 0, "{reply}");
    }

    let Family { estate, wirkd, .. } = family;
    stop_wirkd(&estate, wirkd);
}

// ---- work obligations: explicit scope, and a denial that names nothing --
//
// `loop-b-basis-access`. The new inspection surface is held to the
// identical boundary `status`, `finding list` and `atlas findings`
// already draw: one of two *named* scopes and never a silent unscoped
// default; an off-lineage requester refused by the same rule and told
// nothing; and, on the lineage, the Route-authored content half withheld
// whole from a requester whose own bindings do not cover the reporting
// Work's, exactly as `settlement_json_scoped` withholds `proves` and the
// proof half of a settled check.

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

/// The administrative read is unscoped and complete; a narrowed child on
/// the lineage gets identity and the admission answer with the authored
/// content withheld; an unrelated Work is refused and learns nothing at
/// all — not the alias, not the path, not the recipe, not the sentence.
#[test]
fn work_obligations_scope_is_named_and_a_denied_requester_learns_nothing() {
    let family = build_family(&["open:read", "helper:write"]);
    let estate = &family.estate;
    let parent = &family.parent.work_id;

    // Administrative: the whole authored obligation.
    let (code, admin, stderr) = obligations_cli(estate, &["--work", parent, "--admin"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(admin["scope"].as_str().unwrap(), "administrative");
    assert!(
        admin.get("disclosure").is_none(),
        "the administrative reply asked for no scope and is bound to none: {admin}"
    );
    let admin_entry = admin["obligations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["waypoint"].as_str() == Some("outer/leaf-a"))
        .unwrap_or_else(|| panic!("the container's own leaf declares an obligation: {admin}"));
    assert_eq!(
        admin_entry["waypoint_kind"].as_str().unwrap(),
        "deterministic"
    );
    assert_eq!(
        admin_entry["obligation"]["id"].as_str().unwrap(),
        "a-produced"
    );
    assert!(admin_entry["obligation"]["proves"].is_string());
    let disclosed_basis = admin_entry["basis"]["basis"].as_str().unwrap().to_string();
    assert_eq!(
        disclosed_basis,
        obligation_basis_for(estate, parent, "outer/leaf-a"),
        "the canonical basis, for the Deterministic class too"
    );

    // On the lineage, bindings not covering the parent's: identity and
    // the admission answer survive, the authored content is withheld.
    let (code, scoped, stderr) = obligations_cli(
        estate,
        &["--work", parent, "--requesting-work", &family.child.work_id],
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(scoped["scope"].as_str().unwrap(), "requester");
    assert_eq!(scoped["work_id"].as_str().unwrap(), parent.as_str());
    assert!(
        scoped["disclosure"]["withheld"].as_u64().unwrap() > 0,
        "the count is reported: {scoped}"
    );
    let scoped_entry = scoped["obligations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["waypoint"].as_str() == Some("outer/leaf-a"))
        .unwrap();
    assert_eq!(
        scoped_entry["obligation"]["id"].as_str().unwrap(),
        "a-produced",
        "the obligation *name* is journal identity and stays"
    );
    assert_eq!(
        scoped_entry["obligation"]["proves"],
        serde_json::json!({"withheld": true}),
        "the authored sentence is withheld whole, as it is on a settled record"
    );
    assert_eq!(
        scoped_entry["obligation"]["outputs"],
        serde_json::json!({"withheld": true})
    );
    assert_eq!(
        scoped_entry["basis"]["basis"].as_str().unwrap(),
        disclosed_basis,
        "the content address is the same class of value as `status`'s own world_hash"
    );
    assert!(scoped_entry["admission"]["state"].is_string());

    // Off the lineage: refused by the same rule `status` applies, and
    // the refusal names nothing.
    let (code, denied, stderr) = obligations_cli(
        estate,
        &[
            "--work",
            parent,
            "--requesting-work",
            &family.unrelated.work_id,
        ],
    );
    assert_eq!(code, Some(2), "{stderr}");
    assert!(
        stderr.contains("not the requesting work's own journal or its parent/child lineage"),
        "{stderr}"
    );
    let mut needles: Vec<&str> = family.closed_secrets.iter().map(String::as_str).collect();
    needles.push("a-produced");
    needles.push(disclosed_basis.as_str());
    needles.push("outer/leaf-a");
    assert_discloses_nothing(
        "an off-lineage work obligations read",
        &denied,
        &stderr,
        &needles,
    );

    // Neither scope, and both, are refused rather than defaulting.
    let (code, _, _) = obligations_cli(estate, &["--work", parent]);
    assert_eq!(code, Some(2));
    let (code, _, _) = obligations_cli(
        estate,
        &[
            "--work",
            parent,
            "--admin",
            "--requesting-work",
            &family.child.work_id,
        ],
    );
    assert_eq!(code, Some(2));

    let Family { estate, wirkd, .. } = family;
    stop_wirkd(&estate, wirkd);
}

// ---- 12. Estate publication off the lineage --------------------------

/// The other half of `LATER-DISCOVERY-ADJUDICATION.md`, on the family
/// this file already builds and against the *Deterministic* settlement
/// shape: the proof's artifact receipts are read out of the producing
/// parent's own checkout, and the parent additionally holds `embargo`,
/// which nothing in the proof names.
///
/// Four requesters, one settled publication:
///
/// | Requester | Bindings | Route to the row |
/// |---|---|---|
/// | the parent | everything | its own |
/// | the narrowed child | no `embargo` | lineage — unchanged, receipt without sources |
/// | a `peer`, no relation, every one of the parent's bindings | publication |
/// | a `neighbour`, no relation, everything the *proof* names but not `embargo` | none |
/// | `unrelated`, holding neither | none |
///
/// `neighbour` is the case the adjudication insists on: admitting every
/// source the settlement's own evidence names is deliberately not
/// enough, because the row carries the producer's authored prose too.
#[test]
fn an_off_lineage_settled_publication_needs_the_producers_own_admission() {
    let family = build_family(&["scratch:write", "open:read", "helper:write"]);
    let estate = &family.estate;

    // Two later Works with no relation to the parent at all.
    let peer_repo = family.dir.path().join("peer-repo");
    init_repo(&peer_repo);
    let peer = submit(
        estate,
        "wa_simple_leaf",
        &peer_repo,
        &[
            "embargo:write",
            "open:read",
            "helper:write",
            "scratch:write",
        ],
        None,
    )
    .unwrap();
    let neighbour_repo = family.dir.path().join("neighbour-repo");
    init_repo(&neighbour_repo);
    let neighbour = submit(
        estate,
        "wa_simple_leaf",
        &neighbour_repo,
        &["open:read", "helper:write", "scratch:write"],
        None,
    )
    .unwrap();

    // P3 execution-recovery item 1: the parent's own Deterministic
    // leaf now runs in this Work's own worktree, not the caller's
    // shared --repo-path checkout.
    write_file(
        &family.estate.join("worktrees").join(&family.parent.work_id),
        "a.md",
        "a\n",
    );
    claim_ok(
        estate,
        &family.parent.work_id,
        &family.parent.run_id,
        "a.md=a.md",
    );
    let basis = obligation_basis_for(estate, &family.parent.work_id, "outer/leaf-a");
    write_policy_admitting(estate, "a-produced", "1", &basis);
    let claim_event = journal_events(estate, &family.parent.work_id)
        .into_iter()
        .find_map(|event| {
            matches!(&event.kind, EventKind::ClaimRecorded { .. }).then_some(event.id.0)
        })
        .expect("the parent's own ClaimRecorded event");
    let evidence = format!("work/{}/event/{claim_event}", family.parent.work_id);
    let (code, raised, stderr) = raise_cli(
        estate,
        &family.parent.work_id,
        &family.parent.run_id,
        &[
            "--kind",
            "verified_outcome",
            "--scope",
            "estate_local",
            "--claim",
            "outer/leaf-a produced a.md",
            "--evidence",
            &evidence,
            "--obligation",
            "a-produced@1",
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let settled_id = raised["id"].as_str().unwrap().to_string();
    let (code, settled, stderr) =
        finding_cli(estate, &["settle", "--finding", &settled_id, "--admin"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(settled["settled"].is_object(), "{settled}");
    let (ok, _, err) = atlas(estate, &["findings", "--rebuild", "--admin"]);
    assert!(ok, "{err}");

    let index = |requester: &str| -> serde_json::Value {
        let (ok, reply, err) = atlas(estate, &["findings", "--requesting-work", requester]);
        assert!(ok, "{err}");
        reply
    };
    let has_row = |reply: &serde_json::Value| -> bool {
        reply["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["finding"]["id"].as_str() == Some(settled_id.as_str()))
    };

    // The lineage positives, unchanged: the parent sees it whole, the
    // narrowed child sees the receipt with its source half withheld.
    let own = index(&family.parent.work_id);
    assert!(has_row(&own), "{own}");
    assert!(!own["rows"][0]["settlement"]["check"]["artifacts"].is_null());
    let child = index(&family.child.work_id);
    assert!(
        has_row(&child),
        "a narrowed child's own lineage route is untouched: {child}"
    );
    assert_eq!(
        child["rows"][0]["settlement"]["proves"],
        serde_json::json!({"withheld": true}),
        "and it still gets the receipt without the sources: {child}"
    );
    assert!(child["disclosure"]["withheld"].as_u64().unwrap() >= 1);

    // The publication positive: no relation at all, every binding the
    // producer holds, the whole row and nothing withheld.
    let peer_reply = index(&peer.work_id);
    assert!(
        has_row(&peer_reply),
        "an independent Work admitting the producer's own sources must discover the publication: {peer_reply}"
    );
    assert_eq!(peer_reply["disclosure"]["withheld"].as_u64().unwrap(), 0);
    assert_eq!(peer_reply["disclosure"]["off_lineage"].as_u64().unwrap(), 0);
    assert!(
        !peer_reply["rows"][0]["settlement"]["check"]["artifacts"].is_null(),
        "a published row is admitted whole or not at all: {peer_reply}"
    );

    // The publication negatives. `neighbour` holds every source the
    // proof itself names and still may not have the row, because the
    // producer could read `embargo` and the row carries its prose.
    for (label, requester) in [
        (
            "a neighbour without the producer's embargo binding",
            &neighbour,
        ),
        ("an unrelated work", &family.unrelated),
    ] {
        let reply = index(&requester.work_id);
        assert!(
            !has_row(&reply),
            "{label} must not receive the publication: {reply}"
        );
        assert_eq!(
            reply["disclosure"]["off_lineage"].as_u64().unwrap(),
            1,
            "{label} learns a count and nothing else: {reply}"
        );
        assert_eq!(reply["disclosure"]["withheld"].as_u64().unwrap(), 0);
        assert_discloses_nothing(label, &reply, "", &family.closed_secrets_slice());
        assert_discloses_nothing(label, &reply, "", &[settled_id.as_str(), "a-produced"]);
    }

    // Discovery is not consultation: the journal boundary is exactly
    // where it was for every one of them.
    for requester in [&peer, &neighbour] {
        let (code, reply, stderr) = finding_cli(
            estate,
            &[
                "list",
                "--requesting-work",
                &requester.work_id,
                "--work",
                &family.parent.work_id,
            ],
        );
        assert_eq!(
            code,
            Some(2),
            "publication never opens the producer's journal: {reply} {stderr}"
        );
    }

    stop_wirkd(estate, family.wirkd);
}

// ---- 12. A finding relation is a reference, not a second way in --------

/// `NATIVE-CHAIN-ADJUDICATION.md` G3: a Work could not say it disagreed
/// with another Work's finding at all. `EvidenceRef::Finding` is the
/// relation that lets it, and the first thing that must be true of a new
/// reference kind is that it inherits every boundary the old ones have.
///
/// One relation, read four ways:
///
/// - The parent, which holds `closed`, records a relation to its own
///   earlier `closed`-backed finding. Legitimate, and the readback says
///   which record it resolved to, by which route, and what that record's
///   own standing was — four separate facts, none of them a claim that
///   the parent is right.
/// - The narrowed child may cite neither that relation nor the finding
///   under it: a relation wraps its target exactly as a journal
///   reference does, and `a_journal_wrapper_does_not_launder_a_denied_source`
///   is the same defect one noun over.
/// - Listing the parent's record, the child is shown the relation
///   *withheld* — the entry counted, the target unnamed.
/// - A relation to an `open`-backed finding is admitted for the child,
///   because the child independently holds `open`. The gate is the
///   sources, never the noun.
///
/// And the unrelated Work, off the lineage entirely, may name none of
/// them: nothing here is settled, so the estate-publication route this
/// reference also has is not open, and lineage is the only other one.
#[test]
fn a_finding_relation_is_neither_a_wrapper_nor_a_listing_for_a_narrowed_child() {
    let family = build_family(&["scratch:write", "open:read", "helper:write"]);
    let estate = &family.estate;

    let raise_parent = |kind: &str, claim: &str, flag: &str, token: &str| {
        raise_cli(
            estate,
            &family.parent.work_id,
            &family.parent.run_id,
            &[
                "--kind",
                kind,
                "--scope",
                "estate_local",
                "--claim",
                claim,
                flag,
                token,
            ],
        )
    };

    let (code, closed_finding, stderr) = raise_parent(
        "gap",
        "a real closed-source gap the parent may legitimately raise",
        "--evidence",
        &family.closed_coordinate,
    );
    assert_eq!(code, Some(0), "{stderr}");
    let closed_id = closed_finding["id"].as_str().unwrap().to_string();

    let (code, open_finding, stderr) = raise_parent(
        "gap",
        "an open-source gap every member of this family may read",
        "--evidence",
        &family.open_coordinate,
    );
    assert_eq!(code, Some(0), "{stderr}");
    let open_id = open_finding["id"].as_str().unwrap().to_string();

    // The parent's own relation to its own record: admitted, and
    // described rather than merely accepted.
    let closed_relation = format!("work/{}/finding/{closed_id}", family.parent.work_id);
    // Estate scope still needs a source of its own: a relation is a
    // claim about a record, never the admitted evidence that earns an
    // EstateLocal publication, so the parent brings its own coordinate.
    let (code, wrapper, stderr) = raise_cli(
        estate,
        &family.parent.work_id,
        &family.parent.run_id,
        &[
            "--kind",
            "contradicted_assumption",
            "--scope",
            "estate_local",
            "--claim",
            "the parent's own disagreement with its own earlier closed-source finding",
            "--contradicts",
            &closed_relation,
            "--evidence",
            &family.closed_coordinate,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let wrapper_id = wrapper["id"].as_str().unwrap().to_string();
    let entry = &wrapper["contradicts"][0];
    assert_eq!(entry["reference"], "finding");
    assert_eq!(entry["work"].as_str().unwrap(), family.parent.work_id);
    assert_eq!(entry["finding"].as_str().unwrap(), closed_id);
    assert_eq!(entry["outcome"], "admitted");
    assert_eq!(entry["admitted_by"], "own_journal");
    assert_eq!(
        entry["target_standing"], "unsettled",
        "naming a target says what that record's standing is, not what the claim is worth: {wrapper}"
    );
    assert_eq!(
        entry["resolved"]["work"].as_str().unwrap(),
        family.parent.work_id
    );
    assert!(
        entry["resolved"]["origin_event"].is_string(),
        "an admitted relation resolves to the exact record it names: {wrapper}"
    );
    assert!(
        wrapper["settled"].is_null(),
        "naming a target settles nothing: {wrapper}"
    );

    // The child may cite neither the relation nor its target.
    for (what, token) in [
        (
            "the relation itself",
            format!("work/{}/finding/{wrapper_id}", family.parent.work_id),
        ),
        ("its target directly", closed_relation.clone()),
    ] {
        let (code, reply, stderr) = raise_cli(
            estate,
            &family.child.work_id,
            &family.child.run_id,
            &[
                "--kind",
                "gap",
                "--scope",
                "estate_local",
                "--claim",
                "laundering closed evidence through a finding relation",
                "--evidence",
                &token,
            ],
        );
        assert_eq!(
            code,
            Some(3),
            "a narrowed child must not reach a closed source through {what}: {reply} {stderr}"
        );
        assert_discloses_nothing(
            "the relation refusal",
            &reply,
            &stderr,
            &family.closed_secrets_slice(),
        );
    }

    // Listing the parent's own record, the child is told a part was
    // withheld and nothing about it.
    let (code, listed, stderr) = finding_cli(
        estate,
        &[
            "list",
            "--work",
            &family.parent.work_id,
            "--requesting-work",
            &family.child.work_id,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let shown = listed["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|record| record["id"].as_str() == Some(wrapper_id.as_str()))
        .expect("the child lists its parent's records")
        .clone();
    assert_eq!(
        shown["contradicts"][0],
        serde_json::json!({"withheld": true}),
        "the relation entry is withheld whole, target and all: {shown}"
    );
    assert!(
        listed["disclosure"]["withheld"].as_u64().unwrap() > 0,
        "{listed}"
    );
    assert_discloses_nothing(
        "the child's listing",
        &listed,
        &stderr,
        &family.closed_secrets_slice(),
    );

    // The positive the boundary must not eat: an `open`-backed target.
    let open_relation = format!("work/{}/finding/{open_id}", family.parent.work_id);
    let (code, admitted, stderr) = raise_cli(
        estate,
        &family.child.work_id,
        &family.child.run_id,
        &[
            "--kind",
            "contradicted_assumption",
            "--scope",
            "estate_local",
            "--claim",
            "the child's own disagreement with an open-source finding of its parent's",
            "--contradicts",
            &open_relation,
            "--evidence",
            &family.open_coordinate,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(admitted["contradicts"][0]["admitted_by"], "lineage");
    assert_eq!(admitted["contradicts"][0]["target_standing"], "unsettled");

    // And off the lineage there is no route at all to an unsettled
    // record, however open its sources.
    let (code, refused, stderr) = raise_cli(
        estate,
        &family.unrelated.work_id,
        &family.unrelated.run_id,
        &[
            "--kind",
            "contradicted_assumption",
            "--scope",
            "work_local",
            "--claim",
            "an unrelated work naming a record that was never published",
            "--contradicts",
            &open_relation,
        ],
    );
    assert_eq!(
        code,
        Some(3),
        "only a settled publication crosses to a stranger: {refused} {stderr}"
    );

    stop_wirkd(estate, family.wirkd);
}

// ---- 9. The scope an actor did not have to name (ruling 0117) -----------
//
// `status` and `watch` chose their surface by the absence of a flag: no
// `--requesting-work` meant the administrative read, and no `--work`
// meant every Work under the estate. An actor running inside a Run —
// the triple injected into its environment, `wirk` on its PATH — typed
// the obvious thing and received the administrative enumeration of the
// whole estate: prior Work ids, evidence names, artifact digests, none
// of it asked for and none of it announced. That is the executed trace
// ruling 0117 was written from (`loop-b-scoped-native-consumer-opus`,
// tool 3). The cases below are its red, and the useful positives on
// both sides of the boundary the correction must not cost.

/// A `wirk` CLI process whose injected actor triple is the test's, not
/// the runner's. The three variables are always removed first: this
/// suite is itself run from inside a real actor pane often enough that
/// inheriting a live triple would make every operator case below read
/// as some unrelated Work — the same environment-shaped surprise the
/// correction is about.
fn wirk_cli(actor_env: &[(&str, &str)]) -> Command {
    let mut command = Command::new(wirk_bin());
    for name in ["WIRK_ESTATE_ROOT", "WIRK_WORK_ID", "WIRK_RUN_ID"] {
        command.env_remove(name);
    }
    for (name, value) in actor_env {
        command.env(name, value);
    }
    command
}

/// **Red on the pre-correction tree.** Inside an actor context, `wirk
/// wirkd status --estate "$WIRK_ESTATE_ROOT"` answered administratively
/// about every Work in the estate. It must answer as that actor, about
/// that actor's own Work, and say which surface it used — while every
/// explicit mode, on both sides of the context, still works.
#[test]
fn wirkd_status_inside_an_actor_context_answers_as_that_actor() {
    let family = build_launch_family(&["scratch:write", "open:read", "helper:write"]);
    let estate = &family.estate;
    let estate_str = estate.to_str().unwrap().to_string();
    let embargoed_path = format!("{}/embargoed.md", family.closed_checkout);

    // Content on the parent worth withholding, and a `needs_input`
    // detail the administrative read renders in full.
    record_launch_events(
        &family.pointer.socket,
        &family.parent.work_id,
        &family.parent.run_id,
        wirk_core::ActorSelection {
            model: Some("opus".to_string()),
            effort: Some("medium".to_string()),
            args: vec!["--add-dir".to_string(), embargoed_path.clone()],
        },
        &format!("{}/.herdr/embargo-session.sock", family.closed_checkout),
        &["claude", "--add-dir", &embargoed_path],
    );

    // A third Work with no kinship to either: the unrelated neighbor an
    // enumeration exposes and a scoped read must not reach.
    let stranger_dir = tempfile::tempdir().unwrap();
    let stranger_repo = stranger_dir.path().join("stranger-repo");
    init_repo(&stranger_repo);
    let stranger = submit(
        estate,
        "disclosure_launch",
        &stranger_repo,
        &["scratch:write", "open:read", "helper:write"],
        None,
    )
    .unwrap();

    let status = |actor_env: &[(&str, &str)], args: &[&str]| -> (Option<i32>, String, String) {
        let mut full = vec!["wirkd", "status", "--estate", estate_str.as_str()];
        full.extend_from_slice(args);
        let output = wirk_cli(actor_env)
            .args(&full)
            .output()
            .expect("wirk wirkd status runs");
        (
            output.status.code(),
            String::from_utf8_lossy(&output.stdout).trim().to_string(),
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        )
    };
    let child_context = |work: &str, run: &str| {
        vec![
            ("WIRK_ESTATE_ROOT", estate_str.clone()),
            ("WIRK_WORK_ID", work.to_string()),
            ("WIRK_RUN_ID", run.to_string()),
        ]
    };
    let owned = child_context(&family.child.work_id, &family.child.run_id);
    let as_child: Vec<(&str, &str)> = owned.iter().map(|(k, v)| (*k, v.as_str())).collect();

    // 1. The inherited scope, no flags at all: the actor's own Work,
    //    asked for as itself, and nothing else read.
    let (code, out, err) = status(&as_child, &[]);
    assert_eq!(
        code,
        Some(0),
        "the inherited scope must answer: {out} {err}"
    );
    assert!(
        out.contains(&family.child.work_id) && out.contains("scope requester"),
        "an actor's omitted scope must answer as that Work: {out} {err}"
    );
    // Its own row and no other. The parent id does appear — inside
    // this child's own `parent` linkage, which is its own admitted
    // record (journal identities on lineage permission, the disclosure
    // contract this correction does not touch). What must not appear is
    // another Work's *status row*, and the unrelated neighbor at all.
    assert!(
        !out.contains(&format!("work_id {}", family.parent.work_id)),
        "an actor's omitted scope must not list another Work's status: {out}"
    );
    assert!(
        !out.contains(&stranger.work_id),
        "an actor's omitted scope must not enumerate unrelated Works: {out}"
    );
    assert_eq!(
        out.lines()
            .filter(|line| line.starts_with("work_id "))
            .count(),
        1,
        "exactly one Work is read, the actor's own: {out}"
    );
    assert!(
        !out.contains("administrative"),
        "an omitted scope must never reach the administrative surface: {out}"
    );
    for needle in family.closed_secrets_slice() {
        assert!(
            !out.contains(needle),
            "the inherited scope disclosed {needle:?}: {out}"
        );
    }
    assert!(
        err.contains(&family.child.work_id),
        "the resolved scope must be said out loud, not assumed: {err}"
    );

    // 2. The same request with the scope named explicitly: identical.
    let (code, explicit, _) = status(
        &as_child,
        &[
            "--requesting-work",
            &family.child.work_id,
            "--work",
            &family.child.work_id,
        ],
    );
    assert_eq!(code, Some(0), "{explicit}");
    assert_eq!(
        explicit, out,
        "naming your own scope must be the same read the default makes"
    );

    // 3. A target named inside the inherited scope: the parent, whose
    //    lineage does cover this child — admitted, narrowed, with the
    //    embargoed halves withheld rather than the whole read refused.
    let (code, narrowed, err) = status(&as_child, &["--work", &family.parent.work_id]);
    assert_eq!(code, Some(0), "{narrowed} {err}");
    assert!(
        narrowed.contains(&family.parent.work_id) && narrowed.contains("scope requester"),
        "a named in-lineage target must still be answered, scoped: {narrowed}"
    );
    for needle in family.closed_secrets_slice() {
        assert!(
            !narrowed.contains(needle),
            "the narrowed read disclosed {needle:?}: {narrowed}"
        );
    }

    // 4. A target with no kinship at all: refused, as the scoped verb
    //    it is — never quietly re-asked administratively.
    let (code, denied, err) = status(&as_child, &["--work", &stranger.work_id]);
    assert_ne!(
        code,
        Some(0),
        "an out-of-lineage target must not be answered: {denied}"
    );
    assert!(
        !denied.contains("scope administrative"),
        "a refused scoped read must not fall back to the administrative one: {denied} {err}"
    );

    // 5. The operator, outside any actor context: unchanged. Every Work
    //    under the estate, administratively, said so on every line.
    let (code, operator, err) = status(&[], &[]);
    assert_eq!(code, Some(0), "{operator} {err}");
    for work in [
        &family.parent.work_id,
        &family.child.work_id,
        &stranger.work_id,
    ] {
        assert!(
            operator.contains(work.as_str()),
            "the operator listing must still name {work}: {operator}"
        );
    }
    assert!(
        operator.contains("scope administrative"),
        "the operator listing must still name its surface: {operator}"
    );

    // 6. `--admin` named explicitly, from the operator's shell: the
    //    same answer, now impossible to reach by omission.
    let (code, named_admin, err) = status(&[], &["--admin"]);
    assert_eq!(code, Some(0), "{named_admin} {err}");
    assert_eq!(
        named_admin, operator,
        "--admin must name exactly the read the operator's omission makes"
    );

    // 7. `--admin` named from inside an actor context: still supported
    //    (the same user runs both, and 0117 claims no isolation) —
    //    deliberate, and announced as not this actor's own scope.
    let (code, actor_admin, err) = status(&as_child, &["--admin"]);
    assert_eq!(code, Some(0), "{actor_admin} {err}");
    assert!(
        actor_admin.contains("scope administrative")
            && actor_admin.contains(&family.parent.work_id),
        "an explicit administrative read must still be the administrative read: {actor_admin}"
    );
    assert!(
        err.contains("--admin") && err.contains(&family.child.work_id),
        "an administrative read taken inside an actor context must say so: {err}"
    );

    stop_wirkd(estate, family.wirkd);
}

/// **Red on the pre-correction tree.** An actor context that does not
/// hold together — half a triple, a foreign requester, an estate that
/// is not this actor's, both scope flags at once — used to resolve to
/// the widest reading available. Each is refused instead, by the
/// command line alone: no daemon is contacted, no Work directory is
/// listed, nothing is printed on stdout.
#[test]
fn wirkd_status_refuses_an_incoherent_actor_context() {
    let dir = tempfile::tempdir().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(estate.join("works").join("work-a-neighbor")).unwrap();
    let estate_str = estate.to_str().unwrap().to_string();
    let elsewhere = dir.path().join("another-estate");
    fs::create_dir_all(&elsewhere).unwrap();

    // No wirkd is started anywhere in this test on purpose: a refusal
    // that needed a running daemon would already have read something.
    let status = |actor_env: &[(&str, &str)], args: &[&str]| -> (Option<i32>, String, String) {
        let mut full = vec!["wirkd", "status", "--estate", estate_str.as_str()];
        full.extend_from_slice(args);
        let output = wirk_cli(actor_env)
            .args(&full)
            .output()
            .expect("wirk wirkd status runs");
        (
            output.status.code(),
            String::from_utf8_lossy(&output.stdout).trim().to_string(),
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        )
    };

    let complete = [
        ("WIRK_ESTATE_ROOT", estate_str.as_str()),
        ("WIRK_WORK_ID", "work-mine-1"),
        ("WIRK_RUN_ID", "run-mine-1"),
    ];
    /// One refusal case: what it is, the environment it runs in, the
    /// flags it passes, and what its refusal must say.
    struct Incoherent<'a> {
        name: &'a str,
        actor_env: Vec<(&'a str, &'a str)>,
        args: Vec<&'a str>,
        needles: Vec<&'a str>,
    }
    let cases = vec![
        Incoherent {
            name: "both scope flags at once",
            actor_env: complete.to_vec(),
            args: vec!["--requesting-work", "work-mine-1", "--admin"],
            needles: vec!["--admin", "--requesting-work"],
        },
        Incoherent {
            name: "a requester that is not this actor",
            actor_env: complete.to_vec(),
            args: vec!["--requesting-work", "work-someone-else-2"],
            needles: vec!["work-someone-else-2", "work-mine-1"],
        },
        Incoherent {
            name: "half a triple",
            actor_env: vec![
                ("WIRK_ESTATE_ROOT", estate_str.as_str()),
                ("WIRK_WORK_ID", "work-mine-1"),
            ],
            args: vec![],
            needles: vec!["WIRK_RUN_ID", "incomplete"],
        },
        Incoherent {
            name: "a blank variable is not an identity",
            actor_env: vec![
                ("WIRK_ESTATE_ROOT", estate_str.as_str()),
                ("WIRK_WORK_ID", "  "),
                ("WIRK_RUN_ID", "run-mine-1"),
            ],
            args: vec![],
            needles: vec!["WIRK_WORK_ID", "incomplete"],
        },
        Incoherent {
            name: "an estate that is not this actor's",
            actor_env: vec![
                ("WIRK_ESTATE_ROOT", elsewhere.to_str().unwrap()),
                ("WIRK_WORK_ID", "work-mine-1"),
                ("WIRK_RUN_ID", "run-mine-1"),
            ],
            args: vec![],
            needles: vec!["WIRK_ESTATE_ROOT"],
        },
    ];

    for case in cases {
        let Incoherent {
            name,
            actor_env,
            args,
            needles,
        } = case;
        let (code, out, err) = status(&actor_env, &args);
        assert_eq!(code, Some(1), "{name} must be refused: {out} {err}");
        assert!(out.is_empty(), "{name} must print no status at all: {out}");
        assert!(
            !err.contains("work-a-neighbor"),
            "{name} must not enumerate the estate to refuse: {err}"
        );
        for needle in needles {
            assert!(
                err.contains(needle),
                "{name} must say why, naming {needle:?}: {err}"
            );
        }
    }

    // The half-triple actor that names a scope explicitly is not
    // guessing, and is not refused.
    let (code, _, err) = status(
        &[
            ("WIRK_ESTATE_ROOT", estate_str.as_str()),
            ("WIRK_WORK_ID", "work-mine-1"),
        ],
        &["--requesting-work", "work-mine-1", "--work", "work-mine-1"],
    );
    assert_eq!(
        code,
        Some(2),
        "an explicitly scoped call must reach the daemon (absent here), not be refused: {err}"
    );
}

/// **Red on the pre-correction tree.** `wirk wirkd watch` carried the
/// identical default: no `--requesting-work` meant the administrative
/// stream, and no `--work` meant one connection per Work in the estate.
/// Inside an actor context it must stream that actor's own journal, and
/// a named out-of-lineage target must be refused as the whole-or-
/// nothing scoped stream it is — not delivered raw.
#[test]
fn wirkd_watch_inside_an_actor_context_streams_only_that_actors_work() {
    let family = build_launch_family(&["scratch:write", "open:read", "helper:write"]);
    let estate = &family.estate;
    let estate_str = estate.to_str().unwrap().to_string();
    let embargoed_path = format!("{}/embargoed.md", family.closed_checkout);

    record_launch_events(
        &family.pointer.socket,
        &family.parent.work_id,
        &family.parent.run_id,
        wirk_core::ActorSelection {
            model: Some("opus".to_string()),
            effort: Some("medium".to_string()),
            args: vec!["--add-dir".to_string(), embargoed_path.clone()],
        },
        &format!("{}/.herdr/embargo-session.sock", family.closed_checkout),
        &["claude", "--add-dir", &embargoed_path],
    );

    let child_context = [
        ("WIRK_ESTATE_ROOT", estate_str.as_str()),
        ("WIRK_WORK_ID", family.child.work_id.as_str()),
        ("WIRK_RUN_ID", family.child.run_id.as_str()),
    ];

    // `watch` blocks by design (ruling 0044), so each case is read
    // exactly one line deep and then killed. The first line is the
    // answer: a journal event, or a refusal.
    let first_line = |actor_env: &[(&str, &str)], args: &[&str]| -> String {
        use std::io::BufRead;
        let mut full = vec!["wirkd", "watch", "--estate", estate_str.as_str()];
        full.extend_from_slice(args);
        let mut child = wirk_cli(actor_env)
            .args(&full)
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("wirk wirkd watch spawns");
        let mut line = String::new();
        let mut reader = std::io::BufReader::new(child.stdout.take().expect("piped stdout"));
        reader.read_line(&mut line).expect("one streamed line");
        let _ = child.kill();
        let _ = child.wait();
        line
    };

    // The inherited scope with no target: this actor's own journal,
    // streamed — a real event, not a timeout that proved nothing.
    let own = first_line(&child_context, &[]);
    assert!(
        own.starts_with(&family.child.work_id) && own.contains("kind"),
        "an actor's omitted scope must stream its own journal: {own}"
    );

    // The parent's stream, named as a target from inside the child's
    // context. `watch` admits or refuses whole (a partially redacted
    // Event is not an Event), and this child's grants do not cover the
    // parent's: refused, with no event line at all.
    let refused = first_line(&child_context, &["--work", &family.parent.work_id]);
    assert!(
        refused.contains("refused"),
        "a scoped stream of an unadmitted Work must be refused, not streamed: {refused}"
    );
    assert!(
        !refused.contains("\"kind\""),
        "no journal event may reach a requester the scope refuses: {refused}"
    );
    for needle in family.closed_secrets_slice() {
        assert!(
            !refused.contains(needle),
            "the refused stream disclosed {needle:?}: {refused}"
        );
    }

    // The operator's own stream, outside any actor context: unchanged.
    let operator = first_line(&[], &["--work", &family.parent.work_id]);
    assert!(
        operator.starts_with(&family.parent.work_id) && operator.contains("kind"),
        "the operator stream must be unchanged: {operator}"
    );

    stop_wirkd(estate, family.wirkd);
}

/// The usage line is the only place a caller who guessed wrong is told
/// what these verbs actually do: that `ping` is a daemon health check
/// and reports nothing about any Work (it was read as "the estate's
/// status"), and which scope `status`/`watch` answer in, in and out of
/// an actor context.
#[test]
fn wirkd_usage_explains_ping_and_the_two_scopes() {
    let output = wirk_cli(&[])
        .args(["wirkd"])
        .output()
        .expect("wirk wirkd runs");
    let usage = String::from_utf8_lossy(&output.stderr).to_string();
    for needle in [
        "ping",
        "health",
        "--admin",
        "WIRK_WORK_ID",
        "actor context",
        "administratively",
    ] {
        assert!(
            usage.contains(needle),
            "the usage line must explain {needle:?}: {usage}"
        );
    }
}

// ---- INDEPENDENT VERIFICATION (loop-b-finding-disagreement-verify) ----

/// The builder's own honest limit: "the narrowed-reader withholding on
/// the *publication* route is real-service evidence, not a pinned test."
/// This is that test. The relation entry being rendered was admitted by
/// `settled_estate_publication` — a route the pinned disclosure test
/// never produces — and the reader is a child that cannot discover the
/// named record at all.
#[test]
fn verify_a_publication_route_relation_is_withheld_from_a_narrowed_child() {
    let family = build_family(&["scratch:write", "open:read", "helper:write"]);
    let estate = &family.estate;

    // An off-lineage producer holding exactly what the parent holds.
    let producer_repo = family.dir.path().join("pub-producer-repo");
    init_repo(&producer_repo);
    let producer = submit(
        estate,
        "disclosure_container",
        &producer_repo,
        &[
            "embargo:write",
            "open:read",
            "helper:write",
            "scratch:write",
        ],
        None,
    )
    .unwrap();
    // P3 execution-recovery item 1: same reasoning — the producer's own
    // Deterministic leaf runs in its own worktree, not `producer_repo`.
    write_file(
        &estate.join("worktrees").join(&producer.work_id),
        "a.md",
        "a\n",
    );
    claim_ok(estate, &producer.work_id, &producer.run_id, "a.md=a.md");
    let basis = obligation_basis_for(estate, &producer.work_id, "outer/leaf-a");
    write_policy_admitting(estate, "a-produced", "1", &basis);
    let claim_event = journal_events(estate, &producer.work_id)
        .into_iter()
        .find_map(|event| {
            matches!(&event.kind, EventKind::ClaimRecorded { .. }).then_some(event.id.0)
        })
        .expect("the producer's own ClaimRecorded event");
    let (code, raised, stderr) = raise_cli(
        estate,
        &producer.work_id,
        &producer.run_id,
        &[
            "--kind",
            "verified_outcome",
            "--scope",
            "estate_local",
            "--claim",
            "outer/leaf-a produced a.md in the off-lineage producer",
            "--evidence",
            &format!("work/{}/event/{claim_event}", producer.work_id),
            "--obligation",
            "a-produced@1",
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let settled_id = raised["id"].as_str().unwrap().to_string();
    assert!(raised["settled"].is_object(), "{raised}");
    let (ok, _, err) = atlas(estate, &["findings", "--rebuild", "--admin"]);
    assert!(ok, "{err}");

    let index = |requester: &str| -> serde_json::Value {
        let (ok, reply, err) = atlas(estate, &["findings", "--requesting-work", requester]);
        assert!(ok, "{err}");
        reply
    };
    let has_row = |reply: &serde_json::Value| -> bool {
        reply["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["finding"]["id"].as_str() == Some(settled_id.as_str()))
    };
    let parent_index = index(&family.parent.work_id);
    assert!(
        has_row(&parent_index),
        "the parent must discover the publication independently: {parent_index}"
    );
    let child_index = index(&family.child.work_id);
    assert!(
        !has_row(&child_index),
        "the narrowed child must not discover it: {child_index}"
    );

    // The parent's own disagreement, admitted by the publication route.
    let relation = format!("work/{}/finding/{settled_id}", producer.work_id);
    let (code, disagreement, stderr) = raise_cli(
        estate,
        &family.parent.work_id,
        &family.parent.run_id,
        &[
            "--kind",
            "contradicted_assumption",
            "--scope",
            "work_local",
            "--claim",
            "the parent's disagreement with a record it discovered off its lineage",
            "--contradicts",
            &relation,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        disagreement["contradicts"][0]["admitted_by"], "settled_estate_publication",
        "this is the route the pinned tests never render: {disagreement}"
    );
    let disagreement_id = disagreement["id"].as_str().unwrap().to_string();

    // The narrowed child, listing its parent, is told a part was
    // withheld and nothing about it.
    let (code, listed, stderr) = finding_cli(
        estate,
        &[
            "list",
            "--work",
            &family.parent.work_id,
            "--requesting-work",
            &family.child.work_id,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let shown = listed["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|record| record["id"].as_str() == Some(disagreement_id.as_str()))
        .expect("the child lists its parent's records")
        .clone();
    eprintln!("VERIFY child's view of the publication-route relation: {shown}");
    assert_eq!(
        shown["contradicts"][0],
        serde_json::json!({"withheld": true}),
        "a publication-route relation must be withheld whole from a reader that \
         cannot itself reach the named record: {shown}"
    );
    assert!(
        listed["disclosure"]["withheld"].as_u64().unwrap() > 0,
        "{listed}"
    );
    let strings = serde_json::to_string(&listed).unwrap();
    assert!(
        !strings.contains(&settled_id) && !strings.contains(&producer.work_id),
        "no target identity crosses: {listed}"
    );

    // And the child cannot name the record for itself either.
    let (code, refused, stderr) = raise_cli(
        estate,
        &family.child.work_id,
        &family.child.run_id,
        &[
            "--kind",
            "contradicted_assumption",
            "--scope",
            "work_local",
            "--claim",
            "the child naming what it cannot discover",
            "--contradicts",
            &relation,
        ],
    );
    assert_eq!(code, Some(3), "{refused} {stderr}");

    let Family { estate, wirkd, .. } = family;
    stop_wirkd(&estate, wirkd);
}

/// **Red on the pre-correction tree.** `--json` was accepted on the
/// command line of both named status entry points — `wirk work status`
/// and `wirk wirkd status` — and then ignored: the human lines were
/// printed instead, so every `json.load` over that stdout failed at
/// character 0 (ruling 0135, "work status ignores --json"). The flag
/// must render the daemon's own reply, on both doors, without moving
/// the human surface, the scope, the withholding or the exit codes.
#[test]
fn status_json_renders_the_daemon_reply_on_both_entry_points() {
    let family = build_launch_family(&["scratch:write", "open:read", "helper:write"]);
    let estate = &family.estate;
    let estate_str = estate.to_str().unwrap().to_string();
    let embargoed_path = format!("{}/embargoed.md", family.closed_checkout);

    // Content on the parent worth withholding: what a scoped `--json`
    // read must be seen to keep back, not merely fail to print.
    record_launch_events(
        &family.pointer.socket,
        &family.parent.work_id,
        &family.parent.run_id,
        wirk_core::ActorSelection {
            model: Some("opus".to_string()),
            effort: Some("medium".to_string()),
            args: vec!["--add-dir".to_string(), embargoed_path.clone()],
        },
        &format!("{}/.herdr/embargo-session.sock", family.closed_checkout),
        &["claude", "--add-dir", &embargoed_path],
    );

    // An unrelated neighbour: the out-of-lineage target the denied
    // control names, and a second row the operator's listing must hold.
    let stranger_dir = tempfile::tempdir().unwrap();
    let stranger_repo = stranger_dir.path().join("stranger-repo");
    init_repo(&stranger_repo);
    let stranger = submit(
        estate,
        "disclosure_launch",
        &stranger_repo,
        &["scratch:write", "open:read", "helper:write"],
        None,
    )
    .unwrap();

    // Both doors, driven exactly as an operator's script drives them.
    let run = |verb: &[&str], args: &[&str]| -> (Option<i32>, String, String) {
        let mut full: Vec<&str> = verb.to_vec();
        full.extend_from_slice(&["--estate", estate_str.as_str()]);
        full.extend_from_slice(args);
        let output = wirk_cli(&[])
            .args(&full)
            .output()
            .expect("wirk status runs");
        (
            output.status.code(),
            String::from_utf8_lossy(&output.stdout).to_string(),
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        )
    };
    let work_status = |args: &[&str]| run(&["work", "status"], args);
    let wirkd_status = |args: &[&str]| run(&["wirkd", "status"], args);

    // 1. The one the prepared operator script actually calls. Pure
    //    JSON on stdout, one object, the daemon's own field names.
    let (code, out, err) = work_status(&["--work", &family.parent.work_id, "--admin", "--json"]);
    assert_eq!(code, Some(0), "{out} {err}");
    let admin: serde_json::Value = serde_json::from_str(&out)
        .unwrap_or_else(|why| panic!("stdout must be JSON ({why}): {out}"));
    assert!(admin.is_object(), "one named Work is one object: {admin}");
    assert_eq!(
        admin["scope"],
        serde_json::json!("administrative"),
        "the administrative reply is rendered, not reshaped: {admin}"
    );
    assert!(
        admin["state"].is_string() && admin["current_waypoint"].is_string(),
        "the fields a caller reads must be the daemon's own: {admin}"
    );

    // 2. The same request through the other named door, byte for byte:
    //    `wirk work status` is an alias, and a flag honoured on one
    //    surface only is the shared-path defect this closes.
    let (code, aliased, err) =
        wirkd_status(&["--work", &family.parent.work_id, "--admin", "--json"]);
    assert_eq!(code, Some(0), "{aliased} {err}");
    assert_eq!(
        aliased, out,
        "both status entry points must render the same JSON for the same request"
    );

    // 3. The human surface, unflagged, is untouched: the same lines,
    //    and no JSON leaking onto them.
    let (code, human, err) = work_status(&["--work", &family.parent.work_id, "--admin"]);
    assert_eq!(code, Some(0), "{human} {err}");
    assert!(
        human.starts_with(&format!("work_id {} state ", family.parent.work_id))
            && human.contains("scope administrative"),
        "the human lines must survive the flag's arrival: {human}"
    );
    assert!(
        serde_json::from_str::<serde_json::Value>(&human).is_err(),
        "the unflagged surface is human text, not JSON: {human}"
    );

    // 4. The operator's estate walk: one JSON document, not a line per
    //    Work, each answer addressable by the id it was asked about.
    let (code, listed, err) = wirkd_status(&["--admin", "--json"]);
    assert_eq!(code, Some(0), "{listed} {err}");
    let rows: serde_json::Value = serde_json::from_str(&listed)
        .unwrap_or_else(|why| panic!("the listing must be one JSON document ({why}): {listed}"));
    let rows = rows.as_array().expect("the listing is an array").clone();
    let named: Vec<&str> = rows
        .iter()
        .filter_map(|row| row["work_id"].as_str())
        .collect();
    for work in [
        &family.parent.work_id,
        &family.child.work_id,
        &stranger.work_id,
    ] {
        assert!(
            named.contains(&work.as_str()),
            "the administrative listing must carry {work}: {listed}"
        );
    }
    let parent_row = rows
        .iter()
        .find(|row| row["work_id"].as_str() == Some(family.parent.work_id.as_str()))
        .expect("the parent's row");
    assert_eq!(
        parent_row["status"], admin,
        "a listed answer is the same reply the single read renders: {parent_row}"
    );

    // 5. A scoped read renders too — and renders *less*. `--json` is a
    //    rendering flag: it must not reach past the withholding the
    //    scoped surface applies.
    let (code, scoped, err) = work_status(&[
        "--work",
        &family.parent.work_id,
        "--requesting-work",
        &family.child.work_id,
        "--json",
    ]);
    assert_eq!(code, Some(0), "{scoped} {err}");
    let scoped_value: serde_json::Value = serde_json::from_str(&scoped)
        .unwrap_or_else(|why| panic!("a scoped read must render JSON too ({why}): {scoped}"));
    assert_eq!(
        scoped_value["scope"],
        serde_json::json!("requester"),
        "the scoped reply must say which surface answered: {scoped_value}"
    );
    assert_eq!(
        scoped_value["work_id"],
        serde_json::json!(family.parent.work_id),
        "the scoped reply names the Work it is about: {scoped_value}"
    );
    assert!(
        scoped_value["disclosure"]["withheld"].as_u64().unwrap_or(0) > 0,
        "this scoped read has parts withheld and must say how many: {scoped_value}"
    );
    for needle in family.closed_secrets_slice() {
        assert!(
            !scoped.contains(needle),
            "the JSON rendering disclosed {needle:?}: {scoped}"
        );
    }

    // 6. The denied control. A refused read prints no JSON at all: a
    //    caller checks the exit status, and never has to tell an answer
    //    apart from an apology on the same stream.
    let (code, denied, err) = work_status(&[
        "--work",
        &stranger.work_id,
        "--requesting-work",
        &family.child.work_id,
        "--json",
    ]);
    assert_eq!(
        code,
        Some(2),
        "an out-of-lineage target must be refused: {denied}"
    );
    assert!(
        denied.is_empty(),
        "a refused read must print nothing on stdout: {denied:?}"
    );
    assert!(
        err.contains("InadmissibleEvidence"),
        "the refusal keeps its code and message on stderr: {err}"
    );

    // 7. The command-line refusal, which happens before wirkd is even
    //    located, is equally silent on stdout under `--json`.
    let (code, both, err) = work_status(&[
        "--work",
        &family.child.work_id,
        "--admin",
        "--requesting-work",
        &family.child.work_id,
        "--json",
    ]);
    assert_eq!(
        code,
        Some(1),
        "naming both scopes stays refused: {both} {err}"
    );
    assert!(
        both.is_empty(),
        "a command-line refusal must print nothing on stdout: {both:?}"
    );

    stop_wirkd(estate, family.wirkd);
}
