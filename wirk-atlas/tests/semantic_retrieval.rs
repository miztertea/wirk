//! P3 W4 B contract checks for public semantic retrieval
//! (`W4-PUBLIC-RETRIEVAL-BUILD.md`, "Decisive implementation proof").
//!
//! Every check runs real child processes across the two real argv/stdin
//! boundaries the product uses in production — `wirk-embed/v2` for the
//! build, `wirk-query/v1` for the query. The backends here are small
//! scripts on disk: the chunk boundaries they return are deliberately
//! simple, and the ranking one returns a fixed order, because what these
//! pin is the *product's* half — the exact original-byte mapping and its
//! re-derivation, the covering unit run, the admitted ranking view, the
//! identity and coverage record, and the continuation contract. They are
//! not the proof that retrieval works: that is the recorded real run
//! against the installed `semble`, the cached model and real repositories
//! in `public-retrieval-build/raw/` (ruling 0040).
//!
//! Each refusal below was watched failing against the same code with its
//! own check removed; see `raw/60-watched-guards.log`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;
use wirk_atlas::{
    AtlasStore, ExtractorPolicy, Membership, QueryScope, SearchRequest, SemanticBuildConfig,
    SemanticBuildOutcome, SemanticChunking, SemanticEdition, SemanticQueryConfig, SemanticRequest,
    SemanticStatus,
};

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// A repository holding one resource of every class the mapping contract
/// has to survive: plain LF code, a CRLF document whose ranking text is
/// *not* its committed bytes, a file of repeated identical stanzas, and a
/// whitespace-only file that yields no row at all.
fn fixture_repo() -> TempDir {
    let repo = TempDir::new().unwrap();
    git(repo.path(), &["init", "-q"]);
    git(repo.path(), &["config", "user.email", "a@b"]);
    git(repo.path(), &["config", "user.name", "A"]);
    fs::write(
        repo.path().join("code.rs"),
        "fn alpha() { let admitted = 1; }\nfn beta() { let ranking = 2; }\nfn gamma() {}\n\
         fn delta() { let coordinate = 3; }\nfn epsilon() { let continuation = 4; }\n\
         fn zeta() { let evidence = 5; }\nfn eta() { let admitted = 6; }\n",
    )
    .unwrap();
    fs::write(
        repo.path().join("doc.md"),
        "# heading\r\nbody one\r\nbody two\r\n",
    )
    .unwrap();
    fs::write(
        repo.path().join("repeat.toml"),
        "a = 1\nb = 2\na = 1\nb = 2\n",
    )
    .unwrap();
    fs::write(repo.path().join("blank.md"), "   \n\t\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "fixture"]);
    repo
}

fn model_dir(directory: &Path) -> PathBuf {
    let model = directory.join("model");
    fs::create_dir_all(&model).unwrap();
    fs::write(model.join("weights.bin"), b"stub-model-weights").unwrap();
    model
}

fn executable(script: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(script).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(script, permissions).unwrap();
}

/// A real `wirk-embed/v2` backend in `chunk-embed` mode.
///
/// It chunks by fixed *character* windows of the normalised text and
/// projects back to original byte offsets the honest way, so the product's
/// own re-derivation has something to agree with. `flavour` makes it
/// defective in one specific way at a time — the point of each negative is
/// that the *product* catches it, so the defect has to live in a real
/// backend rather than in a mocked return value.
fn chunk_backend(directory: &Path, name: &str, flavour: &str) -> PathBuf {
    let script = directory.join(name);
    fs::write(
        &script,
        format!(
            r#"#!/usr/bin/env python3
import hashlib, json, os, struct, sys
FLAVOUR = {flavour:?}
WINDOW = 40

def absorb(d, part):
    d.update(len(part).to_bytes(8, "big"))
    d.update(part)

def model_digest(directory):
    files = []
    for root, _dirs, names in os.walk(directory, followlinks=True):
        for n in names:
            a = os.path.join(root, n)
            if os.path.isfile(a):
                files.append((os.path.relpath(a, directory).encode(), a))
    files.sort(key=lambda p: p[0])
    d = hashlib.sha256()
    absorb(d, b"wirk-model-directory/v1")
    absorb(d, len(files).to_bytes(8, "big"))
    for rel, a in files:
        absorb(d, rel)
        absorb(d, open(a, "rb").read())
    return d.hexdigest()

def decode_with_offsets(data):
    """utf-8 with replacement, then universal newlines, tracking the
    original byte each produced character began at."""
    raw = data.decode("utf-8", "replace")
    # The fixture corpus is valid UTF-8, so character i of the decoded
    # string begins at the byte offset of its own encoding prefix.
    offs = []
    pos = 0
    for ch in raw:
        offs.append(pos)
        pos += len(ch.encode("utf-8"))
    offs.append(len(data))
    text, out = [], []
    i = 0
    while i < len(raw):
        if raw[i] == "\r":
            text.append("\n"); out.append(offs[i])
            i += 2 if i + 1 < len(raw) and raw[i + 1] == "\n" else 1
        else:
            text.append(raw[i]); out.append(offs[i])
            i += 1
    out.append(len(data))
    return "".join(text), out

header = json.loads(sys.stdin.readline())
inputs = [json.loads(line) for line in sys.stdin if line.strip()]
rows, texts = [], []
unmapped = []
FIRED = False
for entry in inputs:
    data = open(entry["bytes_file"], "rb").read()
    source, offs = decode_with_offsets(data)
    if not source.strip():
        continue
    slot = 0
    for start in range(0, len(source), WINDOW):
        end = min(start + WINDOW, len(source))
        piece = source[start:end]
        bs, be = offs[start], offs[end]
        if FLAVOUR == "outside_blob" and not FIRED and slot == 0:
            FIRED = True
            be = len(data) + 5
        if FLAVOUR == "overlapping" and slot > 0:
            bs = max(bs - 3, 0)
        digest = hashlib.sha256(piece.encode()).hexdigest()
        if FLAVOUR == "forged_text" and slot == 1:
            digest = hashlib.sha256(b"something else entirely").hexdigest()
        norm = "identity" if piece.encode() == data[bs:be] else "universal-newline+utf8-replace/v1"
        if FLAVOUR == "wrong_normalization" and slot == 0:
            norm = "identity" if norm != "identity" else "universal-newline+utf8-replace/v1"
        if FLAVOUR == "skipped_slot" and slot == 1:
            slot += 1
        rows.append({{
            "input": entry["input"], "slot": slot,
            "byte_start": bs, "byte_end": be,
            "language": "rust" if entry["ranking_path"].endswith(".rs") else None,
            "text_digest": digest, "text_normalization": norm,
            "text_byte_len": len(piece.encode()),
        }})
        texts.append(piece)
        slot += 1

digest = model_digest(header["model_path"])
with open(header["output"], "wb") as handle:
    for text in texts:
        seed = hashlib.sha256(digest.encode() + b"\x00" + text.encode()).digest()
        handle.write(struct.pack("<4f", *(b / 255.0 for b in seed[:4])))
with open(header["chunks"], "w") as handle:
    for row in rows:
        handle.write(json.dumps(row, sort_keys=True) + "\n")
reply = {{
    "protocol": "wirk-embed/v2",
    "backend": "test-chunker/" + FLAVOUR,
    "model_path": header["model_path"],
    "model_digest": digest,
    "rows": len(rows),
    "dimensions": 4,
    "unmapped": unmapped,
}}
if FLAVOUR != "no_chunker_identity":
    files = {{}}
    me = os.path.abspath(__file__)
    files["test.chunker"] = {{
        "path": me,
        "digest": hashlib.sha256(open(me, "rb").read()).hexdigest(),
        "byte_len": os.path.getsize(me),
    }}
    if FLAVOUR == "lie_about_chunker":
        files["test.chunker"]["digest"] = "0" * 64
    reply["chunker"] = {{
        "implementation": "test-chunker/1.0",
        "entry_point": "fixed_window",
        "constants": "window=40",
        "parsers": "none",
        "files": files,
    }}
    # O1: the parser shared libraries a boundary actually came out of.
    # `library` is a real file in the estate directory, so the product
    # re-reads and digests bytes that exist.
    if FLAVOUR.startswith("grammar_"):
        library = os.path.join(os.path.dirname(me), "libtest_grammar.so")
        if not os.path.exists(library):
            open(library, "wb").write(b"grammar bytes v1\n")
        body = open(library, "rb").read()
        entry = {{
            "path": library,
            "digest": hashlib.sha256(body).hexdigest(),
            "byte_len": len(body),
            "languages": ["rust"],
            "declared_digest": hashlib.sha256(b"grammar bytes v1\n").hexdigest(),
        }}
        if FLAVOUR == "grammar_lie":
            entry["digest"] = "0" * 64
        if FLAVOUR == "grammar_none_loaded":
            reply["chunker"]["grammars"] = {{
                "state": "none_loaded", "provider": "test_grammars.loader",
                "cache_root": os.path.dirname(library),
                "reason": "no parser shared library was loaded in this process",
            }}
        elif FLAVOUR == "grammar_unavailable":
            reply["chunker"]["grammars"] = {{
                "state": "unavailable", "provider": "other_provider._native",
                "reason": "parsers come from other_provider._native, whose loaded grammar files this integration cannot enumerate",
            }}
        elif FLAVOUR == "grammar_empty":
            reply["chunker"]["grammars"] = {{
                "state": "measured", "provider": "test_grammars.loader",
                "cache_root": os.path.dirname(library),
                "libraries": [], "uncovered": [],
            }}
        else:
            reply["chunker"]["grammars"] = {{
                "state": "measured", "provider": "test_grammars.loader",
                "cache_root": os.path.dirname(library),
                "libraries": [entry], "uncovered": [],
            }}
print(json.dumps(reply))
"#
        ),
    )
    .unwrap();
    executable(&script);
    script
}

/// A real `wirk-query/v1` backend. It writes the exact view it was handed
/// to `<script>.view.ndjson` — that file is the evidence for what did and
/// did not reach the ranker — and returns the rows in a fixed order.
fn query_backend(directory: &Path, name: &str, flavour: &str) -> PathBuf {
    let script = directory.join(name);
    fs::write(
        &script,
        format!(
            r#"#!/usr/bin/env python3
import hashlib, json, os, sys
FLAVOUR = {flavour:?}

def absorb(d, part):
    d.update(len(part).to_bytes(8, "big"))
    d.update(part)

def model_digest(directory):
    files = []
    for root, _dirs, names in os.walk(directory, followlinks=True):
        for n in names:
            a = os.path.join(root, n)
            if os.path.isfile(a):
                files.append((os.path.relpath(a, directory).encode(), a))
    files.sort(key=lambda p: p[0])
    d = hashlib.sha256()
    absorb(d, b"wirk-model-directory/v1")
    absorb(d, len(files).to_bytes(8, "big"))
    for rel, a in files:
        absorb(d, rel)
        absorb(d, open(a, "rb").read())
    return d.hexdigest()

header = json.loads(sys.stdin.readline())
rows = [json.loads(line) for line in sys.stdin if line.strip()]
with open(os.path.abspath(__file__) + ".view.ndjson", "w") as handle:
    handle.write(json.dumps(header, sort_keys=True) + "\n")
    for row in rows:
        handle.write(json.dumps(row, sort_keys=True) + "\n")
vectors = open(header["vectors"], "rb").read()
assert len(vectors) == header["rows"] * header["dimensions"] * 4, "view vectors are the wrong size"
reply = {{
    "protocol": "wirk-query/v1",
    "backend": "test-query/" + FLAVOUR,
    "native": "test-native/1.0",
    "model_path": header["model_path"],
    "model_digest": model_digest(header["model_path"]),
    "returned": len(rows),
}}
if FLAVOUR == "out_of_range":
    reply["returned"] = len(rows) + 1
print(json.dumps(reply))
for rank, row in enumerate(rows, 1):
    print(json.dumps({{"row": row["row"], "score": 1.0 / rank, "rank": rank}}))
if FLAVOUR == "out_of_range":
    print(json.dumps({{"row": len(rows), "score": 0.0, "rank": len(rows) + 1}}))
"#
        ),
    )
    .unwrap();
    executable(&script);
    script
}

struct Estate {
    _temporary: TempDir,
    _repo: TempDir,
    store: AtlasStore,
    membership: Membership,
    generation: wirk_atlas::GenerationId,
    model: PathBuf,
    directory: PathBuf,
}

fn estate() -> Estate {
    estate_with(fixture_repo())
}

/// Like `fixture_repo`, but `code.rs` is padded past the 65536-byte
/// packed-unit budget (`wirk-atlas/src/extract.rs`'s `MAX_UNIT_BYTES`)
/// with more short lines, so the default (`v4`) extractor edition is
/// forced to derive more than one raw unit for it. `fixture_repo`'s
/// `code.rs` alone (a few hundred bytes) now packs into exactly one raw
/// unit under `v4`, which would make a covering run that spans more than
/// one unit unreachable — the thing `c_the_unit_run_covers_the_range_...`
/// exists to exercise.
fn large_fixture_repo() -> TempDir {
    let repo = TempDir::new().unwrap();
    git(repo.path(), &["init", "-q"]);
    git(repo.path(), &["config", "user.email", "a@b"]);
    git(repo.path(), &["config", "user.name", "A"]);
    let mut code = String::new();
    for i in 0..2500 {
        code.push_str(&format!("fn f{i}() {{ let admitted = {i}; }}\n"));
    }
    assert!(code.len() > 65536, "fixture must force more than one unit");
    fs::write(repo.path().join("code.rs"), code).unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "fixture"]);
    repo
}

