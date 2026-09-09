//! P3 W-C1, the real-service half: a real `wirkd`, a real Git
//! repository, a real Atlas, the real built binary, and the public
//! `wirk world show` an actor actually types.
//!
//! Nothing here is a fake (ruling 0040): every projection is assembled
//! by the shipped daemon out of a real published generation, written to
//! a real file, and read back over a real socket.

#[path = "support/nested_harness.rs"]
mod harness;
#[path = "support/read_barrier.rs"]
mod read_barrier;
#[path = "../src/wirkd/mod.rs"]
mod wirkd;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use harness::{
    KillOnDrop, claim_ok, init_repo, materialize_actor, start_wirkd, status, stop_wirkd,
    submit_kind, wirk_bin, write_file,
};
use wirk_core::{EventKind, World};

/// The authored question a real stage would carry: a natural-language
/// sentence with exact symbol and path references inside it. The
/// assembler resolves the references and leaves the sentence alone — it
/// never scores it, never judges it and never answers it.
const QUESTION: &str = "Which function in wirkd/server.rs decides whether a Claim is refused for a \
                        changed path outside the declared boundary, and how does reserve_next_leaf \
                        choose the base_sha for the Waypoint it reserves next? Read \
                        src/server.rs and notes/boundary.md before answering.";

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
    (
        output.status.success(),
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim())
            .unwrap_or(serde_json::Value::Null),
        String::from_utf8_lossy(&output.stderr).trim().to_string(),
    )
}

/// A throwaway source repository whose real content is what the
/// assembler resolves against: one code file holding the two identifiers
/// the question names, and one knowledge file so a `Standing` lifetime
/// is a real content-family answer rather than an assumption.
fn source_repo(root: &Path) -> PathBuf {
    let repo = root.join("source-repo");
    fs::create_dir_all(&repo).expect("source repo dir");
    init_repo(&repo);
    fs::create_dir_all(repo.join("src")).expect("src dir");
    fs::create_dir_all(repo.join("notes")).expect("notes dir");
    write_file(
        &repo,
        "src/server.rs",
        "pub fn claim_boundary_refusal(path: &str) -> bool {\n    // the boundary decision\n    \
         path.starts_with(\"src/\")\n}\n\npub fn reserve_next_leaf(after: &str) -> String {\n    \
         format!(\"{after}/next\")\n}\n",
    );
    write_file(
        &repo,
        "notes/boundary.md",
        "# Boundary\n\nThe boundary is the declared mutation surface for one Waypoint.\n",
    );
    // Ruling 0127's executed case, as real committed bytes: the name
    // `embedded_marker` occurs **only** inside the longer token
    // `wrapper_embedded_marker_tail`. The lexical index tokenizes on
    // non-alphanumerics and `_`, so the shorter name is never its own
    // term and candidate discovery returns nothing for it — while the
    // bytes are plainly there.
    write_file(
        &repo,
        "src/markers.rs",
        "pub fn wrapper_embedded_marker_tail() -> u8 {\n    1\n}\n",
    );
    commit_all(&repo);
    repo
}

fn commit_all(repo: &Path) {
    for args in [
        vec!["add", "-A"],
        vec![
            "-c",
            "user.name=projection-test",
            "-c",
            "user.email=projection@example.test",
            "commit",
            "-q",
            "-m",
            "content",
        ],
    ] {
        assert!(
            Command::new("git")
                .args(&args)
                .current_dir(repo)
                .env("GIT_AUTHOR_DATE", "2020-01-01T00:00:00+0000")
                .env("GIT_COMMITTER_DATE", "2020-01-01T00:00:00+0000")
                .status()
                .expect("git runs")
                .success(),
            "git {args:?} failed"
        );
    }
}

fn publish(estate: &Path, alias: &str, repo: &Path) -> String {
    let (ok, acquired, err) = atlas(
        estate,
        &[
            "acquire",
            "--source",
            alias,
            "--repository",
            repo.to_str().unwrap(),
            "--revision",
            "HEAD",
        ],
    );
    assert!(ok, "atlas acquire {alias}: {err}");
    let generation = acquired["generation"]["generation"]
        .as_str()
        .expect("acquired generation id")
        .to_string();
    let (ok, _, err) = atlas(
        estate,
        &["publish", "--source", alias, "--generation", &generation],
    );
    assert!(ok, "atlas publish {alias}: {err}");
    generation
}

/// Writes a Route with two Actor leaves, the first of which orients.
fn orienting_route(estate: &Path, sources: &str) -> PathBuf {
    let dir = estate.join("routes");
    fs::create_dir_all(&dir).expect("routes dir");
    let path = dir.join("orienting.json");
    fs::write(
        &path,
        format!(
            r#"{{"id":"orienting","waypoints":[
              {{"id":"orienting/investigate","kind":"Actor","declared_outputs":[{{"name":"first.md","required":true}}],
                "intent":"Investigate claim_boundary_refusal in src/server.rs.",
                "orient":{{"question":{question},"sources":{sources}}}}},
              {{"id":"orienting/implement","kind":"Actor","declared_outputs":[{{"name":"second.md","required":true}}],
                "intent":"Implement the change in src/server.rs.",
                "orient":{{"question":"Change reserve_next_leaf so the base_sha is read fresh.","sources":{sources}}}}}
            ]}}"#,
            question = serde_json::to_string(QUESTION).unwrap(),
        ),
    )
    .expect("write route");
    path
}

/// A Route with one orienting Actor leaf carrying exactly the authored
/// text a test wants to see resolved. Same shape as `orienting_route`,
/// with the question and intent supplied rather than fixed.
fn authored_route(estate: &Path, name: &str, question: &str, intent: &str) -> PathBuf {
    let dir = estate.join("routes");
    fs::create_dir_all(&dir).expect("routes dir");
    let path = dir.join(format!("{name}.json"));
    fs::write(
        &path,
        format!(
            r#"{{"id":{id},"waypoints":[
              {{"id":{leaf},"kind":"Actor","declared_outputs":[{{"name":"first.md","required":true}}],
                "intent":{intent},
                "orient":{{"question":{question},"sources":["demo"]}}}}
            ]}}"#,
            id = serde_json::to_string(name).unwrap(),
            leaf = serde_json::to_string(&format!("{name}/investigate")).unwrap(),
            intent = serde_json::to_string(intent).unwrap(),
            question = serde_json::to_string(question).unwrap(),
        ),
    )
    .expect("write route");
    path
}

/// Adds `count` real, indexed knowledge files to the estate's source and
/// republishes it, so a question can name far more authored references
/// than any budget the assembler ever had.
fn add_notes(estate: &mut Estate, count: usize) {
    for index in 1..=count {
        write_file(
            &estate.repo,
            &format!("notes/n{index}.md"),
            &format!("# Note {index}\n\nA real, indexed, resolvable knowledge file.\n"),
        );
    }
    commit_all(&estate.repo);
    publish(&estate.root, "demo", &estate.repo);
}

fn plain_route(estate: &Path) -> PathBuf {
    let dir = estate.join("routes");
    fs::create_dir_all(&dir).expect("routes dir");
    let path = dir.join("plain.json");
    fs::write(
        &path,
        r#"{"id":"plain","waypoints":[
          {"id":"plain/one","kind":"Actor","declared_outputs":[{"name":"first.md","required":true}],"intent":"Do the first thing."},
          {"id":"plain/two","kind":"Actor","declared_outputs":[{"name":"second.md","required":true}],"intent":"Do the second thing."}
        ]}"#,
    )
    .expect("write route");
    path
}

