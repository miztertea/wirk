//! Nested-stage loader and fold tests (W-A, p3-world-loop
//! BUILD-BRIEF.md §3.1-3.2, superseding the draft's one-level shortcut
//! per BUILD-AMENDMENTS.md). No wirkd, no journal file — `load_route`
//! and `fold` are pure functions over hand-built values; the real
//! daemon/Git proof of closure evaluation and child Works lives in
//! `wirk/tests/nested_work.rs`.

use wirk_core::{
    Access, ArtifactReceipt, ArtifactSpec, Boundary, ChildOutcomeSpec, ClaimId, ClaimKind,
    ClaimVerdict, Event, EventId, EventKind, OutcomeReceipt, RepositoryBinding, RouteError,
    RouteId, RunId, Timestamp, WaypointDefinition, WaypointId, WaypointKind, WorkId, WorkState,
    load_route,
};

fn actor(id: &str, outputs: &[&str]) -> WaypointDefinition {
    WaypointDefinition {
        id: WaypointId(id.to_string()),
        kind: WaypointKind::Actor,
        declared_outputs: outputs
            .iter()
            .map(|name| ArtifactSpec {
                name: name.to_string(),
                required: true,
            })
            .collect(),
        intent: Some(format!("write {}", outputs.join(", "))),
        command: None,
        boundary: Boundary(vec!["**".to_string()]),
        leaves: Vec::new(),
        required_child_outcomes: Vec::new(),
    }
}

fn container(
    id: &str,
    outputs: &[&str],
    roles: &[&str],
    leaves: Vec<WaypointDefinition>,
) -> WaypointDefinition {
    WaypointDefinition {
        id: WaypointId(id.to_string()),
        kind: WaypointKind::Container,
        declared_outputs: outputs
            .iter()
            .map(|name| ArtifactSpec {
                name: name.to_string(),
                required: true,
            })
            .collect(),
        intent: None,
        command: None,
        boundary: Boundary(Vec::new()),
        leaves,
        required_child_outcomes: roles
            .iter()
            .map(|role| ChildOutcomeSpec {
                role: role.to_string(),
                required: true,
            })
            .collect(),
    }
}

fn write(dir: &std::path::Path, name: &str, content: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, content).expect("write fixture");
    path
}

// ---- load_route: outcome/mechanism split, recursion --------------------

#[test]
fn container_with_intent_or_command_is_refused_at_load() {
    let dir = tempfile::tempdir().expect("tempdir");
    let content = r#"{
      "id": "bad-container",
      "waypoints": [
        {
          "id": "outer",
          "kind": "Container",
          "declared_outputs": [],
          "intent": "should not be here",
          "leaves": [
            {"id": "outer/leaf", "kind": "Deterministic", "command": ["true"], "declared_outputs": []}
          ]
        }
      ]
    }"#;
    let path = write(dir.path(), "route.json", content);
    let err = load_route(&path).expect_err("a container with an intent is refused");
    match err {
        RouteError::ContainerWithMechanism { id } => assert_eq!(id.0, "outer"),
        other => panic!("expected ContainerWithMechanism, got {other:?}"),
    }
}

#[test]
fn container_without_leaves_is_refused_at_load() {
    let dir = tempfile::tempdir().expect("tempdir");
    let content = r#"{
      "id": "vacuous-container",
      "waypoints": [
        {"id": "outer", "kind": "Container", "declared_outputs": []}
      ]
    }"#;
    let path = write(dir.path(), "route.json", content);
    let err = load_route(&path).expect_err("a container with no leaves is refused");
    match err {
        RouteError::ContainerWithoutLeaves { id } => assert_eq!(id.0, "outer"),
        other => panic!("expected ContainerWithoutLeaves, got {other:?}"),
    }
}

#[test]
fn non_container_with_leaves_is_refused_at_load() {
    let dir = tempfile::tempdir().expect("tempdir");
    let content = r#"{
      "id": "leaf-with-leaves",
      "waypoints": [
        {
          "id": "wp-1",
          "kind": "Deterministic",
          "command": ["true"],
          "declared_outputs": [],
          "leaves": [
            {"id": "wp-1/inner", "kind": "Deterministic", "command": ["true"], "declared_outputs": []}
          ]
        }
      ]
    }"#;
    let path = write(dir.path(), "route.json", content);
    let err = load_route(&path).expect_err("a non-container waypoint declaring leaves is refused");
    assert!(
        matches!(err, RouteError::Malformed { .. }),
        "expected Malformed, got {err:?}"
    );
}

