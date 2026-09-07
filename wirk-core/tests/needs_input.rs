//! P2.3 W1 (0033 D102; 0044; states.md §1): a failed, vanished, or
//! stuck Run surfaces the Work as `NeedsInput` with its cause,
//! guarded the same way every other terminal-respecting fold arm is.
//! `wirk-core/tests/contracts.rs` already exercises the Question path
//! (`d9_1_journal_replay_rebuilds_work_state`'s Case 2); this file
//! pins the three new arms plus the terminal guard.

use wirk_core::{
    Access, EventKind, FailureCause, NeedsInputCause, RepositoryBinding, RouteId, RunId, Timestamp,
    WaypointId, WorkId, WorkState, World, WorldHash,
};

fn event(id: &str, work: &str, run: Option<&str>, at: i64, kind: EventKind) -> wirk_core::Event {
    wirk_core::Event {
        id: wirk_core::EventId(id.to_string()),
        work: WorkId(work.to_string()),
        run: run.map(|r| RunId(r.to_string())),
        at: Timestamp(at),
        kind,
    }
}

fn work_submitted(waypoints: Vec<&str>) -> EventKind {
    EventKind::WorkSubmitted {
        route: RouteId("route-1".to_string()),
        repositories: vec![RepositoryBinding {
            name: "wirk".to_string(),
            access: Access::Write,
        }],
        intent: "do the thing".to_string(),
        waypoints: waypoints
            .into_iter()
            .map(|wp| WaypointId(wp.to_string()))
            .collect(),
        waypoint_defs: Vec::new(),
        parent: None,
        execution_repo: None,
        execution_identity: None,
    }
}

fn waypoint_reserved(waypoint: &str) -> EventKind {
    EventKind::WaypointReserved {
        waypoint: WaypointId(waypoint.to_string()),
        world_hash: WorldHash("deadbeef".to_string()),
        world: World::Deterministic(wirk_core::DeterministicWorld {
            command: vec!["true".to_string()],
            base_sha: "abc123".to_string(),
            source_basis: wirk_core::SourceBasis::OutputOnly {
                reference: "abc123".to_string(),
            },
            cwd: std::path::PathBuf::from("/var/tmp/w1"),
            env: std::collections::BTreeMap::new(),
            expected_artifacts: wirk_core::OutputContract(Vec::new()),
        }),
    }
}

fn run_opened(run: &str, waypoint: &str) -> EventKind {
    EventKind::RunOpened {
        run: RunId(run.to_string()),
        waypoint: WaypointId(waypoint.to_string()),
        attempt: 1,
        world_hash: WorldHash("deadbeef".to_string()),
    }
}

fn base_events() -> Vec<wirk_core::Event> {
    vec![
        event("ev-1", "work-1", None, 0, work_submitted(vec!["wp-1"])),
        event("ev-2", "work-1", None, 0, waypoint_reserved("wp-1")),
        event(
            "ev-3",
            "work-1",
            Some("run-1"),
            0,
            run_opened("run-1", "wp-1"),
        ),
    ]
}

/// A `RunFailed` on a non-terminal Work moves it to `NeedsInput` and
/// carries the cause: which Run, `reason == "run_failed"`, and the
/// failure's own `detail` verbatim (states.md §1). Red before this
/// wave: `RunFailed` folded inert (`EventKind::RunFailed { .. } => {}`).
#[test]
fn fold_run_failed_moves_work_to_needs_input_with_cause() {
    let mut events = base_events();
    events.push(event(
        "ev-4",
        "work-1",
        Some("run-1"),
        1,
        EventKind::RunFailed {
            cause: FailureCause {
                status: Some("1".to_string()),
                request_id: None,
                at: Timestamp(1),
                detail: Some("exit 1: command failed".to_string()),
            },
        },
    ));
    let work = wirk_core::fold(&events);
    assert!(
        matches!(work.state, WorkState::NeedsInput),
        "{:?}",
        work.state
    );
    let cause = work
        .needs_input
        .expect("needs_input must be Some after a RunFailed");
    assert_eq!(cause.run, RunId("run-1".to_string()));
    assert_eq!(cause.reason, "run_failed");
    assert_eq!(cause.detail, "exit 1: command failed");
}

