//! D9 contract tests (0001 D9). Each test is named for its contract
//! number; #2, #5 (partial), and the World hash resume-key test are real;
//! #1, #3, #4, #6 are `#[should_panic]` stubs naming the contract stub and
//! the item that lifts them, per W2 build brief §3 W2 ("the stubs are
//! `#[should_panic(expected = "D9#<n> contract stub")]` tests: the suite
//! is green, the stub names its contract, and the item that implements it
//! removes the attribute and writes the assertion, inheriting its red").

use wirk_core::{
    Access, ActorWorld, ArtifactSpec, Boundary, Claim, ClaimId, ClaimKind, ClaimVerdict,
    DeterministicWorld, Event, EventId, EventKind, ExecutionTriple, FailureCause, OutputContract,
    RepositoryBinding, RouteId, Run, RunId, RunState, Timestamp, WaypointId, WorkId, WorkState,
    World, WorldHash,
};

fn triple(run_id: &str) -> ExecutionTriple {
    ExecutionTriple {
        estate_root: "/estate".to_string(),
        work_id: WorkId("work-1".to_string()),
        run_id: RunId(run_id.to_string()),
    }
}

fn open_run(run_id: &str) -> Run {
    Run {
        id: RunId(run_id.to_string()),
        waypoint: WaypointId("route-1/wp-1".to_string()),
        attempt: 1,
        world_hash: WorldHash("deadbeef".to_string()),
        state: RunState::Open,
    }
}

fn event(id: &str, run_id: Option<&str>, kind: EventKind) -> Event {
    Event {
        id: EventId(id.to_string()),
        work: WorkId("work-1".to_string()),
        run: run_id.map(|r| RunId(r.to_string())),
        at: Timestamp(0),
        kind,
    }
}

fn actor_world(repository: &str, branch: &str, base_sha: &str, worktree: &str) -> World {
    World::Actor(ActorWorld {
        repository: repository.to_string(),
        worktree_path: worktree.into(),
        branch: branch.to_string(),
        base_sha: base_sha.to_string(),
        triple: triple("run-1"),
        intent: "do the thing".to_string(),
        output_contract: OutputContract(vec![ArtifactSpec {
            name: "report.md".to_string(),
            required: true,
        }]),
        boundary: Boundary(vec!["src/**".to_string()]),
    })
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
    }
}

