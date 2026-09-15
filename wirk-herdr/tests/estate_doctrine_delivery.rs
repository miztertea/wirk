//! P6.7 (ruling 0393): the *delivery* half of explicitly selected estate
//! doctrine — what an actor is actually pointed at when its World
//! reserved the estate owner's own documents, and the refusals that make
//! that relation load-bearing.
//!
//! Same discipline and same limits as `worker_contract.rs` next door:
//! no harness process runs here, so per ruling 0040 a `FakeHerdrClient`
//! pins *shape* — that wirk points the harness at bytes that hash to
//! what was reserved, keeps the two identities apart, and refuses before
//! any pane side effect when it cannot prove them. It does **not**
//! establish that claude read what it was handed; that is actual native
//! use, and it is a separate stage.
//!
//! The digest half is real: bytes are written to a real filesystem and
//! re-hashed with the same SHA-256 the product uses.

use std::sync::Arc;

use sha2::{Digest, Sha256};
use tempfile::tempdir;

use wirk_core::{
    ActorKind, ActorWorld, ArtifactSpec, Boundary, ContractDeliveryMode, EstateDoctrineRef,
    ExecutionTriple, OutputContract, Run, RunId, RunState, WaypointId, WorkId, WorkerContractRef,
    World, WorldHash,
};
use wirk_herdr::fake::FakeHerdrClient;
use wirk_herdr::{AgentStatus, HerdrExecutor, PaneInfo};

const CONTRACT: &str = "# Wirk worker contract\n\nFile your Claim when the outputs exist.\n";
const ESTATE_RULES: &str =
    "# House rules\n\nCite the rung that resolved each decision.\nDeuxième ligne — ✓\n";
const REPO_RULES: &str = "# The alpha repository\n\nNever land on main without a green gate.\n";

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn run_with_kind(kind: ActorKind) -> Run {
    Run {
        id: RunId("run-1".to_string()),
        waypoint: WaypointId("route-1/wp-1".to_string()),
        attempt: 1,
        world_hash: WorldHash("deadbeef".to_string()),
        state: RunState::Open,
        kind,
        selection: Default::default(),
        launched: false,
        launch_requested: false,
        launch_argv: Vec::new(),
        launch_attempt: None,
        expansions: Vec::new(),
        contract_delivery: None,
        claim_hook: None,
    }
}

fn pane_info(pane_id: &str) -> PaneInfo {
    PaneInfo {
        pane_id: pane_id.to_string(),
        terminal_id: format!("term-{pane_id}"),
        workspace_id: "w1".to_string(),
        tab_id: "tab1".to_string(),
        focused: false,
        agent_status: AgentStatus::Idle,
        revision: 1,
        agent: None,
        agent_session: None,
        cwd: None,
        display_agent: None,
        foreground_cwd: None,
        name: None,
        label: None,
        scroll: None,
        state_labels: None,
        terminal_title: None,
        terminal_title_stripped: None,
        title: None,
        tokens: None,
    }
}

/// Stands in for wirkd's reservation-time write of the shared contract.
fn reserve_contract(estate_root: &std::path::Path) -> WorkerContractRef {
    let digest = sha256_hex(CONTRACT.as_bytes());
    let dir = estate_root.join(".wirk").join("contracts");
    std::fs::create_dir_all(&dir).expect("contracts dir");
    std::fs::write(dir.join(format!("{digest}.md")), CONTRACT).expect("contract bytes");
    WorkerContractRef {
        version: "wirk.worker-contract/v1".to_string(),
        digest,
    }
}

/// Stands in for wirkd's reservation-time resolution of one declared
/// doctrine document: the owner's bytes, content-addressed into the
/// estate's own store, and the reference the World carries for them.
fn reserve_doctrine(
    estate_root: &std::path::Path,
    id: &str,
    version: &str,
    repository: Option<&str>,
    text: &str,
) -> EstateDoctrineRef {
    let dir = wirk_herdr::estate_doctrine::store_dir(estate_root);
    std::fs::create_dir_all(&dir).expect("doctrine dir");
    let digest = sha256_hex(text.as_bytes());
    std::fs::write(dir.join(format!("{digest}.md")), text).expect("doctrine bytes");
    EstateDoctrineRef {
        id: id.to_string(),
        version: version.to_string(),
        digest,
        repository: repository.map(str::to_string),
    }
}

