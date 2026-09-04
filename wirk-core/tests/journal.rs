//! `Journal` store tests (W1, `orient/store.md` §7). The decisive check
//! for the store: round trip, truncated-last-line and seq-discontinuity
//! corruption, and an empty directory replaying to an empty `Vec`. All
//! deterministic — corruption is direct byte surgery on the fixture
//! file, nothing waits on a clock or a process (BRIEF.md's decisive
//! check).

use std::fs;

use tempfile::tempdir;
use wirk_core::{
    Access, ClaimId, ClaimKind, ClaimVerdict, DeterministicWorld, Event, EventId, EventKind,
    Journal, JournalError, OutputContract, RepositoryBinding, RouteId, RunId, Timestamp,
    WaypointId, WorkId, World, WorldHash,
};

fn lifecycle_event(id: &str, work: &str, run: &str, status: &str) -> Event {
    Event {
        id: EventId(id.to_string()),
        work: WorkId(work.to_string()),
        run: Some(RunId(run.to_string())),
        at: Timestamp(0),
        kind: EventKind::LifecycleObserved {
            status: status.to_string(),
        },
    }
}

fn claim_recorded_event(id: &str, work: &str, run: &str, claim: &str) -> Event {
    Event {
        id: EventId(id.to_string()),
        work: WorkId(work.to_string()),
        run: Some(RunId(run.to_string())),
        at: Timestamp(1),
        kind: EventKind::ClaimRecorded {
            claim: ClaimId(claim.to_string()),
            claim_kind: ClaimKind::Done,
            verdict: ClaimVerdict::Validated,
        },
    }
}

/// Compares two `Event`s by their serialized JSON value rather than a
/// derived `PartialEq` — `Event`/`EventKind` carry none today (0027),
/// and adding one is out of this item's allow-list scope (R1: this one
/// call gets the same answer without touching those derives).
fn events_equal(a: &Event, b: &Event) -> bool {
    serde_json::to_value(a).unwrap() == serde_json::to_value(b).unwrap()
}

fn events_vecs_equal(a: &[Event], b: &[Event]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| events_equal(x, y))
}

/// Decisive check (BRIEF.md:30-33, store.md §7): "events appended by one
/// `Journal` value, the value dropped, a new `Journal` opened on the
/// same directory replays to an identical" sequence of events.
#[test]
fn round_trip_replay_equals_appended_events() {
    let dir = tempdir().expect("tempdir");
    let events = vec![
        lifecycle_event("ev-1", "work-1", "run-1", "working"),
        claim_recorded_event("ev-2", "work-1", "run-1", "claim-1"),
        lifecycle_event("ev-3", "work-1", "run-1", "working"),
    ];

    {
        let mut journal = Journal::open(dir.path()).expect("open");
        for event in &events {
            journal.append(event).expect("append");
        }
        // journal dropped here, closing the file handle.
    }

    let journal = Journal::open(dir.path()).expect("reopen");
    let replayed = journal.replay().expect("replay");
    assert!(
        events_vecs_equal(&events, &replayed),
        "replay did not equal the appended events: {replayed:?}"
    );
}

/// `Journal::append` mints a ULID into `event.id` when the caller leaves
/// it empty (store.md §2, §3); an event with a non-empty id passes
/// through unchanged, which is what `round_trip_replay_equals_appended_events`
/// above relies on for exact equality.
#[test]
fn append_fills_an_empty_event_id_with_a_ulid() {
    let dir = tempdir().expect("tempdir");
    let mut journal = Journal::open(dir.path()).expect("open");
    let blank = Event {
        id: EventId(String::new()),
        work: WorkId("work-1".to_string()),
        run: Some(RunId("run-1".to_string())),
        at: Timestamp(0),
        kind: EventKind::LifecycleObserved {
            status: "working".to_string(),
        },
    };
    journal.append(&blank).expect("append");

    let replayed = journal.replay().expect("replay");
    assert_eq!(replayed.len(), 1);
    let filled = &replayed[0].id.0;
    assert!(!filled.is_empty(), "id was not filled");
    assert_eq!(
        filled.len(),
        26,
        "not a 26-char Crockford-base32 ULID: {filled}"
    );
}

