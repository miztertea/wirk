//! Pure `fold` tests for W-B's Finding/Settlement/Assertion/Application
//! types (`knowledge/work/p3-world-loop/W-B-BUILD.md`, corrected by
//! `loop-b-prepare-correct/HANDOFF.md` §6 and
//! `W-B-CONSTRUCTION-REVIEW.md`). No wirkd, no journal file, no Atlas —
//! `fold` is a pure function over hand-built `Event`s; the real
//! daemon/Git proof of admission, settlement policy and Application
//! lives in `wirk/tests/findings.rs`.

use wirk_core::{
    AdmittedEvidence, ArtifactReceipt, ArtifactSpec, Assertion, ClaimId, ClaimKind, ClaimVerdict,
    Decision, DeterministicProof, Event, EventId, EventKind, EvidenceOutcome, EvidenceRef,
    FailureCause, Finding, FindingId, FindingKind, FindingScope, FindingState, ObligationRef,
    PeerIdentity, RepositoryBinding, RouteId, RunId, RunState, Settlement, SettlementAuthority,
    SettlementCheck, SettlementClass, Timestamp, VerificationObligation, WaypointDefinition,
    WaypointId, WaypointKind, WorkId, WorkState, WorldHash, fold, obligation_basis,
};

/// The one obligation every deterministic case in this file discharges:
/// a named check, a named edition, the exact limited statement it
/// proves, and the one obligated output whose receipt discharges it.
fn obligation() -> VerificationObligation {
    VerificationObligation {
        id: "out-produced".to_string(),
        edition: "1".to_string(),
        proves: "the leaf's command ran and produced out.md".to_string(),
        outputs: vec!["out.md".to_string()],
        requires: None,
        review: None,
    }
}

fn named() -> Option<ObligationRef> {
    Some(ObligationRef {
        id: "out-produced".to_string(),
        edition: "1".to_string(),
    })
}

/// A `deterministic_leaf` that declares `obligation()`. The plain
/// `deterministic_leaf` declares none, which is exactly the "this
/// Waypoint discharges no obligation" case every refusal below leans on.
fn obliged_leaf(id: &str) -> WaypointDefinition {
    WaypointDefinition {
        verifies: Some(obligation()),
        ..deterministic_leaf(id)
    }
}

/// The receipt an obliged leaf's own Claim carries: the obligated
/// output, with a real recorded content identity.
fn receipts() -> Vec<ArtifactReceipt> {
    vec![ArtifactReceipt {
        name: "out.md".to_string(),
        path: "out.md".to_string(),
        digest: "2c8b08da5ce60398e1f19af0e5dccc744df274b826abe585eaba68c5254348060".to_string(),
    }]
}

/// The `basis` a settled check must carry for `obliged_leaf` opened
/// against `WorldHash("hash")` — re-derived here exactly as the product
/// derives it, never transcribed.
fn expected_basis(id: &str) -> String {
    obligation_basis(&obliged_leaf(id), Some(&WorldHash("hash".to_string())))
        .expect("an obliged Deterministic leaf has a basis")
}

/// The complete check a real `DeterministicVerified` settlement of
/// `obliged_leaf(id)`'s own Claim carries.
fn validated_claim_check(id: &str) -> SettlementCheck {
    SettlementCheck::ValidatedClaim {
        work: WorkId("work-1".to_string()),
        claim: ClaimId("claim-1".to_string()),
        claim_event: EventId("e-claim".to_string()),
        proof: Some(DeterministicProof {
            obligation: named().unwrap(),
            basis: expected_basis(id),
            proves: obligation().proves,
            waypoint: WaypointId(id.to_string()),
            attempt: 1,
            world_hash: WorldHash("hash".to_string()),
            artifacts: receipts(),
        }),
        unread: Default::default(),
    }
}

/// The evidence token every deterministic case cites: this Work's own
/// Claim event.
fn cites(event: &str) -> Vec<AdmittedEvidence> {
    vec![AdmittedEvidence {
        reference: EvidenceRef::Journal {
            work: WorkId("work-1".to_string()),
            event: EventId(event.to_string()),
        },
        outcome: EvidenceOutcome::Admitted {
            generation: "work-1".to_string(),
            object_id: event.to_string(),
        },
    }]
}

fn deterministic_leaf(id: &str) -> WaypointDefinition {
    WaypointDefinition {
        id: WaypointId(id.to_string()),
        kind: WaypointKind::Deterministic,
        declared_outputs: vec![ArtifactSpec {
            name: "out.md".to_string(),
            required: true,
        }],
        intent: None,
        command: Some(vec!["true".to_string()]),
        boundary: wirk_core::Boundary(vec!["**".to_string()]),
        leaves: Vec::new(),
        required_child_outcomes: Vec::new(),
        selection: None,
        verifies: None,
        orient: None,
    }
}

