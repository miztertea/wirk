//! Ruling 0142: a lexical hit on a large file must *show* the evidence
//! that matched, and name the exact committed span of what it showed.
//!
//! The default extractor edition packs consecutive lines into retrieval
//! units of up to 65,536 bytes. Before this file existed, the reply's
//! inline snippet was the first 2 KiB of that unit regardless of where
//! the match was, so on a real source file a search for a symbol
//! returned four hits and showed the symbol in none of them
//! (`source-coverage-verify/raw/p4-deep-match.txt`: first match at byte
//! 46,878 of a 50,764-byte unit). The recovery — resolve 64 KiB and scan
//! it — was honest but is not "useful relevant evidence with bounded
//! context".
//!
//! Everything here runs the real built `wirk` binary against a real
//! `wirkd` daemon over a real Git repository, and every assertion about
//! *where* a match is compares against the committed bytes themselves,
//! never against a detector of this file's own.

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[path = "../src/wirkd/mod.rs"]
mod wirkd;

use wirkd::WirkdPointer;

/// The presentation budget under test, mirrored from
/// `wirk/src/wirkd/server.rs`. Asserted against the reply, never used to
/// compute an expected answer.
const SNIPPET_BYTES: usize = 2 * 1024;

fn wirk_bin() -> &'static str {
    env!("CARGO_BIN_EXE_wirk")
}

