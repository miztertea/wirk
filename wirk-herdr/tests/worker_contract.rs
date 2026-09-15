//! P4.1 (ruling 0202): the shared worker contract's *delivery*, pinned
//! deterministically. What this file pins is the relation the increment
//! exists to create — **the bytes an actor is actually pointed at are
//! the bytes the World reserved** — plus the refusals that make that
//! relation load-bearing rather than decorative.
//!
//! No harness process runs here. Per ruling 0040 a `FakeHerdrClient`
//! pins *shape*: that wirk submits the right argv element, writes the
//! right file, and refuses before any pane side effect. It does **not**
//! establish that claude, opencode or codex read what it submitted —
//! that is actual native use, and it is a separate stage
//! (`BUILD.md` §"For the verifier").
//!
//! The digest half is real, not a fake: the contract bytes are written
//! to a real filesystem and re-hashed here with the same SHA-256 the
//! product uses.

use std::sync::Arc;

use sha2::{Digest, Sha256};
use tempfile::tempdir;

use wirk_core::{
    ActorKind, ActorWorld, ArtifactSpec, Boundary, ContractDeliveryMode, ExecutionTriple, Executor,
    OutputContract, Run, RunId, RunState, WaypointId, WorkId, WorkerContractRef, World, WorldHash,
};
use wirk_herdr::claim_hook::OPENCODE_CONFIG_ENV;
use wirk_herdr::fake::FakeHerdrClient;
use wirk_herdr::{AgentStatus, HerdrExecutor, PaneInfo};

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