/// A `RunVanished` on a non-terminal Work moves it to `NeedsInput` too
/// (states.md §1: "an actor Run that vanished" is the same surfacing
/// as a failed one). Red before: `RunVanished` folded inert.
#[test]
fn fold_run_vanished_moves_work_to_needs_input() {
    let mut events = base_events();
    events.push(event(
        "ev-4",
        "work-1",
        Some("run-1"),
        1,
        EventKind::RunVanished,
    ));
    let work = wirk_core::fold(&events);
    assert!(
        matches!(work.state, WorkState::NeedsInput),
        "{:?}",
        work.state
    );
    let cause = work
        .needs_input
        .expect("needs_input must be Some after a RunVanished");
    assert_eq!(cause.run, RunId("run-1".to_string()));
    assert_eq!(cause.reason, "run_vanished");
    assert!(
        !cause.detail.is_empty(),
        "a vanished Run's cause must carry a human-readable detail"
    );
}

/// A validated Question claim already moved the Work to `NeedsInput`
/// (0027 D87, pinned by `contracts.rs`'s
/// `d9_1_journal_replay_rebuilds_work_state`); this wave gives it the
/// same `needs_input` field the other two causes get. Red before: the
/// field didn't exist.
#[test]
fn fold_question_claim_populates_needs_input_cause() {
    let mut events = base_events();
    events.push(event(
        "ev-4",
        "work-1",
        Some("run-1"),
        1,
        EventKind::ClaimRecorded {
            artifacts: Vec::new(),
            claim: wirk_core::ClaimId("claim-q".to_string()),
            claim_kind: wirk_core::ClaimKind::Question("which base branch?".to_string()),
            verdict: wirk_core::ClaimVerdict::Validated,
        },
    ));
    let work = wirk_core::fold(&events);
    assert!(
        matches!(work.state, WorkState::NeedsInput),
        "{:?}",
        work.state
    );
    let cause = work
        .needs_input
        .expect("needs_input must be Some after a validated Question");
    assert_eq!(cause.run, RunId("run-1".to_string()));
    assert_eq!(cause.reason, "question");
    assert_eq!(cause.detail, "which base branch?");
}

/// Guard probe (states.md §4): once a Work is terminal (`WorkFailed`
/// fired first), a later `RunFailed` on the same or a different Run
/// changes neither `state` nor `needs_input` — the same
/// `is_terminal()` gate every other non-terminal arm already uses.
/// Named to prove the guard extends to the new arms, not assumed to
/// pass; watched alongside the other three (already-passing here
/// would itself be a finding if the guard were missing).
#[test]
fn fold_terminal_work_ignores_a_later_run_failed() {
    let mut events = base_events();
    events.push(event(
        "ev-4",
        "work-1",
        Some("run-1"),
        1,
        EventKind::WorkFailed {
            cause: FailureCause {
                status: None,
                request_id: None,
                at: Timestamp(1),
                detail: Some("boundary violation".to_string()),
            },
        },
    ));
    events.push(event(
        "ev-5",
        "work-1",
        Some("run-1"),
        2,
        EventKind::RunFailed {
            cause: FailureCause {
                status: Some("1".to_string()),
                request_id: None,
                at: Timestamp(2),
                detail: Some("a later, unrelated failure".to_string()),
            },
        },
    ));
    let work = wirk_core::fold(&events);
    assert!(
        matches!(work.state, WorkState::Failed),
        "a terminal Work must stay terminal: {:?}",
        work.state
    );
    assert!(
        work.needs_input.is_none(),
        "a RunFailed after WorkFailed must not populate needs_input: {:?}",
        work.needs_input
    );
}