#[test]
fn duplicate_waypoint_id_nested_under_different_containers_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let content = r#"{
      "id": "dup-nested",
      "waypoints": [
        {
          "id": "a",
          "kind": "Container",
          "declared_outputs": [],
          "leaves": [
            {"id": "shared", "kind": "Deterministic", "command": ["true"], "declared_outputs": []}
          ]
        },
        {
          "id": "b",
          "kind": "Container",
          "declared_outputs": [],
          "leaves": [
            {"id": "shared", "kind": "Deterministic", "command": ["true"], "declared_outputs": []}
          ]
        }
      ]
    }"#;
    let path = write(dir.path(), "route.json", content);
    let err = load_route(&path).expect_err("a duplicate leaf id under two containers is refused");
    match err {
        RouteError::DuplicateWaypoint { id } => assert_eq!(id.0, "shared"),
        other => panic!("expected DuplicateWaypoint, got {other:?}"),
    }
}

/// BUILD-AMENDMENTS.md: nesting is not bounded to one level — a
/// grandchild container loads clean and `flatten_leaves` walks the
/// whole tree in DFS order.
#[test]
fn grandchild_nested_route_loads_and_flattens_in_dfs_order() {
    let dir = tempfile::tempdir().expect("tempdir");
    let content = r#"{
      "id": "grandchild",
      "waypoints": [
        {
          "id": "outer",
          "kind": "Container",
          "declared_outputs": [],
          "leaves": [
            {"id": "outer/lead", "kind": "Deterministic", "command": ["true"], "declared_outputs": []},
            {
              "id": "outer/inner",
              "kind": "Container",
              "declared_outputs": [],
              "leaves": [
                {"id": "outer/inner/leaf", "kind": "Deterministic", "command": ["true"], "declared_outputs": []}
              ]
            }
          ]
        },
        {"id": "after", "kind": "Deterministic", "command": ["true"], "declared_outputs": []}
      ]
    }"#;
    let path = write(dir.path(), "route.json", content);
    let route = load_route(&path).expect("grandchild-nested route loads");
    let flattened = wirk_core::flatten_leaves(&route.waypoints);
    assert_eq!(
        flattened,
        vec![
            WaypointId("outer/lead".to_string()),
            WaypointId("outer/inner/leaf".to_string()),
            WaypointId("after".to_string()),
        ]
    );
    let ancestors = wirk_core::ancestor_chain(
        &route.waypoints,
        &WaypointId("outer/inner/leaf".to_string()),
    );
    assert_eq!(
        ancestors,
        vec![
            WaypointId("outer/inner".to_string()),
            WaypointId("outer".to_string()),
        ],
        "immediate parent first"
    );
}

// ---- fold: old behavior preserved, container/held mechanics -----------

fn event(id: &str, work: &str, run: Option<&str>, at: i64, kind: EventKind) -> Event {
    Event {
        id: EventId(id.to_string()),
        work: WorkId(work.to_string()),
        run: run.map(|r| RunId(r.to_string())),
        at: Timestamp(at),
        kind,
    }
}

fn work_submitted(waypoints: Vec<WaypointDefinition>) -> EventKind {
    EventKind::WorkSubmitted {
        route: RouteId("route-1".to_string()),
        repositories: vec![RepositoryBinding {
            name: "wirk".to_string(),
            access: Access::Write,
        }],
        intent: "do the thing".to_string(),
        waypoints: wirk_core::flatten_leaves(&waypoints),
        waypoint_defs: waypoints,
        parent: None,
        execution_repo: None,
        execution_identity: None,
    }
}

fn reserved(waypoint: &str) -> EventKind {
    EventKind::WaypointReserved {
        waypoint: WaypointId(waypoint.to_string()),
        world_hash: wirk_core::WorldHash("deadbeef".to_string()),
        world: wirk_core::World::Deterministic(wirk_core::DeterministicWorld {
            command: vec!["true".to_string()],
            base_sha: "abc123".to_string(),
            source_basis: wirk_core::SourceBasis::Unknown,
            cwd: "/tmp".into(),
            env: Default::default(),
            expected_artifacts: wirk_core::OutputContract(Vec::new()),
        }),
    }
}

fn opened(run: &str, waypoint: &str) -> EventKind {
    EventKind::RunOpened {
        run: RunId(run.to_string()),
        waypoint: WaypointId(waypoint.to_string()),
        attempt: 1,
        world_hash: wirk_core::WorldHash("deadbeef".to_string()),
    }
}