fn actor_world(
    run: &Run,
    estate_root: &std::path::Path,
    worktree_path: &std::path::Path,
    contract: Option<WorkerContractRef>,
    doctrine: Vec<EstateDoctrineRef>,
) -> World {
    World::Actor(ActorWorld {
        repository: "alpha".to_string(),
        worktree_path: worktree_path.to_path_buf(),
        branch: "p6/estate-doctrine".to_string(),
        base_sha: "abc123".to_string(),
        source_basis: wirk_core::SourceBasis::Git {
            base: "abc123".to_string(),
        },
        triple: ExecutionTriple {
            estate_root: estate_root.to_string_lossy().into_owned(),
            work_id: WorkId("work-1".to_string()),
            run_id: run.id.clone(),
        },
        intent: "write report.md".to_string(),
        output_contract: OutputContract(vec![ArtifactSpec {
            name: "report.md".to_string(),
            required: true,
        }]),
        boundary: Boundary(vec!["src/**".to_string()]),
        review_targets: Vec::new(),
        evidence: None,
        contract: contract.map(Box::new),
        doctrine,
    })
}

fn launched_claude_args(client: &FakeHerdrClient) -> Vec<String> {
    let calls = client.start_agent_calls.lock().unwrap();
    assert_eq!(calls.len(), 1, "exactly one agent.start");
    calls[0].args.clone()
}

fn appended_path(args: &[String]) -> std::path::PathBuf {
    let at = args
        .iter()
        .position(|arg| arg == "--append-system-prompt-file")
        .unwrap_or_else(|| panic!("claude argv must append by file: {args:?}"));
    std::path::PathBuf::from(
        args.get(at + 1)
            .unwrap_or_else(|| panic!("--append-system-prompt-file needs a path: {args:?}")),
    )
}

// ---------------------------------------------------------------------
// The decisive relation
// ---------------------------------------------------------------------