/// P2.3 W1 probe (states.md, `#[serde(default)]`): a `Work` value
/// serialized before `needs_input` existed still deserializes, with
/// the field defaulting to `None` — `Work` is never itself journaled
/// (only rebuilt fresh by `fold` on every read), so this is the only
/// place the back-compat claim can be pinned.
#[test]
fn old_work_json_without_needs_input_still_deserializes() {
    let pre_existing_json = r#"{
        "id": "work-1",
        "intent": "do the thing",
        "route": "route-1",
        "repositories": [],
        "state": "active",
        "current_waypoint": null,
        "last_activity": 0
    }"#;
    let work: wirk_core::Work = serde_json::from_str(pre_existing_json)
        .expect("a Work JSON written before needs_input existed still deserializes");
    assert_eq!(work.needs_input, None);
}

/// P2.3 W2 (decide.md §1): a `RunOpened` for a fresh Run on a
/// `NeedsInput` Work clears it back to `Active` — the retry verb's own
/// journal write, `fold`'s side of the decision. Red before this wave:
/// `RunOpened`'s arm only touched `run_waypoints`, never `state`/
/// `needs_input`.
#[test]
fn fold_run_opened_clears_needs_input_on_retry() {
    let mut events = base_events();
    events.push(event(
        "ev-4",
        "work-1",
        Some("run-1"),
        1,
        EventKind::RunFailed {
            cause: FailureCause {
                status: Some("1".to_string()),
                request_id: None,
                at: Timestamp(1),
                detail: Some("exit 1: command failed".to_string()),
            },
        },
    ));
    let needs_input_work = wirk_core::fold(&events);
    assert!(matches!(needs_input_work.state, WorkState::NeedsInput));

    events.push(event(
        "ev-5",
        "work-1",
        Some("run-2"),
        2,
        run_opened("run-2", "wp-1"),
    ));
    let retried_work = wirk_core::fold(&events);
    assert!(
        matches!(retried_work.state, WorkState::Active),
        "{:?}",
        retried_work.state
    );
    assert_eq!(
        retried_work.needs_input, None,
        "a retry's RunOpened must clear needs_input"
    );
}

/// P2.3 W2 hazard (decide.md §5, build-brief.md §7): the same
/// `RunOpened` arm is a no-op when the Work is not `NeedsInput` —
/// auto-advance's own `RunOpened` (handle_claim, on a Validated Done
/// claim advancing to the next Waypoint) fires only while the Work is
/// already `Active`, and this probe confirms that path's own
/// `RunOpened` never trips the retry-clearing guard by accident: state
/// stays `Active` throughout, no `needs_input` ever appears.
#[test]
fn fold_run_opened_auto_advance_is_a_noop_on_needs_input() {
    let mut events = base_events();
    let active_work = wirk_core::fold(&events);
    assert!(matches!(active_work.state, WorkState::Active));

    // The shape auto-advance itself appends: a further WaypointReserved
    // + RunOpened pair for the next Waypoint, while the Work is Active,
    // never NeedsInput.
    events.push(event("ev-4", "work-1", None, 1, waypoint_reserved("wp-2")));
    events.push(event(
        "ev-5",
        "work-1",
        Some("run-2"),
        1,
        run_opened("run-2", "wp-2"),
    ));
    let advanced_work = wirk_core::fold(&events);
    assert!(
        matches!(advanced_work.state, WorkState::Active),
        "{:?}",
        advanced_work.state
    );
    assert_eq!(advanced_work.needs_input, None);
}

/// Sanity check that `NeedsInputCause` itself is `PartialEq` (used
/// above via `Option::expect`/`assert_eq!`) — not a red-before test,
/// pins the derive.
#[test]
fn needs_input_cause_is_comparable() {
    let a = NeedsInputCause {
        run: RunId("run-1".to_string()),
        reason: "run_failed".to_string(),
        detail: "x".to_string(),
    };
    let b = a.clone();
    assert_eq!(a, b);
}