fn claimed_done() -> EventKind {
    EventKind::ClaimRecorded {
        artifacts: Vec::new(),
        claim: ClaimId("claim-1".to_string()),
        claim_kind: ClaimKind::Done,
        verdict: ClaimVerdict::Validated,
    }
}

/// Positive control (BUILD-BRIEF §7 W-A): a flat Route with no
/// `Container` nodes folds through exactly the same states as before
/// this wave — the last flattened leaf's Validated Done completes the
/// Work directly, with no `StageHeld`/`StageClosed` involved at all.
#[test]
fn old_flat_route_journal_folds_identically() {
    let defs = vec![actor("wp-1", &["a.md"]), actor("wp-2", &["b.md"])];
    let events = vec![
        event("e1", "w", None, 0, work_submitted(defs)),
        event("e2", "w", None, 1, reserved("wp-1")),
        event("e3", "w", Some("r1"), 2, opened("r1", "wp-1")),
        event("e4", "w", Some("r1"), 3, claimed_done()),
    ];
    let work = wirk_core::fold(&events);
    assert!(matches!(work.state, WorkState::Active), "{:?}", work.state);
    assert!(work.held.is_none());

    let mut events = events;
    events.push(event("e5", "w", None, 4, reserved("wp-2")));
    events.push(event("e6", "w", Some("r2"), 5, opened("r2", "wp-2")));
    events.push(event("e7", "w", Some("r2"), 6, claimed_done()));
    let work = wirk_core::fold(&events);
    assert!(
        matches!(work.state, WorkState::Completed),
        "{:?}",
        work.state
    );
}

/// A leaf nested under a container never completes the Work by itself,
/// however last it is in the flattened sequence — the Work waits for
/// that container's own `StageClosed`.
#[test]
fn last_leaf_done_holds_container_when_required_artifact_missing() {
    let outer = container(
        "outer",
        &["phantom.md"], // no leaf ever produces this
        &[],
        vec![actor("outer/leaf-a", &["a.md"])],
    );
    let events = vec![
        event("e1", "w", None, 0, work_submitted(vec![outer])),
        event("e2", "w", None, 1, reserved("outer/leaf-a")),
        event("e3", "w", Some("r1"), 2, opened("r1", "outer/leaf-a")),
        event("e4", "w", Some("r1"), 3, claimed_done()),
    ];
    let work = wirk_core::fold(&events);
    // Claiming the leaf alone must not complete the Work: it has a
    // container ancestor whose own outcome contract is not evaluated by
    // `ClaimRecorded` at all.
    assert!(matches!(work.state, WorkState::Active), "{:?}", work.state);

    let mut events = events;
    events.push(event(
        "e5",
        "w",
        None,
        4,
        EventKind::StageHeld {
            attempt: 1,
            waypoint: WaypointId("outer".to_string()),
            missing: vec!["phantom.md".to_string()],
        },
    ));
    let work = wirk_core::fold(&events);
    assert!(matches!(work.state, WorkState::Waiting), "{:?}", work.state);
    assert_eq!(work.current_waypoint, Some(WaypointId("outer".to_string())));
    let held = work.held.expect("held is set");
    assert_eq!(held.waypoint, WaypointId("outer".to_string()));
    assert_eq!(held.missing, vec!["phantom.md".to_string()]);
}