fn estate_with(repo: TempDir) -> Estate {
    let temporary = TempDir::new().unwrap();
    let mut store =
        AtlasStore::open(temporary.path(), temporary.path().display().to_string()).unwrap();
    let membership = store
        .register_git("fixture", repo.path().display().to_string(), "HEAD")
        .unwrap();
    let staged = store
        .acquire(&membership, "HEAD", ExtractorPolicy::default())
        .unwrap()
        .staged()
        .unwrap();
    store.publish(&membership, &staged.id).unwrap();
    let model = model_dir(temporary.path());
    let directory = temporary.path().to_path_buf();
    Estate {
        _temporary: temporary,
        _repo: repo,
        store,
        membership,
        generation: staged.id,
        model,
        directory,
    }
}

fn build(estate: &mut Estate, flavour: &str) -> SemanticBuildOutcome {
    let backend = chunk_backend(&estate.directory, &format!("chunk-{flavour}.py"), flavour);
    let generation = estate.generation.clone();
    let membership = estate.membership.clone();
    estate
        .store
        .build_semantic(
            &membership,
            &generation,
            &SemanticBuildConfig {
                backend,
                backend_args: Vec::new(),
                model: estate.model.clone(),
                producer: "test/w4b".into(),
                chunking: SemanticChunking::Native,
            },
        )
        .unwrap()
}

fn staged(outcome: SemanticBuildOutcome) -> SemanticEdition {
    match outcome {
        SemanticBuildOutcome::Staged(edition) => *edition,
        SemanticBuildOutcome::Refused(reason) => panic!("expected a staged edition, got: {reason}"),
    }
}

fn refusal(outcome: SemanticBuildOutcome) -> String {
    match outcome {
        SemanticBuildOutcome::Refused(reason) => reason,
        SemanticBuildOutcome::Staged(edition) => {
            panic!("expected a refusal, got edition {}", edition.id.0)
        }
    }
}

fn search_request(estate: &Estate, backend: Option<&Path>) -> SearchRequest {
    SearchRequest {
        scope: QueryScope::EstateOrientation,
        requested_source: None,
        query: "admitted ranking".into(),
        families: vec![],
        semantic: SemanticRequest::Requested,
        limit: 50,
        pinned: None,
        offset: 0,
        semantic_query: backend.map(|backend| SemanticQueryConfig {
            backend: backend.to_path_buf(),
            backend_args: Vec::new(),
            model: estate.model.clone(),
        }),
        pinned_editions: None,
        pinned_mode: None,
        pinned_producer: wirk_atlas::PinnedProducer::Unrecorded,
    }
}

// ---- the mapping contract ------------------------------------------------

/// A1. Every row of a native-chunk edition addresses exact committed
/// bytes, and the ranking text re-derives from them. This is the 0078/F10
/// re-derivation control over native chunks rather than over units.
#[test]
fn a_every_native_row_re_derives_from_its_committed_bytes() {
    let mut estate = estate();
    let edition = staged(build(&mut estate, "honest"));
    // `v5` is the scheme this product writes now: it binds the parser
    // shared libraries a boundary came out of on top of everything `v4`
    // bound, and earlier records still read back as themselves.
    assert_eq!(edition.identity, wirk_atlas::IDENTITY_V5);
    let rows = read_rows(&estate, &edition);
    assert!(
        rows.len() > 4,
        "expected several native chunks, got {}",
        rows.len()
    );
    for row in &rows {
        let bytes = blob(&estate, &row.object_id);
        let slice = &bytes[row.byte_start as usize..row.byte_end as usize];
        assert_eq!(
            sha256(slice),
            row.content_digest,
            "evidence digest for row {}",
            row.row
        );
        let text = normalise(slice);
        assert_eq!(
            sha256(text.as_bytes()),
            row.ranking_text_digest(),
            "ranking text for row {}",
            row.row
        );
        assert!(row.ranking_path.is_some());
        assert!(row.family.is_some());
    }
}

/// A2. A CRLF resource keeps its evidence digest and its ranking digest
/// apart, and says which transformation stands between them. One digest
/// for both would either forge the evidence or report every such row as
/// corrupt.
#[test]
fn b_a_transformed_resource_keeps_two_digests_and_names_the_transformation() {
    let mut estate = estate();
    let edition = staged(build(&mut estate, "honest"));
    let rows = read_rows(&estate, &edition);
    let doc: Vec<_> = rows.iter().filter(|row| row.path == b"doc.md").collect();
    assert!(!doc.is_empty());
    for row in &doc {
        assert_eq!(row.normalization(), wirk_atlas::TEXT_NORMALIZED);
        assert_ne!(
            row.content_digest,
            row.ranking_text_digest(),
            "a CRLF row's evidence and ranking digests must differ"
        );
    }
    let code: Vec<_> = rows.iter().filter(|row| row.path == b"code.rs").collect();
    assert!(
        code.iter()
            .all(|row| row.normalization() == wirk_atlas::TEXT_IDENTITY)
    );
    assert!(
        code.iter()
            .all(|row| row.content_digest == row.ranking_text_digest())
    );
}

/// A3. The covering unit run is a superset, never an equality claim, and
/// it is contiguous within the row's own resource.
#[test]
fn c_the_unit_run_covers_the_range_without_claiming_to_equal_it() {
    let mut estate = estate_with(large_fixture_repo());
    let edition = staged(build(&mut estate, "honest"));
    let rows = read_rows(&estate, &edition);
    let generation = estate.store.generation(&edition.generation).unwrap();
    let mut strict = 0;
    for row in &rows {
        let resource = generation
            .resources
            .iter()
            .find(|resource| resource.path == row.path)
            .unwrap();
        let first = resource
            .units
            .iter()
            .find(|unit| unit.id == row.unit)
            .unwrap();
        let last_id = row.unit_last.clone().unwrap_or_else(|| row.unit.clone());
        let last = resource
            .units
            .iter()
            .find(|unit| unit.id == last_id)
            .unwrap();
        assert!(
            first.byte_start <= row.byte_start,
            "row {} unit run starts too late",
            row.row
        );
        assert!(
            row.byte_end <= last.byte_end,
            "row {} unit run ends too early",
            row.row
        );
        if first.id != last.id {
            strict += 1;
        }
    }
    assert!(
        strict > 0,
        "no row spans more than one unit; the covering run is not exercised"
    );
}