fn actor_world(
    run: &Run,
    estate_root: &std::path::Path,
    worktree_path: &std::path::Path,
    contract: Option<WorkerContractRef>,
) -> World {
    World::Actor(ActorWorld {
        doctrine: Vec::new(),
        repository: "wirk".to_string(),
        worktree_path: worktree_path.to_path_buf(),
        branch: "p4/worker-contract".to_string(),
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
    })
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

/// Writes `text` where a reservation would have written it and returns
/// the reference a reserved World carries for it: the content-addressed
/// `<estate_root>/.wirk/contracts/<digest>.md`. The tests below stand in
/// for wirkd's own reservation-time write, so the delivery path can be
/// exercised without a daemon.
fn reserve_contract(estate_root: &std::path::Path, text: &str) -> WorkerContractRef {
    let digest = sha256_hex(text.as_bytes());
    let dir = estate_root.join(".wirk").join("contracts");
    std::fs::create_dir_all(&dir).expect("contracts dir");
    std::fs::write(dir.join(format!("{digest}.md")), text).expect("contract bytes");
    WorkerContractRef {
        version: "wirk.worker-contract/v1".to_string(),
        digest,
    }
}

/// A contract whose bytes exercise the transport, not only the happy
/// path: a double quote, a backslash, an embedded newline and non-ASCII.
/// Herdr POSIX-quotes every argv element and types the line into the
/// pane's shell, refusing any element containing a control character
/// (`invalid_agent_argument`), so "survives quoting and carries a
/// newline" is a correctness property of every delivery mode, not
/// cosmetics.
const AWKWARD_CONTRACT: &str =
    "# Wirk worker contract\n\nA \"quoted\" line with a \\ backslash.\nDeuxième ligne — ✓\n";

/// **A, the decisive one.** A claude launch points the harness at the
/// contract the World reserved, by file, and the bytes at that path hash
/// to the reserved digest. Nothing else about the launch moves: the
/// Claim hook's `--settings` pair is still there, and no
/// prompt-*replacing* flag is ever submitted.
///
/// Red before this wave: not because a constant is missing, but because
/// no path exists by which any file wirk writes can hash to a value the
/// World records — the relation this test asserts did not exist.
#[test]
fn claude_launch_appends_the_reserved_contract_by_file_and_the_bytes_hash_to_the_reserved_digest() {
    let estate = tempdir().expect("estate tempdir");
    let worktree = tempdir().expect("worktree tempdir");
    let reference = reserve_contract(estate.path(), AWKWARD_CONTRACT);
    let run = run_with_kind(ActorKind::claude());
    let world = actor_world(
        &run,
        estate.path(),
        worktree.path(),
        Some(reference.clone()),
    );

    let client =
        Arc::new(FakeHerdrClient::default().with_split_pane_response(pane_info(&run.id.0)));
    let executor = HerdrExecutor::new(client.clone());
    executor.launch(&run, &world).expect("launch succeeds");

    let calls = client.start_agent_calls.lock().unwrap();
    assert_eq!(calls.len(), 1, "exactly one agent.start");
    let args = &calls[0].args;

    let at = args
        .iter()
        .position(|arg| arg == "--append-system-prompt-file")
        .unwrap_or_else(|| panic!("claude argv must append the contract by file: {args:?}"));
    let path = std::path::Path::new(
        args.get(at + 1)
            .unwrap_or_else(|| panic!("--append-system-prompt-file needs a path: {args:?}")),
    );

    // The bytes the harness is pointed at are the bytes that were
    // reserved. This is the whole point of the increment.
    let delivered = std::fs::read(path).expect("the contract file is readable at launch");
    assert_eq!(
        sha256_hex(&delivered),
        reference.digest,
        "the delivered bytes must hash to the digest the World reserved"
    );
    assert_eq!(
        String::from_utf8(delivered).expect("utf-8"),
        AWKWARD_CONTRACT,
        "quotes, backslashes, newlines and non-ASCII survive delivery verbatim"
    );

    // Where the file is, and where it is not.
    assert!(
        path.starts_with(estate.path().join(".wirk").join("contracts")),
        "the contract is estate-owned and content-addressed: {path:?}"
    );
    assert!(
        !path.starts_with(worktree.path()),
        "nothing is written into the worktree: {path:?}"
    );
    let home = std::env::var("HOME").unwrap_or_default();
    if !home.is_empty() {
        assert!(
            !path.starts_with(&home),
            "nothing is written under ~/: {path:?}"
        );
    }

    // Nothing the user or the repository owns is replaced.
    for forbidden in [
        "--system-prompt",
        "--system-prompt-file",
        "--append-system-prompt",
    ] {
        assert!(
            !args.iter().any(|arg| arg == forbidden),
            "{forbidden} replaces or duplicates instructions wirk must only add to: {args:?}"
        );
    }

    // Every element still crosses Herdr's shell-typing path safely.
    for arg in args {
        assert!(
            !arg.chars().any(char::is_control),
            "Herdr refuses an argv element containing a control character: {arg:?}"
        );
    }

    // The Claim hook is untouched.
    assert!(
        args.iter().any(|arg| arg == "--plugin-dir"),
        "the Claim hook's own --plugin-dir pair must survive: {args:?}"
    );
}

/// **C.** opencode's delivery is its own native additive `instructions`
/// array in the wirk-owned overlay — the same configuration that
/// already carries the Claim plugin, which must still be there. No argv
/// element at all, and nothing written under the worktree.
///
/// This launch carries neither config variable, so the overlay reaches
/// opencode through `OPENCODE_CONFIG_CONTENT`, its own inline layer, and
/// **not** through `OPENCODE_CONFIG`: that
/// variable names a single file, so setting it would replace one a
/// launch already carries (review, OpenCode config scope). Measured
/// with `opencode debug config` 1.18.30 on 2026-09-12: content, file
/// and global all resolve together.
#[test]
fn opencode_config_gains_instructions_without_losing_the_claim_plugin() {
    let estate = tempdir().expect("estate tempdir");
    let worktree = tempdir().expect("worktree tempdir");
    let reference = reserve_contract(estate.path(), AWKWARD_CONTRACT);
    let run = run_with_kind(ActorKind::opencode());
    let world = actor_world(
        &run,
        estate.path(),
        worktree.path(),
        Some(reference.clone()),
    );

    let client =
        Arc::new(FakeHerdrClient::default().with_split_pane_response(pane_info(&run.id.0)));
    let executor = HerdrExecutor::new(client.clone());
    executor.launch(&run, &world).expect("launch succeeds");

    // Ruling 0205: `split[0]` is the short-lived probe pane that reads
    // this placement's own opencode configuration; `split[1]` is the
    // actor's pane, which is what these assertions are about.
    let split = client.split_pane_calls.lock().unwrap();
    assert_eq!(split.len(), 2, "the probe pane, then the actor's pane");
    let split = &split[1..];
    assert!(
        !split[0].env.contains_key(OPENCODE_CONFIG_ENV),
        "wirk must not set {OPENCODE_CONFIG_ENV}: it names one file, and a launch that already \
         carries one would lose it: {:?}",
        split[0].env
    );
    let content = split[0]
        .env
        .get("OPENCODE_CONFIG_CONTENT")
        .unwrap_or_else(|| panic!("OPENCODE_CONFIG_CONTENT missing: {:?}", split[0].env));
    assert!(
        !content.chars().any(char::is_control),
        "the inline layer crosses the pane environment as one line: {content:?}"
    );
    let config: serde_json::Value =
        serde_json::from_str(content).expect("the inline layer is JSON");

    let instructions = config["instructions"]
        .as_array()
        .unwrap_or_else(|| panic!("the overlay must carry an instructions array: {config}"));
    assert_eq!(
        instructions.len(),
        1,
        "exactly one entry is added, never a rewritten array: {config}"
    );
    let entry = std::path::Path::new(instructions[0].as_str().expect("an absolute path"));
    assert_eq!(
        sha256_hex(&std::fs::read(entry).expect("instruction file readable")),
        reference.digest,
        "opencode is pointed at the bytes the World reserved"
    );
    assert!(
        config["plugin"]
            .as_array()
            .is_some_and(|plugin| plugin.len() == 1),
        "the Claim plugin must still be registered beside it: {config}"
    );

    let calls = client.start_agent_calls.lock().unwrap();
    assert!(
        !calls[0]
            .args
            .iter()
            .any(|arg| arg.contains("instruction") || arg.contains("prompt")),
        "opencode's delivery is the overlay, not an argv flag: {:?}",
        calls[0].args
    );
    assert!(
        !entry.starts_with(worktree.path()),
        "nothing is written into the worktree: {entry:?}"
    );

    // The overlay file beside the plugin is the same configuration, so
    // the delivered layer and the readable record cannot drift.
    let on_disk: serde_json::Value = serde_json::from_slice(
        &std::fs::read(
            estate
                .path()
                .join(".wirk")
                .join("opencode")
                .join(&run.id.0)
                .join("wirk-opencode-config.json"),
        )
        .expect("overlay file is readable"),
    )
    .expect("overlay file is JSON");
    assert_eq!(on_disk, config, "the file and the inline layer must agree");
}

/// **E.** A reserved contract whose bytes this binary cannot verify is
/// refused **before any pane side effect** — no workspace, no pane, no
/// agent. wirkd is a long-lived daemon and the driver is a separately
/// pinned binary; a contract file that was lost, truncated or rewritten
/// between reservation and launch must not produce an actor operating
/// under bytes nobody reserved.
#[test]
fn a_contract_whose_bytes_do_not_match_the_reserved_digest_is_refused_before_any_pane() {
    let estate = tempdir().expect("estate tempdir");
    let worktree = tempdir().expect("worktree tempdir");
    let reference = reserve_contract(estate.path(), AWKWARD_CONTRACT);
    std::fs::write(
        estate
            .path()
            .join(".wirk")
            .join("contracts")
            .join(format!("{}.md", reference.digest)),
        "tampered\n",
    )
    .expect("corrupt the reserved bytes");

    let run = run_with_kind(ActorKind::claude());
    let world = actor_world(&run, estate.path(), worktree.path(), Some(reference));
    let client =
        Arc::new(FakeHerdrClient::default().with_split_pane_response(pane_info(&run.id.0)));
    let executor = HerdrExecutor::new(client.clone());

    let err = executor
        .launch(&run, &world)
        .expect_err("a contract that does not hash to the reserved digest must refuse the launch");
    let message = err.to_string();
    assert!(
        message.contains("contract"),
        "the refusal must name what it refused: {message}"
    );

    assert!(
        client.start_agent_calls.lock().unwrap().is_empty(),
        "no agent may start"
    );
    assert!(
        client.split_pane_calls.lock().unwrap().is_empty(),
        "no pane may be created: the bytes are verified before any pane side effect"
    );
}

/// **E2.** The same refusal for a reserved contract whose file is not
/// there at all — the case a cleanup pass over the estate can create.
#[test]
fn a_reserved_contract_with_no_bytes_on_disk_is_refused_before_any_pane() {
    let estate = tempdir().expect("estate tempdir");
    let worktree = tempdir().expect("worktree tempdir");
    let run = run_with_kind(ActorKind::claude());
    let world = actor_world(
        &run,
        estate.path(),
        worktree.path(),
        Some(WorkerContractRef {
            version: "wirk.worker-contract/v1".to_string(),
            digest: "0".repeat(64),
        }),
    );
    let client =
        Arc::new(FakeHerdrClient::default().with_split_pane_response(pane_info(&run.id.0)));
    let executor = HerdrExecutor::new(client.clone());

    executor
        .launch(&run, &world)
        .expect_err("a reserved contract with no bytes must refuse the launch");
    assert!(
        client.split_pane_calls.lock().unwrap().is_empty(),
        "no pane may be created"
    );
    assert!(
        client.start_agent_calls.lock().unwrap().is_empty(),
        "no agent may start"
    );
}

/// **G.** Every World reserved before this wave carries no contract, and
/// its launch is byte-for-byte the launch it always was: the Claim
/// hook's `--settings` pair and nothing else added, and no refusal.
/// This is the retained-historical-World case with an actual consumer —
/// the launch path itself.
#[test]
fn a_world_reserved_without_a_contract_launches_exactly_as_it_always_did() {
    let estate = tempdir().expect("estate tempdir");
    let worktree = tempdir().expect("worktree tempdir");
    let run = run_with_kind(ActorKind::claude());
    let world = actor_world(&run, estate.path(), worktree.path(), None);

    let client =
        Arc::new(FakeHerdrClient::default().with_split_pane_response(pane_info(&run.id.0)));
    let executor = HerdrExecutor::new(client.clone());
    executor
        .launch(&run, &world)
        .expect("a World with no contract still launches");

    let calls = client.start_agent_calls.lock().unwrap();
    let args = &calls[0].args;
    assert!(
        !args
            .iter()
            .any(|arg| arg.starts_with("--append-system-prompt")),
        "no contract was reserved, so none is delivered: {args:?}"
    );
    assert_eq!(
        args.iter().filter(|arg| *arg == "--plugin-dir").count(),
        1,
        "the Claim hook is unaffected: {args:?}"
    );
    assert!(
        !estate.path().join(".wirk").join("contracts").exists(),
        "a launch never writes a contract; only a reservation does"
    );
}

/// The delivery mode a launch reports is the mode it actually used —
/// the one fact `RunLaunched.launch_argv` cannot reconstruct for
/// opencode, whose mechanism is an environment variable.
#[test]
fn the_journaled_delivery_mode_names_the_mechanism_actually_used() {
    for (kind, expected) in [
        (
            ActorKind::claude(),
            ContractDeliveryMode::AppendSystemPromptFile,
        ),
        (
            ActorKind::opencode(),
            ContractDeliveryMode::OpencodeInstructions,
        ),
    ] {
        let estate = tempdir().expect("estate tempdir");
        let worktree = tempdir().expect("worktree tempdir");
        let reference = reserve_contract(estate.path(), AWKWARD_CONTRACT);
        let run = run_with_kind(kind.clone());
        let world = actor_world(
            &run,
            estate.path(),
            worktree.path(),
            Some(reference.clone()),
        );

        let client =
            Arc::new(FakeHerdrClient::default().with_split_pane_response(pane_info(&run.id.0)));
        let executor = HerdrExecutor::new(client);
        let launched = executor.launch_actor(&run, &world).expect("launch");
        let delivery = launched
            .contract
            .as_ref()
            .unwrap_or_else(|| panic!("{kind} must report how it delivered the contract"));
        assert_eq!(delivery.mode, expected, "kind {kind}");
        assert_eq!(delivery.digest, reference.digest, "kind {kind}");
        assert_eq!(delivery.version, reference.version, "kind {kind}");
        assert!(
            delivery.fallback_reason.is_none(),
            "a native delivery discloses no fallback: {delivery:?}"
        );
    }
}

/// A probe standing in for `codex debug prompt-input`: `baseline` is
/// what codex renders today, `overridden` what it renders with wirk's
/// `-c developer_instructions=` element. Both answers are real shapes
/// measured from codex-cli 0.154.0 on 2026-09-12 (`BUILD.md` §2).
struct FixedCodexProbe {
    baseline: Vec<String>,
    overridden: Vec<String>,
    seen: std::sync::Mutex<Vec<Vec<String>>>,
}

impl FixedCodexProbe {
    fn new(baseline: &[&str], overridden: &[&str]) -> Self {
        FixedCodexProbe {
            baseline: baseline.iter().map(|t| (*t).to_string()).collect(),
            overridden: overridden.iter().map(|t| (*t).to_string()).collect(),
            seen: std::sync::Mutex::new(Vec::new()),
        }
    }
}

impl wirk_herdr::worker_contract::CodexProbe for FixedCodexProbe {
    fn render(&self, _cwd: &std::path::Path, args: &[String]) -> Result<Vec<String>, String> {
        let mut seen = self.seen.lock().unwrap();
        seen.push(args.to_vec());
        // Baseline first, composed second — the order the decision asks
        // in, and the only unambiguous way to tell the two apart when
        // the launch itself may carry a `developer_instructions`.
        Ok(if seen.len() == 1 {
            self.baseline.clone()
        } else {
            self.overridden.clone()
        })
    }
}

/// **B.** When codex's own dry render says the override adds the
/// contract and displaces nothing, codex takes it natively — as exactly
/// one `-c developer_instructions=` element that is a TOML basic string
/// and carries no control character, so Herdr's shell-typing path
/// accepts it. The effort override wirk already emits stays separate and
/// intact.
#[test]
fn codex_takes_the_contract_as_one_control_char_free_developer_instructions_element() {
    let estate = tempdir().expect("estate tempdir");
    let worktree = tempdir().expect("worktree tempdir");
    let reference = reserve_contract(estate.path(), AWKWARD_CONTRACT);
    let mut run = run_with_kind(ActorKind("codex".to_string()));
    // Effort only. A requested `--model` is an argument `codex debug
    // prompt-input` refuses outright (measured, 0.154.0), so a launch
    // carrying one takes the disclosed fallback below rather than this
    // path — the native path is the one whose whole configuration the
    // renderer can actually carry.
    run.selection = wirk_core::ActorSelection {
        model: None,
        effort: Some("high".to_string()),
        args: Vec::new(),
    };
    let world = actor_world(
        &run,
        estate.path(),
        worktree.path(),
        Some(reference.clone()),
    );

    let probe = Arc::new(FixedCodexProbe::new(
        &["skills", "permissions", "AGENTS.md"],
        &[AWKWARD_CONTRACT, "skills", "permissions", "AGENTS.md"],
    ));
    let client =
        Arc::new(FakeHerdrClient::default().with_split_pane_response(pane_info(&run.id.0)));
    let executor = HerdrExecutor::new(client.clone()).with_codex_probe(probe.clone());
    let launched = executor
        .launch_actor(&run, &world)
        .expect("launch succeeds");

    let calls = client.start_agent_calls.lock().unwrap();
    let args = &calls[0].args;
    let values: Vec<&String> = args
        .iter()
        .filter(|arg| arg.starts_with("developer_instructions="))
        .collect();
    assert_eq!(
        values.len(),
        1,
        "exactly one developer_instructions override: {args:?}"
    );
    let value = values[0];
    assert!(
        !value.chars().any(char::is_control),
        "Herdr refuses an argv element containing a control character: {value:?}"
    );
    // The TOML basic string decodes back to the reserved contract, with
    // its quotes, backslash, newlines and non-ASCII intact.
    let toml = value
        .strip_prefix("developer_instructions=")
        .expect("key=value");
    assert_eq!(
        decode_toml_basic_string(toml),
        AWKWARD_CONTRACT,
        "the override must carry the reserved bytes verbatim: {toml}"
    );

    // wirk's own effort override is a separate element and survives.
    assert!(
        args.iter().any(|arg| arg == "model_reasoning_effort=high"),
        "the effort override must be unaffected: {args:?}"
    );
    assert_eq!(
        args.iter().filter(|arg| *arg == "-c").count(),
        2,
        "one -c for effort and one for the contract: {args:?}"
    );
    // Nothing that would replace codex's own base instructions.
    for forbidden in [
        "base_instructions",
        "model_instructions_file",
        "experimental_instructions_file",
        "managed_developer_instructions",
    ] {
        assert!(
            !args.iter().any(|arg| arg.contains(forbidden)),
            "{forbidden} replaces instructions wirk must only add to: {args:?}"
        );
    }
    // A profile or any other unrelated setting is neither required nor
    // refused: wirk resolves a concrete transport conflict, not a
    // category of settings.
    assert!(
        !args.iter().any(|arg| arg == "--profile"),
        "wirk adds no profile of its own: {args:?}"
    );

    assert_eq!(
        launched.contract.expect("a delivery is reported").mode,
        ContractDeliveryMode::CodexDeveloperInstructions
    );
    assert_eq!(
        probe.seen.lock().unwrap().len(),
        2,
        "the decision is taken from codex's own baseline and composed renders"
    );
}

/// **B2, the qualification ruling 0202 adds.** A codex configuration
/// that already sets `developer_instructions` to a nonempty value would
/// have that value *replaced* by the override — measured on this box.
/// So wirk does not send the override at all: the user's value is left
/// exactly as it was, and the contract is delivered as disclosed prompt
/// text instead.
#[test]
fn an_existing_codex_developer_instructions_value_is_preserved_and_the_contract_falls_back() {
    let estate = tempdir().expect("estate tempdir");
    let worktree = tempdir().expect("worktree tempdir");
    let reference = reserve_contract(estate.path(), AWKWARD_CONTRACT);
    let run = run_with_kind(ActorKind("codex".to_string()));
    let world = actor_world(
        &run,
        estate.path(),
        worktree.path(),
        Some(reference.clone()),
    );

    // The user's own rule is the first developer item; the override
    // replaces it rather than adding to it.
    let probe = Arc::new(FixedCodexProbe::new(
        &["EXISTING USER RULE", "skills", "AGENTS.md"],
        &[AWKWARD_CONTRACT, "skills", "AGENTS.md"],
    ));
    let client =
        Arc::new(FakeHerdrClient::default().with_split_pane_response(pane_info(&run.id.0)));
    let executor = HerdrExecutor::new(client.clone()).with_codex_probe(probe);
    let launched = executor
        .launch_actor(&run, &world)
        .expect("launch succeeds");

    let calls = client.start_agent_calls.lock().unwrap();
    assert!(
        !calls[0]
            .args
            .iter()
            .any(|arg| arg.contains("developer_instructions")),
        "the user's configured developer_instructions must not be overridden: {:?}",
        calls[0].args
    );
    let delivery = launched.contract.expect("a delivery is still reported");
    assert_eq!(
        delivery.mode,
        ContractDeliveryMode::Prompt,
        "the contract is still delivered — by prompt — never dropped"
    );
    assert!(
        delivery
            .fallback_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("developer_instructions")),
        "the fallback must disclose what it protected: {delivery:?}"
    );
}