/// `StageClosed` on a container that is not itself the top-level last
/// Route element advances the Work to `Active` (the auto-advance to the
/// element after the container is the server's own next journal line,
/// not fold's job) — the Work completes only when the *outermost*
/// closing container is the last top-level entry.
#[test]
fn container_closes_with_leaf_receipts_and_advances_to_next_waypoint() {
    let outer = container(
        "outer",
        &["a.md", "b.md"],
        &[],
        vec![
            actor("outer/leaf-a", &["a.md"]),
            actor("outer/leaf-b", &["b.md"]),
        ],
    );
    let after = actor("after", &["c.md"]);
    let events = vec![
        event("e1", "w", None, 0, work_submitted(vec![outer, after])),
        event("e2", "w", None, 1, reserved("outer/leaf-a")),
        event("e3", "w", Some("r1"), 2, opened("r1", "outer/leaf-a")),
        event("e4", "w", Some("r1"), 3, claimed_done()),
        event("e5", "w", None, 4, reserved("outer/leaf-b")),
        event("e6", "w", Some("r2"), 5, opened("r2", "outer/leaf-b")),
        event("e7", "w", Some("r2"), 6, claimed_done()),
    ];
    let work = wirk_core::fold(&events);
    assert!(matches!(work.state, WorkState::Active), "{:?}", work.state);

    let mut events = events;
    events.push(event(
        "e8",
        "w",
        None,
        7,
        EventKind::StageClosed {
            attempt: 1,
            waypoint: WaypointId("outer".to_string()),
            receipts: vec![
                OutcomeReceipt::Leaf {
                    waypoint: WaypointId("outer/leaf-a".to_string()),
                    run: RunId("r1".to_string()),
                    claim: ClaimId("claim-1".to_string()),
                    artifacts: vec![ArtifactReceipt {
                        name: "a.md".to_string(),
                        path: "a.md".to_string(),
                        digest: "sha-of-a.md".to_string(),
                    }],
                },
                OutcomeReceipt::Leaf {
                    waypoint: WaypointId("outer/leaf-b".to_string()),
                    run: RunId("r2".to_string()),
                    claim: ClaimId("claim-1".to_string()),
                    artifacts: vec![ArtifactReceipt {
                        name: "b.md".to_string(),
                        path: "b.md".to_string(),
                        digest: "sha-of-b.md".to_string(),
                    }],
                },
            ],
        },
    ));
    let work = wirk_core::fold(&events);
    assert!(
        matches!(work.state, WorkState::Active),
        "closing a non-last container must not complete the Work: {:?}",
        work.state
    );
    assert!(work.held.is_none());

    // The Route's own next element, reserved by the server in the same
    // journal call: completes the Work once claimed.
    events.push(event("e9", "w", None, 8, reserved("after")));
    events.push(event("e10", "w", Some("r3"), 9, opened("r3", "after")));
    events.push(event("e11", "w", Some("r3"), 10, claimed_done()));
    let work = wirk_core::fold(&events);
    assert!(
        matches!(work.state, WorkState::Completed),
        "{:?}",
        work.state
    );
}

/// BUILD-AMENDMENTS.md: at least a grandchild nested stage is required.
/// A container closing at every nested level, the outermost of which is
/// the Route's top-level last entry, completes the Work.
#[test]
fn grandchild_container_closure_completes_the_work() {
    let inner = container(
        "outer/inner",
        &["b.md"],
        &[],
        vec![actor("outer/inner/leaf", &["b.md"])],
    );
    let outer = container(
        "outer",
        &["a.md", "b.md"],
        &[],
        vec![actor("outer/lead", &["a.md"]), inner],
    );
    let events = vec![
        event("e1", "w", None, 0, work_submitted(vec![outer])),
        event("e2", "w", None, 1, reserved("outer/lead")),
        event("e3", "w", Some("r1"), 2, opened("r1", "outer/lead")),
        event("e4", "w", Some("r1"), 3, claimed_done()),
        event("e5", "w", None, 4, reserved("outer/inner/leaf")),
        event("e6", "w", Some("r2"), 5, opened("r2", "outer/inner/leaf")),
        event("e7", "w", Some("r2"), 6, claimed_done()),
        event(
            "e8",
            "w",
            None,
            7,
            EventKind::StageClosed {
                attempt: 1,
                waypoint: WaypointId("outer/inner".to_string()),
                receipts: vec![OutcomeReceipt::Leaf {
                    waypoint: WaypointId("outer/inner/leaf".to_string()),
                    run: RunId("r2".to_string()),
                    claim: ClaimId("claim-1".to_string()),
                    artifacts: vec![ArtifactReceipt {
                        name: "b.md".to_string(),
                        path: "b.md".to_string(),
                        digest: "sha-of-b.md".to_string(),
                    }],
                }],
            },
        ),
        event(
            "e9",
            "w",
            None,
            8,
            EventKind::StageClosed {
                attempt: 1,
                waypoint: WaypointId("outer".to_string()),
                receipts: vec![
                    OutcomeReceipt::Leaf {
                        waypoint: WaypointId("outer/lead".to_string()),
                        run: RunId("r1".to_string()),
                        claim: ClaimId("claim-1".to_string()),
                        artifacts: vec![ArtifactReceipt {
                            name: "a.md".to_string(),
                            path: "a.md".to_string(),
                            digest: "sha-of-a.md".to_string(),
                        }],
                    },
                    OutcomeReceipt::Container {
                        waypoint: WaypointId("outer/inner".to_string()),
                        receipts: vec![OutcomeReceipt::Leaf {
                            waypoint: WaypointId("outer/inner/leaf".to_string()),
                            run: RunId("r2".to_string()),
                            claim: ClaimId("claim-1".to_string()),
                            artifacts: vec![ArtifactReceipt {
                                name: "b.md".to_string(),
                                path: "b.md".to_string(),
                                digest: "sha-of-b.md".to_string(),
                            }],
                        }],
                    },
                ],
            },
        ),
    ];
    let work = wirk_core::fold(&events);
    assert!(
        matches!(work.state, WorkState::Completed),
        "{:?}",
        work.state
    );
}