fn work_submitted(defs: Vec<WaypointDefinition>) -> Event {
    let waypoints = defs.iter().map(|d| d.id.clone()).collect();
    Event {
        id: EventId(String::new()),
        work: WorkId("work-1".to_string()),
        run: None,
        at: Timestamp(0),
        kind: EventKind::WorkSubmitted {
            route: RouteId("route-1".to_string()),
            repositories: vec![RepositoryBinding {
                name: "demo".to_string(),
                access: wirk_core::Access::Write,
            }],
            intent: "do the thing".to_string(),
            waypoints,
            waypoint_defs: defs,
            parent: None,
            execution_repo: None,
            execution_identity: None,
        },
    }
}

fn run_opened(run: &str, waypoint: &str) -> Event {
    run_opened_against(run, waypoint, "hash")
}

fn run_opened_against(run: &str, waypoint: &str, world: &str) -> Event {
    Event {
        id: EventId(String::new()),
        work: WorkId("work-1".to_string()),
        run: Some(RunId(run.to_string())),
        at: Timestamp(1),
        kind: EventKind::RunOpened {
            run: RunId(run.to_string()),
            waypoint: WaypointId(waypoint.to_string()),
            attempt: 1,
            world_hash: WorldHash(world.to_string()),
        },
    }
}

fn claim_recorded(id: &str, run: &str, claim: &str) -> Event {
    claim_recorded_with(id, run, claim, receipts())
}

/// The same Claim, with whatever receipt set the case needs — an empty
/// or digest-less set is how "the obligated output was never actually
/// produced/read" is expressed.
fn claim_recorded_with(id: &str, run: &str, claim: &str, artifacts: Vec<ArtifactReceipt>) -> Event {
    Event {
        id: EventId(id.to_string()),
        work: WorkId("work-1".to_string()),
        run: Some(RunId(run.to_string())),
        at: Timestamp(2),
        kind: EventKind::ClaimRecorded {
            claim: ClaimId(claim.to_string()),
            claim_kind: ClaimKind::Done,
            verdict: ClaimVerdict::Validated,
            artifacts,
        },
    }
}

/// A reservation of `waypoint` at `world` — the fact
/// `deterministic_verified_readiness` checks a Run's own opening World
/// against, so a re-reserved Waypoint's superseded activation is
/// visible.
fn waypoint_reserved(waypoint: &str, world: &str) -> Event {
    Event {
        id: EventId(format!("e-reserved-{waypoint}-{world}")),
        work: WorkId("work-1".to_string()),
        run: None,
        at: Timestamp(1),
        kind: EventKind::WaypointReserved {
            waypoint: WaypointId(waypoint.to_string()),
            world_hash: WorldHash(world.to_string()),
            world: wirk_core::World::Deterministic(wirk_core::DeterministicWorld {
                command: vec!["true".to_string()],
                base_sha: "0".repeat(40),
                source_basis: wirk_core::SourceBasis::Unknown,
                cwd: std::path::PathBuf::from("/nonexistent"),
                env: std::collections::BTreeMap::new(),
                expected_artifacts: wirk_core::OutputContract(Vec::new()),
            }),
        },
    }
}

fn finding_raised(event_id: &str, run: &str, finding: Finding) -> Event {
    Event {
        id: EventId(event_id.to_string()),
        work: WorkId("work-1".to_string()),
        run: Some(RunId(run.to_string())),
        at: Timestamp(3),
        kind: EventKind::FindingRaised { finding },
    }
}

fn peer() -> PeerIdentity {
    PeerIdentity {
        uid: 1000,
        gid: 1000,
    }
}

fn work_local_finding(kind: FindingKind, claim: &str) -> Finding {
    Finding {
        id: FindingId("finding-1".to_string()),
        work: WorkId("work-1".to_string()),
        run: RunId("run-1".to_string()),
        waypoint: WaypointId("leaf".to_string()),
        kind,
        scope: FindingScope::WorkLocal,
        claim: claim.to_string(),
        evidence: Vec::new(),
        contradicts: Vec::new(),
        applies_to: Vec::new(),
        supersedes: None,
        proposed_change: None,
        obligation: None,
        confirmed_by: None,
    }
}

#[test]
fn work_local_finding_is_visible_immediately_and_never_needs_settlement() {
    let events = vec![
        work_submitted(vec![deterministic_leaf("leaf")]),
        run_opened("run-1", "leaf"),
        finding_raised(
            "e-find",
            "run-1",
            work_local_finding(FindingKind::Gap, "a gap exists"),
        ),
    ];
    let work = fold(&events);
    let record = work
        .findings
        .get(&FindingId("finding-1".to_string()))
        .expect("finding folded");
    assert_eq!(record.state, FindingState::Proposed);
    assert!(record.assertions.is_empty());
    assert!(record.applied.is_empty());
    // A WorkLocal Gap finding has no pure-derivable settlement path.
    assert!(work.settlement_ready.is_empty());
}