/// A probe that resolves `developer_instructions` the way codex itself
/// does over an argument list: the **last** `-c developer_instructions=`
/// element wins, and everything else in the render is fixed. Measured
/// shape, codex-cli 0.154.0, 2026-09-12 (§ CORRECTION.md).
struct LastWinsCodexProbe {
    rejects: Vec<String>,
    seen: std::sync::Mutex<Vec<Vec<String>>>,
}

impl LastWinsCodexProbe {
    fn new(rejects: &[&str]) -> Self {
        LastWinsCodexProbe {
            rejects: rejects.iter().map(|r| (*r).to_string()).collect(),
            seen: std::sync::Mutex::new(Vec::new()),
        }
    }
}

impl wirk_herdr::worker_contract::CodexProbe for LastWinsCodexProbe {
    fn render(&self, _cwd: &std::path::Path, args: &[String]) -> Result<Vec<String>, String> {
        self.seen.lock().unwrap().push(args.to_vec());
        if let Some(bad) = args.iter().find(|arg| self.rejects.contains(arg)) {
            // Exactly the shape `codex debug prompt-input` produces for
            // an argument only the interactive CLI accepts.
            return Err(format!(
                "`codex debug prompt-input` exited exit status: 2: error: unexpected argument \
                 '{bad}' found"
            ));
        }
        let mut items = Vec::new();
        if let Some(value) = args
            .iter()
            .filter_map(|arg| arg.strip_prefix("developer_instructions="))
            .next_back()
        {
            items.push(decode_toml_basic_string(value));
        }
        items.push("skills".to_string());
        items.push("AGENTS.md".to_string());
        Ok(items)
    }
}

