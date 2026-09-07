//! `load_route` tests (p2-route-files W1, `orient/format.md` §5,
//! `orient/build-brief.md` §3 W1). Each writes a Route file into a
//! tempdir (R2: same `tempfile` fixture convention
//! `journal.rs`/`contracts.rs` already use, no host path in any
//! fixture text) and calls `wirk_core::load_route` directly — no
//! wirkd, no journal; the wire-level refusal-before-journal-write
//! property is `wirk/tests/route_files.rs`'s job.

use std::fs;

use tempfile::tempdir;
use wirk_core::{RouteError, load_route};

fn write(dir: &std::path::Path, name: &str, content: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    fs::write(&path, content).expect("write fixture");
    path
}

const VALID_TWO_WAYPOINT: &str = r#"{
  "id": "loader-test",
  "waypoints": [
    {
      "id": "loader-test/wp-1",
      "kind": "Actor",
      "intent": "write report.md",
      "declared_outputs": [{"name": "report.md", "required": true}],
      "boundary": ["**"]
    },
    {
      "id": "loader-test/wp-2",
      "kind": "Deterministic",
      "command": ["sh", "-c", "wc -l < report.md > summary.md"],
      "declared_outputs": [{"name": "summary.md", "required": true}]
    }
  ]
}"#;

#[test]
fn valid_route_file_loads() {
    let dir = tempdir().expect("tempdir");
    let path = write(dir.path(), "route.json", VALID_TWO_WAYPOINT);
    let route = load_route(&path).expect("valid route loads");
    assert_eq!(route.id.0, "loader-test");
    assert_eq!(route.waypoints.len(), 2);
    assert_eq!(route.waypoints[0].id.0, "loader-test/wp-1");
    assert_eq!(route.waypoints[1].id.0, "loader-test/wp-2");
}

#[test]
fn route_file_missing_is_refused() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("does-not-exist.json");
    let err = load_route(&path).expect_err("missing file is refused");
    assert!(
        matches!(err, RouteError::NotFound { .. }),
        "expected NotFound, got {err:?}"
    );
}

#[test]
fn route_file_malformed_json_is_refused() {
    let dir = tempdir().expect("tempdir");
    let path = write(
        dir.path(),
        "route.json",
        "{ \"id\": \"x\", \"waypoints\": [",
    );
    let err = load_route(&path).expect_err("truncated json is refused");
    assert!(
        matches!(err, RouteError::Malformed { .. }),
        "expected Malformed, got {err:?}"
    );
}

#[test]
fn route_file_unknown_field_is_refused() {
    let dir = tempdir().expect("tempdir");
    let content = r#"{
      "id": "unknown-field",
      "waypoints": [
        {
          "id": "unknown-field/wp-1",
          "kind": "Deterministic",
          "command": ["sh", "-c", "true"],
          "declared_outputs": [],
          "retries": 3
        }
      ]
    }"#;
    let path = write(dir.path(), "route.json", content);
    let err = load_route(&path).expect_err("an unknown field (D134 row 10) is refused");
    assert!(
        matches!(err, RouteError::Malformed { .. }),
        "expected Malformed (deny_unknown_fields), got {err:?}"
    );
}

#[test]
fn route_file_no_waypoints_is_refused() {
    let dir = tempdir().expect("tempdir");
    let content = r#"{ "id": "empty", "waypoints": [] }"#;
    let path = write(dir.path(), "route.json", content);
    let err = load_route(&path).expect_err("no waypoints is refused");
    assert!(
        matches!(err, RouteError::NoWaypoints),
        "expected NoWaypoints, got {err:?}"
    );
}

#[test]
fn route_file_duplicate_waypoint_id_is_refused() {
    let dir = tempdir().expect("tempdir");
    let content = r#"{
      "id": "dup",
      "waypoints": [
        {"id": "dup/wp-1", "kind": "Deterministic", "command": ["true"], "declared_outputs": []},
        {"id": "dup/wp-1", "kind": "Deterministic", "command": ["true"], "declared_outputs": []}
      ]
    }"#;
    let path = write(dir.path(), "route.json", content);
    let err = load_route(&path).expect_err("a duplicate waypoint id is refused");
    match err {
        RouteError::DuplicateWaypoint { id } => assert_eq!(id.0, "dup/wp-1"),
        other => panic!("expected DuplicateWaypoint, got {other:?}"),
    }
}