/// The terminal design's own bug (HANDOFF.md §6): computing readiness
/// only "at the moment the qualifying event folds" makes
/// `DeterministicVerified` unreachable, because that class's own finding
/// always names an *earlier* Claim. Proven both ways: the Claim first,
/// then the raise; and the raise first, then the Claim.
#[test]
fn deterministic_verified_readiness_is_order_independent_claim_first() {
    let events = vec![
        work_submitted(vec![obliged_leaf("leaf")]),
        waypoint_reserved("leaf", "hash"),
        run_opened("run-1", "leaf"),
        claim_recorded("e-claim", "run-1", "claim-1"),
        finding_raised(
            "e-find",
            "run-1",
            Finding {
                evidence: cites("e-claim"),
                scope: FindingScope::EstateLocal,
                obligation: named(),
                ..work_local_finding(FindingKind::VerifiedOutcome, "the deterministic leaf ran")
            },
        ),
    ];
    let work = fold(&events);
    assert_eq!(work.settlement_ready.len(), 1);
    let ready = &work.settlement_ready[0];
    assert_eq!(ready.finding, FindingId("finding-1".to_string()));
    assert_eq!(ready.class, SettlementClass::DeterministicVerified);
    // The whole check, not just the Claim id: the obligation named, the
    // content basis a policy must admit, the exact statement discharged,
    // the current activation, and the receipt that discharged it.
    assert_eq!(ready.check, validated_claim_check("leaf"));
}

#[test]
fn deterministic_verified_readiness_is_order_independent_raise_first() {
    let events = vec![
        work_submitted(vec![obliged_leaf("leaf")]),
        waypoint_reserved("leaf", "hash"),
        run_opened("run-1", "leaf"),
        finding_raised(
            "e-find",
            "run-1",
            Finding {
                evidence: cites("e-claim"),
                scope: FindingScope::EstateLocal,
                obligation: named(),
                ..work_local_finding(FindingKind::VerifiedOutcome, "the deterministic leaf ran")
            },
        ),
        claim_recorded("e-claim", "run-1", "claim-1"),
    ];
    let work = fold(&events);
    assert_eq!(work.settlement_ready.len(), 1);
    assert_eq!(
        work.settlement_ready[0].class,
        SettlementClass::DeterministicVerified
    );
}

/// A claim of an unrelated successful command never proves the named
/// obligation (construction review): the evidence must name a Claim of
/// a *Deterministic* leaf — an Actor leaf's own Validated Done claim
/// does not produce readiness.
#[test]
fn deterministic_verified_readiness_refuses_an_actor_leafs_claim() {
    // Everything else is satisfied — the leaf declares the obligation,
    // the Finding names it, the receipt is real — so this refusal is
    // about the Actor kind and nothing else.
    let mut actor_leaf = obliged_leaf("leaf");
    actor_leaf.kind = WaypointKind::Actor;
    actor_leaf.command = None;
    actor_leaf.intent = Some("do the thing".to_string());
    let events = vec![
        work_submitted(vec![actor_leaf]),
        waypoint_reserved("leaf", "hash"),
        run_opened("run-1", "leaf"),
        claim_recorded("e-claim", "run-1", "claim-1"),
        finding_raised(
            "e-find",
            "run-1",
            Finding {
                evidence: cites("e-claim"),
                scope: FindingScope::EstateLocal,
                obligation: named(),
                ..work_local_finding(FindingKind::VerifiedOutcome, "the actor leaf ran")
            },
        ),
    ];
    let work = fold(&events);
    assert!(
        work.settlement_ready.is_empty(),
        "an Actor leaf's Claim must never satisfy deterministic-verified"
    );
}

/// W-B-CORRECT.md defect 1 ("policy-bound proof"): a Validated Done
/// Claim of a *different* Deterministic leaf, on a *different* Run, must
/// never settle a `VerifiedOutcome` finding raised against an unrelated
/// leaf just because the finding's evidence names some exact — but
/// unrelated — Claim event. The check must bind to the finding's own
/// Run/Waypoint (the exact obligation it was raised against), not to
/// "any Deterministic leaf's Claim, anywhere in this Work."
#[test]
fn deterministic_verified_readiness_refuses_an_unrelated_deterministic_leafs_claim() {
    // `leaf-b` discharges a *different* obligation than the one the
    // Finding names, so this stays a real refusal now that naming is
    // required: leaf-b's Claim is real, Validated, Done, current and
    // earlier in Route order, and still proves nothing about
    // `out-produced@1`.
    let mut other = obliged_leaf("leaf-b");
    other.verifies = Some(VerificationObligation {
        id: "something-else".to_string(),
        ..obligation()
    });
    let events = vec![
        work_submitted(vec![obliged_leaf("leaf-a"), other]),
        waypoint_reserved("leaf-a", "hash"),
        waypoint_reserved("leaf-b", "hash"),
        run_opened("run-a", "leaf-a"),
        run_opened("run-b", "leaf-b"),
        // The real, Validated Done Claim belongs to leaf-b's own run —
        // an entirely different obligation than the one this Finding
        // names below.
        claim_recorded("e-claim-b", "run-b", "claim-b"),
        finding_raised(
            "e-find",
            "run-a",
            Finding {
                run: RunId("run-a".to_string()),
                waypoint: WaypointId("leaf-a".to_string()),
                evidence: cites("e-claim-b"),
                scope: FindingScope::EstateLocal,
                obligation: named(),
                ..work_local_finding(FindingKind::VerifiedOutcome, "leaf-a's outcome is verified")
            },
        ),
    ];
    let work = fold(&events);
    assert!(
        work.settlement_ready.is_empty(),
        "an unrelated leaf's own Claim must never settle this Finding's own obligation"
    );
}