/// **Review F1, the confirmed regression.** A Route may author its own
/// `-c developer_instructions=…` in `selection.args`. codex resolves the
/// last such element, so wirk's own override would replace it — and a
/// baseline render taken *without* the launch's arguments cannot see
/// that. The decision must be rendered under this launch's own
/// configuration; when it shows a replacement, the Route's value is left
/// alone and the contract is delivered by disclosed prompt.
#[test]
fn a_developer_instructions_value_in_the_launch_s_own_args_is_never_replaced() {
    let estate = tempdir().expect("estate tempdir");
    let worktree = tempdir().expect("worktree tempdir");
    let reference = reserve_contract(estate.path(), AWKWARD_CONTRACT);
    let mut run = run_with_kind(ActorKind("codex".to_string()));
    let route_rule = "developer_instructions=\"EXISTING ROUTE RULE\"".to_string();
    run.selection = wirk_core::ActorSelection {
        model: None,
        effort: Some("high".to_string()),
        args: vec!["-c".to_string(), route_rule.clone()],
    };
    let world = actor_world(
        &run,
        estate.path(),
        worktree.path(),
        Some(reference.clone()),
    );

    let probe = Arc::new(LastWinsCodexProbe::new(&[]));
    let client =
        Arc::new(FakeHerdrClient::default().with_split_pane_response(pane_info(&run.id.0)));
    let executor = HerdrExecutor::new(client.clone()).with_codex_probe(probe.clone());
    let launched = executor
        .launch_actor(&run, &world)
        .expect("launch succeeds");

    let calls = client.start_agent_calls.lock().unwrap();
    let args = &calls[0].args;
    let developer: Vec<&String> = args
        .iter()
        .filter(|arg| arg.starts_with("developer_instructions="))
        .collect();
    assert_eq!(
        developer,
        vec![&route_rule],
        "the Route's own developer_instructions is the only one submitted: {args:?}"
    );
    assert!(
        args.iter().any(|arg| arg == "model_reasoning_effort=high"),
        "the requested effort is still honoured in the launch: {args:?}"
    );

    let delivery = launched.contract.expect("a delivery is still reported");
    assert_eq!(
        delivery.mode,
        ContractDeliveryMode::Prompt,
        "the contract is delivered by prompt, never by silently discarding the Route's value"
    );
    assert!(
        delivery
            .fallback_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("developer_instructions")),
        "the disclosure names what it protected: {delivery:?}"
    );

    let seen = probe.seen.lock().unwrap();
    assert!(
        seen[0].contains(&route_rule),
        "the baseline is rendered under the launch's own arguments: {seen:?}"
    );
}