fn waypoint_reserved(waypoint: &str) -> EventKind {
    EventKind::WaypointReserved {
        waypoint: WaypointId(waypoint.to_string()),
        world_hash: WorldHash("deadbeef".to_string()),
        world: actor_world("wirk", "p1/journal", "abc123", "/var/tmp/w1"),
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

fn claim_recorded(claim: &str, kind: ClaimKind, verdict: ClaimVerdict) -> EventKind {
    EventKind::ClaimRecorded {
        claim: ClaimId(claim.to_string()),
        claim_kind: kind,
        verdict,
    }
}

/// D9#1 (0001 D9): "Journal replay rebuilds Work state, no in-memory
/// objects." Real: `fold(&events) -> Work` is a pure reducer over
/// `fold.md` §1's transition table (item 2, this build). Three cases per
/// `fold.md` §5, plus `LifecycleObserved` inert at Work level.
#[test]
fn d9_1_journal_replay_rebuilds_work_state() {
    // Case 1: completion on the last waypoint after Active on the
    // first; current_waypoint tracks reservation, not claim; a
    // LifecycleObserved event in between changes nothing (D9#2, inert
    // here too).
    let events = vec![
        event("ev-1", None, work_submitted(vec!["wp-1", "wp-2"])),
        event("ev-2", None, waypoint_reserved("wp-1")),
        event("ev-3", Some("run-1"), run_opened("run-1", "wp-1")),
        event(
            "ev-4",
            Some("run-1"),
            EventKind::RunLaunched {
                run: RunId("run-1".to_string()),
            },
        ),
        event(
            "ev-5",
            Some("run-1"),
            claim_recorded("claim-1", ClaimKind::Done, ClaimVerdict::Validated),
        ),
    ];
    let work = wirk_core::fold(&events);
    assert!(matches!(work.state, WorkState::Active), "{:?}", work.state);
    assert_eq!(work.current_waypoint, Some(WaypointId("wp-1".to_string())));

    let mut events = events;
    events.push(event(
        "ev-6",
        Some("run-1"),
        EventKind::LifecycleObserved {
            status: "idle".to_string(),
        },
    ));
    let work = wirk_core::fold(&events);
    assert!(
        matches!(work.state, WorkState::Active),
        "LifecycleObserved changed Work state: {:?}",
        work.state
    );

    events.push(event("ev-7", None, waypoint_reserved("wp-2")));
    events.push(event("ev-8", Some("run-2"), run_opened("run-2", "wp-2")));
    events.push(event(
        "ev-9",
        Some("run-2"),
        claim_recorded("claim-2", ClaimKind::Done, ClaimVerdict::Validated),
    ));
    let work = wirk_core::fold(&events);
    assert!(
        matches!(work.state, WorkState::Completed),
        "{:?}",
        work.state
    );

    // Case 2: a validated Question claim moves the Work to NeedsInput
    // (0027 D87).
    let question_events = vec![
        event("ev-1", None, work_submitted(vec!["wp-1"])),
        event("ev-2", None, waypoint_reserved("wp-1")),
        event("ev-3", Some("run-1"), run_opened("run-1", "wp-1")),
        event(
            "ev-4",
            Some("run-1"),
            claim_recorded(
                "claim-q",
                ClaimKind::Question("which base branch?".to_string()),
                ClaimVerdict::Validated,
            ),
        ),
    ];
    let work = wirk_core::fold(&question_events);
    assert!(
        matches!(work.state, WorkState::NeedsInput),
        "{:?}",
        work.state
    );

    // Case 3: an event for an unknown (never-opened) Run is folded
    // without panicking and without changing state.
    let unknown_run_events = vec![
        event("ev-1", None, work_submitted(vec!["wp-1"])),
        event("ev-2", None, waypoint_reserved("wp-1")),
        event(
            "ev-3",
            Some("run-unopened"),
            claim_recorded("claim-x", ClaimKind::Done, ClaimVerdict::Validated),
        ),
    ];
    let work = wirk_core::fold(&unknown_run_events);
    assert!(
        matches!(work.state, WorkState::Active),
        "unknown-Run event changed Work state: {:?}",
        work.state
    );
    assert_eq!(work.current_waypoint, Some(WaypointId("wp-1".to_string())));
}

/// D9#2 (0001 D9): "Lifecycle events never advance a Waypoint; only a
/// validated Claim does." Real: `LifecycleObserved` never changes
/// `RunState` (0001 D9 #2; 0017 D56); a `ClaimRecorded{Refused}` leaves the
/// Run open (D9#3: the Run stays open); `ClaimRecorded{Validated}` moves
/// the Run to `Claimed`. An event for another Run's id is ignored.
#[test]
fn d9_2_lifecycle_events_never_advance_a_waypoint() {
    let mut run = open_run("run-1");

    for status in ["idle", "done", "working"] {
        run.apply(&event(
            "ev-lifecycle",
            Some("run-1"),
            EventKind::LifecycleObserved {
                status: status.to_string(),
            },
        ));
        assert!(matches!(run.state, RunState::Open), "status {status}");
    }

    run.apply(&event(
        "ev-refused",
        Some("run-1"),
        EventKind::ClaimRecorded {
            claim: ClaimId("claim-refused".to_string()),
            claim_kind: ClaimKind::Done,
            verdict: ClaimVerdict::Refused(wirk_core::ClaimRefusal::MissingArtifact(
                "report.md".to_string(),
            )),
        },
    ));
    assert!(matches!(run.state, RunState::Open));

    // A validated Question claim is not completion (W3, issue 283): the
    // Run stays Open. The Work moving to NeedsInput is item 2's fold
    // over Work, not this Run-level reducer's concern.
    run.apply(&event(
        "ev-question",
        Some("run-1"),
        EventKind::ClaimRecorded {
            claim: ClaimId("claim-question".to_string()),
            claim_kind: ClaimKind::Question("which base branch?".to_string()),
            verdict: ClaimVerdict::Validated,
        },
    ));
    assert!(matches!(run.state, RunState::Open));

    run.apply(&event(
        "ev-validated",
        Some("run-1"),
        EventKind::ClaimRecorded {
            claim: ClaimId("claim-good".to_string()),
            claim_kind: ClaimKind::Done,
            verdict: ClaimVerdict::Validated,
        },
    ));
    match &run.state {
        RunState::Claimed(id) => assert_eq!(id, &ClaimId("claim-good".to_string())),
        other => panic!("expected Claimed, got {other:?}"),
    }

    // An event for another run id leaves state unchanged.
    let mut other_run = open_run("run-1");
    other_run.state = RunState::Claimed(ClaimId("claim-good".to_string()));
    other_run.apply(&event(
        "ev-elsewhere",
        Some("run-2"),
        EventKind::RunFailed {
            cause: FailureCause {
                status: Some("500".to_string()),
                request_id: None,
                at: Timestamp(1),
                detail: None,
            },
        },
    ));
    assert!(matches!(other_run.state, RunState::Claimed(_)));
}

/// D9#3 (0001 D9): "A Claim missing a required artifact is refused; the
/// Run stays open." Stub: `validate_claim`'s real body is item 3's "claim
/// validation and wirkd" (0023 D81; build-brief.md §2, J5 over R7). The
/// real assertion this lifts to: a `WaypointDefinition.declared_outputs`
/// entry with `required: true` absent from `Claim.artifacts` makes
/// `validate_claim` return `Refused(MissingArtifact)`, folding to
/// `RunState::Open`.
#[test]
#[should_panic(expected = "D9#3 D9#4 contract stub")]
fn d9_3_claim_missing_required_artifact_is_refused() {
    let waypoint = wirk_core::WaypointDefinition {
        id: WaypointId("route-1/wp-1".to_string()),
        kind: wirk_core::WaypointKind::Actor,
        declared_outputs: vec![ArtifactSpec {
            name: "report.md".to_string(),
            required: true,
        }],
    };
    let run = open_run("run-1");
    let claim = Claim {
        id: ClaimId("claim-1".to_string()),
        run: RunId("run-1".to_string()),
        triple: triple("run-1"),
        artifacts: vec![],
        kind: ClaimKind::Done,
    };
    let _verdict = wirk_core::validate_claim(&waypoint, &run, &claim);
}

/// D9#4 (0001 D9): "The injected triple round-trips through `wirk claim`
/// and is journal-validated; a fabricated one is recorded, not honored."
/// Stub: same body as D9#3 (`validate_claim`), item 3's territory. The
/// real assertion this lifts to: an `ExecutionTriple` wrapped in a `Claim`,
/// validated against the `Run` it names; a `run_id` mismatch yields
/// `ClaimRefusal::TripleMismatch`, folding to `ClaimRecorded{verdict:
/// Refused}`.
#[test]
#[should_panic(expected = "D9#3 D9#4 contract stub")]
fn d9_4_fabricated_triple_is_recorded_not_honored() {
    let waypoint = wirk_core::WaypointDefinition {
        id: WaypointId("route-1/wp-1".to_string()),
        kind: wirk_core::WaypointKind::Actor,
        declared_outputs: vec![],
    };
    let run = open_run("run-1");
    let claim = Claim {
        id: ClaimId("claim-1".to_string()),
        run: RunId("run-1".to_string()),
        // Fabricated: names a different run than the one it is filed
        // against.
        triple: triple("run-999"),
        artifacts: vec![],
        kind: ClaimKind::Done,
    };
    let _verdict = wirk_core::validate_claim(&waypoint, &run, &claim);
}

/// D9#5 (0001 D9): "A moved pane rebinds from `session.snapshot`; a
/// vanished pane ends unresolved, not complete." Real half:
/// `EventKind::RunVanished` folds to `RunState::Vanished`, never `Claimed`.
/// A late but valid claim arriving after Vanished is still honored (J1:
/// local, reversible, no contract crossed — `Run::apply`'s doc comment).
/// The real `terminal_id` rebind (executor-herdr.md D51) is wirk-herdr's,
/// item 4, out of scope here (BRIEF: "types and the trait only") — W3's.
#[test]
fn d9_5_vanished_run_ends_unresolved_not_complete() {
    let mut run = open_run("run-1");
    run.apply(&event("ev-vanished", Some("run-1"), EventKind::RunVanished));
    assert!(matches!(run.state, RunState::Vanished));
    assert!(!matches!(run.state, RunState::Claimed(_)));

    // A late but valid claim, arriving after Vanished, is still honored.
    run.apply(&event(
        "ev-late-claim",
        Some("run-1"),
        EventKind::ClaimRecorded {
            claim: ClaimId("claim-late".to_string()),
            claim_kind: ClaimKind::Done,
            verdict: ClaimVerdict::Validated,
        },
    ));
    match &run.state {
        RunState::Claimed(id) => assert_eq!(id, &ClaimId("claim-late".to_string())),
        other => panic!("expected Claimed after a late valid claim, got {other:?}"),
    }
}

/// D9#6 (0001 D9): "Worktree creation pins the exact base SHA; branch
/// retained after retirement." Stub half: the real assertion — the `git
/// worktree add` call pinning the SHA — is item 4's (0022 D77; build-brief
/// §2: "the executor", not item 5), out of scope here (BRIEF: "Any carve
/// of sergeant code (items 5, 9)"). This test proves the type carries the
/// exact string (`WorktreeCreated{repo, base_sha}`), then panics naming
/// item 4 as the item that lifts it. W2 build brief §7: "stays
/// should_panic" — it can no longer borrow `fold`'s own stub panic
/// (`fold` is real as of this item), so the panic is explicit here
/// instead (J1: local, reversible, test-only).
#[test]
#[should_panic(expected = "item 4 contract stub: worktree base_sha pin")]
fn d9_6_worktree_base_sha_pinned() {
    let exact_sha = "a1b2c3d4e5f60718293a4b5c6d7e8f9012345678";
    let created = event(
        "ev-worktree",
        Some("run-1"),
        EventKind::WorktreeCreated {
            repo: "wirk".to_string(),
            base_sha: exact_sha.to_string(),
        },
    );
    match &created.kind {
        EventKind::WorktreeCreated { base_sha, .. } => assert_eq!(base_sha, exact_sha),
        other => panic!("expected WorktreeCreated, got {other:?}"),
    }
    // The event carries the exact SHA (asserted above); the real pin — the
    // `git worktree add` call itself — is item 4's.
    panic!("item 4 contract stub: worktree base_sha pin");
}

/// Not a D9 number, but pins the resume key (world.md §2): the same
/// `ActorWorld` with a different `worktree_path` and a different `triple`
/// hashes equal (both excluded fields, world.md §2); a changed `base_sha`
/// hashes different; a `Deterministic` world with a reordered command
/// hashes different.
#[test]
fn world_hash_covers_content_not_location_or_identity() {
    let a = actor_world("wirk", "p1/executor-design", "abc123", "/var/tmp/w1");
    let mut b = actor_world("wirk", "p1/executor-design", "abc123", "/var/tmp/w2");
    if let World::Actor(actor) = &mut b {
        actor.triple = triple("run-different");
    }
    assert_eq!(WorldHash::of(&a), WorldHash::of(&b));

    let c = actor_world("wirk", "p1/executor-design", "def456", "/var/tmp/w1");
    assert_ne!(WorldHash::of(&a), WorldHash::of(&c));

    let det1 = World::Deterministic(DeterministicWorld {
        command: vec!["cargo".to_string(), "test".to_string()],
        cwd: "/var/tmp/w1".into(),
        env: Default::default(),
        expected_artifacts: OutputContract(vec![]),
    });
    let det2 = World::Deterministic(DeterministicWorld {
        command: vec!["test".to_string(), "cargo".to_string()],
        cwd: "/var/tmp/w1".into(),
        env: Default::default(),
        expected_artifacts: OutputContract(vec![]),
    });
    assert_ne!(WorldHash::of(&det1), WorldHash::of(&det2));

    // W2's finding (ASSESSMENT.md: "the reordered-command hash test
    // passes even without the separator, because that pair differs
    // anyway"): a word-boundary shift where concatenating without the
    // 0x1f separator would coincidentally produce the same bytes.
    // ["ab", "c"] and ["a", "bc"] both concatenate to "abc" with no
    // separator; with the separator they hash different.
    let det3 = World::Deterministic(DeterministicWorld {
        command: vec!["ab".to_string(), "c".to_string()],
        cwd: "/var/tmp/w1".into(),
        env: Default::default(),
        expected_artifacts: OutputContract(vec![]),
    });
    let det4 = World::Deterministic(DeterministicWorld {
        command: vec!["a".to_string(), "bc".to_string()],
        cwd: "/var/tmp/w1".into(),
        env: Default::default(),
        expected_artifacts: OutputContract(vec![]),
    });
    assert_ne!(WorldHash::of(&det3), WorldHash::of(&det4));
}