/// Decisive check for §5: a truncated last line is reported, not
/// silently skipped. Byte surgery on the fixture file after the
/// `Journal` that wrote it is dropped — the same failure shape a real
/// crash leaves (a torn last write), deterministic and instant.
#[test]
fn truncated_last_line_reports_malformed() {
    let dir = tempdir().expect("tempdir");
    {
        let mut journal = Journal::open(dir.path()).expect("open");
        journal
            .append(&lifecycle_event("ev-1", "work-1", "run-1", "working"))
            .expect("append 1");
        journal
            .append(&lifecycle_event("ev-2", "work-1", "run-1", "working"))
            .expect("append 2");
    }

    let path = dir.path().join("journal.ndjson");
    let contents = fs::read(&path).expect("read fixture");
    // Cut into the last line's JSON, well short of its closing brace —
    // leaves an unparsable torn line, not an empty trailing one.
    let truncated = &contents[..contents.len() - 12];
    fs::write(&path, truncated).expect("truncate fixture");

    let journal = Journal::open(dir.path());
    let err = match journal {
        Ok(journal) => journal
            .replay()
            .expect_err("replay should fail on the torn line"),
        Err(err) => err,
    };
    match err {
        JournalError::Malformed { line, .. } => assert_eq!(line, 2, "wrong line reported"),
        other => panic!("expected Malformed, got {other:?}"),
    }
}

/// Decisive check for §5: a sequence gap is reported, not silently
/// accepted. Byte surgery on a mid-file `seq` field.
#[test]
fn seq_discontinuity_is_reported() {
    let dir = tempdir().expect("tempdir");
    {
        let mut journal = Journal::open(dir.path()).expect("open");
        journal
            .append(&lifecycle_event("ev-1", "work-1", "run-1", "working"))
            .expect("append 1");
        journal
            .append(&lifecycle_event("ev-2", "work-1", "run-1", "working"))
            .expect("append 2");
        journal
            .append(&lifecycle_event("ev-3", "work-1", "run-1", "working"))
            .expect("append 3");
    }

    let path = dir.path().join("journal.ndjson");
    let contents = fs::read_to_string(&path).expect("read fixture");
    // Line 2 carries seq 2; corrupt it to 5, leaving a gap the reader
    // must report rather than silently continue past.
    let mangled = contents.replacen("\"seq\":2,", "\"seq\":5,", 1);
    assert_ne!(contents, mangled, "fixture line 2 was not found to mangle");
    fs::write(&path, mangled).expect("mangle fixture");

    let journal = Journal::open(dir.path());
    let err = match journal {
        Ok(journal) => journal
            .replay()
            .expect_err("replay should fail on the seq gap"),
        Err(err) => err,
    };
    match err {
        JournalError::SeqDiscontinuity {
            line,
            expected,
            found,
        } => {
            assert_eq!(line, 2);
            assert_eq!(expected, 2);
            assert_eq!(found, 5);
        }
        other => panic!("expected SeqDiscontinuity, got {other:?}"),
    }
}

/// Decisive check: "an empty directory replays to an empty `Vec`."
#[test]
fn empty_directory_replays_to_empty_vec() {
    let dir = tempdir().expect("tempdir");
    let journal = Journal::open(dir.path()).expect("open");
    let replayed = journal.replay().expect("replay");
    assert!(replayed.is_empty());
}

/// `Journal::open` creates the directory it is given if absent (Outcome:
/// "the directory created on open").
#[test]
fn open_creates_the_directory_if_absent() {
    let root = tempdir().expect("tempdir");
    let work_dir = root.path().join("works").join("work-1");
    assert!(!work_dir.exists());
    let _journal = Journal::open(&work_dir).expect("open");
    assert!(work_dir.is_dir());
    assert!(work_dir.join("journal.ndjson").exists());
}

fn deterministic_world() -> World {
    World::Deterministic(DeterministicWorld {
        command: vec!["cargo".to_string(), "test".to_string()],
        base_sha: "abc123".to_string(),
        cwd: "/var/tmp/w1".into(),
        env: Default::default(),
        expected_artifacts: OutputContract(vec![]),
    })
}