/// **Review F2.** `codex debug prompt-input` accepts only `-c`, `-i`,
/// `--enable` and `--disable`; an interactive-only argument such as
/// `--profile` makes it refuse. That is a truthful "this launch's
/// composition cannot be shown", so the contract falls back with codex's
/// own complaint in the disclosure — and the argument itself is still
/// submitted to the actual launch, unchanged and unrefused.
#[test]
fn a_codex_argument_the_native_renderer_rejects_falls_back_and_keeps_the_argument() {
    let estate = tempdir().expect("estate tempdir");
    let worktree = tempdir().expect("worktree tempdir");
    let reference = reserve_contract(estate.path(), AWKWARD_CONTRACT);
    let mut run = run_with_kind(ActorKind("codex".to_string()));
    run.selection = wirk_core::ActorSelection {
        model: None,
        effort: None,
        args: vec!["--profile".to_string(), "review".to_string()],
    };
    let world = actor_world(&run, estate.path(), worktree.path(), Some(reference));

    let probe = Arc::new(LastWinsCodexProbe::new(&["--profile"]));
    let client =
        Arc::new(FakeHerdrClient::default().with_split_pane_response(pane_info(&run.id.0)));
    let executor = HerdrExecutor::new(client.clone()).with_codex_probe(probe);
    let launched = executor
        .launch_actor(&run, &world)
        .expect("launch succeeds");

    let calls = client.start_agent_calls.lock().unwrap();
    let args = &calls[0].args;
    assert_eq!(
        args.iter().filter(|arg| *arg == "--profile").count(),
        1,
        "the Route's profile is neither dropped nor refused: {args:?}"
    );
    assert!(
        !args
            .iter()
            .any(|arg| arg.contains("developer_instructions")),
        "nothing is sent that wirk could not show composes: {args:?}"
    );
    let delivery = launched.contract.expect("a delivery is still reported");
    assert_eq!(delivery.mode, ContractDeliveryMode::Prompt);
    assert!(
        delivery
            .fallback_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("--profile")),
        "the disclosure names the argument that stopped the proof: {delivery:?}"
    );
}