/// `wirk world show` exactly as an actor types it: the injected triple
/// in the environment, no `--work`, no `--estate`.
fn world_show(estate: &Path, work: &str, run: &str) -> (Option<i32>, serde_json::Value, String) {
    let output = Command::new(wirk_bin())
        .args(["world", "show", "--json"])
        .env("WIRK_ESTATE_ROOT", estate)
        .env("WIRK_WORK_ID", work)
        .env("WIRK_RUN_ID", run)
        .output()
        .expect("wirk world show runs");
    (
        output.status.code(),
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim())
            .unwrap_or(serde_json::Value::Null),
        String::from_utf8_lossy(&output.stderr).trim().to_string(),
    )
}

fn reserved_world(socket: &Path, work: &str) -> World {
    serde_json::from_value(status(socket, work)["world"].clone()).expect("reserved World")
}

struct Estate {
    _dir: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    daemon: Option<KillOnDrop>,
    socket: PathBuf,
}

impl Estate {
    /// A whole estate: a real daemon, a real source repository acquired
    /// and published into a real Atlas.
    fn new() -> Estate {
        let dir = tempfile::tempdir().expect("temp estate");
        let root = dir.path().join("estate");
        fs::create_dir_all(&root).expect("estate dir");
        let repo = source_repo(dir.path());
        let (daemon, pointer) = start_wirkd(&root);
        publish(&root, "demo", &repo);
        Estate {
            _dir: dir,
            root,
            repo,
            daemon: Some(daemon),
            socket: pointer.socket,
        }
    }

    fn stop(&mut self) {
        if let Some(daemon) = self.daemon.take() {
            stop_wirkd(&self.root, daemon);
        }
    }

    fn restart(&mut self) {
        self.stop();
        let (daemon, pointer) = start_wirkd(&self.root);
        self.daemon = Some(daemon);
        self.socket = pointer.socket;
    }
}