/// The "no in-memory objects" half of D9#1 (fold.md §5, second
/// paragraph; W2 outcome): a real lifecycle appended by one `Journal`
/// value, that value dropped, a new `Journal` opened on the same
/// directory, its `replay()` folded — equals `fold`ing the original
/// `Vec<Event>` directly. Proves the store's read path reproduces
/// exactly what `fold` computes, not a second reducer with independent
/// logic.
#[test]
fn replay_then_fold_equals_folding_the_original_events() {
    let dir = tempdir().expect("tempdir");
    let events = vec![
        Event {
            id: EventId(String::new()),
            work: WorkId("work-1".to_string()),
            run: None,
            at: Timestamp(0),
            kind: EventKind::WorkSubmitted {
                route: RouteId("route-1".to_string()),
                repositories: vec![RepositoryBinding {
                    name: "wirk".to_string(),
                    access: Access::Write,
                }],
                intent: "do the thing".to_string(),
                waypoints: vec![WaypointId("wp-1".to_string())],
                wp2_command: None,
            },
        },
        Event {
            id: EventId(String::new()),
            work: WorkId("work-1".to_string()),
            run: None,
            at: Timestamp(1),
            kind: EventKind::WaypointReserved {
                waypoint: WaypointId("wp-1".to_string()),
                world_hash: WorldHash("deadbeef".to_string()),
                world: deterministic_world(),
            },
        },
        Event {
            id: EventId(String::new()),
            work: WorkId("work-1".to_string()),
            run: Some(RunId("run-1".to_string())),
            at: Timestamp(2),
            kind: EventKind::RunOpened {
                run: RunId("run-1".to_string()),
                waypoint: WaypointId("wp-1".to_string()),
                attempt: 1,
                world_hash: WorldHash("deadbeef".to_string()),
            },
        },
        Event {
            id: EventId(String::new()),
            work: WorkId("work-1".to_string()),
            run: Some(RunId("run-1".to_string())),
            at: Timestamp(3),
            kind: EventKind::RunLaunched {
                run: RunId("run-1".to_string()),
                actor_kind: Default::default(),
            },
        },
        Event {
            id: EventId(String::new()),
            work: WorkId("work-1".to_string()),
            run: Some(RunId("run-1".to_string())),
            at: Timestamp(4),
            kind: EventKind::ClaimFiled {
                claim: ClaimId("claim-1".to_string()),
            },
        },
        Event {
            id: EventId(String::new()),
            work: WorkId("work-1".to_string()),
            run: Some(RunId("run-1".to_string())),
            at: Timestamp(5),
            kind: EventKind::ClaimRecorded {
                claim: ClaimId("claim-1".to_string()),
                claim_kind: ClaimKind::Done,
                verdict: ClaimVerdict::Validated,
            },
        },
    ];

    {
        let mut journal = Journal::open(dir.path()).expect("open");
        for event in &events {
            journal.append(event).expect("append");
        }
        // journal dropped here, closing the file handle.
    }

    let journal = Journal::open(dir.path()).expect("reopen");
    let replayed = journal.replay().expect("replay");
    let folded_from_replay = wirk_core::fold(&replayed);
    let folded_from_original = wirk_core::fold(&events);

    assert_eq!(folded_from_replay, folded_from_original);
    assert_eq!(folded_from_replay.state, wirk_core::WorkState::Completed);
}