/// BUILD-AMENDMENTS.md: a leaf nested under a held container has a
/// usable retry path even though the Work is `Waiting`, not
/// `NeedsInput` — its fresh `RunOpened` clears the hold back to
/// `Active`.
#[test]
fn fold_run_opened_clears_a_waiting_hold_on_retry() {
    let outer = container(
        "outer",
        &["phantom.md"],
        &[],
        vec![actor("outer/leaf-a", &["a.md"])],
    );
    let events = vec![
        event("e1", "w", None, 0, work_submitted(vec![outer])),
        event("e2", "w", None, 1, reserved("outer/leaf-a")),
        event("e3", "w", Some("r1"), 2, opened("r1", "outer/leaf-a")),
        event("e4", "w", Some("r1"), 3, claimed_done()),
        event(
            "e5",
            "w",
            None,
            4,
            EventKind::StageHeld {
                attempt: 1,
                waypoint: WaypointId("outer".to_string()),
                missing: vec!["phantom.md".to_string()],
            },
        ),
    ];
    let work = wirk_core::fold(&events);
    assert!(matches!(work.state, WorkState::Waiting));

    let mut events = events;
    events.push(event("e6", "w", None, 5, reserved("outer/leaf-a")));
    events.push(event(
        "e7",
        "w",
        Some("r2"),
        6,
        opened("r2", "outer/leaf-a"),
    ));
    let work = wirk_core::fold(&events);
    assert!(matches!(work.state, WorkState::Active), "{:?}", work.state);
    assert!(work.held.is_none(), "a fresh Run clears the stale hold");
}

/// W-A correction (legacy compatibility): a journal line written before
/// the correction — `StageClosed` with no `attempt`, its `Leaf`
/// receipts carrying bare artifact *names*, and `ClaimRecorded` with no
/// `artifacts` at all — still deserializes, folds, and stays
/// inspectable. Refusing the whole journal over a field shape would
/// destroy exactly the historical inspectability the correction is
/// meant to protect; a name-only receipt reads as content identity that
/// was never recorded, never as evidence that still holds.
#[test]
fn a_pre_correction_journal_line_still_deserializes_and_folds() {
    let closed: Event = serde_json::from_str(
        r#"{"id":"e1","work":"w1","run":null,"at":1,"kind":{"kind":"StageClosed",
            "waypoint":"outer/inner","receipts":[{"Leaf":{"waypoint":"outer/inner/leaf",
            "run":"r1","claim":"c1","artifacts":["b.md"]}}]}}"#,
    )
    .expect("a pre-correction StageClosed still deserializes");
    let EventKind::StageClosed {
        attempt, receipts, ..
    } = &closed.kind
    else {
        panic!("expected StageClosed, got {:?}", closed.kind);
    };
    assert_eq!(*attempt, 1, "an unstated activation is the first one");
    let [OutcomeReceipt::Leaf { artifacts, .. }] = receipts.as_slice() else {
        panic!("expected one Leaf receipt: {receipts:?}");
    };
    assert_eq!(artifacts[0].name, "b.md");
    assert_eq!(
        artifacts[0].digest, "",
        "a name-only receipt records no content identity"
    );

    let recorded: Event = serde_json::from_str(
        r#"{"id":"e2","work":"w1","run":"r1","at":2,"kind":{"kind":"ClaimRecorded",
            "claim":"c1","claim_kind":"Done","verdict":"Validated"}}"#,
    )
    .expect("a pre-correction ClaimRecorded still deserializes");
    let EventKind::ClaimRecorded { artifacts, .. } = &recorded.kind else {
        panic!("expected ClaimRecorded, got {:?}", recorded.kind);
    };
    assert!(artifacts.is_empty());

    let held: Event = serde_json::from_str(
        r#"{"id":"e3","work":"w1","run":null,"at":3,"kind":{"kind":"StageHeld",
            "waypoint":"outer","missing":["a.md"]}}"#,
    )
    .expect("a pre-correction StageHeld still deserializes");
    let EventKind::StageHeld { attempt, .. } = &held.kind else {
        panic!("expected StageHeld, got {:?}", held.kind);
    };
    assert_eq!(*attempt, 1);
}