/// Authority review §1 ("currency gap"): a superseded (retried) attempt's
/// own Claim proves nothing about the *current* activation of its own
/// Waypoint — the same currency `handle_finding_raise` already requires
/// of the raising Run must hold for the cited one too.
#[test]
fn deterministic_verified_readiness_refuses_a_superseded_attempts_claim() {
    let events = vec![
        work_submitted(vec![obliged_leaf("leaf")]),
        waypoint_reserved("leaf", "hash"),
        run_opened("run-1", "leaf"),
        claim_recorded("e-claim", "run-1", "claim-1"),
        // A second attempt reopens the same Waypoint after the first
        // attempt's own Claim already posted — `run-1` is no longer the
        // current attempt for `leaf`.
        run_opened("run-2", "leaf"),
        finding_raised(
            "e-find",
            "run-2",
            Finding {
                run: RunId("run-2".to_string()),
                waypoint: WaypointId("leaf".to_string()),
                evidence: cites("e-claim"),
                scope: FindingScope::EstateLocal,
                obligation: named(),
                ..work_local_finding(FindingKind::VerifiedOutcome, "the superseded attempt ran")
            },
        ),
    ];
    let work = fold(&events);
    assert!(
        work.settlement_ready.is_empty(),
        "a superseded attempt's own Claim must never settle a Finding, even citing the same Waypoint"
    );
}

/// The obligation contract's own decisive refusal, in pure form: this
/// is the counterexample reproduced through the real service against
/// `0634657` before a line of this wave changed
/// (`loop-b-obligation-build/raw/00-counterexample-frozen-base.txt`).
/// An earlier deterministic leaf ran `echo one > out1.md`; a Finding
/// whose sentence is an unrelated security-audit claim cited that leaf's
/// own real, Validated, Done, current Claim and settled
/// `deterministic_verified`.
///
/// Both halves are asserted here, because the point is not "refuse more"
/// but "prove the right thing":
///
/// - the security sentence, naming no obligation, is refused;
/// - the *same* Claim, cited by a Finding that names the obligation the
///   leaf actually declares, settles — and what it settles carries the
///   leaf's own limited `proves` statement, never the Finding's
///   sentence.
#[test]
fn an_unrelated_sentence_never_settles_and_the_named_obligation_proves_only_itself() {
    let security_sentence = "wirk has no remote code execution vulnerability and its full security audit passed with zero findings";
    let base = vec![
        work_submitted(vec![obliged_leaf("leaf-1"), obliged_leaf("leaf-2")]),
        waypoint_reserved("leaf-1", "hash"),
        run_opened("run-1", "leaf-1"),
        claim_recorded("e-claim", "run-1", "claim-1"),
        run_opened("run-2", "leaf-2"),
    ];

    let mut refused = base.clone();
    refused.push(finding_raised(
        "e-find",
        "run-2",
        Finding {
            run: RunId("run-2".to_string()),
            waypoint: WaypointId("leaf-2".to_string()),
            evidence: cites("e-claim"),
            scope: FindingScope::EstateLocal,
            ..work_local_finding(FindingKind::VerifiedOutcome, security_sentence)
        },
    ));
    assert!(
        fold(&refused).settlement_ready.is_empty(),
        "an unrelated sentence citing a real earlier Claim must never be ready: \
         Route position is not an obligation proof"
    );

    let mut settled = base;
    settled.push(finding_raised(
        "e-find",
        "run-2",
        Finding {
            run: RunId("run-2".to_string()),
            waypoint: WaypointId("leaf-2".to_string()),
            evidence: cites("e-claim"),
            scope: FindingScope::EstateLocal,
            obligation: named(),
            ..work_local_finding(FindingKind::VerifiedOutcome, security_sentence)
        },
    ));
    let work = fold(&settled);
    assert_eq!(
        work.settlement_ready.len(),
        1,
        "naming the obligation the cited leaf actually declares is the legitimate path, \
         and it must still reach the same handler"
    );
    match &work.settlement_ready[0].check {
        SettlementCheck::ValidatedClaim {
            proof: Some(proof), ..
        } => {
            let (proves, discharged, basis, artifacts, waypoint) = (
                &proof.proves,
                &proof.obligation,
                &proof.basis,
                &proof.artifacts,
                &proof.waypoint,
            );
            assert_eq!(
                proves,
                &obligation().proves,
                "the settled check proves the Route-authored obligation statement, \
                 never the Finding's own sentence"
            );
            assert_ne!(proves, security_sentence);
            assert_eq!(discharged, &named().unwrap());
            assert_eq!(basis, &expected_basis("leaf-1"));
            assert_eq!(artifacts, &receipts());
            assert_eq!(waypoint, &WaypointId("leaf-1".to_string()));
        }
        other => panic!("expected ValidatedClaim, got {other:?}"),
    }
}

