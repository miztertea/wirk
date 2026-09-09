//! Ruling 0142, the remaining consumer: a **World** must deliver
//! evidence that is local to the match that put it there.
//!
//! `atlas search` was corrected first (`evidence_locality.rs`): a hit's
//! displayed bytes are now the window where the query's own terms are,
//! with an exact coordinate for those bytes. The assembler was not. A
//! ranked item and a resolved identifier reference were each summarised
//! from the **head of the retrieval unit** — so a stage's freshly
//! assembled orientation could name `carried_source_coverage` as the
//! reason an item was delivered and show 320 bytes in which the symbol
//! does not appear.
//!
//! Everything here runs the shipped binary against a real `wirkd`, a
//! real Atlas and real Git objects (ruling 0040). Every assertion about
//! *where* something is compares against the committed bytes, through
//! the shipped `wirk atlas resolve`, never against a detector of this
//! file's own.

#[path = "support/nested_harness.rs"]
mod harness;
#[path = "../src/wirkd/mod.rs"]
mod wirkd;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use harness::{KillOnDrop, init_repo, start_wirkd, stop_wirkd, submit_kind, wirk_bin, write_file};
use serde_json::Value;

/// The assembler's own presentation budget, mirrored from
/// `wirk/src/wirkd/server.rs`. Asserted against what was delivered,
/// never used to compute an expected answer.
const ASSEMBLY_SUMMARY_BYTES: usize = 320;

// ---- the fixture ----------------------------------------------------------

/// A realistic packed source: ordinary multi-line Rust with non-ASCII
/// throughout, with the needles placed deep enough that the head of
/// their retrieval unit cannot contain them.
///
/// The retrieval unit is up to 65,536 bytes and the assembly summary is
/// 320, so a needle at ~100 KiB is tens of thousands of bytes past the
/// head of the unit it lands in. `needle_offset` asserts that rather
/// than assuming it.
fn packed_source() -> String {
    let mut out = String::new();
    let mut index = 0usize;
    let mut placed_definition = false;
    let mut placed_expansion = false;
    while out.len() < 160_000 {
        if !placed_definition && out.len() >= 100_000 {
            out.push_str(
                "/// La règle portée — vérifiée, jamais supposée.\n\
                 pub fn carried_definition_marker(unit: &Unit) -> bool {\n\
                 \x20   // deepmarker: the packed unit keeps its own identity here.\n\
                 \x20   unit.coverage().is_complete()\n\
                 }\n\n",
            );
            placed_definition = true;
        }
        if !placed_expansion && out.len() >= 140_000 {
            out.push_str(
                "/// Étape suivante : ce que l'expansion doit atteindre.\n\
                 pub fn expansionmarker_rule(unit: &Unit) -> usize {\n\
                 \x20   unit.len()\n\
                 }\n\n",
            );
            placed_expansion = true;
        }
        out.push_str(&format!(
            "/// Étape {index} : ce commentaire décrit une fonction ordinaire.\n\
             fn ordinary_helper_{index}(input: &str) -> usize {{\n\
             \x20   let trimmed = input.trim();\n\
             \x20   trimmed.len() + {index}\n\
             }}\n\n"
        ));
        index += 1;
    }
    assert!(placed_definition && placed_expansion, "needles placed");
    out
}

/// The one-based line and byte offset of a needle in the committed text.
fn locate(text: &str, needle: &str) -> (usize, usize) {
    let offset = text.find(needle).expect("needle is in the committed text");
    (text[..offset].matches('\n').count() + 1, offset)
}

/// One real repository: a small file an authored path names, and the
/// packed one the needles live in.
fn source_repo(root: &Path) -> PathBuf {
    let repo = root.join("source-repo");
    fs::create_dir_all(&repo).expect("source repo dir");
    init_repo(&repo);
    fs::create_dir_all(repo.join("src")).expect("src dir");
    write_file(
        &repo,
        "src/server.rs",
        "pub fn claim_boundary_refusal(path: &str) -> bool {\n    path.starts_with(\"src/\")\n}\n",
    );
    write_file(&repo, "src/packed.rs", &packed_source());
    commit_all(&repo);
    repo
}

/// A published, indexed source no Work below binds, so every scope
/// assertion has something real to fail to leak.
fn foreign_repo(root: &Path) -> PathBuf {
    let repo = root.join("other-repo");
    fs::create_dir_all(&repo).expect("other repo dir");
    init_repo(&repo);
    fs::create_dir_all(repo.join("notes")).expect("notes dir");
    write_file(
        &repo,
        "notes/elsewhere.md",
        "# Elsewhere\n\ncarried_definition_marker and deepmarker are discussed here too, in a \
         source no Work below binds.\n",
    );
    commit_all(&repo);
    repo
}