struct KillOnDrop(std::process::Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn wait_for_pointer(estate: &Path) -> WirkdPointer {
    let path = estate.join(".wirk").join("wirkd.json");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(bytes) = fs::read(&path)
            && let Ok(pointer) = serde_json::from_slice::<WirkdPointer>(&bytes)
        {
            return pointer;
        }
        assert!(
            Instant::now() < deadline,
            "wirkd pointer file never appeared at {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn start_wirkd(estate: &Path) -> KillOnDrop {
    let child = KillOnDrop(
        Command::new(wirk_bin())
            .args(["wirkd", "start", "--estate"])
            .arg(estate)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn wirkd"),
    );
    wait_for_pointer(estate);
    child
}

fn stop_wirkd(estate: &Path, mut child: KillOnDrop) {
    let stop = Command::new(wirk_bin())
        .args(["wirkd", "stop", "--estate"])
        .arg(estate)
        .output()
        .expect("wirkd stop runs");
    assert!(
        stop.status.success(),
        "wirkd stop failed: {}",
        String::from_utf8_lossy(&stop.stderr)
    );
    let exit = child.0.wait().expect("reap wirkd");
    assert!(exit.success(), "wirkd did not exit clean: {exit:?}");
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn seed_repo(repo: &Path, files: &[(&str, &str)]) {
    fs::create_dir_all(repo).expect("create repo dir");
    git(repo, &["init", "-q"]);
    git(repo, &["config", "user.email", "locality@example.test"]);
    git(repo, &["config", "user.name", "locality"]);
    for (name, contents) in files {
        fs::write(repo.join(name), contents).expect("write seed file");
    }
    git(repo, &["add", "."]);
    git(repo, &["commit", "-q", "-m", "seed"]);
}

fn atlas(estate: &Path, args: &[&str]) -> (bool, serde_json::Value, String) {
    let mut full = vec!["atlas"];
    full.extend_from_slice(args);
    full.push("--estate");
    let estate = estate.to_str().expect("estate path is utf-8");
    full.push(estate);
    full.push("--json");
    let output = Command::new(wirk_bin())
        .args(&full)
        .output()
        .expect("wirk atlas runs");
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let value = serde_json::from_str(&stdout).unwrap_or(serde_json::Value::Null);
    (output.status.success(), value, stderr)
}

/// Acquire + publish one repository under one alias, and return nothing:
/// every later assertion reads the reply, not this helper.
fn acquire_and_publish(estate: &Path, alias: &str, repo: &Path) {
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
    assert!(ok, "acquire failed: {err}");
    let generation = acquired["generation"]["generation"]
        .as_str()
        .expect("acquire names a generation")
        .to_string();
    let (ok, _, err) = atlas(
        estate,
        &["publish", "--source", alias, "--generation", &generation],
    );
    assert!(ok, "publish failed: {err}");
}

/// A realistic multi-line Rust source: ~190 KiB of ordinary declarations
/// with non-ASCII comment text throughout, so line and character
/// boundaries are both genuinely exercised, plus the named needles at
/// the byte depths this file asserts against.
fn long_source(blocks: &[(String, usize)]) -> String {
    let mut out = String::new();
    let mut placed = vec![false; blocks.len()];
    let mut index = 0usize;
    while out.len() < 190_000 {
        for (block_index, (block, depth)) in blocks.iter().enumerate() {
            if !placed[block_index] && out.len() >= *depth {
                out.push_str("/// Le décompte réel — vérifié, jamais supposé.\n");
                out.push_str(block);
                out.push_str("\n\n");
                placed[block_index] = true;
            }
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
    for (block_index, (block, _)) in blocks.iter().enumerate() {
        assert!(placed[block_index], "block {block} was never placed");
    }
    out
}

/// One ordinary declaration, at the depth it must be placed.
fn declaration(name: &str, depth: usize) -> (String, usize) {
    (
        format!("fn {name}(state: &State) -> Coverage {{\n    state.coverage().clone()\n}}"),
        depth,
    )
}

/// The first `limit` bytes of `text`, cut on a character boundary — for
/// failure messages only, so a panic never panics again on multibyte
/// source.
fn head(text: &str, limit: usize) -> &str {
    let mut end = limit.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// The one-based line, and the byte offset, at which `needle` occurs in
/// `text` — computed off the committed text itself.
fn locate(text: &str, needle: &str) -> (usize, usize) {
    let offset = text.find(needle).expect("needle is in the committed text");
    (text[..offset].matches('\n').count() + 1, offset)
}

fn hit_for<'a>(search: &'a serde_json::Value, path_suffix: &str) -> &'a serde_json::Value {
    search["hits"]
        .as_array()
        .expect("hits is an array")
        .iter()
        .find(|hit| {
            hit["path"]
                .as_str()
                .is_some_and(|path| path.ends_with(path_suffix))
        })
        .unwrap_or_else(|| panic!("no hit named a path ending {path_suffix}: {search}"))
}

/// `atlas resolve` of the hit's evidence coordinate, asserted resolved.
fn resolve_evidence(estate: &Path, hit: &serde_json::Value) -> String {
    let coordinate = hit["evidence"]["coordinate"]
        .as_str()
        .unwrap_or_else(|| panic!("hit carries no evidence coordinate: {hit}"))
        .to_string();
    let (ok, resolved, err) = atlas(estate, &["resolve", "--coordinate", &coordinate]);
    assert!(ok, "resolve of the evidence coordinate failed: {err}");
    assert_eq!(resolved["outcome"].as_str(), Some("resolved"));
    resolved["text"]
        .as_str()
        .expect("resolved text is a string")
        .to_string()
}

/// The decisive one. A symbol that lives 50 KiB deep inside a packed
/// 64 KiB unit must be *visible* in the reply, inside the same 2 KiB
/// budget, and the reply must say exactly which committed bytes were
/// shown.
#[test]
fn a_deep_match_in_a_packed_unit_is_shown_and_its_span_resolves_to_those_bytes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    let repo = dir.path().join("repo");

    let source = long_source(&[declaration("carried_source_coverage", 120_000)]);
    let (needle_line, needle_offset) = locate(&source, "fn carried_source_coverage");
    assert!(
        needle_offset > 65_536 + SNIPPET_BYTES,
        "the fixture must place the match beyond the first unit's display budget, not at \
         byte {needle_offset}"
    );
    seed_repo(&repo, &[("server.rs", source.as_str())]);

    let wirkd = start_wirkd(&estate);
    acquire_and_publish(&estate, "wirk", &repo);

    let (ok, search, err) = atlas(&estate, &["search", "--query", "carried_source_coverage"]);
    assert!(ok, "search failed: {err}");
    let hit = hit_for(&search, "server.rs");

    let snippet = hit["snippet"].as_str().expect("snippet is a string");
    assert!(
        snippet.len() <= SNIPPET_BYTES,
        "the displayed evidence must stay inside the presentation budget, not grow to the \
         unit: {} bytes",
        snippet.len()
    );
    assert!(
        hit["unit_bytes"].as_u64().unwrap() > SNIPPET_BYTES as u64,
        "the fixture must produce a packed unit larger than the budget"
    );
    assert!(
        snippet.contains("carried_source_coverage"),
        "the displayed evidence does not contain the term that was searched for; snippet \
         begins {:?}",
        head(snippet, 120)
    );

    // The window is a *narrower* span than the ranked unit, and says so:
    // the hit's own coordinate still names the whole unit.
    let unit_start = hit["line_start"].as_u64().unwrap();
    let unit_end = hit["line_end"].as_u64().unwrap();
    let shown_start = hit["evidence"]["line_start"].as_u64().unwrap();
    let shown_end = hit["evidence"]["line_end"].as_u64().unwrap();
    assert!(
        unit_start <= shown_start
            && shown_end <= unit_end
            && (shown_end - shown_start) < (unit_end - unit_start),
        "shown lines {shown_start}-{shown_end} must be a proper part of the unit's \
         {unit_start}-{unit_end}"
    );
    assert!(
        shown_start <= needle_line as u64 && needle_line as u64 <= shown_end,
        "the definition is on committed line {needle_line}; the reply showed \
         {shown_start}-{shown_end}"
    );
    assert_eq!(
        hit["evidence"]["matched_terms"].as_array().unwrap(),
        &vec![serde_json::json!("carried_source_coverage")],
        "the reply must name the query terms it actually located"
    );
    assert_eq!(hit["evidence"]["whole_match_shown"].as_bool(), Some(true));
    assert_eq!(hit["snippet_truncated"].as_bool(), Some(true));

    // The span is a real supported coordinate over the real committed
    // blob: resolving it returns exactly the bytes that were displayed,
    // and those bytes are the file's own.
    let resolved = resolve_evidence(&estate, hit);
    assert_eq!(
        resolved, snippet,
        "the evidence coordinate must resolve to exactly the displayed bytes"
    );
    let byte_start = hit["evidence"]["byte_start"].as_u64().unwrap() as usize;
    let byte_end = hit["evidence"]["byte_end"].as_u64().unwrap() as usize;
    assert_eq!(
        &source[byte_start..byte_end],
        snippet,
        "the evidence byte span must name the committed bytes it displayed"
    );

    // The whole-unit coordinate is untouched and still resolves the
    // whole unit — the recovery path this change does not remove.
    let whole = hit["coordinate"].as_str().unwrap().to_string();
    let (ok, resolved_unit, err) = atlas(&estate, &["resolve", "--coordinate", &whole]);
    assert!(ok, "resolve of the unit coordinate failed: {err}");
    assert_eq!(
        resolved_unit["text"].as_str().unwrap().len() as u64,
        hit["unit_bytes"].as_u64().unwrap()
    );

    stop_wirkd(&estate, wirkd);
}

/// Two query terms, three places: one holds only the first, one only the
/// second, one holds both. The window must land where the query actually
/// converges, not merely on the first occurrence of anything.
#[test]
fn a_multi_term_query_shows_the_place_the_terms_actually_meet() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    let repo = dir.path().join("repo");

    // The first term alone, the second term alone, and — 40 KiB deep,
    // far past the head of the unit — the one place both actually
    // occur. Each is a separate token, which is what the ranker scores.
    let source = long_source(&[
        declaration("coverage_disposition_only", 4_000),
        declaration("retrieval_unit_only", 20_000),
        (
            "fn where_they_meet(unit: &retrieval_unit_only) -> coverage_disposition_only {\n    \
             unit.disposition()\n}"
                .to_string(),
            40_000,
        ),
    ]);
    seed_repo(&repo, &[("query.rs", source.as_str())]);

    let wirkd = start_wirkd(&estate);
    acquire_and_publish(&estate, "wirk", &repo);

    let (ok, search, err) = atlas(
        &estate,
        &[
            "search",
            "--query",
            "coverage_disposition_only retrieval_unit_only",
        ],
    );
    assert!(ok, "search failed: {err}");
    let hit = hit_for(&search, "query.rs");
    let snippet = hit["snippet"].as_str().unwrap();
    assert!(
        snippet.contains("coverage_disposition_only") && snippet.contains("retrieval_unit_only"),
        "the window must contain both matched terms; it showed {:?}",
        head(snippet, 200)
    );
    let mut terms: Vec<&str> = hit["evidence"]["matched_terms"]
        .as_array()
        .unwrap()
        .iter()
        .map(|term| term.as_str().unwrap())
        .collect();
    terms.sort_unstable();
    assert_eq!(terms, ["coverage_disposition_only", "retrieval_unit_only"]);
    assert_eq!(resolve_evidence(&estate, hit), snippet);

    stop_wirkd(&estate, wirkd);
}

/// A file whose whole unit fits the budget keeps exactly the reply it
/// had: the whole unit inline, nothing cut, and no narrower span — the
/// hit's own coordinate already names what was shown.
#[test]
fn a_small_unit_is_still_shown_whole_and_names_no_narrower_span() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    let repo = dir.path().join("repo");
    let contents = "/// petit fichier, rien de plus\nfn validate_claim() {}\n";
    seed_repo(&repo, &[("lib.rs", contents)]);

    let wirkd = start_wirkd(&estate);
    acquire_and_publish(&estate, "wirk", &repo);

    let (ok, search, err) = atlas(&estate, &["search", "--query", "validate_claim"]);
    assert!(ok, "search failed: {err}");
    let hit = hit_for(&search, "lib.rs");
    assert_eq!(hit["snippet"].as_str(), Some(contents));
    assert_eq!(hit["snippet_truncated"].as_bool(), Some(false));
    assert_eq!(
        hit["unit_bytes"].as_u64(),
        Some(contents.len() as u64),
        "the unit is the whole small file"
    );
    assert!(
        hit["evidence"].is_null(),
        "nothing was cut, so there is no narrower span to name: {hit}"
    );

    stop_wirkd(&estate, wirkd);
}

/// The honest fallback. When the matched token is itself larger than the
/// display budget there is no bounded text that can hold the whole
/// match, so the reply shows what fits, starting at the match, and says
/// the match was not shown whole. It still names the exact span of what
/// it did show.
#[test]
fn a_token_larger_than_the_budget_is_shown_cut_and_the_reply_says_so() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    let repo = dir.path().join("repo");

    let giant = format!("gigantic_{}", "z".repeat(3_000));
    let mut source = String::new();
    for index in 0..400 {
        source.push_str(&format!("fn filler_{index}() {{}}\n"));
    }
    source.push_str(&format!("static {giant}: usize = 1;\n"));
    for index in 400..800 {
        source.push_str(&format!("fn filler_{index}() {{}}\n"));
    }
    seed_repo(&repo, &[("huge_token.rs", source.as_str())]);

    let wirkd = start_wirkd(&estate);
    acquire_and_publish(&estate, "wirk", &repo);

    let (ok, search, err) = atlas(&estate, &["search", "--query", &giant]);
    assert!(ok, "search failed: {err}");
    let hit = hit_for(&search, "huge_token.rs");
    let snippet = hit["snippet"].as_str().unwrap();
    assert!(snippet.len() <= SNIPPET_BYTES);
    assert!(
        snippet.starts_with(&giant[..SNIPPET_BYTES.min(giant.len()) - 1])
            || snippet.contains(&giant[..500]),
        "the window must begin at the match it cannot show whole"
    );
    assert_eq!(
        hit["evidence"]["whole_match_shown"].as_bool(),
        Some(false),
        "a match wider than the budget must be declared cut, not shown as though whole"
    );
    assert_eq!(resolve_evidence(&estate, hit), snippet);

    stop_wirkd(&estate, wirkd);
}