/// A4. Repeated identical text keeps distinct coordinates, and a
/// whitespace-only resource is *named* as producing no row rather than
/// reported as missing or silently dropped.
#[test]
fn d_repeated_text_stays_distinct_and_a_chunk_free_resource_is_named() {
    let mut estate = estate();
    let edition = staged(build(&mut estate, "honest"));
    let rows = read_rows(&estate, &edition);
    let mut triples: Vec<_> = rows
        .iter()
        .map(|row| {
            (
                row.path.clone(),
                row.object_id.clone(),
                row.byte_start,
                row.byte_end,
            )
        })
        .collect();
    let before = triples.len();
    triples.sort();
    triples.dedup();
    assert_eq!(before, triples.len(), "duplicate coordinate triples");

    assert!(rows.iter().all(|row| row.path != b"blank.md"));
    let named: Vec<_> = edition
        .coverage
        .resources_without_rows
        .iter()
        .map(|entry| entry.name.clone())
        .collect();
    assert!(
        named.iter().any(|name| name.ends_with("blank.md")),
        "the chunk-free resource must be named: {named:?}"
    );
    // The edition must not claim to tile the blobs.
    assert!(edition.coverage.covered_bytes <= edition.coverage.indexed_bytes);
    assert_eq!(edition.coverage.resources_indexed, 4);
    assert_eq!(edition.coverage.resources_with_rows, 3);
}

// ---- what the product refuses to record ----------------------------------

/// A5. A backend that reports a ranking text the committed bytes do not
/// produce is refused: the mapping is re-derived on this side, never
/// believed.
#[test]
fn e_a_forged_ranking_text_digest_is_refused() {
    let mut estate = estate();
    let reason = refusal(build(&mut estate, "forged_text"));
    assert!(reason.contains("normalise to"), "{reason}");
    assert!(
        reason.contains("refuses rather than record a mapping"),
        "{reason}"
    );
}

/// A6. A range outside the blob it claims is refused.
#[test]
fn f_a_range_outside_its_own_blob_is_refused() {
    let mut estate = estate();
    let reason = refusal(build(&mut estate, "outside_blob"));
    assert!(reason.contains("outside its own"), "{reason}");
}

/// A7. Overlapping ranges within one resource are refused: rows are
/// ordered and non-overlapping or they are not an index.
#[test]
fn g_overlapping_ranges_are_refused() {
    let mut estate = estate();
    let reason = refusal(build(&mut estate, "overlapping"));
    assert!(reason.contains("inside the previous chunk"), "{reason}");
}

/// A8. A non-consecutive slot is refused: the slot is the native document
/// key's own counter, so a gap in it silently changes document identity.
#[test]
fn h_a_skipped_slot_is_refused() {
    let mut estate = estate();
    let reason = refusal(build(&mut estate, "skipped_slot"));
    assert!(reason.contains("not consecutive"), "{reason}");
}

/// A9. A mislabelled normalization is refused even when the digest is
/// right: the label is what tells a reader why two digests may differ.
#[test]
fn i_a_mislabelled_normalization_is_refused() {
    let mut estate = estate();
    let reason = refusal(build(&mut estate, "wrong_normalization"));
    assert!(reason.contains("reports normalization"), "{reason}");
}

/// A10. Native boundaries whose producer is unrecorded cannot become an
/// edition, and a chunker that misreports its own implementation bytes is
/// refused — the same rule the model and the backend environment follow.
#[test]
fn j_the_chunker_must_be_recorded_and_re_measured() {
    let mut estate = estate();
    let reason = refusal(build(&mut estate, "no_chunker_identity"));
    assert!(reason.contains("described no chunker"), "{reason}");
    let reason = refusal(build(&mut estate, "lie_about_chunker"));
    assert!(reason.contains("chunker module"), "{reason}");

    let edition = staged(build(&mut estate, "honest"));
    let chunks = edition
        .chunker
        .chunks
        .expect("a native edition records its chunker");
    assert_eq!(chunks.implementation, "test-chunker/1.0");
    assert_eq!(chunks.constants, "window=40");
    assert_eq!(chunks.files.len(), 1);
    assert_eq!(chunks.files[0].file_count, 1);
}

// ---- the admitted ranking view -------------------------------------------

/// A11. The view handed to the ranker contains exactly the admitted rows.
/// A source the scope denies is not in it — not selected out of a wider
/// index, absent from it — and no metadata of that source appears anywhere
/// in the answer.
#[test]
fn k_the_ranking_view_contains_only_admitted_rows() {
    let mut estate = estate();
    let edition = staged(build(&mut estate, "honest"));
    estate
        .store
        .select_semantic(&estate.membership.clone(), &edition.id)
        .unwrap()
        .unwrap();
    let backend = query_backend(&estate.directory, "query-honest.py", "honest");
    let answer =
        wirk_atlas::search(&estate.store, &search_request(&estate, Some(&backend))).unwrap();
    assert_eq!(
        answer.semantic,
        SemanticStatus::Applied,
        "{:?}",
        answer.semantic
    );
    assert_eq!(answer.mode, wirk_atlas::RankingMode::Semantic);
    let rows = read_rows(&estate, &edition);
    let view = view_rows(&backend);
    assert_eq!(
        view.len(),
        rows.len(),
        "the view is exactly the edition's rows"
    );

    // Now deny everything and prove nothing at all is ranked, and that the
    // refusal discloses no edition.
    let mut denied = search_request(&estate, Some(&backend));
    denied.scope = QueryScope::Work(vec![]);
    let answer = wirk_atlas::search(&estate.store, &denied).unwrap();
    assert!(answer.coverage.denied);
    assert!(answer.hits.is_empty());
    assert!(answer.editions.is_empty());
    let SemanticStatus::Unavailable(reason) = &answer.semantic else {
        panic!("expected unavailable, got {:?}", answer.semantic);
    };
    assert!(
        !reason.contains(&edition.id.0),
        "a denial must not disclose an edition: {reason}"
    );
    assert!(
        !reason.contains("fixture"),
        "a denial must not disclose a source alias: {reason}"
    );
    assert!(
        !reason.contains(&estate.membership.locator),
        "a denial must not disclose a locator: {reason}"
    );
}

/// A12. A family filter is applied to *rows*, before the view exists, so
/// an excluded row cannot reach a corpus statistic.
#[test]
fn l_family_admission_shrinks_the_view_itself() {
    let mut estate = estate();
    let edition = staged(build(&mut estate, "honest"));
    estate
        .store
        .select_semantic(&estate.membership.clone(), &edition.id)
        .unwrap()
        .unwrap();
    let backend = query_backend(&estate.directory, "query-family.py", "honest");
    let mut request = search_request(&estate, Some(&backend));
    request.families = vec![wirk_atlas::ContentFamily::Code];
    let answer = wirk_atlas::search(&estate.store, &request).unwrap();
    let view = view_rows(&backend);
    assert!(!view.is_empty());
    assert!(
        view.iter()
            .all(|row| row.ends_with(".rs\"") || row.contains("code.rs")),
        "only code rows may reach the view: {view:?}"
    );
    let all = read_rows(&estate, &edition).len();
    assert!(
        view.len() < all,
        "the family filter must shrink the view ({} of {all})",
        view.len()
    );
    assert_eq!(answer.application.unwrap().rows_ranked as usize, view.len());
}

/// A13. A selected edition whose record on disk no longer computes to its
/// own identity is refused, and the answer falls back to lexical with a
/// reason rather than ranking through it.
///
/// The neighbouring rule — an edition that carries *no* retrieval identity
/// at all (every edition built before W4 B) is refused rather than ranked
/// under whatever this build happens to do today — cannot be reached from
/// a hand-edited record, because the identity check above fires first. It
/// is pinned instead by executed evidence against a real historical `v3`
/// edition built by the previous binary: `raw/30-neg-v3.txt`.
#[test]
fn m_an_edition_whose_record_no_longer_computes_is_refused() {
    let mut estate = estate();
    let edition = staged(build(&mut estate, "honest"));
    estate
        .store
        .select_semantic(&estate.membership.clone(), &edition.id)
        .unwrap()
        .unwrap();
    // Strip the retrieval identity from the record on disk, exactly as a
    // pre-W4-B edition has it, and re-sign it under its own scheme.
    let path = estate
        ._temporary
        .path()
        .join("atlas/semantic")
        .join(&edition.id.0)
        .join(wirk_atlas::EDITION_RECORD);
    let mut record: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    record["identity"] = serde_json::json!(wirk_atlas::IDENTITY_V3);
    record["retrieval"] = serde_json::Value::Null;
    let rewritten: SemanticEdition = serde_json::from_value(record.clone()).unwrap();
    // The id is a digest over the scheme's own fields, so a v3 record has
    // a v3 id; recompute it the way `read_edition` will check it.
    let backend = query_backend(&estate.directory, "query-v3.py", "honest");
    fs::write(&path, serde_json::to_vec_pretty(&rewritten).unwrap()).unwrap();
    let answer =
        wirk_atlas::search(&estate.store, &search_request(&estate, Some(&backend))).unwrap();
    let SemanticStatus::Unavailable(reason) = &answer.semantic else {
        panic!("expected unavailable, got {:?}", answer.semantic);
    };
    assert!(reason.contains("lexical"), "{reason}");
    // Specifically the identity failure, not merely "some refusal": with
    // that check removed the record deserializes and a *different* guard
    // catches it, which would leave this check pinning nothing in
    // particular.
    assert!(
        reason.contains("does not compute to that identity"),
        "{reason}"
    );
    assert_eq!(answer.mode, wirk_atlas::RankingMode::Lexical);
}

/// A14. A query model that did not embed the rows is refused before the
/// backend is even run, and a backend returning a row the view never sent
/// is refused after it.
#[test]
fn n_a_foreign_model_and_a_foreign_row_are_both_refused() {
    let mut estate = estate();
    let edition = staged(build(&mut estate, "honest"));
    estate
        .store
        .select_semantic(&estate.membership.clone(), &edition.id)
        .unwrap()
        .unwrap();
    let backend = query_backend(&estate.directory, "query-model.py", "honest");

    let other = estate.directory.join("other-model");
    fs::create_dir_all(&other).unwrap();
    fs::write(other.join("weights.bin"), b"a different model entirely").unwrap();
    let mut request = search_request(&estate, Some(&backend));
    request.semantic_query.as_mut().unwrap().model = other;
    let answer = wirk_atlas::search(&estate.store, &request).unwrap();
    let SemanticStatus::Unavailable(reason) = &answer.semantic else {
        panic!("expected unavailable, got {:?}", answer.semantic);
    };
    assert!(reason.contains("not comparable"), "{reason}");

    let rogue = query_backend(&estate.directory, "query-rogue.py", "out_of_range");
    let answer = wirk_atlas::search(&estate.store, &search_request(&estate, Some(&rogue))).unwrap();
    let SemanticStatus::Unavailable(reason) = &answer.semantic else {
        panic!("expected unavailable, got {:?}", answer.semantic);
    };
    assert!(reason.contains("never sent"), "{reason}");
}