fn commit_all(repo: &Path) {
    for args in [
        vec!["add", "-A"],
        vec![
            "-c",
            "user.name=world-locality-test",
            "-c",
            "user.email=world-locality@example.test",
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

fn atlas(estate: &Path, args: &[&str]) -> (bool, Value, String) {
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
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).unwrap_or(Value::Null),
        String::from_utf8_lossy(&output.stderr).trim().to_string(),
    )
}

fn publish(estate: &Path, alias: &str, repo: &Path) {
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
}

struct Estate {
    _dir: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    daemon: Option<KillOnDrop>,
}

impl Estate {
    fn new() -> Estate {
        let dir = tempfile::tempdir().expect("temp estate");
        let root = dir.path().join("estate");
        fs::create_dir_all(&root).expect("estate dir");
        let repo = source_repo(dir.path());
        let other = foreign_repo(dir.path());
        let (daemon, _pointer) = start_wirkd(&root);
        publish(&root, "demo", &repo);
        publish(&root, "unadmittedsource", &other);
        Estate {
            _dir: dir,
            root,
            repo,
            daemon: Some(daemon),
        }
    }

    fn stop(&mut self) {
        if let Some(daemon) = self.daemon.take() {
            stop_wirkd(&self.root, daemon);
        }
    }
}

/// One orienting Actor leaf whose authored question names the small
/// path, the deep identifier and the deep plain term — so the assembly
/// exercises the literal-reference path and the ranked path at once.
fn one_stage_route(estate: &Path, name: &str) -> PathBuf {
    let dir = estate.join("routes");
    fs::create_dir_all(&dir).expect("routes dir");
    let path = dir.join(format!("{name}.json"));
    fs::write(
        &path,
        format!(
            r#"{{"id":{id},"waypoints":[
              {{"id":{leaf},"kind":"Actor",
                "declared_outputs":[{{"name":"out.md","required":true}}],
                "intent":"Decide the coverage rule.",
                "orient":{{"question":"Where does carried_definition_marker decide deepmarker, and what does src/server.rs require?","sources":["demo"]}}}}
            ]}}"#,
            id = serde_json::to_string(name).unwrap(),
            leaf = serde_json::to_string(&format!("{name}/only")).unwrap(),
        ),
    )
    .expect("write route");
    path
}