// ---- P2.6 W2 (ruling 0052 D156) --------------------------------------
//
// A `Blocked` pane is an actor waiting on a human, not a failed Run —
// distinct from `RunFailed`/`RunVanished` above (0049 D147 amended).
// Red before this wave: `EventKind::LifecycleObserved { .. } => {}`
// (wholly inert at the Work level, D9#2's own comment).

/// (a) A `LifecycleObserved{Blocked}` on a non-terminal Work moves it
/// to `NeedsInput` carrying the reason `"blocked"` and the event's own
/// `detail` (the pane and its last screen lines, journaled by the
/// loop) verbatim — the same shape `RunFailed`/`RunVanished` already
/// get, guarded by `is_terminal()` the same way. Probed by hand
/// (BUILD.md): reverting the fold's `"Blocked"` arm to a no-op (the
/// pre-wave inert shape) makes this fail — the Work stays `Active`.
#[test]
fn fold_lifecycle_blocked_moves_work_to_needs_input_with_screen_lines() {
    let mut events = base_events();
    events.push(event(
        "ev-4",
        "work-1",
        Some("run-1"),
        1,
        EventKind::LifecycleObserved {
            status: "Blocked".to_string(),
            detail: Some(
                "the actor is waiting on its pane w1:p1:\n\
                 ┃ Permission required\n\
                 ┃ Access external directory /tmp"
                    .to_string(),
            ),
        },
    ));
    let work = wirk_core::fold(&events);
    assert!(
        matches!(work.state, WorkState::NeedsInput),
        "{:?}",
        work.state
    );
    let cause = work
        .needs_input
        .expect("needs_input must be Some after a Blocked observation");
    assert_eq!(cause.run, RunId("run-1".to_string()));
    assert_eq!(cause.reason, "blocked");
    assert!(
        cause.detail.contains("w1:p1") && cause.detail.contains("Permission required"),
        "cause.detail must carry the pane and its last screen lines verbatim: {:?}",
        cause.detail
    );
}

/// (b) A later `LifecycleObserved{Working}` — the same event kind the
/// loop already journals for every status — clears a `"blocked"`
/// `NeedsInput` back to `Active` with `needs_input` reset to `None`.
/// Red before this wave: `LifecycleObserved` folded inert, so the Work
/// never left `NeedsInput` at all (there was nothing to clear it).
#[test]
fn fold_lifecycle_working_clears_a_blocked_needs_input() {
    let mut events = base_events();
    events.push(event(
        "ev-4",
        "work-1",
        Some("run-1"),
        1,
        EventKind::LifecycleObserved {
            status: "Blocked".to_string(),
            detail: Some("waiting on pane w1:p1".to_string()),
        },
    ));
    let blocked_work = wirk_core::fold(&events);
    assert!(matches!(blocked_work.state, WorkState::NeedsInput));

    events.push(event(
        "ev-5",
        "work-1",
        Some("run-1"),
        2,
        EventKind::LifecycleObserved {
            status: "Working".to_string(),
            detail: None,
        },
    ));
    let resolved_work = wirk_core::fold(&events);
    assert!(
        matches!(resolved_work.state, WorkState::Active),
        "{:?}",
        resolved_work.state
    );
    assert_eq!(
        resolved_work.needs_input, None,
        "a Working observation must clear a blocked needs_input"
    );
}