/// A15. A query creates nothing: the estate is byte-identical before and
/// after, no edition appears, and the publication revision does not move.
#[test]
fn o_a_query_writes_no_durable_byte() {
    let mut estate = estate();
    let edition = staged(build(&mut estate, "honest"));
    estate
        .store
        .select_semantic(&estate.membership.clone(), &edition.id)
        .unwrap()
        .unwrap();
    let backend = query_backend(&estate.directory, "query-pure.py", "honest");
    let root = estate._temporary.path().join("atlas");
    let before = tree_digest(&root);
    let revision = estate.store.publication_revision();
    for _ in 0..3 {
        wirk_atlas::search(&estate.store, &search_request(&estate, Some(&backend))).unwrap();
    }
    assert_eq!(
        before,
        tree_digest(&root),
        "a query changed durable estate bytes"
    );
    assert_eq!(revision, estate.store.publication_revision());
}

/// A16. A continuation keeps the editions and the ranking mode it was
/// issued under, and refuses explicitly — never silently restarting or
/// downgrading — when they can no longer be reproduced.
#[test]
fn p_a_continuation_pins_its_editions_and_its_mode() {
    let mut estate = estate();
    let edition = staged(build(&mut estate, "honest"));
    estate
        .store
        .select_semantic(&estate.membership.clone(), &edition.id)
        .unwrap()
        .unwrap();
    // A continuation control needs a backend whose record has a measured
    // basis; an unreported one is refused for that instead, which would
    // make this test prove something else (`VERDICT.md` V1, Q8).
    let cont_module = measured_module(&estate.directory, "cont_module.py", "RANKER = 1\n");
    let backend = identity_query_backend(
        &estate.directory,
        "query-cont.py",
        "forward",
        Some(cont_module.as_path()),
    );
    let mut first = search_request(&estate, Some(&backend));
    first.limit = 2;
    let page1 = wirk_atlas::search(&estate.store, &first).unwrap();
    assert_eq!(page1.mode, wirk_atlas::RankingMode::Semantic);
    assert_eq!(page1.editions.len(), 1);

    let pinned: std::collections::BTreeMap<_, _> = page1.editions.iter().cloned().collect();
    let generations: std::collections::BTreeMap<_, _> = page1.generations.iter().cloned().collect();
    let mut second = search_request(&estate, Some(&backend));
    second.limit = 2;
    second.offset = 2;
    second.pinned = Some(generations.clone());
    second.pinned_editions = Some(pinned.clone());
    second.pinned_mode = Some(wirk_atlas::RankingMode::Semantic);
    second.pinned_producer = wirk_atlas::PinnedProducer::Recorded(
        page1.application.as_ref().unwrap().producer_pin.clone(),
    );
    let page2 = wirk_atlas::search(&estate.store, &second).unwrap();
    assert_eq!(page2.editions, page1.editions);
    assert!(page2.hits.iter().all(|hit| !page1.hits.contains(hit)));

    // The pinned edition's bytes rot: the page is refused, not restarted.
    let vectors = estate
        ._temporary
        .path()
        .join("atlas/semantic")
        .join(&edition.id.0)
        .join(&edition.vectors.file);
    let mut bytes = fs::read(&vectors).unwrap();
    bytes[0] ^= 0xff;
    fs::write(&vectors, &bytes).unwrap();
    let refused = wirk_atlas::search(&estate.store, &second).unwrap();
    assert!(refused.coverage.continuation_unrecoverable);
    assert!(refused.hits.is_empty());
    assert_eq!(refused.mode, wirk_atlas::RankingMode::Semantic);
    assert!(!refused.coverage.is_complete());

    // A lexical continuation stays lexical even once semantics is fine
    // again — the mode is the receipt's, not the estate's.
    bytes[0] ^= 0xff;
    fs::write(&vectors, &bytes).unwrap();
    let mut lexical = search_request(&estate, Some(&backend));
    lexical.limit = 2;
    lexical.offset = 2;
    lexical.pinned = Some(generations);
    lexical.pinned_mode = Some(wirk_atlas::RankingMode::Lexical);
    let answer = wirk_atlas::search(&estate.store, &lexical).unwrap();
    assert_eq!(answer.mode, wirk_atlas::RankingMode::Lexical);
    let SemanticStatus::Unavailable(reason) = &answer.semantic else {
        panic!("expected unavailable, got {:?}", answer.semantic);
    };
    assert!(reason.contains("keeps the ranking mode"), "{reason}");
}

/// A16b. When only *some* of a continuation's captured editions can still
/// be ranked through, the page is refused rather than quietly re-ranked
/// over the surviving subset — a narrower corpus under the first page's
/// receipt would be a different answer wearing it.
#[test]
fn p2_a_continuation_over_a_surviving_subset_is_still_refused() {
    let mut estate = estate();
    let second = TempDir::new().unwrap();
    git(second.path(), &["init", "-q"]);
    git(second.path(), &["config", "user.email", "a@b"]);
    git(second.path(), &["config", "user.name", "A"]);
    fs::write(
        second.path().join("other.rs"),
        "fn other() { let admitted = 7; }\n",
    )
    .unwrap();
    git(second.path(), &["add", "."]);
    git(second.path(), &["commit", "-qm", "second"]);
    let other = estate
        .store
        .register_git("second", second.path().display().to_string(), "HEAD")
        .unwrap();
    let staged_other = estate
        .store
        .acquire(&other, "HEAD", ExtractorPolicy::default())
        .unwrap()
        .staged()
        .unwrap();
    estate.store.publish(&other, &staged_other.id).unwrap();

    let backend = chunk_backend(&estate.directory, "chunk-subset.py", "honest");
    let configuration = SemanticBuildConfig {
        backend,
        backend_args: Vec::new(),
        model: estate.model.clone(),
        producer: "test/w4b".into(),
        chunking: SemanticChunking::Native,
    };
    let first_edition = staged(
        estate
            .store
            .build_semantic(
                &estate.membership.clone(),
                &estate.generation.clone(),
                &configuration,
            )
            .unwrap(),
    );
    let other_edition = staged(
        estate
            .store
            .build_semantic(&other, &staged_other.id, &configuration)
            .unwrap(),
    );
    estate
        .store
        .select_semantic(&estate.membership.clone(), &first_edition.id)
        .unwrap()
        .unwrap();
    estate
        .store
        .select_semantic(&other, &other_edition.id)
        .unwrap()
        .unwrap();

    let subset_module = measured_module(&estate.directory, "subset_module.py", "RANKER = 1\n");
    let query = identity_query_backend(
        &estate.directory,
        "query-subset.py",
        "forward",
        Some(subset_module.as_path()),
    );
    let mut first = search_request(&estate, Some(&query));
    first.limit = 2;
    let page1 = wirk_atlas::search(&estate.store, &first).unwrap();
    assert_eq!(page1.editions.len(), 2);

    // One of the two rots. The other is still perfectly rankable — and
    // that is exactly the case a subset re-rank would silently serve.
    let vectors = estate
        ._temporary
        .path()
        .join("atlas/semantic")
        .join(&other_edition.id.0)
        .join(&other_edition.vectors.file);
    let mut bytes = fs::read(&vectors).unwrap();
    bytes[0] ^= 0xff;
    fs::write(&vectors, &bytes).unwrap();

    let mut second_page = search_request(&estate, Some(&query));
    second_page.limit = 2;
    second_page.offset = 2;
    second_page.pinned = Some(page1.generations.iter().cloned().collect());
    second_page.pinned_editions = Some(page1.editions.iter().cloned().collect());
    second_page.pinned_mode = Some(wirk_atlas::RankingMode::Semantic);
    // The producer this continuation was issued under, unchanged: the
    // subject here is the *corpus* that can no longer be reproduced, so
    // the implementation must be held still for it to be the reason.
    second_page.pinned_producer = wirk_atlas::PinnedProducer::Recorded(
        page1.application.as_ref().unwrap().producer_pin.clone(),
    );
    let refused = wirk_atlas::search(&estate.store, &second_page).unwrap();
    assert!(
        refused.coverage.continuation_unrecoverable,
        "{:?}",
        refused.semantic
    );
    assert!(refused.hits.is_empty());
    let SemanticStatus::Unavailable(reason) = &refused.semantic else {
        panic!("expected unavailable, got {:?}", refused.semantic);
    };
    assert!(
        reason.contains("only 1 can be ranked through now"),
        "{reason}"
    );
}