/// The two-Waypoint half of item 8's "fold completes a Work on the
/// last waypoint of `WorkSubmitted`'s ordered list" claim
/// (`orient/route.md` §4), through a real `Journal` store rather than
/// `fold` called on hand-built events directly (`wirk-core/tests/
/// contracts.rs`'s `d9_1_journal_replay_rebuilds_work_state` already
/// covers that half) — appended by one `Journal` value, that value
/// dropped, reopened, and folded from the replay: `Active` right after
/// wp-1's Validated Done (wp-2 not yet reserved), `Completed` only once
/// wp-2's own Validated Done lands, matching the same shape this file's
/// `replay_then_fold_equals_folding_the_original_events` already pins
/// for one Waypoint.
#[test]
fn replay_then_fold_two_waypoints_completes_only_after_the_last() {
    let dir = tempdir().expect("tempdir");
    let submitted = Event {
        id: EventId(String::new()),
        work: WorkId("work-1".to_string()),
        run: None,
        at: Timestamp(0),
        kind: EventKind::WorkSubmitted {
            route: RouteId("proving".to_string()),
            repositories: vec![RepositoryBinding {
                name: "wirk".to_string(),
                access: Access::Write,
            }],
            intent: "write report.md with one line".to_string(),
            waypoints: vec![
                WaypointId("proving/wp-1".to_string()),
                WaypointId("proving/wp-2".to_string()),
            ],
            wp2_command: None,
        },
    };
    let wp1_events = [
        Event {
            id: EventId(String::new()),
            work: WorkId("work-1".to_string()),
            run: None,
            at: Timestamp(1),
            kind: EventKind::WaypointReserved {
                waypoint: WaypointId("proving/wp-1".to_string()),
                world_hash: WorldHash("deadbeef1".to_string()),
                world: deterministic_world(),
            },
        },
        Event {
            id: EventId(String::new()),
            work: WorkId("work-1".to_string()),
            run: Some(RunId("run-1".to_string())),
            at: Timestamp(2),
            kind: EventKind::RunOpened {
                run: RunId("run-1".to_string()),
                waypoint: WaypointId("proving/wp-1".to_string()),
                attempt: 1,
                world_hash: WorldHash("deadbeef1".to_string()),
            },
        },
        Event {
            id: EventId(String::new()),
            work: WorkId("work-1".to_string()),
            run: Some(RunId("run-1".to_string())),
            at: Timestamp(3),
            kind: EventKind::ClaimFiled {
                claim: ClaimId("claim-1".to_string()),
            },
        },
        Event {
            id: EventId(String::new()),
            work: WorkId("work-1".to_string()),
            run: Some(RunId("run-1".to_string())),
            at: Timestamp(4),
            kind: EventKind::ClaimRecorded {
                claim: ClaimId("claim-1".to_string()),
                claim_kind: ClaimKind::Done,
                verdict: ClaimVerdict::Validated,
            },
        },
    ];
    let wp2_events = [
        Event {
            id: EventId(String::new()),
            work: WorkId("work-1".to_string()),
            run: None,
            at: Timestamp(5),
            kind: EventKind::WaypointReserved {
                waypoint: WaypointId("proving/wp-2".to_string()),
                world_hash: WorldHash("deadbeef2".to_string()),
                world: deterministic_world(),
            },
        },
        Event {
            id: EventId(String::new()),
            work: WorkId("work-1".to_string()),
            run: Some(RunId("run-2".to_string())),
            at: Timestamp(6),
            kind: EventKind::RunOpened {
                run: RunId("run-2".to_string()),
                waypoint: WaypointId("proving/wp-2".to_string()),
                attempt: 1,
                world_hash: WorldHash("deadbeef2".to_string()),
            },
        },
        Event {
            id: EventId(String::new()),
            work: WorkId("work-1".to_string()),
            run: Some(RunId("run-2".to_string())),
            at: Timestamp(7),
            kind: EventKind::ClaimFiled {
                claim: ClaimId("claim-2".to_string()),
            },
        },
        Event {
            id: EventId(String::new()),
            work: WorkId("work-1".to_string()),
            run: Some(RunId("run-2".to_string())),
            at: Timestamp(8),
            kind: EventKind::ClaimRecorded {
                claim: ClaimId("claim-2".to_string()),
                claim_kind: ClaimKind::Done,
                verdict: ClaimVerdict::Validated,
            },
        },
    ];

    {
        let mut journal = Journal::open(dir.path()).expect("open");
        journal.append(&submitted).expect("append WorkSubmitted");
        for event in &wp1_events {
            journal.append(event).expect("append wp-1 event");
        }
        // journal dropped here, closing the file handle.
    }

    // Reopen and fold after wp-1's Validated Done alone: Active, not
    // Completed — wp-2 has not been reserved yet (fold.md §1's own
    // "current_waypoint unchanged until the next WaypointReserved").
    let journal = Journal::open(dir.path()).expect("reopen after wp-1");
    let replayed = journal.replay().expect("replay after wp-1");
    let work = wirk_core::fold(&replayed);
    assert_eq!(
        work.state,
        wirk_core::WorkState::Active,
        "wp-1's Validated Done alone must not complete the Work: {:?}",
        work.state
    );
    assert_eq!(
        work.current_waypoint,
        Some(WaypointId("proving/wp-1".to_string()))
    );
    drop(journal);

    {
        let mut journal = Journal::open(dir.path()).expect("reopen to append wp-2");
        for event in &wp2_events {
            journal.append(event).expect("append wp-2 event");
        }
    }

    let journal = Journal::open(dir.path()).expect("reopen after wp-2");
    let replayed = journal.replay().expect("replay after wp-2");
    let work = wirk_core::fold(&replayed);
    assert_eq!(
        work.state,
        wirk_core::WorkState::Completed,
        "wp-2's Validated Done (the last Waypoint) must complete the Work: {:?}",
        work.state
    );
}