/// Guard: a `Working` observation must not clear a `NeedsInput` caused
/// by something else (a validated Question here) — only a `"blocked"`
/// cause is this arm's to clear. Otherwise an unrelated pane going
/// Working on the same Run could silently discard a human's still-open
/// Question.
#[test]
fn fold_lifecycle_working_does_not_clear_a_non_blocked_needs_input() {
    let mut events = base_events();
    events.push(event(
        "ev-4",
        "work-1",
        Some("run-1"),
        1,
        EventKind::ClaimRecorded {
            artifacts: Vec::new(),
            claim: wirk_core::ClaimId("claim-q".to_string()),
            claim_kind: wirk_core::ClaimKind::Question("which base branch?".to_string()),
            verdict: wirk_core::ClaimVerdict::Validated,
        },
    ));
    events.push(event(
        "ev-5",
        "work-1",
        Some("run-1"),
        2,
        EventKind::LifecycleObserved {
            status: "Working".to_string(),
            detail: None,
        },
    ));
    let work = wirk_core::fold(&events);
    assert!(
        matches!(work.state, WorkState::NeedsInput),
        "a Working observation must not clear a Question's needs_input: {:?}",
        work.state
    );
    assert_eq!(
        work.needs_input.expect("still needs_input").reason,
        "question"
    );
}

// ---- loop-a-reverify (22-rvF1-legitimate-completion.log; a second,
// independently reproduced coordinator report) --------------------------
//
// A resolved, historical `needs_input` cause must not be read back as
// the Work's *current* status once `state` has moved off `NeedsInput`
// for a real, later reason. `fold_run_opened_clears_needs_input_on_retry`
// above already pins the one arm that got this right; these four pin
// the arms that didn't.

/// A `RunVanished` NeedsInput resolved by a later, legitimate Claim on
/// the same Run (D9#5: a late Claim after `RunVanished` is honored, not
/// stale) must not leave the stale `run_vanished` cause behind once the
/// Work is `Completed`. Red before this wave: `ClaimRecorded`'s
/// `(Validated, Done)` arm moved `state` off `NeedsInput` without
/// touching `needs_input`.
#[test]
fn fold_claim_completion_after_run_vanished_clears_needs_input() {
    let mut events = base_events();
    events.push(event(
        "ev-4",
        "work-1",
        Some("run-1"),
        1,
        EventKind::RunVanished,
    ));
    let vanished_work = wirk_core::fold(&events);
    assert!(matches!(vanished_work.state, WorkState::NeedsInput));

    events.push(event(
        "ev-5",
        "work-1",
        Some("run-1"),
        2,
        EventKind::ClaimRecorded {
            artifacts: Vec::new(),
            claim: wirk_core::ClaimId("claim-late".to_string()),
            claim_kind: wirk_core::ClaimKind::Done,
            verdict: wirk_core::ClaimVerdict::Validated,
        },
    ));
    let work = wirk_core::fold(&events);
    assert!(
        matches!(work.state, WorkState::Completed),
        "a validated Done claim on the last waypoint completes the Work: {:?}",
        work.state
    );
    assert_eq!(
        work.needs_input, None,
        "a resolved run_vanished cause must not survive the Work's legitimate completion"
    );
}

/// The same contract for `StageClosed`: a Work recovering from
/// `NeedsInput` through a container's own closure must not keep
/// reporting the earlier cause once `state` has moved off `NeedsInput`.
/// Red before this wave: `StageClosed`'s arm moved `state` without
/// touching `needs_input`.
#[test]
fn fold_stage_closed_after_run_vanished_clears_needs_input() {
    let mut events = base_events();
    events.push(event(
        "ev-4",
        "work-1",
        Some("run-1"),
        1,
        EventKind::RunVanished,
    ));
    events.push(event(
        "ev-5",
        "work-1",
        None,
        2,
        EventKind::StageClosed {
            waypoint: WaypointId("wp-1".to_string()),
            attempt: 1,
            receipts: Vec::new(),
        },
    ));
    let work = wirk_core::fold(&events);
    assert!(
        !matches!(work.state, WorkState::NeedsInput),
        "a StageClosed must move the Work off NeedsInput: {:?}",
        work.state
    );
    assert_eq!(
        work.needs_input, None,
        "a resolved run_vanished cause must not survive a StageClosed"
    );
}