/// **Review F3.** A claude launch that already carries an append control
/// of its own occupies the very mechanism wirk delivers through. Whether
/// claude concatenates two append sources or keeps the last is not
/// settled, so wirk adds nothing, leaves the launch's own append exactly
/// as authored, and discloses the prompt fallback.
#[test]
fn an_existing_claude_append_control_is_left_alone_and_the_contract_falls_back() {
    for existing in [
        vec![
            "--append-system-prompt".to_string(),
            "ROUTE RULE".to_string(),
        ],
        vec!["--append-system-prompt-file=/tmp/route-rule.md".to_string()],
    ] {
        let estate = tempdir().expect("estate tempdir");
        let worktree = tempdir().expect("worktree tempdir");
        let reference = reserve_contract(estate.path(), AWKWARD_CONTRACT);
        let mut run = run_with_kind(ActorKind::claude());
        run.selection = wirk_core::ActorSelection {
            model: None,
            effort: None,
            args: existing.clone(),
        };
        let world = actor_world(&run, estate.path(), worktree.path(), Some(reference));

        let client =
            Arc::new(FakeHerdrClient::default().with_split_pane_response(pane_info(&run.id.0)));
        let executor = HerdrExecutor::new(client.clone());
        let launched = executor
            .launch_actor(&run, &world)
            .expect("launch succeeds");

        let calls = client.start_agent_calls.lock().unwrap();
        let args = &calls[0].args;
        for element in &existing {
            assert!(
                args.contains(element),
                "the launch's own append control is untouched: {args:?}"
            );
        }
        assert!(
            !args.iter().any(|arg| arg == "--append-system-prompt-file"),
            "wirk adds no second append source it cannot show composes: {args:?}"
        );
        let delivery = launched.contract.expect("a delivery is still reported");
        assert_eq!(delivery.mode, ContractDeliveryMode::Prompt);
        assert!(
            delivery
                .fallback_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("--append-system-prompt")),
            "the disclosure names the collision: {delivery:?}"
        );
    }
}