/// Multibyte text on both sides of the match: the window is chosen on
/// line boundaries over committed UTF-8, so what is shown is a whole
/// number of committed lines and resolves byte-for-byte.
#[test]
fn a_window_in_multibyte_text_lands_on_committed_line_boundaries() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    let repo = dir.path().join("repo");

    let mut source = String::new();
    for index in 0..900 {
        source.push_str(&format!(
            "// « mesuré » — étape {index} : ça, ça compte — 日本語も、ここに。\n"
        ));
        if index == 600 {
            source.push_str("fn point_de_repère_unique() -> Résultat { Résultat::Ok }\n");
        }
    }
    seed_repo(&repo, &[("accents.rs", source.as_str())]);

    let wirkd = start_wirkd(&estate);
    acquire_and_publish(&estate, "wirk", &repo);

    let (ok, search, err) = atlas(&estate, &["search", "--query", "point_de_repère_unique"]);
    assert!(ok, "search failed: {err}");
    let hit = hit_for(&search, "accents.rs");
    let snippet = hit["snippet"].as_str().unwrap();
    assert!(snippet.contains("point_de_repère_unique"));
    assert!(snippet.len() <= SNIPPET_BYTES);
    assert!(
        snippet.ends_with('\n'),
        "a window cut on a line boundary ends at one"
    );
    let byte_start = hit["evidence"]["byte_start"].as_u64().unwrap() as usize;
    assert!(
        byte_start == 0 || source.as_bytes()[byte_start - 1] == b'\n',
        "a window cut on a line boundary starts after one"
    );
    assert_eq!(resolve_evidence(&estate, hit), snippet);
    assert_eq!(&source[byte_start..byte_start + snippet.len()], snippet);

    stop_wirkd(&estate, wirkd);
}