/// **The decisive one.** A launch whose World reserved the estate
/// owner's own documents points the harness at bytes that carry those
/// documents *and* the shared contract — and the two keep separate
/// identities: the journaled delivery still names the worker contract's
/// own version and digest, and names the estate documents separately
/// beside it.
///
/// Watched red at b0972dc, where `ActorWorld` has no `doctrine` field at
/// all: an owner's document outside every worktree ancestry could not be
/// reserved, could not be delivered, and had no identity to carry.
#[test]
fn a_reserved_estate_document_reaches_the_launch_with_its_own_identity_intact() {
    let estate = tempdir().expect("estate tempdir");
    let worktree = tempdir().expect("worktree tempdir");
    let contract = reserve_contract(estate.path());
    let house = reserve_doctrine(
        estate.path(),
        "house-rules",
        "2026-09-15.1",
        None,
        ESTATE_RULES,
    );
    let repo = reserve_doctrine(
        estate.path(),
        "alpha-rules",
        "v3",
        Some("alpha"),
        REPO_RULES,
    );
    let run = run_with_kind(ActorKind::claude());
    let world = actor_world(
        &run,
        estate.path(),
        worktree.path(),
        Some(contract.clone()),
        vec![house.clone(), repo.clone()],
    );

    let client =
        Arc::new(FakeHerdrClient::default().with_split_pane_response(pane_info(&run.id.0)));
    let executor = HerdrExecutor::new(client.clone());
    let launched = executor
        .launch_actor(&run, &world)
        .expect("launch succeeds");

    let args = launched_claude_args(&client);
    let path = appended_path(&args);
    let delivered = std::fs::read_to_string(&path).expect("the delivered file is readable");

    // Both owners' text is actually in front of the actor.
    assert!(
        delivered.contains("Cite the rung that resolved each decision."),
        "the estate owner's own rules must reach the actor: {delivered}"
    );
    assert!(
        delivered.contains("Never land on main without a green gate."),
        "a document scoped to this Work's own binding must reach it too: {delivered}"
    );
    assert!(
        delivered.contains("File your Claim when the outputs exist."),
        "the shared worker contract is still delivered, whole: {delivered}"
    );
    // …and each is attributed, by the owner's id and version and by the
    // digest of the exact bytes, so a reader can tell whose rule is
    // whose rather than reading one undifferentiated wall.
    assert!(
        delivered.contains(&format!(
            "house-rules (version 2026-09-15.1, sha256 {})",
            house.digest
        )),
        "each document is named by its owner's id and version and its own digest: {delivered}"
    );
    assert!(
        delivered.contains("declared for the `alpha` repository binding"),
        "and by the scope that made it apply: {delivered}"
    );

    // Where the bytes are, and where they are not.
    assert!(
        path.starts_with(wirk_herdr::estate_doctrine::store_dir(estate.path())),
        "the composed document is estate-owned and content-addressed: {path:?}"
    );
    assert!(
        !path.starts_with(worktree.path()),
        "nothing is written into the worktree: {path:?}"
    );

    // The two identities stay apart. The delivery record still names the
    // *contract* — product protocol, same in every estate — and names
    // the estate's own documents separately.
    let delivery = launched
        .contract
        .as_ref()
        .expect("a launch that delivered something records what it delivered");
    assert_eq!(
        delivery.digest, contract.digest,
        "still the contract's own digest"
    );
    assert_eq!(delivery.version, contract.version);
    assert_eq!(delivery.mode, ContractDeliveryMode::AppendSystemPromptFile);
    let composed = delivery
        .composed
        .as_ref()
        .expect("a composed delivery says so");
    assert_eq!(
        composed.documents,
        vec![house.clone(), repo.clone()],
        "in the owner's own declared order"
    );
    assert_eq!(
        composed.digest,
        sha256_hex(delivered.as_bytes()),
        "the composed document's recorded digest is the digest of what was delivered"
    );

    // Nothing the user or the repository owns is replaced.
    for forbidden in [
        "--system-prompt",
        "--system-prompt-file",
        "--append-system-prompt",
    ] {
        assert!(
            !args.iter().any(|arg| arg == forbidden),
            "{forbidden} replaces instructions wirk may only add to: {args:?}"
        );
    }
    for arg in &args {
        assert!(
            !arg.chars().any(char::is_control),
            "Herdr refuses an argv element with a control character: {arg:?}"
        );
    }
}

/// An estate that selected no doctrine launches exactly as it always
/// did: the harness is pointed at the contract's own file, and the
/// delivery record carries no composition at all.
#[test]
fn an_estate_with_no_doctrine_delivers_the_contract_alone_exactly_as_before() {
    let estate = tempdir().expect("estate tempdir");
    let worktree = tempdir().expect("worktree tempdir");
    let contract = reserve_contract(estate.path());
    let run = run_with_kind(ActorKind::claude());
    let world = actor_world(
        &run,
        estate.path(),
        worktree.path(),
        Some(contract.clone()),
        Vec::new(),
    );

    let client =
        Arc::new(FakeHerdrClient::default().with_split_pane_response(pane_info(&run.id.0)));
    let executor = HerdrExecutor::new(client.clone());
    let launched = executor
        .launch_actor(&run, &world)
        .expect("launch succeeds");

    let path = appended_path(&launched_claude_args(&client));
    assert_eq!(
        path,
        wirk_herdr::worker_contract::contract_path(estate.path(), &contract.digest),
        "with nothing selected, the contract's own file is what is delivered"
    );
    assert!(
        launched
            .contract
            .as_ref()
            .expect("delivery recorded")
            .composed
            .is_none(),
        "and nothing is recorded as composed with it"
    );
    assert!(
        !wirk_herdr::estate_doctrine::store_dir(estate.path()).exists(),
        "no doctrine store is created for an estate that declared none"
    );
}