#[test]
fn route_file_actor_with_command_is_refused() {
    let dir = tempdir().expect("tempdir");
    let content = r#"{
      "id": "actor-cmd",
      "waypoints": [
        {
          "id": "actor-cmd/wp-1",
          "kind": "Actor",
          "intent": "do it",
          "command": ["sh", "-c", "true"],
          "declared_outputs": []
        }
      ]
    }"#;
    let path = write(dir.path(), "route.json", content);
    let err = load_route(&path).expect_err("an Actor Waypoint carrying a command is refused");
    match err {
        RouteError::ActorWithCommand { id } => assert_eq!(id.0, "actor-cmd/wp-1"),
        other => panic!("expected ActorWithCommand, got {other:?}"),
    }
}

#[test]
fn route_file_deterministic_missing_command_is_refused() {
    let dir = tempdir().expect("tempdir");
    let content = r#"{
      "id": "det-no-cmd",
      "waypoints": [
        {"id": "det-no-cmd/wp-1", "kind": "Deterministic", "declared_outputs": []}
      ]
    }"#;
    let path = write(dir.path(), "route.json", content);
    let err = load_route(&path).expect_err("a Deterministic Waypoint with no command is refused");
    match err {
        RouteError::DeterministicMissingCommand { id } => assert_eq!(id.0, "det-no-cmd/wp-1"),
        other => panic!("expected DeterministicMissingCommand, got {other:?}"),
    }
}

#[test]
fn route_file_bad_kind_string_is_refused() {
    let dir = tempdir().expect("tempdir");
    let content = r#"{
      "id": "bad-kind",
      "waypoints": [
        {"id": "bad-kind/wp-1", "kind": "Bogus", "declared_outputs": []}
      ]
    }"#;
    let path = write(dir.path(), "route.json", content);
    let err = load_route(&path).expect_err("an unrecognized kind string is refused");
    assert!(
        matches!(err, RouteError::Malformed { .. }),
        "expected Malformed (closed two-variant enum), got {err:?}"
    );
}

#[test]
fn route_file_actor_missing_intent_is_refused() {
    let dir = tempdir().expect("tempdir");
    let content = r#"{
      "id": "actor-no-intent",
      "waypoints": [
        {"id": "actor-no-intent/wp-1", "kind": "Actor", "declared_outputs": []}
      ]
    }"#;
    let path = write(dir.path(), "route.json", content);
    let err = load_route(&path).expect_err("an Actor Waypoint with no intent is refused");
    assert!(
        matches!(err, RouteError::Malformed { .. }),
        "expected Malformed (build-brief.md §2 Disagreement 3: row 2, same as Actor-with-command), got {err:?}"
    );
}

#[test]
fn route_file_output_with_empty_name_is_refused() {
    let dir = tempdir().expect("tempdir");
    let content = r#"{
      "id": "empty-name",
      "waypoints": [
        {
          "id": "empty-name/wp-1",
          "kind": "Deterministic",
          "command": ["true"],
          "declared_outputs": [{"name": "", "required": true}]
        }
      ]
    }"#;
    let path = write(dir.path(), "route.json", content);
    let err = load_route(&path).expect_err("a declared output with an empty name is refused");
    match err {
        RouteError::EmptyArtifactName { waypoint } => assert_eq!(waypoint.0, "empty-name/wp-1"),
        other => panic!("expected EmptyArtifactName, got {other:?}"),
    }
}