fn submit_oriented(estate: &Estate, name: &str) -> harness::Submitted {
    let route = one_stage_route(&estate.root, name);
    submit_kind(
        &estate.root,
        route.to_str().unwrap(),
        &estate.repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .unwrap_or_else(|err| panic!("submit {name}: {err}"))
}

// ---- the public verbs, exactly as an actor types them ---------------------

fn world_show(estate: &Path, work: &str, run: &str) -> Value {
    let output = Command::new(wirk_bin())
        .args(["world", "show", "--json"])
        .env("WIRK_ESTATE_ROOT", estate)
        .env("WIRK_WORK_ID", work)
        .env("WIRK_RUN_ID", run)
        .output()
        .expect("wirk world show runs");
    assert_eq!(
        output.status.code(),
        Some(0),
        "world show: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim())
        .expect("world show emits json")
}

fn world_show_text(estate: &Path, work: &str, run: &str) -> String {
    let output = Command::new(wirk_bin())
        .args(["world", "show"])
        .env("WIRK_ESTATE_ROOT", estate)
        .env("WIRK_WORK_ID", work)
        .env("WIRK_RUN_ID", run)
        .output()
        .expect("wirk world show runs");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn expand_ok(estate: &Path, work: &str, run: &str, args: &[&str]) -> Value {
    let mut full = vec!["world", "expand", "--json"];
    full.extend_from_slice(args);
    let output = Command::new(wirk_bin())
        .args(&full)
        .env("WIRK_ESTATE_ROOT", estate)
        .env("WIRK_WORK_ID", work)
        .env("WIRK_RUN_ID", run)
        .output()
        .expect("wirk world expand runs");
    assert_eq!(
        output.status.code(),
        Some(0),
        "world expand {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim())
        .expect("world expand emits json")
}

/// `wirk atlas resolve` as an actor types it: the injected triple and a
/// coordinate the projection itself delivered.
fn resolve(estate: &Path, work: &str, run: &str, coordinate: &str) -> Value {
    let output = Command::new(wirk_bin())
        .args(["atlas", "resolve", "--coordinate", coordinate, "--json"])
        .env("WIRK_ESTATE_ROOT", estate)
        .env("WIRK_WORK_ID", work)
        .env("WIRK_RUN_ID", run)
        .output()
        .expect("wirk atlas resolve runs");
    let value: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).unwrap_or(Value::Null);
    assert_eq!(
        value["outcome"],
        "resolved",
        "resolve {coordinate}: {} {value}",
        String::from_utf8_lossy(&output.stderr)
    );
    value
}

fn items<'a>(projection: &'a Value, key: &str) -> &'a Vec<Value> {
    projection[key]
        .as_array()
        .unwrap_or_else(|| panic!("{key} must be a list: {projection}"))
}

/// Every delivered item of a projection, bound and referenced together —
/// what the stage actually reads.
fn delivered(projection: &Value) -> Vec<Value> {
    let mut all = items(projection, "bound").clone();
    all.extend(items(projection, "referenced").iter().cloned());
    all
}

fn item_where(projection: &Value, needle: &str) -> Value {
    delivered(projection)
        .into_iter()
        .find(|item| item["reason"].as_str().unwrap_or_default().contains(needle))
        .unwrap_or_else(|| panic!("no delivered item whose reason mentions {needle}: {projection}"))
}

/// The one property every shown span owes, checked against committed
/// bytes through the shipped resolver: the span names exactly the source
/// the summary was made from, the summary is inside the budget, and
/// every term the item says it located is really in what it showed.
fn assert_span_locates_the_summary(estate: &Estate, work: &str, run: &str, item: &Value) {
    let summary = item["summary"].as_str().expect("a summary");
    assert!(
        summary.len() <= ASSEMBLY_SUMMARY_BYTES,
        "a delivered summary must stay inside the assembly budget: {} bytes",
        summary.len()
    );
    let shown = &item["shown"];
    assert!(!shown.is_null(), "item names no shown span: {item}");
    let coordinate = shown["coordinate"].as_str().expect("a shown coordinate");
    let resolved = resolve(&estate.root, work, run, coordinate);
    let text = resolved["text"].as_str().expect("resolved text");
    assert_eq!(
        text.replace(['\n', '\r'], " "),
        summary,
        "the shown coordinate must resolve to exactly the source the summary was made from: \
         {item}"
    );
    // The committed file the resolver itself says these bytes are in —
    // never a path this test picked.
    let source = fs::read_to_string(
        estate
            .repo
            .join(resolved["path"].as_str().expect("the resolved path")),
    )
    .expect("read the committed source");
    let byte_start = shown["byte_start"].as_u64().expect("byte_start") as usize;
    let byte_end = shown["byte_end"].as_u64().expect("byte_end") as usize;
    assert!(byte_end <= source.len() && byte_start < byte_end, "{item}");
    assert_eq!(
        &source[byte_start..byte_end],
        text,
        "the shown byte span must name the committed bytes it displayed: {item}"
    );
    for term in shown["matched_terms"].as_array().expect("matched_terms") {
        let term = term.as_str().expect("a term");
        assert!(
            summary.to_lowercase().contains(&term.to_lowercase()),
            "a term the item says it located must be in what it showed: {term} not in {summary}"
        );
    }
}

// ---- the tests ------------------------------------------------------------

/// The decisive one for the ranked path. A term the authored question
/// names, matched ~100 KiB into a packed unit, must be **visible** in
/// what the World delivers, and the item must say exactly which
/// committed bytes it showed.
#[test]
fn a_ranked_world_item_shows_the_match_and_its_span_resolves_to_those_bytes() {
    let mut estate = Estate::new();
    let work = submit_oriented(&estate, "world-ranked");
    let source = packed_source();
    let (_, needle_offset) = locate(&source, "pub fn carried_definition_marker");
    assert!(
        needle_offset > 65_536 + ASSEMBLY_SUMMARY_BYTES,
        "the fixture must place the match beyond the head of any unit's summary, not at byte \
         {needle_offset}"
    );

    let projection = world_show(&estate.root, &work.work_id, &work.run_id)["projection"].clone();
    let visible: Vec<Value> = delivered(&projection)
        .into_iter()
        .filter(|item| {
            item["summary"]
                .as_str()
                .unwrap_or_default()
                .contains("deepmarker")
        })
        .collect();
    assert!(
        !visible.is_empty(),
        "a question naming deepmarker must deliver evidence in which deepmarker is visible: \
         {projection}"
    );

    // The ranked item on the packed unit — the one whose delivery this
    // ruling is about. A small file ranked beside it is delivered whole
    // and is the guard below, not this.
    let ranked = visible
        .iter()
        .find(|item| {
            item["reason"]
                .as_str()
                .unwrap_or_default()
                .contains("ranked for the authored question")
        })
        .unwrap_or_else(|| panic!("the ranked list must show the match: {projection}"))
        .clone();
    assert_span_locates_the_summary(&estate, &work.work_id, &work.run_id, &ranked);
    // The ranked unit keeps its own identity: the item's coordinate is
    // still the whole unit it was scored as, and still resolves.
    let unit = resolve(
        &estate.root,
        &work.work_id,
        &work.run_id,
        ranked["coordinate"].as_str().expect("unit coordinate"),
    );
    let unit_text = unit["text"].as_str().expect("unit text");
    assert!(
        unit_text.len() > ranked["summary"].as_str().unwrap().len(),
        "the item's own coordinate must still name the whole ranked unit"
    );
    estate.stop();
}

/// The literal-reference path. The item's reason says the identifier
/// occurs literally in this resource; what it shows must be where.
#[test]
fn a_literal_identifier_reference_is_summarized_where_the_identifier_occurs() {
    let mut estate = Estate::new();
    let work = submit_oriented(&estate, "world-literal");
    let projection = world_show(&estate.root, &work.work_id, &work.run_id)["projection"].clone();

    let item = item_where(
        &projection,
        "names the identifier `carried_definition_marker`",
    );
    assert!(
        item["summary"]
            .as_str()
            .unwrap_or_default()
            .contains("carried_definition_marker"),
        "an item delivered *because* it contains an identifier must show it: {item}"
    );
    assert_span_locates_the_summary(&estate, &work.work_id, &work.run_id, &item);
    estate.stop();
}

/// The guard. A small resource named by an authored path is not a
/// lexical window and must not acquire one: it is summarised from its
/// head exactly as before, and names no narrower span.
#[test]
fn a_small_resource_is_summarized_from_its_head_and_names_no_narrower_span() {
    let mut estate = Estate::new();
    let work = submit_oriented(&estate, "world-small");
    let projection = world_show(&estate.root, &work.work_id, &work.run_id)["projection"].clone();

    let item = item_where(
        &projection,
        "the authored text names the path `src/server.rs`",
    );
    let source = fs::read_to_string(estate.repo.join("src/server.rs")).expect("read source");
    assert_eq!(
        item["summary"].as_str().unwrap_or_default(),
        source.replace('\n', " "),
        "a small resource is delivered whole, exactly as before: {item}"
    );
    assert!(
        item["shown"].is_null(),
        "nothing lexical located anything here, so no narrower span may be named: {item}"
    );
    estate.stop();
}

/// Expansion is the same consumer. A question this Run authors itself
/// must deliver evidence local to *its* terms.
#[test]
fn an_expansion_question_delivers_the_evidence_its_own_terms_located() {
    let mut estate = Estate::new();
    let work = submit_oriented(&estate, "world-expand");
    let before = world_show(&estate.root, &work.work_id, &work.run_id);
    let initial_observation = before["reference"]["observation"]
        .as_str()
        .expect("observation")
        .to_string();
    let initial_bytes = fs::read(
        estate
            .root
            .join("works")
            .join(&work.work_id)
            .join("projections")
            .join(format!("{initial_observation}.json")),
    )
    .expect("read revision 0");

    let expanded = expand_ok(
        &estate.root,
        &work.work_id,
        &work.run_id,
        &[
            "--question",
            "What does expansionmarker_rule require?",
            "--reason",
            "the coverage rule turned on the expansion rule",
        ],
    );
    let revision = expanded["projection"].clone();
    assert_eq!(revision["revision"], 1, "{revision}");

    let visible: Vec<Value> = delivered(&revision)
        .into_iter()
        .filter(|item| {
            item["summary"]
                .as_str()
                .unwrap_or_default()
                .contains("expansionmarker_rule")
        })
        .collect();
    assert!(
        !visible.is_empty(),
        "an expansion naming expansionmarker_rule must deliver evidence showing it: {revision}"
    );
    for item in &visible {
        assert_span_locates_the_summary(&estate, &work.work_id, &work.run_id, item);
    }

    // The revision it expanded is untouched, byte for byte.
    assert_eq!(
        fs::read(
            estate
                .root
                .join("works")
                .join(&work.work_id)
                .join("projections")
                .join(format!("{initial_observation}.json")),
        )
        .expect("re-read revision 0"),
        initial_bytes,
        "an expansion never edits the revision it expands"
    );
    estate.stop();
}

/// The plain surface a human and a fresh actor actually read must say
/// which lines it showed, so the whole-unit coordinate printed beside it
/// is not read as the range displayed.
#[test]
fn the_plain_world_surface_names_the_lines_it_showed() {
    let mut estate = Estate::new();
    let work = submit_oriented(&estate, "world-plain");
    let text = world_show_text(&estate.root, &work.work_id, &work.run_id);
    assert!(
        text.contains("shown: lines "),
        "the plain rendering must name the lines it showed: {text}"
    );
    assert!(
        text.contains("shown coordinate "),
        "the plain rendering must name the shown window's own coordinate: {text}"
    );
    estate.stop();
}