/// Naming *an* obligation is necessary and never sufficient: a Finding
/// naming `out-produced@2` against a leaf that declares
/// `out-produced@1` is discharging a check edition that never ran.
#[test]
fn deterministic_verified_readiness_refuses_a_wrong_obligation_edition() {
    let events = vec![
        work_submitted(vec![obliged_leaf("leaf")]),
        waypoint_reserved("leaf", "hash"),
        run_opened("run-1", "leaf"),
        claim_recorded("e-claim", "run-1", "claim-1"),
        finding_raised(
            "e-find",
            "run-1",
            Finding {
                evidence: cites("e-claim"),
                scope: FindingScope::EstateLocal,
                obligation: Some(ObligationRef {
                    id: "out-produced".to_string(),
                    edition: "2".to_string(),
                }),
                ..work_local_finding(FindingKind::VerifiedOutcome, "the leaf ran")
            },
        ),
    ];
    assert!(
        fold(&events).settlement_ready.is_empty(),
        "a check edition the Waypoint does not declare discharges nothing"
    );
}

/// A Waypoint declaring no obligation at all discharges none, however
/// successful its Claim: this is the frozen candidate's `wp-1`.
#[test]
fn deterministic_verified_readiness_refuses_a_leaf_that_declares_no_obligation() {
    let events = vec![
        work_submitted(vec![deterministic_leaf("leaf")]),
        waypoint_reserved("leaf", "hash"),
        run_opened("run-1", "leaf"),
        claim_recorded("e-claim", "run-1", "claim-1"),
        finding_raised(
            "e-find",
            "run-1",
            Finding {
                evidence: cites("e-claim"),
                scope: FindingScope::EstateLocal,
                obligation: named(),
                ..work_local_finding(FindingKind::VerifiedOutcome, "the leaf ran")
            },
        ),
    ];
    assert!(
        fold(&events).settlement_ready.is_empty(),
        "a proposer naming an obligation no Waypoint declares settles nothing"
    );
}

/// The obligated outcome must actually be in the receipt, with a real
/// recorded content identity. Two nearby shapes, both refused: the
/// receipt names a different artifact, and the receipt names the right
/// artifact with no recorded digest (a pre-correction, name-only
/// receipt, which is not an evidence basis).
#[test]
fn deterministic_verified_readiness_refuses_a_receipt_that_misses_the_obligated_outcome() {
    let case = |artifacts: Vec<ArtifactReceipt>| {
        vec![
            work_submitted(vec![obliged_leaf("leaf")]),
            waypoint_reserved("leaf", "hash"),
            run_opened("run-1", "leaf"),
            claim_recorded_with("e-claim", "run-1", "claim-1", artifacts),
            finding_raised(
                "e-find",
                "run-1",
                Finding {
                    evidence: cites("e-claim"),
                    scope: FindingScope::EstateLocal,
                    obligation: named(),
                    ..work_local_finding(FindingKind::VerifiedOutcome, "the leaf ran")
                },
            ),
        ]
    };
    assert!(
        fold(&case(Vec::new())).settlement_ready.is_empty(),
        "an empty receipt set never discharges a named obligated output"
    );
    assert!(
        fold(&case(vec![ArtifactReceipt {
            name: "elsewhere.md".to_string(),
            path: "elsewhere.md".to_string(),
            digest: "a".repeat(64),
        }]))
        .settlement_ready
        .is_empty(),
        "a receipt for a different artifact never discharges this obligation"
    );
    assert!(
        fold(&case(vec![ArtifactReceipt {
            name: "out.md".to_string(),
            path: String::new(),
            digest: String::new(),
        }]))
        .settlement_ready
        .is_empty(),
        "a receipt with no recorded content identity is not an evidence basis"
    );
}

/// Stale activation on the World axis: the Waypoint has been re-reserved
/// against a different World since this Run opened, so the Claim proves
/// what the old World obliged, never what the current activation
/// obliges (construction review: "do not settle a superseded
/// activation"). The Run is still the *latest* Run for the Waypoint here,
/// so this is a genuinely distinct check from the attempt-currency one.
#[test]
fn deterministic_verified_readiness_refuses_a_run_opened_against_a_superseded_world() {
    let events = vec![
        work_submitted(vec![obliged_leaf("leaf")]),
        waypoint_reserved("leaf", "hash"),
        run_opened_against("run-1", "leaf", "hash"),
        claim_recorded("e-claim", "run-1", "claim-1"),
        // The Waypoint is re-reserved against a different World; no new
        // Run is opened, so `run-1` is still the latest for `leaf`.
        waypoint_reserved("leaf", "a-different-world"),
        finding_raised(
            "e-find",
            "run-1",
            Finding {
                evidence: cites("e-claim"),
                scope: FindingScope::EstateLocal,
                obligation: named(),
                ..work_local_finding(FindingKind::VerifiedOutcome, "the leaf ran")
            },
        ),
    ];
    assert!(
        fold(&events).settlement_ready.is_empty(),
        "a Claim from a superseded reservation never discharges the current activation"
    );
}