/// A17. Two memberships publishing the same source-relative path are two
/// distinct native documents through the ranking path itself, not merely
/// in a final adapter, and each recovers its own committed bytes.
#[test]
fn q_colliding_relative_paths_are_distinct_ranking_documents() {
    let mut estate = estate();
    let second = TempDir::new().unwrap();
    git(second.path(), &["init", "-q"]);
    git(second.path(), &["config", "user.email", "a@b"]);
    git(second.path(), &["config", "user.name", "A"]);
    // The SAME relative path, deliberately different bytes.
    fs::write(
        second.path().join("code.rs"),
        "fn delta() { let other = 9; }\n",
    )
    .unwrap();
    git(second.path(), &["add", "."]);
    git(second.path(), &["commit", "-qm", "collide"]);
    let other = estate
        .store
        .register_git("second", second.path().display().to_string(), "HEAD")
        .unwrap();
    let staged_other = estate
        .store
        .acquire(&other, "HEAD", ExtractorPolicy::default())
        .unwrap()
        .staged()
        .unwrap();
    estate.store.publish(&other, &staged_other.id).unwrap();

    let backend = chunk_backend(&estate.directory, "chunk-collide.py", "honest");
    let configuration = |model: PathBuf| SemanticBuildConfig {
        backend: backend.clone(),
        backend_args: Vec::new(),
        model,
        producer: "test/w4b".into(),
        chunking: SemanticChunking::Native,
    };
    let first_edition = staged(
        estate
            .store
            .build_semantic(
                &estate.membership.clone(),
                &estate.generation.clone(),
                &configuration(estate.model.clone()),
            )
            .unwrap(),
    );
    let other_edition = staged(
        estate
            .store
            .build_semantic(
                &other,
                &staged_other.id,
                &configuration(estate.model.clone()),
            )
            .unwrap(),
    );
    estate
        .store
        .select_semantic(&estate.membership.clone(), &first_edition.id)
        .unwrap()
        .unwrap();
    estate
        .store
        .select_semantic(&other, &other_edition.id)
        .unwrap()
        .unwrap();

    let query = query_backend(&estate.directory, "query-collide.py", "honest");
    let answer = wirk_atlas::search(&estate.store, &search_request(&estate, Some(&query))).unwrap();
    assert_eq!(answer.editions.len(), 2);
    let view = view_rows(&query);
    let paths: Vec<String> = view
        .iter()
        .filter_map(|row| {
            serde_json::from_str::<serde_json::Value>(row)
                .ok()?
                .get("ranking_path")?
                .as_str()
                .map(str::to_owned)
        })
        .collect();
    let mut keys: Vec<String> = view
        .iter()
        .filter_map(|row| {
            let row: serde_json::Value = serde_json::from_str(row).ok()?;
            Some(format!("{}:{}", row["ranking_path"].as_str()?, row["slot"]))
        })
        .collect();
    let before = keys.len();
    keys.sort();
    keys.dedup();
    assert_eq!(
        before,
        keys.len(),
        "the ranking view collides on a native document key"
    );
    assert!(
        paths.iter().any(|path| path.starts_with("fixture/"))
            && paths.iter().any(|path| path.starts_with("second/")),
        "both memberships must appear under the frozen path convention: {paths:?}"
    );
    // Both memberships' own bytes are recoverable at the colliding path.
    let hits: Vec<_> = answer
        .hits
        .iter()
        .filter(|hit| hit.coordinate.path == b"code.rs")
        .collect();
    assert!(hits.len() >= 2);
    for hit in hits {
        let owner = if hit.coordinate.membership == estate.membership.id {
            estate.membership.clone()
        } else {
            other.clone()
        };
        let resolved = estate.store.resolve_exact(&owner, &hit.coordinate).unwrap();
        let wirk_atlas::ResolveOutcome::Resolved(evidence) = resolved else {
            panic!("a ranked coordinate must resolve");
        };
        assert_eq!(sha256(&evidence.bytes), sha256(hit.snippet.as_bytes()));
    }
}

// ---- helpers -------------------------------------------------------------

fn read_rows(estate: &Estate, edition: &SemanticEdition) -> Vec<wirk_atlas::MappingRow> {
    let path = estate
        ._temporary
        .path()
        .join("atlas/semantic")
        .join(&edition.id.0)
        .join(&edition.mapping.file);
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn view_rows(backend: &Path) -> Vec<String> {
    let path = format!("{}.view.ndjson", backend.display());
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .skip(1) // the header
        .filter(|line| !line.trim().is_empty())
        .map(str::to_owned)
        .collect()
}

fn blob(estate: &Estate, object_id: &str) -> Vec<u8> {
    let output = Command::new("git")
        .arg("-C")
        .arg(estate._repo.path())
        .args(["cat-file", "blob", object_id])
        .output()
        .unwrap();
    assert!(output.status.success());
    output.stdout
}

fn sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The product's own rule, restated independently here so the test does
/// not check the implementation against itself.
fn normalise(bytes: &[u8]) -> String {
    let decoded = String::from_utf8_lossy(bytes);
    let mut out = String::new();
    let mut characters = decoded.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '\r' {
            if characters.peek() == Some(&'\n') {
                characters.next();
            }
            out.push('\n');
        } else {
            out.push(character);
        }
    }
    out
}

// ---- the query producer identity contract --------------------------------
//
// `public-retrieval-verify/VERDICT.md` O1, executed as this stage's red
// (`public-retrieval-identity-correct/raw/06-RED-SUMMARY.txt`): page 1 and
// page 2 of one continuation, model/source/generation/edition all fixed,
// and the file at the same configured backend path replaced in between by
// a differently *configured* run of the same installed native ranker. The
// second page came back reranked, under the first page's token, reporting
// `applied` and the same version string. What the token pinned was the
// backend's spelling; what changed was its bytes.

/// A `wirk-query/v1` backend whose *ranking order* and whose *reported
/// environment* are both parameters, so a test can change one and hold
/// the other still. `order` is `forward` or `reverse`; `module`, when
/// given, is a file this backend claims as a loaded module — the
/// interpreter's account of what ran, which the product re-reads and
/// digests itself.
fn identity_query_backend(
    directory: &Path,
    name: &str,
    order: &str,
    module: Option<&Path>,
) -> PathBuf {
    let script = directory.join(name);
    let module = module
        .map(|path| path.display().to_string())
        .unwrap_or_default();
    fs::write(
        &script,
        format!(
            r#"#!/usr/bin/env python3
import hashlib, json, os, sys
ORDER = {order:?}
MODULE = {module:?}

def absorb(d, part):
    d.update(len(part).to_bytes(8, "big"))
    d.update(part)

def model_digest(directory):
    files = []
    for root, _dirs, names in os.walk(directory, followlinks=True):
        for n in names:
            a = os.path.join(root, n)
            if os.path.isfile(a):
                files.append((os.path.relpath(a, directory).encode(), a))
    files.sort(key=lambda p: p[0])
    d = hashlib.sha256()
    absorb(d, b"wirk-model-directory/v1")
    absorb(d, len(files).to_bytes(8, "big"))
    for rel, a in files:
        absorb(d, rel)
        absorb(d, open(a, "rb").read())
    return d.hexdigest()

header = json.loads(sys.stdin.readline())
rows = [json.loads(line) for line in sys.stdin if line.strip()]
vectors = open(header["vectors"], "rb").read()
assert len(vectors) == header["rows"] * header["dimensions"] * 4, "view vectors are the wrong size"
reply = {{
    "protocol": "wirk-query/v1",
    "backend": "test-query/identity",
    # Deliberately constant: a version string is not an identity, and this
    # backend keeps saying the same one however it is changed.
    "native": "test-native/1.0",
    "model_path": header["model_path"],
    "model_digest": model_digest(header["model_path"]),
    "returned": len(rows),
}}
if MODULE:
    body = open(MODULE, "rb").read()
    reply["environment"] = {{
        "kind": "test-modules/v1",
        "root": os.path.dirname(MODULE),
        "runtime": "test/1.0",
        "executable": sys.executable,
        "distributions": [],
        "undescribed_distributions": [],
        "modules": [{{
            "name": "ranker",
            "origin": MODULE,
            "path": MODULE,
            "digest": hashlib.sha256(body).hexdigest(),
            "byte_len": len(body),
            "claims": [],
        }}],
        "unmeasured_modules": [],
    }}
print(json.dumps(reply))
ordered = rows if ORDER == "forward" else list(reversed(rows))
for rank, row in enumerate(ordered, 1):
    print(json.dumps({{"row": row["row"], "score": 1.0 / rank, "rank": rank}}))
"#
        ),
    )
    .unwrap();
    executable(&script);
    script
}

/// A file a query backend can claim as a loaded module, so its answer
/// carries a *measured* basis.
///
/// Every continuation control below needs one. `VERDICT.md` V1: a backend
/// that reports no environment publishes an honest `configuration_only`
/// basis, and this product refuses to page under it — so a control that
/// used such a backend would refuse for the wrong reason and prove
/// nothing about the thing it names.
fn measured_module(directory: &Path, name: &str, body: &str) -> PathBuf {
    let path = directory.join(name);
    fs::write(&path, body).unwrap();
    path
}

/// Page 1 over a freshly selected native edition, and the request that
/// asks for page 2 of it.
fn paged(estate: &Estate, backend: &Path) -> (wirk_atlas::SearchAnswer, SearchRequest) {
    let mut first = search_request(estate, Some(backend));
    first.limit = 2;
    let page1 = wirk_atlas::search(&estate.store, &first).unwrap();
    assert_eq!(page1.mode, wirk_atlas::RankingMode::Semantic);
    let application = page1
        .application
        .as_ref()
        .expect("a semantic answer names the implementation that ranked it");
    let mut second = search_request(estate, Some(backend));
    second.limit = 2;
    second.offset = 2;
    second.pinned = Some(page1.generations.iter().cloned().collect());
    second.pinned_editions = Some(page1.editions.iter().cloned().collect());
    second.pinned_mode = Some(wirk_atlas::RankingMode::Semantic);
    second.pinned_producer = wirk_atlas::PinnedProducer::Recorded(application.producer_pin.clone());
    (page1, second)
}

fn selected_estate(name: &str) -> Estate {
    let mut estate = estate();
    let edition = staged(build(&mut estate, "honest"));
    estate
        .store
        .select_semantic(&estate.membership.clone(), &edition.id)
        .unwrap()
        .unwrap();
    let _ = name;
    estate
}

fn refused_continuation(answer: &wirk_atlas::SearchAnswer) -> String {
    assert!(
        answer.coverage.continuation_unrecoverable,
        "expected an unrecoverable continuation"
    );
    assert!(answer.hits.is_empty(), "a refused page returned hits");
    assert_eq!(answer.mode, wirk_atlas::RankingMode::Semantic);
    assert!(answer.application.is_none());
    let SemanticStatus::Unavailable(reason) = &answer.semantic else {
        panic!("expected unavailable, got {:?}", answer.semantic);
    };
    // VERDICT.md D5: an unrecoverable continuation returns no hit at all,
    // so the sentence must not describe hits it did not return.
    assert!(
        !reason.contains("these hits are lexical"),
        "a refusal that returned no page called its hits lexical: {reason}"
    );
    assert!(
        reason.contains("no page was returned"),
        "the refusal does not say a page was refused: {reason}"
    );
    reason.clone()
}