/// P3 native launch selection (BUILD-BRIEF.md item 1): an Actor
/// Waypoint's own `selection` (harness/model/effort/args) loads and
/// parses, the workflow-authored half of the contract this wave adds.
#[test]
fn route_file_actor_selection_loads() {
    let dir = tempdir().expect("tempdir");
    let content = r#"{
      "id": "with-selection",
      "waypoints": [
        {
          "id": "with-selection/wp-1",
          "kind": "Actor",
          "intent": "do it",
          "declared_outputs": [],
          "selection": {
            "harness": "codex",
            "model": "gpt-6-astra",
            "effort": "high",
            "args": ["--sandbox", "workspace-write"]
          }
        }
      ]
    }"#;
    let path = write(dir.path(), "route.json", content);
    let route = load_route(&path).expect("an Actor Waypoint's selection loads");
    let selection = route.waypoints[0]
        .selection
        .as_ref()
        .expect("selection is present");
    assert_eq!(
        selection.harness,
        Some(wirk_core::ActorKind("codex".to_string()))
    );
    assert_eq!(selection.model.as_deref(), Some("gpt-6-astra"));
    assert_eq!(selection.effort.as_deref(), Some("high"));
    assert_eq!(
        selection.args,
        vec!["--sandbox".to_string(), "workspace-write".to_string()]
    );
}

/// A Waypoint with no `selection` at all still loads clean —
/// `selection` is additive, same posture as `boundary`'s own empty
/// case just above.
#[test]
fn route_file_no_selection_is_allowed() {
    let dir = tempdir().expect("tempdir");
    let content = r#"{
      "id": "no-selection",
      "waypoints": [
        {"id": "no-selection/wp-1", "kind": "Actor", "intent": "do it", "declared_outputs": []}
      ]
    }"#;
    let path = write(dir.path(), "route.json", content);
    let route = load_route(&path).expect("no selection loads clean");
    assert_eq!(route.waypoints[0].selection, None);
}

/// P3 native launch selection (BUILD-BRIEF.md item 1: "Reject
/// actor-only configuration on non-Actor stages where it would
/// otherwise be silently unused"): a Deterministic Waypoint carries no
/// Run to launch, so an authored `selection` there would never be
/// read by anything — refused at load, not silently accepted and
/// dropped.
#[test]
fn route_file_deterministic_with_selection_is_refused() {
    let dir = tempdir().expect("tempdir");
    let content = r#"{
      "id": "det-selection",
      "waypoints": [
        {
          "id": "det-selection/wp-1",
          "kind": "Deterministic",
          "command": ["true"],
          "declared_outputs": [],
          "selection": {"model": "gpt-6-astra"}
        }
      ]
    }"#;
    let path = write(dir.path(), "route.json", content);
    let err =
        load_route(&path).expect_err("a Deterministic Waypoint carrying selection is refused");
    match err {
        RouteError::ActorSelectionOnNonActor { id } => assert_eq!(id.0, "det-selection/wp-1"),
        other => panic!("expected ActorSelectionOnNonActor, got {other:?}"),
    }
}

/// Same refusal, `Container` side: a container has no Run of its own
/// either (`ContainerWithMechanism` already refuses `intent`/`command`/
/// `boundary` there for the same reason).
#[test]
fn route_file_container_with_selection_is_refused() {
    let dir = tempdir().expect("tempdir");
    let content = r#"{
      "id": "container-selection",
      "waypoints": [
        {
          "id": "container-selection/wp-1",
          "kind": "Container",
          "declared_outputs": [],
          "selection": {"model": "gpt-6-astra"},
          "leaves": [
            {"id": "container-selection/wp-1/leaf", "kind": "Deterministic", "command": ["true"], "declared_outputs": []}
          ]
        }
      ]
    }"#;
    let path = write(dir.path(), "route.json", content);
    let err = load_route(&path).expect_err("a Container carrying selection is refused");
    match err {
        RouteError::ActorSelectionOnNonActor { id } => assert_eq!(id.0, "container-selection/wp-1"),
        other => panic!("expected ActorSelectionOnNonActor, got {other:?}"),
    }
}

#[test]
fn route_file_empty_boundary_is_allowed() {
    let dir = tempdir().expect("tempdir");
    let content = r#"{
      "id": "empty-boundary",
      "waypoints": [
        {"id": "empty-boundary/wp-1", "kind": "Deterministic", "command": ["true"], "declared_outputs": []}
      ]
    }"#;
    let path = write(dir.path(), "route.json", content);
    let route = load_route(&path)
        .expect("an empty/omitted boundary loads clean (refusal 9: allowed, P2.4 enforces)");
    assert!(route.waypoints[0].boundary.0.is_empty());
}