/// The basis is what the estate policy admits, so it must move when
/// anything the proposer could author moves. Guards against a future
/// change that hashes only the id/edition and lets the proven statement,
/// the obligated outputs or the execution basis drift free.
#[test]
fn obligation_basis_covers_every_authored_field_and_the_execution_basis() {
    let leaf = obliged_leaf("leaf");
    let world = WorldHash("hash".to_string());
    let base = obligation_basis(&leaf, Some(&world)).unwrap();

    let widened = WaypointDefinition {
        verifies: Some(VerificationObligation {
            proves: "wirk has no remote code execution vulnerability".to_string(),
            ..obligation()
        }),
        ..obliged_leaf("leaf")
    };
    assert_ne!(
        base,
        obligation_basis(&widened, Some(&world)).unwrap(),
        "widening the proven statement must change the admitted basis"
    );

    let dropped = WaypointDefinition {
        verifies: Some(VerificationObligation {
            outputs: Vec::new(),
            ..obligation()
        }),
        ..obliged_leaf("leaf")
    };
    assert_ne!(
        base,
        obligation_basis(&dropped, Some(&world)).unwrap(),
        "dropping an obligated output must change the admitted basis"
    );

    assert_ne!(
        base,
        obligation_basis(&leaf, Some(&WorldHash("another-world".to_string()))).unwrap(),
        "a different command/source basis must change the admitted basis"
    );

    let mut actor = obliged_leaf("leaf");
    actor.kind = WaypointKind::Actor;
    // Not "an Actor can have no basis" — it can, and `wirk`'s own
    // findings suite settles one. What this fixture lacks is the
    // declared mechanism: `obligation()` carries no `review` contract,
    // and the `Actor` arm is the one that refuses without it. A
    // `Container` carrying no `requires` is the contrast, not the
    // parallel — it still gets a basis, and is refused as a settlement
    // candidate at readiness instead (asserted just below).
    assert!(
        actor.verifies.as_ref().unwrap().review.is_none(),
        "the fixture this turns on declares no review contract"
    );
    assert!(
        obligation_basis(&actor, Some(&world)).is_none(),
        "an Actor obligation that declares no review mechanism has no basis to admit"
    );
    assert!(
        obligation_basis(&deterministic_leaf("leaf"), Some(&world)).is_none(),
        "a Waypoint declaring no obligation has no basis"
    );

    // The contrast, executed rather than asserted in prose: a
    // `Container` obligation declaring no `requires` still gets a basis
    // — the arm hashes the absence and carries on — and the absence
    // changes that value rather than withholding it. The estate's
    // fail-closed for a mechanism-less container is a readiness
    // refusal, not a missing basis, and reading this function's return
    // as that refusal is the doc error ruling 0137 carried.
    let mut container = obliged_leaf("leaf");
    container.kind = WaypointKind::Container;
    assert!(
        container.verifies.as_ref().unwrap().requires.is_none(),
        "the fixture this turns on declares no required child obligation"
    );
    let without_requires = obligation_basis(&container, Some(&world))
        .expect("a Container obligation with no `requires` still has a basis");
    let mut required = container.clone();
    required.verifies = Some(VerificationObligation {
        requires: Some(ObligationRef {
            id: "child-out".to_string(),
            edition: "1".to_string(),
        }),
        ..obligation()
    });
    assert_ne!(
        without_requires,
        obligation_basis(&required, Some(&world)).unwrap(),
        "declaring the required child obligation changes the admitted basis"
    );
    assert_eq!(
        without_requires,
        obligation_basis(&container, None).unwrap(),
        "and a Container's basis never reads the World hash, with or without `requires`"
    );
}