/// Q1. A semantic answer names the implementation that ranked it, in the
/// product's own measurement of it — not only in the child's self-report.
#[test]
fn q1_an_answer_binds_the_measured_query_producer() {
    let estate = selected_estate("q1");
    let backend = identity_query_backend(&estate.directory, "q1.py", "forward", None);
    let answer = wirk_atlas::search(
        &estate.store,
        &search_request(&estate, Some(backend.as_path())),
    )
    .unwrap();
    let application = answer.application.as_ref().unwrap();
    let producer = &application.producer;
    assert_eq!(producer.protocol, wirk_atlas::QUERY_PROTOCOL);
    assert_eq!(
        producer.program.canonical,
        backend.canonicalize().unwrap().display().to_string()
    );
    // The product's own reading of the file it executed, not the child's
    // claim about it.
    assert_eq!(
        producer.program.digest,
        sha256(&fs::read(&backend).unwrap())
    );
    assert_eq!(
        producer.program.byte_len,
        fs::metadata(&backend).unwrap().len()
    );
    assert_eq!(producer.reported, "test-native/1.0");
    // This backend reports no environment, which is a legal answer and is
    // recorded as an honest absence rather than as coverage nobody has.
    assert!(matches!(
        producer.environment,
        wirk_atlas::BackendEnvironment::Unreported
    ));
    assert_eq!(producer.argv.len(), 0);
    assert!(!application.producer_pin.configuration.is_empty());
    assert!(!application.producer_pin.identity.is_empty());
    assert_ne!(
        application.producer_pin.configuration,
        application.producer_pin.identity
    );
    // Deterministic: the same implementation over the same request digests
    // the same way, so a pin is a comparison and not a nonce.
    let again = wirk_atlas::search(
        &estate.store,
        &search_request(&estate, Some(backend.as_path())),
    )
    .unwrap();
    assert_eq!(
        again.application.as_ref().unwrap().producer_pin,
        application.producer_pin
    );
}

/// Q2. The executed red, as a guard: the file at the *same* configured
/// path is replaced between two pages of one continuation. Every string
/// the token carries is unchanged; the bytes are not. No page is served.
#[test]
fn q2_a_changed_implementation_at_the_same_path_refuses_the_page() {
    let estate = selected_estate("q2");
    let module = measured_module(&estate.directory, "q2_module.py", "RANKER = 1\n");
    let backend = identity_query_backend(
        &estate.directory,
        "q2.py",
        "forward",
        Some(module.as_path()),
    );
    let (page1, second) = paged(&estate, &backend);

    // The unchanged positive first, so the refusal below is the change and
    // not the setup: the very same request serves page 2.
    let page2 = wirk_atlas::search(&estate.store, &second).unwrap();
    assert!(!page2.coverage.continuation_unrecoverable);
    assert!(!page2.hits.is_empty());
    assert!(page2.hits.iter().all(|hit| !page1.hits.contains(hit)));
    assert_eq!(
        Some(&page2.application.as_ref().unwrap().producer_pin),
        match &second.pinned_producer {
            wirk_atlas::PinnedProducer::Recorded(pin) => Some(pin),
            _ => None,
        }
    );

    // Same path, same argv, same reported version string, different bytes
    // and a genuinely different order.
    identity_query_backend(
        &estate.directory,
        "q2.py",
        "reverse",
        Some(module.as_path()),
    );
    let refused = wirk_atlas::search(&estate.store, &second).unwrap();
    let reason = refused_continuation(&refused);
    assert!(
        reason.contains("configuration"),
        "the refusal does not name the configuration that moved: {reason}"
    );
    // And decisively: nothing was reranked and handed out under the old
    // token — the page the changed implementation *would* have produced is
    // not what the caller got.
    let mut fresh = search_request(&estate, Some(backend.as_path()));
    fresh.limit = 2;
    let reranked = wirk_atlas::search(&estate.store, &fresh).unwrap();
    assert_ne!(reranked.hits, page1.hits);
    assert!(refused.hits.is_empty());
}

/// Q3. An alias retargeted to a different file is a different producer:
/// the configured string never moves, and canonicalization plus the
/// digest of what was actually opened both do.
#[test]
fn q3_an_alias_retargeted_between_pages_refuses_the_page() {
    let estate = selected_estate("q3");
    let module = measured_module(&estate.directory, "q3_module.py", "RANKER = 1\n");
    let forward = identity_query_backend(
        &estate.directory,
        "q3-forward.py",
        "forward",
        Some(module.as_path()),
    );
    let reverse = identity_query_backend(
        &estate.directory,
        "q3-reverse.py",
        "reverse",
        Some(module.as_path()),
    );
    let alias = estate.directory.join("q3-alias.py");
    std::os::unix::fs::symlink(&forward, &alias).unwrap();
    let (_page1, second) = paged(&estate, &alias);
    let page2 = wirk_atlas::search(&estate.store, &second).unwrap();
    assert!(!page2.coverage.continuation_unrecoverable);

    fs::remove_file(&alias).unwrap();
    std::os::unix::fs::symlink(&reverse, &alias).unwrap();
    let refused = wirk_atlas::search(&estate.store, &second).unwrap();
    refused_continuation(&refused);
}

/// Q4. The case a file digest cannot see: the backend script and its
/// reported version string are byte-identical across both pages, and a
/// module that loaded inside it is not. The environment record carries
/// it, so the identity moves and the page is refused.
#[test]
fn q4_the_same_version_with_different_module_bytes_refuses_the_page() {
    let estate = selected_estate("q4");
    let module = estate.directory.join("ranker_module.py");
    fs::write(&module, "RANKER = 1\n").unwrap();
    let backend = identity_query_backend(
        &estate.directory,
        "q4.py",
        "forward",
        Some(module.as_path()),
    );
    let before = sha256(&fs::read(&backend).unwrap());
    let (page1, second) = paged(&estate, &backend);
    let application = page1.application.as_ref().unwrap();
    let wirk_atlas::BackendEnvironment::Reported(environment) = &application.producer.environment
    else {
        panic!("expected a reported environment");
    };
    assert_eq!(environment.modules.len(), 1);
    // Measured by the product itself, at the origin the backend named.
    assert_eq!(environment.modules[0].digest, sha256(b"RANKER = 1\n"));
    // No distribution declares it, and the record says so instead of
    // claiming a membership it never verified.
    assert!(matches!(
        environment.modules[0].attribution,
        wirk_atlas::ModuleAttribution::Undeclared(_)
    ));
    assert!(matches!(
        environment.coverage,
        wirk_atlas::EnvironmentCoverage::Partial(_)
    ));

    let page2 = wirk_atlas::search(&estate.store, &second).unwrap();
    assert!(!page2.coverage.continuation_unrecoverable);

    fs::write(&module, "RANKER = 2\n").unwrap();
    assert_eq!(
        before,
        sha256(&fs::read(&backend).unwrap()),
        "the backend file itself must not have moved for this control to mean anything"
    );
    let refused = wirk_atlas::search(&estate.store, &second).unwrap();
    let reason = refused_continuation(&refused);
    assert!(
        reason.contains("identity"),
        "the refusal does not name the identity that moved: {reason}"
    );
}

/// Q5. Configuration is part of the implementation: the same file, run
/// with a different argv, is a different producer. (The build side's own
/// `W4-LIFECYCLE-CORRECTION.md` item 4, at the query boundary.)
#[test]
fn q5_a_changed_argv_between_pages_refuses_the_page() {
    let estate = selected_estate("q5");
    let module = measured_module(&estate.directory, "q5_module.py", "RANKER = 1\n");
    let backend = identity_query_backend(
        &estate.directory,
        "q5.py",
        "forward",
        Some(module.as_path()),
    );
    let (_page1, mut second) = paged(&estate, &backend);
    let page2 = wirk_atlas::search(&estate.store, &second).unwrap();
    assert!(!page2.coverage.continuation_unrecoverable);
    second.semantic_query = Some(SemanticQueryConfig {
        backend: backend.clone(),
        backend_args: vec!["--alpha".into(), "0.2".into()],
        model: estate.model.clone(),
    });
    let refused = wirk_atlas::search(&estate.store, &second).unwrap();
    refused_continuation(&refused);
}

/// Q6. A semantic continuation issued before a producer was ever recorded
/// carries no pin. That is unknown history, not a clean bill of health,
/// and the page is refused for exactly that reason.
#[test]
fn q6_a_semantic_continuation_without_a_recorded_producer_is_refused() {
    let estate = selected_estate("q6");
    let backend = identity_query_backend(&estate.directory, "q6.py", "forward", None);
    let (_page1, mut second) = paged(&estate, &backend);
    second.pinned_producer = wirk_atlas::PinnedProducer::Unrecorded;
    let refused = wirk_atlas::search(&estate.store, &second).unwrap();
    let reason = refused_continuation(&refused);
    assert!(
        reason.contains("issued before the query producer identity was recorded"),
        "{reason}"
    );
}

/// Q7. A legitimate backend that enumerates nothing about itself stays
/// fully usable for the query it was asked, and its answer says exactly
/// what that record is worth. Provenance that only worked for one blessed
/// implementation would be a different product.
#[test]
fn q7_a_backend_that_reports_no_environment_is_still_usable() {
    let estate = selected_estate("q7");
    let backend = identity_query_backend(&estate.directory, "q7.py", "forward", None);
    let answer = wirk_atlas::search(
        &estate.store,
        &search_request(&estate, Some(backend.as_path())),
    )
    .unwrap();
    assert!(matches!(answer.semantic, SemanticStatus::Applied));
    assert!(!answer.hits.is_empty());
    let application = answer.application.as_ref().unwrap();
    assert!(matches!(
        application.producer.environment,
        wirk_atlas::BackendEnvironment::Unreported
    ));
    // The answer is complete and the ranking is real. What it also does,
    // rather than leaving a reader to infer it from `environment.state`,
    // is publish that its two digests cover the configured executable and
    // its argv and nothing beneath them.
    assert_eq!(
        application.producer_pin.basis,
        wirk_atlas::QueryProducerBasis::ConfigurationOnly
    );
}