/// **The decisive check** (BUILD.md §8, W-C1): submit a Route with an
/// `orient` block against a real daemon; the reservation's `evidence`
/// ref is on the journaled World, the file exists at the observation id
/// it names, and its `bound` list holds coordinates resolved from the
/// authored question and intent — resolvable, through the public CLI, by
/// the actor that received them.
#[test]
fn an_orienting_reservation_delivers_a_real_projection_an_actor_can_inspect() {
    let mut estate = Estate::new();
    let route = orienting_route(&estate.root, r#"["demo"]"#);
    let submitted = submit_kind(
        &estate.root,
        route.to_str().unwrap(),
        &estate.repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .expect("submit");

    // The journaled World carries the reference.
    let world = reserved_world(&estate.socket, &submitted.work_id);
    let reference = world
        .evidence()
        .cloned()
        .expect("the reserved World names a projection");
    assert_eq!(reference.format, wirk_core::PROJECTION_FORMAT);
    assert_eq!(reference.revision, 0);

    // The file is at the observation id the reference names, under this
    // Work's own directory.
    let path = estate
        .root
        .join("works")
        .join(&submitted.work_id)
        .join("projections")
        .join(format!("{}.json", reference.observation.0));
    assert!(path.is_file(), "no projection file at {}", path.display());

    // And the public verb serves it, from the triple alone.
    let (code, shown, err) = world_show(&estate.root, &submitted.work_id, &submitted.run_id);
    assert_eq!(code, Some(0), "world show failed: {err}");
    assert_eq!(shown["orientation"], "delivered", "{shown}");
    assert_eq!(shown["current"], true);
    let projection = &shown["projection"];
    assert_eq!(projection["question"], QUESTION);
    assert_eq!(projection["compilation_policy"], wirk_core::ASSEMBLY_POLICY);

    // Real coordinates, resolved out of the authored text against the
    // published generation — not a fixture and not a guess.
    let bound = projection["bound"].as_array().expect("bound list");
    let reasons: Vec<&str> = bound
        .iter()
        .map(|item| item["reason"].as_str().unwrap_or_default())
        .collect();
    assert!(
        reasons.iter().any(|r| r.contains("`src/server.rs`")),
        "the authored path src/server.rs must bind: {reasons:?}"
    );
    assert!(
        reasons.iter().any(|r| r.contains("`notes/boundary.md`")),
        "the authored path notes/boundary.md must bind: {reasons:?}"
    );
    assert!(
        reasons
            .iter()
            .any(|r| r.contains("`claim_boundary_refusal`")),
        "the authored identifier claim_boundary_refusal must bind: {reasons:?}"
    );
    assert!(
        bound.iter().any(|item| item["lifetime"] == "standing"),
        "the Knowledge-family resource must bind as Standing: {bound:?}"
    );

    // Every bound coordinate is resolvable by the actor that was handed
    // it, through the public Atlas verb — the projection delivers
    // addresses, and they work.
    for item in bound {
        let coordinate = item["coordinate"].as_str().expect("coordinate string");
        let (ok, resolved, err) = atlas(
            &estate.root,
            &[
                "resolve",
                "--coordinate",
                coordinate,
                "--work",
                &submitted.work_id,
            ],
        );
        assert!(ok, "atlas resolve {coordinate}: {err} {resolved}");
    }

    // Every generation the projection names is one it actually read at.
    let generations = projection["generations"].as_array().expect("generations");
    assert_eq!(generations.len(), 1, "{projection}");
    for item in bound {
        assert_eq!(
            item["identity"]["generation"], generations[0][1],
            "a bound item must name a generation from the captured vector"
        );
    }

    // Ordinary prose is not an unknown fact, and an unresolved reference
    // is not a judgement.
    let unknowns = projection["unknowns"].as_array().expect("unknowns");
    let unknown_text: Vec<&str> = unknowns
        .iter()
        .map(|s| s["text"].as_str().unwrap_or_default())
        .collect();
    for prose in ["Which", "function", "whether", "declared", "answering"] {
        assert!(
            !unknown_text
                .iter()
                .any(|t| t.contains(&format!("`{prose}`"))),
            "ordinary prose word {prose:?} was reported as an unknown: {unknown_text:?}"
        );
    }
    for statement in unknowns {
        assert_eq!(statement["attributed_to"], "intent");
    }
    // The assembler says what it did, rather than letting a
    // literal-only projection read as a complete orientation. W-C3
    // narrowed the sentence — expansion stopped being among the things
    // this product cannot do — and W-C4 replaced its remaining half
    // outright: consulted findings and the index note *are* assembled
    // now, so the sentence states what was consulted and what the index
    // backing it actually was. Leaving the old disclosure standing while
    // adding the fields underneath would have left a delivered context
    // describing itself incorrectly.
    let assumptions: Vec<&str> = projection["assumptions"]
        .as_array()
        .expect("assumptions")
        .iter()
        .map(|s| s["text"].as_str().unwrap_or_default())
        .collect();
    assert!(
        !assumptions.iter().any(|text| text.contains(
            "consulted estate findings and the findings-index health note are not \
                      assembled here"
        )),
        "the wave's own limit is no longer a limit and must not still be claimed: {assumptions:?}"
    );
    assert!(
        assumptions
            .iter()
            .any(|text| text.contains("record(s) of this Work's own journal")
                && text.contains("settled estate publication(s) were consulted")),
        "the projection must say what it consulted: {assumptions:?}"
    );
    assert!(
        assumptions
            .iter()
            .any(|text| text.contains("`wirk world expand` adds a later revision")),
        "and must not claim there is no verb that adds to it: {assumptions:?}"
    );
    // And the two fields are really there, with the estate's own state
    // in the note rather than a placeholder.
    assert!(
        projection["consulted"].is_array(),
        "a delivered projection carries a consulted list: {projection}"
    );
    assert_eq!(
        projection["findings_index"]["state"], "synchronized",
        "a real daemon reconciles its index at startup: {projection}"
    );

    estate.stop();
}

/// Ruling 0126, F3: the one instruction W-C1 gives a fresh actor for
/// closing the loop is the line `world show` prints under every bound
/// item, and it did not run — `wirk atlas resolve --coordinate <c>`
/// failed with the `atlas` usage banner inside a valid pane, because
/// `--estate` and `--work` were required.
///
/// This runs the printed line **verbatim**, with nothing in the
/// environment but the injected triple, and resolves the real committed
/// bytes. Beside it: an explicit flag still wins, a half-injected
/// environment is refused rather than widened, and no context at all is
/// still the usage error it always was.
#[test]
fn the_resolve_command_world_show_prints_runs_verbatim_inside_the_pane() {
    let mut estate = Estate::new();
    let route = orienting_route(&estate.root, r#"["demo"]"#);
    let submitted = submit_kind(
        &estate.root,
        route.to_str().unwrap(),
        &estate.repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .expect("submit");

    // The line an actor reads, taken out of `world show`'s own text
    // rendering rather than reconstructed from the JSON.
    let printed = Command::new(wirk_bin())
        .args(["world", "show"])
        .env("WIRK_ESTATE_ROOT", &estate.root)
        .env("WIRK_WORK_ID", &submitted.work_id)
        .env("WIRK_RUN_ID", &submitted.run_id)
        .output()
        .expect("world show runs");
    let rendered = String::from_utf8_lossy(&printed.stdout).to_string();
    let instruction = rendered
        .lines()
        .find_map(|line| line.trim().strip_prefix("resolve with: "))
        .unwrap_or_else(|| panic!("world show prints a resolve instruction: {rendered}"))
        .to_string();
    let argv: Vec<&str> = instruction.split_whitespace().collect();
    assert_eq!(
        &argv[..2],
        &["wirk", "atlas"],
        "the printed instruction is the public verb: {instruction}"
    );

    let in_pane = |args: &[&str], triple: &[(&str, &str)]| {
        let mut command = Command::new(wirk_bin());
        command.args(args).env_clear().env("PATH", "/usr/bin:/bin");
        for (name, value) in triple {
            command.env(name, value);
        }
        command.output().expect("wirk runs")
    };
    let full_triple = [
        ("WIRK_ESTATE_ROOT", estate.root.to_str().unwrap()),
        ("WIRK_WORK_ID", submitted.work_id.as_str()),
        ("WIRK_RUN_ID", submitted.run_id.as_str()),
    ];

    let output = in_pane(&argv[1..], &full_triple);
    assert!(
        output.status.success(),
        "the printed instruction must run as printed: {}\n{}",
        instruction,
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("outcome resolved")
            && (text.contains("src/server.rs") || text.contains("notes/boundary.md")),
        "it must resolve the real bound bytes: {text}"
    );

    // An explicit flag still wins over the injected context.
    let coordinate = argv.last().expect("the coordinate is the last argument");
    let explicit = in_pane(
        &[
            "atlas",
            "resolve",
            "--estate",
            estate.root.to_str().unwrap(),
            "--work",
            &submitted.work_id,
            "--coordinate",
            coordinate,
        ],
        &full_triple,
    );
    assert!(explicit.status.success(), "{:?}", explicit.status);

    // Half a triple names no identity, and the one thing it must never
    // do is widen to the administrative read.
    let partial = in_pane(
        &["atlas", "resolve", "--coordinate", coordinate],
        &full_triple[..1],
    );
    assert!(!partial.status.success());
    let refusal = String::from_utf8_lossy(&partial.stderr);
    assert!(
        refusal.contains("incomplete") && refusal.contains("WIRK_WORK_ID"),
        "an incomplete context is refused by name: {refusal}"
    );

    // No context at all: the usage error it always was, unchanged.
    let bare = in_pane(&["atlas", "resolve", "--coordinate", coordinate], &[]);
    assert!(!bare.status.success());
    assert!(
        String::from_utf8_lossy(&bare.stderr).contains("usage: wirk atlas"),
        "outside an actor context nothing changes"
    );

    estate.stop();
}

/// A Waypoint with no `orient` block does exactly what it always did:
/// no Atlas work, no projection, and a World that serializes without the
/// field at all — so its journal line, not only its hash, is unchanged.
#[test]
fn a_waypoint_without_an_orient_block_reserves_an_unchanged_world_and_says_so() {
    let mut estate = Estate::new();
    let route = plain_route(&estate.root);
    let submitted = submit_kind(
        &estate.root,
        route.to_str().unwrap(),
        &estate.repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .expect("submit");

    let world = reserved_world(&estate.socket, &submitted.work_id);
    assert!(world.evidence().is_none());
    let serialized = serde_json::to_string(&world).expect("serialize");
    assert!(
        !serialized.contains("evidence"),
        "a World without a projection must serialize no evidence field: {serialized}"
    );
    assert!(
        !estate
            .root
            .join("works")
            .join(&submitted.work_id)
            .join("projections")
            .exists(),
        "an unoriented reservation must do no projection work at all"
    );

    let (code, shown, err) = world_show(&estate.root, &submitted.work_id, &submitted.run_id);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(shown["orientation"], "none", "{shown}");
    assert_eq!(
        shown["detail"], "this Waypoint declared no orientation request",
        "{shown}"
    );
    assert!(shown.get("projection").is_none());

    estate.stop();
}

/// The refusal aimed where `Unknown` is minted (BUILD.md §3.3), with its
/// positive control: the *same* Route, submitted the way an orienting
/// Actor stage must be submitted, is accepted with **no**
/// `--source-basis` flag at all and journals a Git basis.
#[test]
fn an_orienting_route_is_refused_on_the_unknown_basis_submit_arm_and_accepted_with_a_checkout() {
    let mut estate = Estate::new();
    let route = orienting_route(&estate.root, r#"["demo"]"#);

    // The bare arm: no `--kind actor`, no `--repo-path`. Refused, before
    // anything is journaled.
    let output = Command::new(wirk_bin())
        .args(["work", "submit", "--estate"])
        .arg(&estate.root)
        .args(["--repo", "demo:write", "--base", "HEAD", "--route"])
        .arg(&route)
        .output()
        .expect("work submit runs");
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(!output.status.success(), "the bare arm must refuse");
    assert!(
        stderr.contains("UnsupportedAssembly") && stderr.contains("orienting/investigate"),
        "refusal must name the Waypoint: {stderr}"
    );
    // Ruling 0126, F4: a refusal an author will actually hit is read by
    // a person, and this one carried a broken string continuation —
    // `…which needs a recorded<26 spaces>source basis`. A user-facing
    // sentence has no run of whitespace in it.
    assert!(
        stderr.contains("which needs a recorded source basis: submit it with --kind actor"),
        "the refusal must read as one sentence: {stderr:?}"
    );
    assert!(
        !stderr.contains("  "),
        "a mangled string continuation leaks into the message: {stderr:?}"
    );
    let works = estate.root.join("works");
    let journaled = fs::read_dir(&works).map(|d| d.count()).unwrap_or(0);

    // The positive control: `--kind actor --repo-path`, and no
    // `--source-basis` anywhere on the line.
    let output = Command::new(wirk_bin())
        .args(["work", "submit", "--estate"])
        .arg(&estate.root)
        .args([
            "--repo",
            "demo:write",
            "--base",
            "HEAD",
            "--kind",
            "actor",
            "--repo-path",
        ])
        .arg(&estate.repo)
        .args(["--route"])
        .arg(&route)
        .output()
        .expect("work submit runs");
    assert!(
        output.status.success(),
        "the checkout arm must be accepted with no --source-basis: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_dir(&works).map(|d| d.count()).unwrap_or(0),
        journaled + 1,
        "exactly one Work was journaled: the refused submit wrote none"
    );
    let work_id = String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .collect::<Vec<_>>()
        .chunks(2)
        .find_map(|pair| (pair[0] == "work_id").then(|| pair[1].to_string()))
        .expect("work_id");
    let world = reserved_world(&estate.socket, &work_id);
    assert!(matches!(
        world.source_basis(),
        wirk_core::SourceBasis::Git { .. }
    ));
    assert!(world.evidence().is_some());

    estate.stop();
}

/// A hand-edited journal line pairing an `Unknown` Actor basis with a
/// projection is refused at the reader, **before** the legacy upgrade —
/// and the control immediately beside it proves the upgrade itself still
/// works, so a legacy journal stays replayable.
#[test]
fn an_unknown_basis_world_carrying_a_projection_is_refused_on_replay_but_a_legacy_one_is_not() {
    let mut estate = Estate::new();
    let route = orienting_route(&estate.root, r#"["demo"]"#);
    let oriented = submit_kind(
        &estate.root,
        route.to_str().unwrap(),
        &estate.repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .expect("submit");
    let plain = plain_route(&estate.root);
    let legacy = submit_kind(
        &estate.root,
        plain.to_str().unwrap(),
        &estate.repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .expect("submit");
    estate.stop();

    // Both journals are rewritten the way a corrupted or hand-edited one
    // would be: the Actor World's basis is dropped back to `Unknown`.
    // One of them carries a projection; the other does not.
    for work in [&oriented.work_id, &legacy.work_id] {
        let path = estate.root.join("works").join(work).join("journal.ndjson");
        let text = fs::read_to_string(&path).expect("read journal");
        let mut hashes: Vec<String> = Vec::new();
        let rewritten: Vec<String> = text
            .lines()
            .map(|line| {
                let mut value: serde_json::Value = match serde_json::from_str(line) {
                    Ok(value) => value,
                    Err(_) => return line.to_string(),
                };
                if let Some(actor) = value
                    .pointer_mut("/event/kind/world/Actor")
                    .and_then(|w| w.as_object_mut())
                {
                    actor.insert(
                        "source_basis".to_string(),
                        serde_json::json!({"kind": "unknown"}),
                    );
                    // The forger recomputes the hash too — otherwise the
                    // reservation is refused for a hash mismatch and the
                    // refusal under test never runs.
                    let world: World =
                        serde_json::from_value(value["event"]["kind"]["world"].clone())
                            .expect("edited World still parses");
                    let hash = wirk_core::WorldHash::of(&world).0;
                    value["event"]["kind"]["world_hash"] = serde_json::json!(hash);
                    hashes.push(hash);
                } else if value["event"]["kind"]["kind"] == "RunOpened"
                    && let Some(hash) = hashes.last()
                {
                    value["event"]["kind"]["world_hash"] = serde_json::json!(hash);
                }
                value.to_string()
            })
            .collect();
        fs::write(&path, format!("{}\n", rewritten.join("\n"))).expect("rewrite journal");
    }

    estate.restart();
    let oriented_status = status(&estate.socket, &oriented.work_id);
    let binding = &oriented_status["runs"][0]["world_binding"];
    assert_eq!(binding["state"], "unavailable", "{oriented_status}");
    assert!(
        binding["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("stage projection with no recorded source basis"),
        "{binding}"
    );

    // The control: a legacy Unknown Actor World with no projection still
    // resolves exactly as it always did, and says it was upgraded.
    let legacy_status = status(&estate.socket, &legacy.work_id);
    let binding = &legacy_status["runs"][0]["world_binding"];
    assert_eq!(binding["state"], "resolved", "{legacy_status}");
    assert_eq!(binding["legacy_basis"], true, "{binding}");

    estate.stop();
}

/// `world show` resolves from the triple, and only from the triple. A
/// caller naming another Work's Run — its real, journaled Run id — reads
/// nothing: the Run is looked for in *this* Work's journal, and there is
/// no argument on the surface that could point it elsewhere.
#[test]
fn world_show_refuses_a_foreign_triple_and_serves_the_callers_own() {
    let mut estate = Estate::new();
    let route = orienting_route(&estate.root, r#"["demo"]"#);
    let mine = submit_kind(
        &estate.root,
        route.to_str().unwrap(),
        &estate.repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .expect("submit");
    let theirs = submit_kind(
        &estate.root,
        route.to_str().unwrap(),
        &estate.repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .expect("submit");

    // Control: my own triple works.
    let (code, shown, err) = world_show(&estate.root, &mine.work_id, &mine.run_id);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(shown["orientation"], "delivered");

    // My Work id, their Run id.
    let (code, _, err) = world_show(&estate.root, &mine.work_id, &theirs.run_id);
    assert_eq!(code, Some(3), "a foreign Run must be refused: {err}");
    assert!(err.contains("TripleMismatch"), "{err}");

    // Their Work id, my Run id.
    let (code, _, err) = world_show(&estate.root, &theirs.work_id, &mine.run_id);
    assert_eq!(code, Some(3), "{err}");
    assert!(err.contains("TripleMismatch"), "{err}");

    // A second estate: the same ids resolve to nothing at all.
    let other = tempfile::tempdir().expect("second estate");
    let other_root = other.path().join("estate");
    fs::create_dir_all(&other_root).expect("estate dir");
    let (daemon, _) = start_wirkd(&other_root);
    let (code, _, err) = world_show(&other_root, &mine.work_id, &mine.run_id);
    assert_eq!(code, Some(3), "{err}");
    assert!(err.contains("NotFound"), "{err}");
    stop_wirkd(&other_root, daemon);

    estate.stop();
}

/// A source the Work is not bound to is a count and nothing else: no
/// alias, no membership id, no statement about whether it exists.
#[test]
fn an_unbound_source_alias_is_a_count_only_omission_and_the_bound_one_still_resolves() {
    let mut estate = Estate::new();
    let secret = source_repo(&estate.root.join("secret-src"));
    write_file(&secret, "src/server.rs", "pub fn secret_only() {}\n");
    commit_all(&secret);
    publish(&estate.root, "secret", &secret);

    let route = orienting_route(&estate.root, r#"["demo","secret"]"#);
    let submitted = submit_kind(
        &estate.root,
        route.to_str().unwrap(),
        &estate.repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .expect("submit");

    let (code, shown, err) = world_show(&estate.root, &submitted.work_id, &submitted.run_id);
    assert_eq!(code, Some(0), "{err}");
    let projection = &shown["projection"];
    let omitted = projection["omitted"].as_array().expect("omitted");
    let inadmissible: Vec<&serde_json::Value> = omitted
        .iter()
        .filter(|item| item["kind"] == "inadmissible")
        .collect();
    assert_eq!(inadmissible.len(), 1, "{omitted:?}");
    assert_eq!(inadmissible[0]["count"], 1);
    assert_eq!(
        inadmissible[0].as_object().unwrap().len(),
        2,
        "an inadmissible omission carries a kind and a count, nothing else: {}",
        inadmissible[0]
    );
    let rendered = shown.to_string();
    assert!(
        !rendered.contains("secret"),
        "the unbound alias must not appear anywhere in the projection: {rendered}"
    );
    // The bound source still resolved, so this is a narrowing and not a
    // failure.
    assert_eq!(projection["generations"].as_array().unwrap().len(), 1);
    assert_eq!(projection["coverage"]["state"], "partial");

    estate.stop();
}

/// Ruling 0126, F2. The first candidate cut the authored reference list
/// to 64 **before** resolving any of it and then computed coverage
/// without considering the cut, so 70 authored, indexed, entirely
/// resolvable paths were delivered as 64 bound items reporting
/// `coverage: complete` — a projection asserting complete factual
/// coverage in the same document in which it reported that 6 of 70
/// authored references were never looked up.
///
/// The repair is not the reviewer's proposed `Partial`: 0124 says a
/// budget must not decide factual coverage in *either* direction. Every
/// authored reference is resolved, the set being bounded by the authored
/// input and nothing else, and `complete` is then true because it is
/// true.
#[test]
fn every_authored_reference_is_resolved_and_no_budget_decides_coverage() {
    let mut estate = Estate::new();
    add_notes(&mut estate, 70);

    // 70 distinct paths, one of them written twice: a duplicate is one
    // reference, and ordinary prose between them is none.
    let mut question = String::from("Read these notes before answering, in order: ");
    for index in 1..=70 {
        question.push_str(&format!("notes/n{index}.md "));
    }
    question.push_str("and read notes/n1.md again for the summary.");
    let route = authored_route(
        &estate.root,
        "bulk",
        &question,
        "Summarise every note the question names.",
    );
    let submitted = submit_kind(
        &estate.root,
        route.to_str().unwrap(),
        &estate.repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .expect("submit");

    let (code, shown, err) = world_show(&estate.root, &submitted.work_id, &submitted.run_id);
    assert_eq!(code, Some(0), "{err}");
    let projection = &shown["projection"];
    let bound = projection["bound"].as_array().expect("bound");
    let omitted = projection["omitted"].as_array().expect("omitted");

    assert!(
        !omitted
            .iter()
            .any(|item| item["kind"] == "over_budget" && item["of"] == "references"),
        "no budget may cut the reference list before resolution: {omitted:?}"
    );
    for index in 1..=70 {
        let wanted = format!("`notes/n{index}.md`");
        assert!(
            bound.iter().any(|item| item["reason"]
                .as_str()
                .unwrap_or_default()
                .contains(&wanted)),
            "authored reference {wanted} was never resolved; {} bound",
            bound.len()
        );
    }
    assert_eq!(
        bound.len(),
        70,
        "70 distinct authored paths, resolved once each in the one admitted source: the \
         duplicate is one reference and the prose is none"
    );
    assert_eq!(projection["unknowns"].as_array().unwrap().len(), 0);
    assert_eq!(projection["coverage"]["state"], "complete", "{omitted:?}");

    estate.stop();
}

/// The other half of 0126's F2: a *presentation* cut is allowed, and it
/// must be honest and inert. `ASSEMBLY_UNKNOWN_MAX` renders at most 32
/// unresolved references and discloses the real total — while every one
/// of the 70 was still looked up, and the coverage state is the factual
/// one it would be at any value of that constant.
#[test]
fn the_unknown_presentation_cut_never_moves_factual_coverage() {
    let mut estate = Estate::new();
    let mut question = String::from("Read these before answering: ");
    for index in 1..=70 {
        question.push_str(&format!("notes/absent{index}.md "));
    }
    question.push_str("Then say what is missing.");
    let route = authored_route(
        &estate.root,
        "absent",
        &question,
        "Report on what the question names.",
    );
    let submitted = submit_kind(
        &estate.root,
        route.to_str().unwrap(),
        &estate.repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .expect("submit");

    let (code, shown, err) = world_show(&estate.root, &submitted.work_id, &submitted.run_id);
    assert_eq!(code, Some(0), "{err}");
    let projection = &shown["projection"];
    let omitted = projection["omitted"].as_array().expect("omitted");

    // Shown: the cut. Total: the fact. The two are stated together.
    let cut = omitted
        .iter()
        .find(|item| item["kind"] == "over_budget" && item["of"] == "unknowns")
        .unwrap_or_else(|| panic!("the unknown list was cut and must say so: {omitted:?}"));
    assert_eq!(cut["shown"], 32);
    assert_eq!(cut["total"], 70, "the real total, not the shown count");
    assert_eq!(projection["unknowns"].as_array().unwrap().len(), 32);
    assert!(projection["bound"].as_array().unwrap().is_empty());
    // Factual, and independent of what was rendered.
    assert_eq!(projection["coverage"]["state"], "partial");
    assert_eq!(projection["coverage"]["reason"], "unresolved_references");
    // Ordinary prose in the same sentence is neither resolved nor
    // reported as an unknown fact (ruling 0124).
    let rendered = projection["unknowns"].to_string();
    for prose in [
        "Read",
        "these",
        "before",
        "answering",
        "Then",
        "say",
        "what",
        "is",
        "missing",
    ] {
        assert!(
            !rendered.contains(&format!("`{prose}`")),
            "{prose:?} is prose, not an unresolved reference: {rendered}"
        );
    }

    estate.stop();
}

/// Auto-advance: a second orienting leaf gets its **own** projection,
/// its own observation and its own file — assembled from its own
/// authored text, not inherited.
#[test]
fn auto_advance_to_an_orienting_leaf_assembles_a_second_distinct_projection() {
    let mut estate = Estate::new();
    let route = orienting_route(&estate.root, r#"["demo"]"#);
    let submitted = submit_kind(
        &estate.root,
        route.to_str().unwrap(),
        &estate.repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .expect("submit");
    let first = reserved_world(&estate.socket, &submitted.work_id)
        .evidence()
        .cloned()
        .expect("first projection");

    let worktree = materialize_actor(
        &estate.socket,
        &estate.root,
        &submitted.work_id,
        &submitted.run_id,
    );
    fs::write(worktree.join("first.md"), "done\n").expect("write artifact");
    claim_ok(
        &estate.root,
        &submitted.work_id,
        &submitted.run_id,
        "first.md=first.md",
    );

    let advanced = status(&estate.socket, &submitted.work_id);
    assert_eq!(advanced["current_waypoint"], "orienting/implement");
    let second = reserved_world(&estate.socket, &submitted.work_id)
        .evidence()
        .cloned()
        .expect("second projection");
    assert_ne!(first.observation, second.observation);
    assert_ne!(
        first.projection, second.projection,
        "a different question delivers a different context"
    );

    // Both files are on disk; the first was not rewritten.
    let dir = estate
        .root
        .join("works")
        .join(&submitted.work_id)
        .join("projections");
    for reference in [&first, &second] {
        assert!(
            dir.join(format!("{}.json", reference.observation.0))
                .is_file(),
            "missing {}",
            reference.observation.0
        );
    }

    // The advanced Run reads its own projection, whose question is the
    // second leaf's.
    let run_id = advanced["run_id"]
        .as_str()
        .expect("status names the current run")
        .to_string();
    let (code, shown, err) = world_show(&estate.root, &submitted.work_id, &run_id);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(shown["waypoint"], "orienting/implement");
    assert!(
        shown["projection"]["question"]
            .as_str()
            .unwrap()
            .contains("Change reserve_next_leaf"),
        "{shown}"
    );

    // And the first leaf's own Run still serves its own projection,
    // unchanged, at its own observation. `current` follows the estate's
    // existing Run-currency rule — the latest Run *for its own
    // Waypoint* — so this Run is still current for the leaf it belongs
    // to even though the Work has moved past it.
    let (code, historical, err) = world_show(&estate.root, &submitted.work_id, &submitted.run_id);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(historical["waypoint"], "orienting/investigate");
    assert_eq!(historical["current"], true);
    assert_eq!(historical["reference"]["observation"], first.observation.0);

    estate.stop();
}

/// A retry re-orients rather than inheriting: a fresh observation, a
/// fresh file, revision 0, and the historical Run keeps its own.
#[test]
fn a_retry_mints_its_own_projection_and_the_historical_run_keeps_its_own() {
    let mut estate = Estate::new();
    let route = orienting_route(&estate.root, r#"["demo"]"#);
    let submitted = submit_kind(
        &estate.root,
        route.to_str().unwrap(),
        &estate.repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .expect("submit");
    let first = reserved_world(&estate.socket, &submitted.work_id)
        .evidence()
        .cloned()
        .expect("first projection");

    harness::fail_via_socket(
        &estate.socket,
        &estate.root,
        &submitted.work_id,
        &submitted.run_id,
    );
    let (code, out) = harness::retry_cli(&estate.root, &submitted.work_id);
    assert_eq!(code, Some(0), "retry: {out}");

    let second = reserved_world(&estate.socket, &submitted.work_id)
        .evidence()
        .cloned()
        .expect("retried projection");
    assert_ne!(first.observation, second.observation);
    assert_eq!(second.revision, 0, "a retry mints revision zero");
    let dir = estate
        .root
        .join("works")
        .join(&submitted.work_id)
        .join("projections");
    assert!(dir.join(format!("{}.json", first.observation.0)).is_file());
    assert!(dir.join(format!("{}.json", second.observation.0)).is_file());

    // The failed Run's own World still names the projection it was
    // delivered, and reads it.
    let (code, historical, err) = world_show(&estate.root, &submitted.work_id, &submitted.run_id);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(historical["reference"]["observation"], first.observation.0);
    assert_eq!(historical["orientation"], "delivered");
    // The retry opened a newer Run for the same Waypoint, so this one is
    // reported as superseded — and is still readable, because the
    // context it was delivered is its own historical fact.
    assert_eq!(historical["current"], false, "{historical}");

    estate.stop();
}

/// A projection whose file is deleted or corrupted after the fact is an
/// explicit unavailability with a closed reason — never a silent empty
/// projection, and never re-assembled against today's estate.
#[test]
fn a_missing_or_corrupt_projection_file_reads_unavailable_and_is_not_regenerated() {
    let mut estate = Estate::new();
    let route = orienting_route(&estate.root, r#"["demo"]"#);
    let submitted = submit_kind(
        &estate.root,
        route.to_str().unwrap(),
        &estate.repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .expect("submit");
    let reference = reserved_world(&estate.socket, &submitted.work_id)
        .evidence()
        .cloned()
        .expect("projection");
    let path = estate
        .root
        .join("works")
        .join(&submitted.work_id)
        .join("projections")
        .join(format!("{}.json", reference.observation.0));

    // Corrupt one byte of the delivered content. The edited value is
    // deliberately a policy name no binary ever writes: editing it to
    // the *next* real one would stop being a corruption the day that
    // one ships, which is exactly what happened when W-C4 advanced the
    // tag and this test silently started asserting nothing.
    let text = fs::read_to_string(&path).expect("read projection");
    fs::write(
        &path,
        text.replace(wirk_core::ASSEMBLY_POLICY, "wirk.assembly/none"),
    )
    .expect("corrupt projection");
    let (code, shown, err) = world_show(&estate.root, &submitted.work_id, &submitted.run_id);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(shown["orientation"], "unavailable", "{shown}");
    assert_eq!(shown["reason"], "content-mismatch", "{shown}");
    assert!(shown.get("projection").is_none());

    // Delete it entirely.
    fs::remove_file(&path).expect("remove projection");
    let (code, shown, err) = world_show(&estate.root, &submitted.work_id, &submitted.run_id);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(shown["reason"], "file-missing", "{shown}");
    assert!(
        !path.exists(),
        "an unavailable projection must never be regenerated"
    );

    estate.stop();
}

/// Restart: the daemon comes back, the projections are still there and
/// still readable, and the `.tmp-` residue of a crash between the temp
/// write and the rename is swept while a renamed file that no event
/// names is left exactly where it is (ruling 0124: no age heuristic
/// deletes an unreferenced projection artifact).
///
/// **The readiness boundary, held open rather than hoped past.** The
/// daemon writes its pointer file immediately after `bind_socket` and
/// runs its four startup sweeps *afterwards*, `sweep_projection_
/// temporaries` last; the harness's readiness signal is that pointer
/// file. So the pointer appearing means the socket is bound, not that
/// the sweep has run — which is why the parent of this test, asserting
/// on the filesystem straight after `restart()`, failed under the
/// independent review's load (`loop-c2-verify/VERDICT.md` F4, ruling
/// 0128).
///
/// The first repair of that made the window *wide* — 40 000 planted
/// `.tmp-` residues, so the sweep would take long enough that the wrong
/// assertion order would lose. That establishes nothing: it makes a race
/// likely instead of establishing arrival and release, and it is still a
/// race. Ruling 0044: a test is deterministic, or it is not a test.
///
/// This holds it instead. Startup is parked at
/// `load_or_create_continuation_key`, which runs **after**
/// `write_pointer` and **before** every sweep, by replacing the key file
/// with a FIFO (`support/read_barrier.rs`, R4 — a native facility, no
/// product instrumentation, and the daemon reads its real key when
/// released). At that held point the pointer is published, the socket is
/// bound, and no sweep has run; the test asserts *that* state, which is
/// exactly the state the parent's assertion order would have been making
/// its claims in. It then issues a real `wirk world show` — which
/// connects, because the socket is bound, and cannot be answered,
/// because `listener.incoming()` has not been reached — releases the
/// barrier, and only then reads the reply.
///
/// So the completed request is proof that every startup sweep finished:
/// the product runs the sweeps before it accepts, so a client never
/// observes unswept residue — it simply waits. No timer, no poll, no
/// sleep, no product test hook, and no serialized suite.
#[test]
fn a_restart_preserves_projections_sweeps_temporaries_and_keeps_orphans() {
    let mut estate = Estate::new();
    let route = orienting_route(&estate.root, r#"["demo"]"#);
    let submitted = submit_kind(
        &estate.root,
        route.to_str().unwrap(),
        &estate.repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .expect("submit");
    let reference = reserved_world(&estate.socket, &submitted.work_id)
        .evidence()
        .cloned()
        .expect("projection");
    let dir = estate
        .root
        .join("works")
        .join(&submitted.work_id)
        .join("projections");

    // The daemon's own key, read while it is still an ordinary file. It
    // is handed back verbatim on release, so the restarted daemon is the
    // same daemon with the same continuation key.
    let key_path = estate.root.join(".wirk").join("continuation-key");
    let key = fs::read(&key_path).expect("the running daemon minted a continuation key");
    assert_eq!(
        key.len(),
        32,
        "the key this test hands back is the real one"
    );

    estate.stop();

    // The two residues a crash can leave.
    let temporary = dir.join(".tmp-obs-crashed");
    let orphan = dir.join("obs-orphaned.json");
    fs::write(&temporary, b"{}").expect("write temp residue");
    fs::write(&orphan, b"{}").expect("write orphan");

    // --- start, and hold between the pointer and the sweeps ---------------
    let barrier = read_barrier::ReadBarrier::arm(&key_path);
    let (daemon, pointer) = start_wirkd(&estate.root);
    estate.daemon = Some(daemon);
    estate.socket = pointer.socket;
    let mut held = barrier.park("wirkd startup reads its continuation key");

    // --- the controlled pre-sweep state -----------------------------------
    // Everything the harness calls readiness has happened: the pointer
    // file is published and the socket is bound. No sweep has run.
    assert!(
        estate.root.join(".wirk").join("wirkd.json").exists(),
        "the pointer is published before this daemon sweeps anything"
    );
    assert!(
        temporary.exists(),
        "at pointer-readiness the startup sweep has not run: this is the state the parent of \
         this test made its filesystem assertions in"
    );
    assert!(orphan.exists());

    // A real request, issued into that state. It connects — the socket
    // is bound — and cannot be answered until the accept loop is
    // reached, which is after every sweep.
    let (started, start_signal) = std::sync::mpsc::channel();
    let (finished, reply) = std::sync::mpsc::channel();
    let request_root = estate.root.clone();
    let request_work = submitted.work_id.clone();
    let request_run = submitted.run_id.clone();
    let requester = std::thread::spawn(move || {
        let _ = started.send(());
        let _ = finished.send(world_show(&request_root, &request_work, &request_run));
    });
    start_signal.recv().expect("the requesting thread started");
    assert!(
        matches!(reply.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)),
        "a request answered before the accept loop was even reached would mean the sweeps do \
         not precede acceptance"
    );
    assert!(
        temporary.exists(),
        "the residue is still unswept with a request already in flight"
    );

    // --- release ----------------------------------------------------------
    held.supply(&key);
    held.release();

    let (code, shown, err) = reply
        .recv()
        .expect("the request is answered once the sweeps are done");
    requester.join().expect("requesting thread");
    assert_eq!(code, Some(0), "{err}");

    // The request completed, so the accept loop was reached, so every
    // startup sweep finished before it.
    assert!(
        !temporary.exists(),
        "a pre-rename temporary is unreachable by construction and is swept"
    );
    assert!(
        orphan.exists(),
        "a renamed projection no event names is left alone: age is not evidence of orphanhood"
    );
    assert_eq!(shown["orientation"], "delivered");
    assert_eq!(shown["reference"]["observation"], reference.observation.0);

    // Leave the estate as it was found: an ordinary key file, so the
    // stop below and anything after it read it normally.
    fs::remove_file(&key_path).expect("remove the barrier");
    fs::write(&key_path, &key).expect("restore the continuation key");

    estate.stop();
}

/// The assembly runs with no journal guard held. `no_journal_guard_held`
/// is a `debug_assert`, live in this profile, and it fires from inside
/// the daemon — so a reservation that completes at all is the proof, and
/// this test pins that the daemon did not die trying.
#[test]
fn assembly_never_runs_under_a_journal_guard() {
    let mut estate = Estate::new();
    let route = orienting_route(&estate.root, r#"["demo"]"#);
    // Submit, auto-advance and retry: the three assembly call sites that
    // sit next to a journal guard in the reservation paths.
    let submitted = submit_kind(
        &estate.root,
        route.to_str().unwrap(),
        &estate.repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .expect("submit");
    let worktree = materialize_actor(
        &estate.socket,
        &estate.root,
        &submitted.work_id,
        &submitted.run_id,
    );
    fs::write(worktree.join("first.md"), "done\n").expect("artifact");
    claim_ok(
        &estate.root,
        &submitted.work_id,
        &submitted.run_id,
        "first.md=first.md",
    );
    let advanced = status(&estate.socket, &submitted.work_id);
    let run_id = advanced["run_id"]
        .as_str()
        .expect("status names the current run")
        .to_string();
    harness::fail_via_socket(&estate.socket, &estate.root, &submitted.work_id, &run_id);
    let (code, out) = harness::retry_cli(&estate.root, &submitted.work_id);
    assert_eq!(code, Some(0), "retry: {out}");

    // Three *reservations* — submit, auto-advance and retry — each with
    // its own observation. The fourth `WaypointReserved` is
    // materialization re-emitting the first leaf's own World with the
    // worktree filled in, which carries the identical reference: a
    // materialization that dropped or swapped it would be refused as an
    // invalid transition by `resolve_run_binding`'s structural equality.
    let observations: Vec<String> = harness::journal_events(&estate.root, &submitted.work_id)
        .into_iter()
        .filter_map(|event| match event.kind {
            EventKind::WaypointReserved { world, .. } => {
                world.evidence().map(|r| r.observation.0.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(observations.len(), 4, "{observations:?}");
    assert_eq!(
        observations[0], observations[1],
        "materialization preserves the projection reference: {observations:?}"
    );
    let unique: std::collections::HashSet<&String> = observations.iter().collect();
    assert_eq!(
        unique.len(),
        3,
        "each reservation observes for itself: {observations:?}"
    );

    // The daemon is still alive and answering: the debug assertion
    // inside it never fired.
    assert_eq!(
        status(&estate.socket, &submitted.work_id)["state"],
        "active"
    );
    estate.stop();
}

/// Nesting, recursively: a grandchild leaf assembles under **its own**
/// Work's bindings, never its parent's and never its grandparent's.
/// Nothing here caps depth, and the assembly is keyed on the Waypoint
/// being reserved at whatever depth that is.
#[test]
fn a_grandchild_leaf_assembles_under_its_own_work_bindings() {
    let mut estate = Estate::new();
    // A second published source the top Work is bound to and the
    // descendants are not.
    let other = source_repo(&estate.root.join("other-src"));
    write_file(&other, "src/server.rs", "pub fn only_in_other() {}\n");
    commit_all(&other);
    publish(&estate.root, "other", &other);

    // Top: a container declaring a required child role, whose own leaf
    // orients. Bound to both sources.
    let routes = estate.root.join("routes");
    fs::create_dir_all(&routes).expect("routes dir");
    let container = |id: &str, role: &str, leaf: &str, output: &str| {
        format!(
            r#"{{"id":"{id}","waypoints":[
              {{"id":"outer","kind":"Container","declared_outputs":[{{"name":"{output}","required":true}}],
                "required_child_outcomes":[{{"role":"{role}","required":true}}],
                "leaves":[
                  {{"id":"{leaf}","kind":"Actor","declared_outputs":[{{"name":"{output}","required":true}}],
                    "intent":"Investigate src/server.rs.",
                    "orient":{{"question":"Where is claim_boundary_refusal defined in src/server.rs?"}}}}
                ]}}]}}"#
        )
    };
    let top_route = routes.join("top.json");
    fs::write(&top_route, container("top", "child", "outer/leaf", "a.md")).expect("write");
    let child_route = routes.join("child.json");
    fs::write(
        &child_route,
        container("child", "grandchild", "outer/leaf", "b.md"),
    )
    .expect("write");
    let grandchild_route = routes.join("grandchild.json");
    fs::write(
        &grandchild_route,
        r#"{"id":"grandchild","waypoints":[
          {"id":"leaf","kind":"Actor","declared_outputs":[{"name":"c.md","required":true}],
           "intent":"Investigate src/server.rs.",
           "orient":{"question":"Where is claim_boundary_refusal defined in src/server.rs?"}}]}"#,
    )
    .expect("write");

    let top = submit_kind(
        &estate.root,
        top_route.to_str().unwrap(),
        &estate.repo,
        &["demo:write", "other:write"],
        None,
        Some("actor"),
    )
    .expect("submit top");

    let child = submit_kind(
        &estate.root,
        child_route.to_str().unwrap(),
        &estate.repo,
        &["demo:read"],
        Some(harness::ParentRef {
            work: &top.work_id,
            waypoint: "outer",
            run: &top.run_id,
            role: "child",
            attempt: None,
        }),
        Some("actor"),
    )
    .expect("submit child");

    let grandchild = submit_kind(
        &estate.root,
        grandchild_route.to_str().unwrap(),
        &estate.repo,
        &["demo:read"],
        Some(harness::ParentRef {
            work: &child.work_id,
            waypoint: "outer",
            run: &child.run_id,
            role: "grandchild",
            attempt: None,
        }),
        Some("actor"),
    )
    .expect("submit grandchild");

    // The top Work, bound to both sources, captures both.
    let (code, top_shown, err) = world_show(&estate.root, &top.work_id, &top.run_id);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(
        top_shown["projection"]["generations"]
            .as_array()
            .unwrap()
            .len(),
        2,
        "{top_shown}"
    );

    // Each descendant captures only its own single binding, at every
    // depth — the parent's wider grant is not inherited by being nested
    // under it.
    for (label, work) in [("child", &child), ("grandchild", &grandchild)] {
        let (code, shown, err) = world_show(&estate.root, &work.work_id, &work.run_id);
        assert_eq!(code, Some(0), "{label}: {err}");
        assert_eq!(shown["orientation"], "delivered", "{label}: {shown}");
        let projection = &shown["projection"];
        assert_eq!(
            projection["generations"].as_array().unwrap().len(),
            1,
            "{label} must capture only its own binding: {projection}"
        );
        let reasons: Vec<&str> = projection["bound"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["reason"].as_str().unwrap_or_default())
            .collect();
        assert!(
            reasons.iter().all(|reason| !reason.contains("`other`")),
            "{label} must not bind a source it is not bound to: {reasons:?}"
        );
        assert!(
            reasons.iter().any(|reason| reason.contains("`demo`")),
            "{label} must still bind its own source: {reasons:?}"
        );
        // W-C2: the wider lists a descendant now also receives obey the
        // same narrowing. A ranked hit and a discovery handle are
        // reachable *for this Work*, so neither may name a source the
        // parent bound and this Work did not.
        for entry in projection["reachable"].as_array().unwrap() {
            assert_eq!(
                entry["source"], "demo",
                "{label}: a reachable handle must never name a source outside its own bindings: \
                 {entry}"
            );
        }
        for item in projection["referenced"].as_array().unwrap() {
            assert!(
                item["reason"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("source `demo`"),
                "{label}: a ranked hit must come from its own bindings: {item}"
            );
        }
    }

    // And each Work reads only its own delivered context: the parent's
    // triple does not open the child's, and the child's does not open
    // the parent's.
    let (code, _, err) = world_show(&estate.root, &top.work_id, &grandchild.run_id);
    assert_eq!(code, Some(3), "{err}");
    let (code, _, err) = world_show(&estate.root, &grandchild.work_id, &top.run_id);
    assert_eq!(code, Some(3), "{err}");

    estate.stop();
}

/// A delivered projection names the generation it read at, and keeps
/// naming it. Publishing a newer generation moves what a *later*
/// assembly captures; it does not move, rewrite or re-resolve what an
/// earlier one already delivered.
#[test]
fn publishing_a_newer_generation_does_not_move_an_already_delivered_projection() {
    let mut estate = Estate::new();
    let route = orienting_route(&estate.root, r#"["demo"]"#);
    let before = submit_kind(
        &estate.root,
        route.to_str().unwrap(),
        &estate.repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .expect("submit");
    let (code, first, err) = world_show(&estate.root, &before.work_id, &before.run_id);
    assert_eq!(code, Some(0), "{err}");
    let first_generation = first["projection"]["generations"][0][1]
        .as_str()
        .expect("generation")
        .to_string();
    let first_object = first["projection"]["bound"][0]["identity"]["object_id"]
        .as_str()
        .expect("object id")
        .to_string();

    // The source moves: the same path, different bytes, a new published
    // generation.
    write_file(
        &estate.repo,
        "src/server.rs",
        "pub fn claim_boundary_refusal(path: &str) -> bool {\n    // rewritten\n    \
         path.is_empty()\n}\n\npub fn reserve_next_leaf(after: &str) -> String {\n    \
         after.to_string()\n}\n",
    );
    commit_all(&estate.repo);
    let (ok, refreshed, err) = atlas(
        &estate.root,
        &["refresh", "--source", "demo", "--revision", "HEAD"],
    );
    assert!(ok, "atlas refresh: {err}");
    let generation = refreshed["generation"]["generation"]
        .as_str()
        .expect("refreshed generation")
        .to_string();
    assert_ne!(generation, first_generation);
    let (ok, _, err) = atlas(
        &estate.root,
        &["publish", "--source", "demo", "--generation", &generation],
    );
    assert!(ok, "atlas publish: {err}");

    // The already-delivered projection is unchanged, byte for byte, and
    // still names its own generation and its own object.
    let (code, again, err) = world_show(&estate.root, &before.work_id, &before.run_id);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(again["projection"], first["projection"]);
    assert_eq!(again["projection"]["generations"][0][1], first_generation);
    assert_eq!(
        again["projection"]["bound"][0]["identity"]["object_id"],
        first_object
    );

    // A fresh assembly captures the new generation, and its publication
    // revision has moved.
    let after = submit_kind(
        &estate.root,
        route.to_str().unwrap(),
        &estate.repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .expect("submit");
    let (code, second, err) = world_show(&estate.root, &after.work_id, &after.run_id);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(second["projection"]["generations"][0][1], generation);
    assert_ne!(
        second["projection"]["bound"][0]["identity"]["object_id"],
        first_object
    );
    assert!(
        second["projection"]["publication_revision"]
            .as_u64()
            .unwrap()
            > first["projection"]["publication_revision"]
                .as_u64()
                .unwrap()
    );

    estate.stop();
}

/// Ruling 0127: **an unresolved identifier and an unresolved path do not
/// establish the same thing, and the projection must not say they do.**
///
/// The first wording said both "resolve to nothing in the admitted
/// sources at the captured generations". For a path that is true — exact
/// lookup walks every admitted source's captured generation manifest. For
/// an identifier it is a claim about bytes the assembler never read:
/// candidate discovery is bounded, ranked and index-derived, and the
/// index tokenizes on `_`, so a name occurring only inside a longer token
/// returns zero candidates while its bytes sit in the committed object.
///
/// Three controls in one real assembly, so the wording is proved against
/// behaviour rather than against itself:
///
/// * **negative (identifier)** — `embedded_marker`, present in the
///   committed bytes of `src/markers.rs` and invisible to the index,
///   is reported as *not found among the examined candidates* and
///   explicitly not as absence;
/// * **negative (path)** — `notes/missing.md`, recorded by no captured
///   generation, may and does state the exact-lookup result;
/// * **positive** — `claim_boundary_refusal`, in the same question,
///   binds. Identifier resolution really ran and really reached this
///   handler, so the negative is a finding rather than a broken step.
#[test]
fn an_unresolved_identifier_reports_examined_candidates_and_a_path_reports_exact_lookup() {
    let mut estate = Estate::new();

    // The bytes are literally in the committed source: the control that
    // makes the honest wording about a real case and not a vacuous one.
    let committed = fs::read_to_string(estate.repo.join("src/markers.rs")).expect("markers.rs");
    assert!(
        committed.contains("embedded_marker"),
        "the identifier's bytes must really be present: {committed:?}"
    );

    let route = authored_route(
        &estate.root,
        "wording",
        "Does embedded_marker reach claim_boundary_refusal, and what does notes/missing.md say?",
        "Look at embedded_marker and notes/missing.md.",
    );
    let submitted = submit_kind(
        &estate.root,
        route.to_str().unwrap(),
        &estate.repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .expect("submit");

    let (code, shown, err) = world_show(&estate.root, &submitted.work_id, &submitted.run_id);
    assert_eq!(code, Some(0), "{err}");
    let projection = &shown["projection"];

    // Positive control: identifier resolution ran and bound a real one.
    let bound = projection["bound"].as_array().expect("bound");
    assert!(
        bound.iter().any(|item| item["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("`claim_boundary_refusal`")),
        "the resolvable identifier must bind, or the negative proves nothing: {bound:?}"
    );

    let unknowns: Vec<&str> = projection["unknowns"]
        .as_array()
        .expect("unknowns")
        .iter()
        .map(|item| item["text"].as_str().unwrap_or_default())
        .collect();
    assert!(
        !unknowns
            .iter()
            .any(|text| text.contains("`claim_boundary_refusal`")),
        "a resolved identifier is not an unknown: {unknowns:?}"
    );

    let identifier = unknowns
        .iter()
        .find(|text| text.contains("`embedded_marker`"))
        .unwrap_or_else(|| panic!("the embedded identifier must be an unknown: {unknowns:?}"));
    assert!(
        identifier.contains("not found among the indexed candidates this assembly examined"),
        "the identifier unknown must name the observation it made: {identifier}"
    );
    assert!(
        identifier.contains(
            "not a statement that these bytes are absent from the admitted \
                             sources"
        ),
        "the identifier unknown must refuse the absence claim outright: {identifier}"
    );

    let path = unknowns
        .iter()
        .find(|text| text.contains("`notes/missing.md`"))
        .unwrap_or_else(|| panic!("the absent path must be an unknown: {unknowns:?}"));
    assert!(
        path.contains("which no admitted source records at the captured generations"),
        "exact path lookup may state its exact result: {path}"
    );
    assert!(
        !path.contains("candidates"),
        "a path is not resolved by candidate discovery: {path}"
    );

    estate.stop();
}