#[test]
fn finding_settled_removes_it_from_settlement_ready() {
    let mut events = vec![
        work_submitted(vec![obliged_leaf("leaf")]),
        waypoint_reserved("leaf", "hash"),
        run_opened("run-1", "leaf"),
        claim_recorded("e-claim", "run-1", "claim-1"),
        finding_raised(
            "e-find",
            "run-1",
            Finding {
                evidence: cites("e-claim"),
                scope: FindingScope::EstateLocal,
                obligation: named(),
                ..work_local_finding(FindingKind::VerifiedOutcome, "the deterministic leaf ran")
            },
        ),
    ];
    assert_eq!(fold(&events).settlement_ready.len(), 1);

    events.push(Event {
        id: EventId("e-settled".to_string()),
        work: WorkId("work-1".to_string()),
        run: None,
        at: Timestamp(4),
        kind: EventKind::FindingSettled {
            finding: FindingId("finding-1".to_string()),
            settlement: Settlement {
                authority: SettlementAuthority {
                    class: SettlementClass::DeterministicVerified,
                    policy_version: 1,
                    policy_digest: "deadbeef".to_string(),
                },
                check: validated_claim_check("leaf"),
                settled_by: EventId("e-claim".to_string()),
                at: Timestamp(5),
                minted_at_startup: false,
            },
        },
    });
    let work = fold(&events);
    assert!(
        work.settlement_ready.is_empty(),
        "a settled finding must never remain a settlement candidate"
    );
    let record = work
        .findings
        .get(&FindingId("finding-1".to_string()))
        .unwrap();
    assert!(matches!(record.state, FindingState::Settled(_)));
}

/// §2.5's decisive rule: an assertion never settles, and it never
/// suppresses the finding from later consultation.
#[test]
fn finding_asserted_never_settles_and_never_suppresses() {
    let events = vec![
        work_submitted(vec![deterministic_leaf("leaf")]),
        run_opened("run-1", "leaf"),
        finding_raised(
            "e-find",
            "run-1",
            work_local_finding(FindingKind::Gap, "a gap exists"),
        ),
        Event {
            id: EventId("e-assert".to_string()),
            work: WorkId("work-1".to_string()),
            run: None,
            at: Timestamp(4),
            kind: EventKind::FindingAsserted {
                finding: FindingId("finding-1".to_string()),
                assertion: Assertion {
                    decision: Decision::Rejected {
                        reason: "an actor's own unverified say-so".to_string(),
                    },
                    by: "an-actor-pretending-to-be-root".to_string(),
                    reason: None,
                    peer: peer(),
                    at: Timestamp(4),
                    author: None,
                },
            },
        },
    ];
    let work = fold(&events);
    let record = work
        .findings
        .get(&FindingId("finding-1".to_string()))
        .unwrap();
    assert_eq!(
        record.state,
        FindingState::Proposed,
        "an assertion must never move a finding to Settled"
    );
    assert_eq!(
        record.assertions.len(),
        1,
        "the assertion is still recorded"
    );
}

/// `ASSERTION-AUTHOR-ADJUDICATION.md`: assertions journaled before
/// `Assertion.author` existed must keep replaying, and must fold to an
/// *unknown* author — never to the Work whose journal holds them, and
/// never to `by` or `peer`, which §2.5 records as attribution and
/// refuses as identity. The serialized shape below is the exact
/// pre-correction one.
#[test]
fn an_assertion_journaled_before_authorship_folds_as_unknown() {
    let legacy = serde_json::json!({
        "decision": {"Rejected": {"reason": "rejected: the embargomarker roster says otherwise"}},
        "by": "parent reviewer",
        "reason": null,
        "peer": {"uid": 1000, "gid": 1000},
        "at": 4
    });
    let assertion: Assertion = serde_json::from_value(legacy).expect("old journals still replay");
    assert_eq!(
        assertion.author, None,
        "an assertion with no recorded author has an unknown one, and it stays unknown"
    );
    assert_eq!(assertion.by, "parent reviewer");
}

/// A `FindingSettled`/`FindingAsserted`/`FindingApplied` naming a finding
/// this journal never raised is ignored — fail closed, never a panic and
/// never a phantom record.
#[test]
fn settlement_for_unknown_finding_is_ignored() {
    let events = vec![
        work_submitted(vec![deterministic_leaf("leaf")]),
        run_opened("run-1", "leaf"),
        Event {
            id: EventId("e-settled".to_string()),
            work: WorkId("work-1".to_string()),
            run: None,
            at: Timestamp(2),
            kind: EventKind::FindingSettled {
                finding: FindingId("never-raised".to_string()),
                settlement: Settlement {
                    authority: SettlementAuthority {
                        class: SettlementClass::DeterministicVerified,
                        policy_version: 1,
                        policy_digest: "deadbeef".to_string(),
                    },
                    check: validated_claim_check("leaf"),
                    settled_by: EventId("e-claim".to_string()),
                    at: Timestamp(3),
                    minted_at_startup: false,
                },
            },
        },
    ];
    let work = fold(&events);
    assert!(work.findings.is_empty());
    assert_eq!(work.state, WorkState::Pending);
}