/// Q8. The executed V1 red, as a guard
/// (`public-retrieval-identity-verify/VERDICT.md`). A backend that reports
/// no environment is legal and its answer honest — and its *continuation*
/// cannot be checked, because a module changed inside the same process at
/// the same path leaves the executable's bytes, every argv token and
/// therefore both digests exactly where they were.
///
/// Before this, that continuation was served: `applied`, a re-ranked page,
/// `continuation_unrecoverable: false`. It is now refused, and the refusal
/// names the missing basis rather than two digests that agree.
#[test]
fn q8_a_configuration_only_continuation_is_refused_with_its_missing_basis() {
    let estate = selected_estate("q8");
    let backend = identity_query_backend(&estate.directory, "q8.py", "forward", None);
    let (page1, second) = paged(&estate, &backend);
    // Page 1 is a real semantic page over the real view. The refusal below
    // is the continuation contract, not a broken setup.
    assert!(matches!(page1.semantic, SemanticStatus::Applied));
    assert!(!page1.hits.is_empty());
    assert_eq!(
        page1.application.as_ref().unwrap().producer_pin.basis,
        wirk_atlas::QueryProducerBasis::ConfigurationOnly
    );

    // Nothing whatsoever is changed: the same backend, the same argv, the
    // same edition. The page is refused anyway, because the pin cannot
    // tell this ranking from a different one.
    let refused = wirk_atlas::search(&estate.store, &second).unwrap();
    let reason = refused_continuation(&refused);
    assert!(
        reason.contains("measured no module at all") || reason.contains("reported no environment"),
        "the refusal does not name the missing basis: {reason}"
    );
    assert!(
        !reason.contains("issued before the query producer identity was recorded"),
        "a configuration-only pin is not pre-correction history: {reason}"
    );

    // And the same estate, same query, same everything, through a backend
    // that *does* report its modules, pages normally — so the refusal
    // above is the basis and not the corpus.
    let module = measured_module(&estate.directory, "q8_module.py", "RANKER = 1\n");
    let measured = identity_query_backend(
        &estate.directory,
        "q8-measured.py",
        "forward",
        Some(module.as_path()),
    );
    let (_page1, second) = paged(&estate, &measured);
    let page2 = wirk_atlas::search(&estate.store, &second).unwrap();
    assert!(!page2.coverage.continuation_unrecoverable);
    assert!(!page2.hits.is_empty());
    assert_eq!(
        page2.application.as_ref().unwrap().producer_pin.basis,
        wirk_atlas::QueryProducerBasis::ImplementationMeasured
    );
}

/// Q9. The other half of the same gap, and the reason the state is not a
/// boolean on `environment`: a backend that *does* report an environment
/// but enumerates no loaded module reads `coverage: unmeasured`, which is
/// honest and is exactly as unusable as a basis for a continuation.
///
/// `partial` is deliberately not in that class. Its modules were read and
/// digested by the product; only their distribution attribution is
/// incomplete (`QUERY-IDENTITY-REVIEW-ADJUDICATION.md`), and Q4 already
/// shows a partial record catching a changed module byte.
#[test]
fn q9_an_unmeasured_environment_is_not_a_continuation_basis_but_partial_is() {
    let estate = selected_estate("q9");
    let backend = nomodules_query_backend(&estate.directory, "q9.py");
    let answer = wirk_atlas::search(
        &estate.store,
        &search_request(&estate, Some(backend.as_path())),
    )
    .unwrap();
    assert!(matches!(answer.semantic, SemanticStatus::Applied));
    let application = answer.application.as_ref().unwrap();
    let wirk_atlas::BackendEnvironment::Reported(environment) = &application.producer.environment
    else {
        panic!("expected a reported environment");
    };
    // Reported, and never latched to complete by the absence of a list.
    assert!(matches!(
        environment.coverage,
        wirk_atlas::EnvironmentCoverage::Unmeasured
    ));
    assert_eq!(
        application.producer_pin.basis,
        wirk_atlas::QueryProducerBasis::ConfigurationOnly
    );

    let (_page1, second) = paged(&estate, &backend);
    let refused = wirk_atlas::search(&estate.store, &second).unwrap();
    let reason = refused_continuation(&refused);
    assert!(reason.contains("measured no module at all"), "{reason}");
}

/// Q10. `VERDICT.md` V2's second half. A token carrying some producer
/// fields and not others is a malformed token, and saying it "was issued
/// before the query producer identity was recorded" would be a false
/// statement about a token this build may have issued minutes ago. The
/// two absences are told apart.
#[test]
fn q10_an_incomplete_producer_pin_is_not_called_pre_correction_history() {
    let estate = selected_estate("q10");
    let module = measured_module(&estate.directory, "q10_module.py", "RANKER = 1\n");
    let backend = identity_query_backend(
        &estate.directory,
        "q10.py",
        "forward",
        Some(module.as_path()),
    );
    let (_page1, mut second) = paged(&estate, &backend);

    second.pinned_producer = wirk_atlas::PinnedProducer::Incomplete;
    let refused = wirk_atlas::search(&estate.store, &second).unwrap();
    let reason = refused_continuation(&refused);
    assert!(reason.contains("incomplete query producer pin"), "{reason}");
    assert!(
        !reason.contains("issued before the query producer identity was recorded"),
        "an incomplete pin is not history: {reason}"
    );

    // And the genuine legacy token still gets the genuine legacy sentence.
    second.pinned_producer = wirk_atlas::PinnedProducer::Unrecorded;
    let refused = wirk_atlas::search(&estate.store, &second).unwrap();
    let reason = refused_continuation(&refused);
    assert!(
        reason.contains("issued before the query producer identity was recorded"),
        "{reason}"
    );
}

/// G1. `EMPTY-PRODUCER-REVIEW-ADJUDICATION.md` O1, executed as a guard.
/// Boundaries come out of parse trees, and the parse trees come out of a
/// shared library the provider extracts into a cache — a file that is no
/// module, that no `sys.modules` entry names, and that a distribution
/// version does not pin. Before this, changing those bytes under the same
/// version left the edition identity exactly where it was.
///
/// The library is now digested by the product and bound into the id, so
/// the same rows produced by different grammar bytes are a different
/// edition — which is what an immutable edition id is for.
#[test]
fn g1_grammar_library_bytes_are_bound_into_the_edition_identity() {
    let mut estate = estate();
    let first = staged(build(&mut estate, "grammar_measured"));
    assert_eq!(first.identity, wirk_atlas::IDENTITY_V5);
    let library_path = estate.directory.join("libtest_grammar.so");
    let chunks = first.chunker.chunks.as_ref().unwrap();
    let wirk_atlas::GrammarCoverage::Measured(measured) = &chunks.grammars else {
        panic!(
            "expected measured grammar libraries, got {:?}",
            chunks.grammars
        );
    };
    assert_eq!(measured.libraries.len(), 1);
    let library = &measured.libraries[0];
    assert!(matches!(
        library.declaration,
        wirk_atlas::ModuleAttribution::Declared(_)
    ));
    assert!(measured.scope.contains("not measured"));

    // The same backend script, run again over the same generation with
    // the same model — and one changed byte in the library it loads. The
    // provider, its version, the entry point, the constants and the rows
    // are all identical, so nothing but the grammar bytes can move the
    // id.
    fs::write(
        &library_path,
        b"grammar bytes v2, same version, other bytes\n",
    )
    .unwrap();
    let second = staged(build(&mut estate, "grammar_measured"));
    let mutated = second.chunker.chunks.as_ref().unwrap();
    assert_eq!(chunks.implementation, mutated.implementation);
    assert_eq!(chunks.parsers, mutated.parsers);
    assert_eq!(first.mapping.digest, second.mapping.digest);
    assert_ne!(
        first.id.0, second.id.0,
        "a grammar library changed under an unchanged version must move the edition identity"
    );
    // And the cached file that disagrees with what the provider's archive
    // declares is *recorded* as undeclared, not silently trusted because
    // it exists.
    let wirk_atlas::GrammarCoverage::Measured(measured) = &mutated.grammars else {
        panic!("expected measured grammar libraries");
    };
    let wirk_atlas::ModuleAttribution::Undeclared(detail) = &measured.libraries[0].declaration
    else {
        panic!("a library whose bytes the archive does not declare must say so");
    };
    assert!(detail.contains("does not re-check"), "{detail}");
}

/// G2. The three honest absences, each its own state and none of them
/// readable as coverage: nothing loaded, nothing enumerable, and — D1(a)'s
/// lesson applied here — an empty measured list, which is refused rather
/// than recorded as a measurement of zero libraries.
#[test]
fn g2_absent_grammar_coverage_is_named_and_an_empty_measured_list_is_refused() {
    let mut estate = estate();
    let none = staged(build(&mut estate, "grammar_none_loaded"));
    let wirk_atlas::GrammarCoverage::NoneLoaded(reason) =
        &none.chunker.chunks.as_ref().unwrap().grammars
    else {
        panic!("expected none_loaded");
    };
    assert!(reason.contains("no parser shared library"), "{reason}");

    let unavailable = staged(build(&mut estate, "grammar_unavailable"));
    let wirk_atlas::GrammarCoverage::Unavailable(reason) =
        &unavailable.chunker.chunks.as_ref().unwrap().grammars
    else {
        panic!("expected unavailable");
    };
    assert!(reason.contains("cannot enumerate"), "{reason}");
    assert_ne!(
        none.id.0, unavailable.id.0,
        "two different honest absences must not digest alike"
    );

    let refused = refusal(build(&mut estate, "grammar_empty"));
    assert!(
        refused.contains("an empty list measures nothing"),
        "{refused}"
    );
}

/// G3. The product re-reads the library at the path the backend named and
/// refuses a report that does not match those bytes — the chunker
/// modules' discipline, over the file that actually decides a boundary.
#[test]
fn g3_a_misreported_grammar_library_is_refused() {
    let mut estate = estate();
    let refused = refusal(build(&mut estate, "grammar_lie"));
    assert!(
        refused.contains("grammar library") && refused.contains("but its bytes digest to"),
        "{refused}"
    );
}

/// G4. An edition built before grammars were measured says `unreported`
/// and gains nothing: no historical record is given coverage it never
/// had, and its `v4` identity keeps verifying under `v4`.
#[test]
fn g4_an_edition_that_reports_no_grammars_stays_unreported() {
    let mut estate = estate();
    let edition = staged(build(&mut estate, "honest"));
    assert!(matches!(
        edition.chunker.chunks.as_ref().unwrap().grammars,
        wirk_atlas::GrammarCoverage::Unreported
    ));
}