/// The exact second bug report (loop-a-reverify): a Work canceled while
/// `NeedsInput` for an `out_of_boundary` refusal must not keep
/// reporting that stale, unrelated cause once it is `Canceled` — a
/// coordinator reading `wirk work status` after `wirk work cancel` must
/// not see a resolved refusal as if it were current. Red before this
/// wave: `WorkCanceled`'s arm set `state` without touching
/// `needs_input`.
#[test]
fn fold_work_canceled_clears_a_stale_out_of_boundary_needs_input() {
    let mut events = base_events();
    events.push(event(
        "ev-4",
        "work-1",
        Some("run-1"),
        1,
        EventKind::ClaimRecorded {
            artifacts: Vec::new(),
            claim: wirk_core::ClaimId("claim-oob".to_string()),
            claim_kind: wirk_core::ClaimKind::Done,
            verdict: wirk_core::ClaimVerdict::Refused(wirk_core::ClaimRefusal::OutOfBoundary(
                "/etc/passwd".to_string(),
            )),
        },
    ));
    let needs_input_work = wirk_core::fold(&events);
    assert!(matches!(needs_input_work.state, WorkState::NeedsInput));
    assert_eq!(
        needs_input_work
            .needs_input
            .as_ref()
            .map(|c| c.reason.as_str()),
        Some("out_of_boundary")
    );

    events.push(event(
        "ev-5",
        "work-1",
        None,
        2,
        EventKind::WorkCanceled {
            reason: Some("operator canceled".to_string()),
            caused_by: None,
        },
    ));
    let work = wirk_core::fold(&events);
    assert!(matches!(work.state, WorkState::Canceled));
    assert_eq!(
        work.needs_input, None,
        "a canceled Work must not still report an earlier, unrelated out_of_boundary refusal"
    );
}

/// The same contract for `WorkFailed`: a Work explicitly failed while
/// `NeedsInput` must not keep reporting the earlier, unrelated cause
/// once it is `Failed`.
#[test]
fn fold_work_failed_clears_a_stale_needs_input() {
    let mut events = base_events();
    events.push(event(
        "ev-4",
        "work-1",
        Some("run-1"),
        1,
        EventKind::RunVanished,
    ));
    let needs_input_work = wirk_core::fold(&events);
    assert!(matches!(needs_input_work.state, WorkState::NeedsInput));

    events.push(event(
        "ev-5",
        "work-1",
        None,
        2,
        EventKind::WorkFailed {
            cause: FailureCause {
                status: None,
                request_id: None,
                at: Timestamp(2),
                detail: Some("operator failed the work".to_string()),
            },
        },
    ));
    let work = wirk_core::fold(&events);
    assert!(matches!(work.state, WorkState::Failed));
    assert_eq!(
        work.needs_input, None,
        "a failed Work must not still report an earlier, unrelated run_vanished cause"
    );
}

/// Guard probe (states.md §4, mirroring `fold_terminal_work_ignores_a_later_run_failed`):
/// once a Work is terminal, a later `Blocked` observation changes
/// neither `state` nor `needs_input`.
#[test]
fn fold_terminal_work_ignores_a_later_blocked_observation() {
    let mut events = base_events();
    events.push(event(
        "ev-4",
        "work-1",
        Some("run-1"),
        1,
        EventKind::WorkFailed {
            cause: FailureCause {
                status: None,
                request_id: None,
                at: Timestamp(1),
                detail: Some("boundary violation".to_string()),
            },
        },
    ));
    events.push(event(
        "ev-5",
        "work-1",
        Some("run-1"),
        2,
        EventKind::LifecycleObserved {
            status: "Blocked".to_string(),
            detail: Some("waiting on pane w1:p1".to_string()),
        },
    ));
    let work = wirk_core::fold(&events);
    assert!(
        matches!(work.state, WorkState::Failed),
        "a terminal Work must stay terminal: {:?}",
        work.state
    );
    assert!(
        work.needs_input.is_none(),
        "a Blocked observation after WorkFailed must not populate needs_input: {:?}",
        work.needs_input
    );
}