/// A later Finding in the same Work naming an earlier one via
/// `supersedes` is a journal fact the Work owns on both ends — it
/// settles the earlier one (`SupersededInOrigin`), never merely by
/// authorship.
#[test]
fn supersedes_produces_settlement_ready_for_the_earlier_finding() {
    let earlier = work_local_finding(FindingKind::Gap, "an early guess");
    let mut later = work_local_finding(FindingKind::Gap, "the corrected claim");
    later.id = FindingId("finding-2".to_string());
    later.supersedes = Some(FindingId("finding-1".to_string()));

    let events = vec![
        work_submitted(vec![deterministic_leaf("leaf")]),
        run_opened("run-1", "leaf"),
        finding_raised("e-1", "run-1", earlier),
        finding_raised("e-2", "run-1", later),
    ];
    let work = fold(&events);
    assert_eq!(work.settlement_ready.len(), 1);
    let ready = &work.settlement_ready[0];
    assert_eq!(ready.finding, FindingId("finding-1".to_string()));
    assert_eq!(ready.class, SettlementClass::SupersededInOrigin);
    match &ready.check {
        SettlementCheck::SupersededBy {
            finding,
            raise_event,
            ..
        } => {
            assert_eq!(finding, &FindingId("finding-2".to_string()));
            assert_eq!(raise_event, &EventId("e-2".to_string()));
        }
        other => panic!("expected SupersededBy, got {other:?}"),
    }
}

/// W-B-CORRECT.md defect 4 ("supersession stays within authority"):
/// naming an *already-settled* record via `supersedes` must never
/// suppress it — a Work owns proposing its own replacement, never
/// overriding a settled estate fact by same-origin authorship alone.
/// The already-real `DeterministicVerified` settlement on `finding-1`
/// must survive a later `finding-2` naming it via `supersedes`
/// untouched: no fresh `ReadySettlement` is ever offered for an already-
/// `Settled` finding, from any class.
#[test]
fn supersedes_naming_an_already_settled_finding_never_suppresses_it() {
    let real_settlement = Settlement {
        authority: SettlementAuthority {
            class: SettlementClass::DeterministicVerified,
            policy_version: 1,
            policy_digest: "deadbeef".to_string(),
        },
        check: validated_claim_check("leaf"),
        settled_by: EventId("e-claim".to_string()),
        at: Timestamp(5),
        minted_at_startup: false,
    };
    let mut later = work_local_finding(FindingKind::Gap, "a same-origin attempt to suppress it");
    later.id = FindingId("finding-2".to_string());
    later.supersedes = Some(FindingId("finding-1".to_string()));

    let events = vec![
        work_submitted(vec![deterministic_leaf("leaf")]),
        run_opened("run-1", "leaf"),
        claim_recorded("e-claim", "run-1", "claim-1"),
        finding_raised(
            "e-1",
            "run-1",
            Finding {
                evidence: vec![AdmittedEvidence {
                    reference: EvidenceRef::Journal {
                        work: WorkId("work-1".to_string()),
                        event: EventId("e-claim".to_string()),
                    },
                    outcome: EvidenceOutcome::Admitted {
                        generation: "work-1".to_string(),
                        object_id: "e-claim".to_string(),
                    },
                }],
                scope: FindingScope::EstateLocal,
                ..work_local_finding(FindingKind::VerifiedOutcome, "the deterministic leaf ran")
            },
        ),
        Event {
            id: EventId("e-settled".to_string()),
            work: WorkId("work-1".to_string()),
            run: None,
            at: Timestamp(4),
            kind: EventKind::FindingSettled {
                finding: FindingId("finding-1".to_string()),
                settlement: real_settlement.clone(),
            },
        },
        // The same Work now tries to suppress its own already-settled
        // finding by merely naming it via `supersedes`.
        finding_raised("e-2", "run-1", later),
    ];
    let work = fold(&events);
    assert!(
        work.settlement_ready.is_empty(),
        "an already-settled finding must never re-enter settlement_ready, from any class"
    );
    let record = work
        .findings
        .get(&FindingId("finding-1".to_string()))
        .unwrap();
    assert_eq!(
        record.state,
        FindingState::Settled(Box::new(real_settlement)),
        "the real settlement must survive a same-origin supersede attempt unchanged"
    );
}

/// A stray `RunFailed` on the same Run must not disturb an already
/// folded finding — findings are independent of a Run's own outcome
/// (a Question or a later retry does not erase evidence already raised).
#[test]
fn finding_survives_a_later_run_failure_on_its_own_run() {
    let events = vec![
        work_submitted(vec![deterministic_leaf("leaf")]),
        run_opened("run-1", "leaf"),
        finding_raised(
            "e-find",
            "run-1",
            work_local_finding(FindingKind::Gap, "a gap exists"),
        ),
        Event {
            id: EventId("e-fail".to_string()),
            work: WorkId("work-1".to_string()),
            run: Some(RunId("run-1".to_string())),
            at: Timestamp(4),
            kind: EventKind::RunFailed {
                cause: FailureCause {
                    status: Some("boom".to_string()),
                    request_id: None,
                    at: Timestamp(4),
                    detail: None,
                },
            },
        },
    ];
    let work = fold(&events);
    assert!(
        work.findings
            .contains_key(&FindingId("finding-1".to_string()))
    );
    assert_eq!(work.state, WorkState::NeedsInput);
    let _ = RunState::Open; // silence an unused-import if RunState is trimmed later
}