/// Bytes that no longer hash to what the reservation named refuse the
/// launch — before any pane exists. An actor operating under an estate's
/// rules that nobody can show are the rules it was given is exactly what
/// the digest exists to prevent.
#[test]
fn tampered_estate_doctrine_refuses_the_launch_before_any_pane_side_effect() {
    let estate = tempdir().expect("estate tempdir");
    let worktree = tempdir().expect("worktree tempdir");
    let contract = reserve_contract(estate.path());
    let house = reserve_doctrine(estate.path(), "house-rules", "v1", None, ESTATE_RULES);
    std::fs::write(
        wirk_herdr::estate_doctrine::stored_path(estate.path(), &house.digest),
        "# House rules\n\nActually, do whatever you like.\n",
    )
    .expect("tamper");

    let run = run_with_kind(ActorKind::claude());
    let world = actor_world(
        &run,
        estate.path(),
        worktree.path(),
        Some(contract),
        vec![house.clone()],
    );
    let client =
        Arc::new(FakeHerdrClient::default().with_split_pane_response(pane_info(&run.id.0)));
    let executor = HerdrExecutor::new(client.clone());
    let error = executor
        .launch_actor(&run, &world)
        .expect_err("rewritten doctrine must refuse the launch");
    let rendered = error.to_string();
    assert!(
        rendered.contains("house-rules") && rendered.contains("refusing to launch"),
        "the refusal names the document that could not be honoured: {rendered}"
    );
    assert!(
        client.split_pane_calls.lock().unwrap().is_empty()
            && client.start_agent_calls.lock().unwrap().is_empty(),
        "nothing was created before the refusal"
    );

    // And a document that is simply gone refuses the same way.
    std::fs::remove_file(wirk_herdr::estate_doctrine::stored_path(
        estate.path(),
        &house.digest,
    ))
    .expect("remove");
    let error = executor
        .launch_actor(&run, &world)
        .expect_err("missing doctrine must refuse the launch");
    assert!(
        error.to_string().contains("house-rules"),
        "the refusal names it: {error}"
    );
}

/// A harness wirk has no verified native mechanism for still gets the
/// estate's doctrine — as disclosed prompt text that says, in the same
/// breath, that this is ordinary prompt text and that it replaces
/// nothing.
#[test]
fn a_prompt_fallback_discloses_that_the_estate_doctrine_is_prompt_text_too() {
    let estate = tempdir().expect("estate tempdir");
    let worktree = tempdir().expect("worktree tempdir");
    let contract = reserve_contract(estate.path());
    let house = reserve_doctrine(estate.path(), "house-rules", "v1", None, ESTATE_RULES);
    let run = run_with_kind(ActorKind("aider".to_string()));
    let world = actor_world(
        &run,
        estate.path(),
        worktree.path(),
        Some(contract.clone()),
        vec![house.clone()],
    );

    let client =
        Arc::new(FakeHerdrClient::default().with_split_pane_response(pane_info(&run.id.0)));
    let executor = HerdrExecutor::new(client.clone());
    let launched = executor
        .launch_actor(&run, &world)
        .expect("an unknown kind is served, never refused");
    let delivery = launched.contract.as_ref().expect("delivery recorded");
    assert_eq!(delivery.mode, ContractDeliveryMode::Prompt);
    let composed = delivery.composed.as_ref().expect("composed even by prompt");
    assert_eq!(composed.documents, vec![house.clone()]);

    let reason = delivery
        .fallback_reason
        .as_deref()
        .expect("a prompt delivery always discloses why");
    let disclosure = wirk_herdr::worker_contract::prompt_disclosure("aider", reason, true);
    assert!(
        disclosure.contains("estate's own doctrine"),
        "the actor is told the estate's rules arrived as prompt text too: {disclosure}"
    );
    assert!(
        disclosure.contains("does not replace"),
        "and that they are additive: {disclosure}"
    );

    // The composed bytes a later prompt re-reads are on disk, and hash
    // to what the launch recorded.
    let bytes = std::fs::read(wirk_herdr::estate_doctrine::stored_path(
        estate.path(),
        &composed.digest,
    ))
    .expect("the composed document is durable for the life of the Run");
    assert_eq!(sha256_hex(&bytes), composed.digest);
}