/// Q11. `EMPTY-PRODUCER-REVIEW-ADJUDICATION.md` D1(a), executed as a
/// guard. A backend that reports its environment *with an empty module
/// list* measured no implementation byte, and before this it read
/// `coverage complete` / `basis implementation_measured` and paged — so a
/// module changed at the same path inside the same process re-ranked page
/// 2 under page 1's token, exactly the shape V1 named.
///
/// The empty list is now the same epistemic state as no list at all. The
/// answer stays legal and useful; only the claim goes.
#[test]
fn q11_an_empty_module_list_measures_nothing_and_is_not_a_continuation_basis() {
    let estate = selected_estate("q11");
    let backend = zero_module_query_backend(&estate.directory, "q11.py", "empty");
    let answer = wirk_atlas::search(
        &estate.store,
        &search_request(&estate, Some(backend.as_path())),
    )
    .unwrap();
    // The fresh query is legal, applied and real: this is not a ban on a
    // backend, it is a correction of what its receipt says.
    assert!(matches!(answer.semantic, SemanticStatus::Applied));
    assert!(!answer.hits.is_empty());
    let application = answer.application.as_ref().unwrap();
    let wirk_atlas::BackendEnvironment::Reported(environment) = &application.producer.environment
    else {
        panic!("expected a reported environment");
    };
    assert!(
        environment.modules.is_empty(),
        "the fixture is supposed to report zero modules"
    );
    assert!(
        matches!(
            environment.coverage,
            wirk_atlas::EnvironmentCoverage::Unmeasured
        ),
        "a list covering no module byte is not complete: {:?}",
        environment.coverage
    );
    assert_eq!(
        application.producer_pin.basis,
        wirk_atlas::QueryProducerBasis::ConfigurationOnly
    );

    let (_page1, second) = paged(&estate, &backend);
    let refused = wirk_atlas::search(&estate.store, &second).unwrap();
    let reason = refused_continuation(&refused);
    assert!(reason.contains("measured no module at all"), "{reason}");

    // The green control that reaches the same path: one genuinely
    // measured module is a basis, and it pages. Partial attribution keeps
    // its adjudicated status; only zero coverage loses it.
    let module = measured_module(&estate.directory, "q11_module.py", "RANKER = 1\n");
    let measured = identity_query_backend(
        &estate.directory,
        "q11-measured.py",
        "forward",
        Some(module.as_path()),
    );
    let (page1, second) = paged(&estate, &measured);
    assert_eq!(
        page1.application.as_ref().unwrap().producer_pin.basis,
        wirk_atlas::QueryProducerBasis::ImplementationMeasured
    );
    let page2 = wirk_atlas::search(&estate.store, &second).unwrap();
    assert!(!page2.coverage.continuation_unrecoverable);
    assert!(!page2.hits.is_empty());
}

/// Q12. The same defect wearing the other shape: a backend that reports
/// every loaded module as *unreadable*. Its measured list is empty and
/// its unavailable list is long, which derived `partial` — the coverage
/// state the adjudication protects as a usable basis — over a record that
/// digested nothing at all.
///
/// Coverage is counted from what was measured, never from the length of
/// the list of what was not.
#[test]
fn q12_an_all_unreadable_module_list_is_not_partial_coverage() {
    let estate = selected_estate("q12");
    let backend = zero_module_query_backend(&estate.directory, "q12.py", "unreadable");
    let answer = wirk_atlas::search(
        &estate.store,
        &search_request(&estate, Some(backend.as_path())),
    )
    .unwrap();
    assert!(matches!(answer.semantic, SemanticStatus::Applied));
    let application = answer.application.as_ref().unwrap();
    let wirk_atlas::BackendEnvironment::Reported(environment) = &application.producer.environment
    else {
        panic!("expected a reported environment");
    };
    assert!(
        !environment.unmeasured_modules.is_empty(),
        "the fixture is supposed to name what it could not read"
    );
    assert!(
        matches!(
            environment.coverage,
            wirk_atlas::EnvironmentCoverage::Unmeasured
        ),
        "unreadable modules are not partial coverage of measured ones: {:?}",
        environment.coverage
    );
    assert_eq!(
        application.producer_pin.basis,
        wirk_atlas::QueryProducerBasis::ConfigurationOnly
    );
    let (_page1, second) = paged(&estate, &backend);
    let refused = wirk_atlas::search(&estate.store, &second).unwrap();
    assert!(
        refused_continuation(&refused).contains("measured no module at all"),
        "{:?}",
        refused_continuation(&refused)
    );
}

/// Q13. D1(b): what the measured sentence is allowed to say. The bound is
/// the *reported* scope — a backend can always omit something, and the
/// executed counterexample (a true, narrowed list that omits the ranker)
/// is a limit this product states rather than repairs.
#[test]
fn q13_the_measured_basis_statement_is_bounded_by_the_reported_scope() {
    let detail = wirk_atlas::QUERY_PRODUCER_BASIS_MEASURED;
    assert!(
        detail.contains("the module files this backend reported"),
        "the measured basis must name the reported scope: {detail}"
    );
    assert!(
        detail.contains("cannot be detected"),
        "the measured basis must state what it does not cover: {detail}"
    );
    assert!(
        !detail.contains("of the process that ranked this answer were measured"),
        "the measured basis must not claim the whole process: {detail}"
    );
    // And the scope statement the answer publishes still refuses
    // execution attestation, which no local argv boundary provides.
    assert!(wirk_atlas::QUERY_PRODUCER_SCOPE.contains("execution attestation"));
}

/// A `wirk-query/v1` backend that reports an environment whose module
/// list measures nothing: either empty (`empty`) or present-but-every-
/// entry-unreadable (`unreadable`). Both are real child processes.
fn zero_module_query_backend(directory: &Path, name: &str, shape: &str) -> PathBuf {
    let script = directory.join(name);
    fs::write(
        &script,
        format!(
            r#"#!/usr/bin/env python3
import hashlib, json, os, sys
SHAPE = {shape:?}

def absorb(d, part):
    d.update(len(part).to_bytes(8, "big"))
    d.update(part)

def model_digest(directory):
    files = []
    for root, _dirs, names in os.walk(directory, followlinks=True):
        for n in names:
            a = os.path.join(root, n)
            if os.path.isfile(a):
                files.append((os.path.relpath(a, directory).encode(), a))
    files.sort(key=lambda p: p[0])
    d = hashlib.sha256()
    absorb(d, b"wirk-model-directory/v1")
    absorb(d, len(files).to_bytes(8, "big"))
    for rel, a in files:
        absorb(d, rel)
        absorb(d, open(a, "rb").read())
    return d.hexdigest()

header = json.loads(sys.stdin.readline())
rows = [json.loads(line) for line in sys.stdin if line.strip()]
environment = {{
    "kind": "test-modules/v1",
    "root": os.path.dirname(sys.executable),
    "runtime": "test/1.0",
    "executable": sys.executable,
    "distributions": [],
    "modules": [],
}}
if SHAPE == "unreadable":
    environment["unmeasured_modules"] = [
        {{"name": "ranker", "reason": "loaded from a source this backend cannot read"}},
        {{"name": "ranker.fusion", "reason": "loaded from a source this backend cannot read"}},
    ]
print(json.dumps({{
    "protocol": "wirk-query/v1",
    "backend": "test-query/zero-modules-" + SHAPE,
    "native": "test-native/1.0",
    "model_path": header["model_path"],
    "model_digest": model_digest(header["model_path"]),
    "returned": len(rows),
    "environment": environment,
}}))
for rank, row in enumerate(rows, 1):
    print(json.dumps({{"row": row["row"], "score": 1.0 / rank, "rank": rank}}))
"#
        ),
    )
    .unwrap();
    executable(&script);
    script
}

/// A `wirk-query/v1` backend that reports an environment carrying no
/// module list at all: the `unmeasured` coverage state, as a real child
/// process rather than a constructed record.
fn nomodules_query_backend(directory: &Path, name: &str) -> PathBuf {
    let script = directory.join(name);
    fs::write(
        &script,
        r#"#!/usr/bin/env python3
import hashlib, json, os, sys

def absorb(d, part):
    d.update(len(part).to_bytes(8, "big"))
    d.update(part)

def model_digest(directory):
    files = []
    for root, _dirs, names in os.walk(directory, followlinks=True):
        for n in names:
            a = os.path.join(root, n)
            if os.path.isfile(a):
                files.append((os.path.relpath(a, directory).encode(), a))
    files.sort(key=lambda p: p[0])
    d = hashlib.sha256()
    absorb(d, b"wirk-model-directory/v1")
    absorb(d, len(files).to_bytes(8, "big"))
    for rel, a in files:
        absorb(d, rel)
        absorb(d, open(a, "rb").read())
    return d.hexdigest()

header = json.loads(sys.stdin.readline())
rows = [json.loads(line) for line in sys.stdin if line.strip()]
print(json.dumps({
    "protocol": "wirk-query/v1",
    "backend": "test-query/nomodules",
    "native": "test-native/1.0",
    "model_path": header["model_path"],
    "model_digest": model_digest(header["model_path"]),
    "returned": len(rows),
    "environment": {
        "kind": "test-modules/v1",
        "root": os.path.dirname(sys.executable),
        "runtime": "test/1.0",
        "executable": sys.executable,
        "distributions": [],
    },
}))
for rank, row in enumerate(rows, 1):
    print(json.dumps({"row": row["row"], "score": 1.0 / rank, "rank": rank}))
"#,
    )
    .unwrap();
    executable(&script);
    script
}

fn tree_digest(root: &Path) -> String {
    let mut entries: Vec<(String, String)> = Vec::new();
    fn walk(root: &Path, directory: &Path, out: &mut Vec<(String, String)>) {
        for entry in fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                out.push((
                    path.strip_prefix(root).unwrap().display().to_string(),
                    sha256(&fs::read(&path).unwrap()),
                ));
            }
        }
    }
    walk(root, root, &mut entries);
    entries.sort();
    sha256(format!("{entries:?}").as_bytes())
}