/// The other half of F3, and the property 0202 protects: settings that
/// are *not* the append mechanism are neither inspected nor downgraded.
/// An `--agent` or a base `--system-prompt` sets what an append is
/// appended to; the contract still rides natively beside them.
#[test]
fn an_agent_or_base_system_prompt_setting_is_not_downgraded_by_name() {
    let estate = tempdir().expect("estate tempdir");
    let worktree = tempdir().expect("worktree tempdir");
    let reference = reserve_contract(estate.path(), AWKWARD_CONTRACT);
    let mut run = run_with_kind(ActorKind::claude());
    run.selection = wirk_core::ActorSelection {
        model: None,
        effort: None,
        args: vec![
            "--agent".to_string(),
            "reviewer".to_string(),
            "--system-prompt".to_string(),
            "BASE".to_string(),
        ],
    };
    let world = actor_world(&run, estate.path(), worktree.path(), Some(reference));

    let client =
        Arc::new(FakeHerdrClient::default().with_split_pane_response(pane_info(&run.id.0)));
    let executor = HerdrExecutor::new(client.clone());
    let launched = executor
        .launch_actor(&run, &world)
        .expect("launch succeeds");

    let calls = client.start_agent_calls.lock().unwrap();
    let args = &calls[0].args;
    for element in ["--agent", "reviewer", "--system-prompt", "BASE"] {
        assert!(
            args.iter().any(|arg| arg == element),
            "{element} is passed through untouched: {args:?}"
        );
    }
    assert!(
        args.iter().any(|arg| arg == "--append-system-prompt-file"),
        "the contract still rides natively beside them: {args:?}"
    );
    assert_eq!(
        launched.contract.expect("a delivery").mode,
        ContractDeliveryMode::AppendSystemPromptFile
    );
}