/// Fold coverage gap named at 0033's close: a `ClaimRecorded` carrying
/// `claim_kind: Question` and a `Refused` verdict is not a Work fact
/// (fold.md §1, mirrored from `Run::apply`) — the Work must stay
/// `Active`, never jump to `NeedsInput`, which only a *Validated*
/// Question moves it to.
#[test]
fn fold_leaves_work_state_unchanged_on_a_refused_question_claim() {
    let events = vec![
        Event {
            id: EventId(String::new()),
            work: WorkId("work-1".to_string()),
            run: None,
            at: Timestamp(0),
            kind: EventKind::WorkSubmitted {
                route: RouteId("route-1".to_string()),
                repositories: vec![RepositoryBinding {
                    name: "wirk".to_string(),
                    access: Access::Write,
                }],
                intent: "do the thing".to_string(),
                waypoints: vec![WaypointId("wp-1".to_string())],
                wp2_command: None,
            },
        },
        Event {
            id: EventId(String::new()),
            work: WorkId("work-1".to_string()),
            run: None,
            at: Timestamp(1),
            kind: EventKind::WaypointReserved {
                waypoint: WaypointId("wp-1".to_string()),
                world_hash: WorldHash("deadbeef".to_string()),
                world: deterministic_world(),
            },
        },
        Event {
            id: EventId(String::new()),
            work: WorkId("work-1".to_string()),
            run: Some(RunId("run-1".to_string())),
            at: Timestamp(2),
            kind: EventKind::RunOpened {
                run: RunId("run-1".to_string()),
                waypoint: WaypointId("wp-1".to_string()),
                attempt: 1,
                world_hash: WorldHash("deadbeef".to_string()),
            },
        },
        Event {
            id: EventId(String::new()),
            work: WorkId("work-1".to_string()),
            run: Some(RunId("run-1".to_string())),
            at: Timestamp(3),
            kind: EventKind::ClaimRecorded {
                claim: ClaimId("claim-1".to_string()),
                claim_kind: ClaimKind::Question("what next?".to_string()),
                verdict: ClaimVerdict::Refused(wirk_core::ClaimRefusal::TripleMismatch),
            },
        },
    ];

    let work = wirk_core::fold(&events);
    assert_eq!(
        work.state,
        wirk_core::WorkState::Active,
        "a Refused Question claim must not move the Work to NeedsInput"
    );
}

/// Fold coverage gap named at 0033's close: `Work.last_activity` (issue
/// 286) advances to the timestamp of every folded event, not just the
/// first or a `WorkSubmitted`.
#[test]
fn fold_advances_last_activity_across_events_with_increasing_timestamps() {
    let events = vec![
        Event {
            id: EventId(String::new()),
            work: WorkId("work-1".to_string()),
            run: None,
            at: Timestamp(100),
            kind: EventKind::WorkSubmitted {
                route: RouteId("route-1".to_string()),
                repositories: vec![RepositoryBinding {
                    name: "wirk".to_string(),
                    access: Access::Write,
                }],
                intent: "do the thing".to_string(),
                waypoints: vec![WaypointId("wp-1".to_string())],
                wp2_command: None,
            },
        },
        Event {
            id: EventId(String::new()),
            work: WorkId("work-1".to_string()),
            run: None,
            at: Timestamp(200),
            kind: EventKind::WaypointReserved {
                waypoint: WaypointId("wp-1".to_string()),
                world_hash: WorldHash("deadbeef".to_string()),
                world: deterministic_world(),
            },
        },
        Event {
            id: EventId(String::new()),
            work: WorkId("work-1".to_string()),
            run: Some(RunId("run-1".to_string())),
            at: Timestamp(300),
            kind: EventKind::RunOpened {
                run: RunId("run-1".to_string()),
                waypoint: WaypointId("wp-1".to_string()),
                attempt: 1,
                world_hash: WorldHash("deadbeef".to_string()),
            },
        },
    ];

    let work = wirk_core::fold(&events[..1]);
    assert_eq!(work.last_activity, Timestamp(100));
    let work = wirk_core::fold(&events[..2]);
    assert_eq!(work.last_activity, Timestamp(200));
    let work = wirk_core::fold(&events);
    assert_eq!(
        work.last_activity,
        Timestamp(300),
        "last_activity did not advance to the last folded event's timestamp"
    );
}