/// Ruling 0202's "a swallowed overlay error followed by an
/// instruction-less actor is not success": when opencode's overlay
/// cannot be written, the launch does not proceed contract-less. It
/// falls back to prompt delivery and says why.
#[test]
fn an_opencode_overlay_that_cannot_be_written_falls_back_rather_than_launching_contract_less() {
    let estate = tempdir().expect("estate tempdir");
    let worktree = tempdir().expect("worktree tempdir");
    let reference = reserve_contract(estate.path(), AWKWARD_CONTRACT);
    let run = run_with_kind(ActorKind::opencode());
    let world = actor_world(&run, estate.path(), worktree.path(), Some(reference));

    // Make the per-Run overlay directory unwritable by occupying its
    // path with a plain file: `create_dir_all` then fails.
    let opencode_dir = estate.path().join(".wirk").join("opencode");
    std::fs::create_dir_all(&opencode_dir).expect("opencode dir");
    std::fs::write(opencode_dir.join(&run.id.0), "not a directory").expect("block the path");

    let client =
        Arc::new(FakeHerdrClient::default().with_split_pane_response(pane_info(&run.id.0)));
    let executor = HerdrExecutor::new(client.clone());
    let launched = executor
        .launch_actor(&run, &world)
        .expect("the launch still happens");

    let delivery = launched.contract.expect("a delivery is still reported");
    assert_eq!(
        delivery.mode,
        ContractDeliveryMode::Prompt,
        "a failed overlay write must not leave the actor with no contract at all"
    );
    assert!(
        delivery
            .fallback_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("overlay")),
        "the fallback names the real reason, not a generic one: {delivery:?}"
    );
}

/// Decodes the TOML basic string wirk emits, so the test asserts against
/// the bytes codex would actually parse rather than against wirk's own
/// escaping function.
fn decode_toml_basic_string(literal: &str) -> String {
    let inner = literal
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .expect("a TOML basic string is quoted");
    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next().expect("an escape has a body") {
            'n' => out.push('\n'),
            't' => out.push('\t'),
            'r' => out.push('\r'),
            '"' => out.push('"'),
            '\\' => out.push('\\'),
            'u' => {
                let hex: String = (0..4)
                    .map(|_| chars.next().expect("4 hex digits"))
                    .collect();
                out.push(
                    char::from_u32(u32::from_str_radix(&hex, 16).expect("hex")).expect("scalar"),
                );
            }
            other => panic!("unexpected TOML escape \\{other}"),
        }
    }
    out
}
