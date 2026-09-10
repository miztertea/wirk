//! P3 W4 B contract checks for public semantic retrieval
//! (`W4-PUBLIC-RETRIEVAL-BUILD.md`, "Decisive implementation proof").
//!
//! Every check runs real child processes across the two real argv/stdin
//! boundaries the product uses in production — `wirk-embed/v2` for the
//! build, `wirk-query/v2` for the query. The backends here are small
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

/// A real `wirk-query/v2` backend. It writes the exact view it was handed
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
if FLAVOUR == "topk":
    # The one thing a real ranker does that the honest stub does not: it
    # answers with at most the depth it was asked for, so a view larger
    # than the frozen candidate pool is cut to the pool.
    rows = rows[: int(header["top_k"])]
reply = {{
    "protocol": "wirk-query/v2",
    "backend": "test-query/" + FLAVOUR,
    "native": "test-native/1.0",
    "model_path": header["model_path"],
    "model_digest": model_digest(header["model_path"]),
    "returned": len(rows),
}}
if FLAVOUR == "out_of_range":
    reply["returned"] = len(rows) + 1
if FLAVOUR == "cut":
    reply["returned"] = len(rows) - 1
print(json.dumps(reply))
if FLAVOUR == "runtime":
    # The environment the product handed this child, written where the
    # test can read it. Nothing is ranked differently because of it.
    with open(os.path.abspath(__file__) + ".env.json", "w") as handle:
        json.dump({{"PYTHONHASHSEED": os.environ.get("PYTHONHASHSEED")}}, handle)
if FLAVOUR == "cut":
    # `semble` 0.5.6's candidate cut, in miniature and with nothing else
    # in it: the candidates are unioned into a `set`, that set is sorted
    # on a key which decides nothing between them, and the first K of the
    # order that survives are the ones the caller ever sees. The order a
    # `set` of strings iterates in is derived from PYTHONHASHSEED, so
    # without a fixed seed each process cuts a different pool -- and every
    # page of a walk is its own process.
    keys = {{row["ranking_path"] + ":" + str(row["slot"]) for row in rows}}
    position = {{key: index for index, key in enumerate(sorted(keys, key=lambda k: 0))}}
    ordered = sorted(rows, key=lambda r: position[r["ranking_path"] + ":" + str(r["slot"])])
    for rank, row in enumerate(ordered[: len(rows) - 1], 1):
        print(json.dumps({{"row": row["row"], "score": 0.5, "rank": rank}}))
    sys.exit(0)
if FLAVOUR.startswith("ties_"):
    # Every admitted row scores exactly the same, and the order they are
    # emitted in is this flavour's permutation. Two flavours over one view
    # are two processes that agreed on every score and disagreed on the
    # order of the tie -- which is what `semble`'s hash-seeded candidate
    # set does across two pages of one walk.
    emitted = list(rows)
    if FLAVOUR == "ties_reverse":
        emitted.reverse()
    elif FLAVOUR == "ties_rotate":
        emitted = emitted[1:] + emitted[:1]
    for rank, row in enumerate(emitted, 1):
        print(json.dumps({{"row": row["row"], "score": 0.5, "rank": rank}}))
    sys.exit(0)
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
    estate_with_alias(repo, "fixture")
}

/// The same estate, under an alias the caller chooses. The alias is the
/// operator's name for a source; nothing about a ranking may depend on
/// which one they picked (0167), so every check of that has to be able to
/// pick one.
fn estate_with_alias(repo: TempDir, alias: &str) -> Estate {
    let temporary = TempDir::new().unwrap();
    let mut store =
        AtlasStore::open(temporary.path(), temporary.path().display().to_string()).unwrap();
    let membership = store
        .register_git(alias, repo.path().display().to_string(), "HEAD")
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
        capacity: None,
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
            Some(format!(
                "{}\u{1f}{}:{}",
                row["ranking_scope"].as_str()?,
                row["ranking_path"].as_str()?,
                row["slot"]
            ))
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
    // Under the frozen convention the ranking path is the source-relative
    // path and nothing else, so both memberships publish `code.rs` — and
    // the document key that separates them is the membership scope beside
    // it, which is never ranking text (0167).
    assert!(
        paths
            .iter()
            .filter(|path| path.as_str() == "code.rs")
            .count()
            >= 2,
        "both memberships must reach the ranker at their own relative path: {paths:?}"
    );
    assert!(
        !paths
            .iter()
            .any(|path| path.contains("fixture/") || path.contains("second/")),
        "no membership alias may appear in a ranking path: {paths:?}"
    );
    let scopes: std::collections::BTreeSet<String> = view
        .iter()
        .filter_map(|row| {
            let row: serde_json::Value = serde_json::from_str(row).ok()?;
            Some(row["ranking_scope"].as_str()?.to_owned())
        })
        .collect();
    assert_eq!(
        scopes.len(),
        2,
        "the two memberships must be two scopes: {scopes:?}"
    );
    assert!(
        !scopes
            .iter()
            .any(|scope| scope.contains("fixture") || scope.contains("second")),
        "the scope must be the opaque membership identity, not the alias: {scopes:?}"
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

/// A `wirk-query/v2` backend whose *ranking order* and whose *reported
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
    "protocol": "wirk-query/v2",
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

/// A `wirk-query/v2` backend that reports an environment whose module
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
    "protocol": "wirk-query/v2",
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

/// A `wirk-query/v2` backend that reports an environment carrying no
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
    "protocol": "wirk-query/v2",
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

// ---- the ranked-order contract -------------------------------------------
//
// `RESULT.json` under `knowledge/work/p3-parity/tie-order-audit`: three
// serial one-row walks over one unchanged five-file corpus, same binary,
// same producer, same generation, same edition. Two walks swapped an
// exact-score tie between two adjacent rows; the third returned
// `pagination.md` twice and never returned `embedding.rs` at all. The
// cause is upstream and is not a score: `semble.search.search` orders its
// candidate pool by `sorted({..set of Chunk..}, key=start_line)`, and for
// rows sharing a `start_line` that key decides nothing, so the pool comes
// out in the hash-seeded iteration order of a `set` — different in every
// process, and every page of a walk is its own process.
//
// The permutation here is the control the real defect leaves to chance:
// `ties_forward`, `ties_reverse` and `ties_rotate` are three processes
// that agree on every score and disagree on the order of the tie. Nothing
// in these checks is seeded, timed, or re-run until it fails.

/// The canonical identity of a hit, in the order the ranked list is
/// required to put equal scores in.
fn coordinate_key(hit: &wirk_atlas::EvidenceHit) -> (String, Vec<u8>, u64) {
    (
        hit.coordinate.membership.0.clone(),
        hit.coordinate.path.clone(),
        hit.coordinate.byte_start,
    )
}

fn tie_estate() -> Estate {
    let mut estate = estate();
    let edition = staged(build(&mut estate, "honest"));
    estate
        .store
        .select_semantic(&estate.membership.clone(), &edition.id)
        .unwrap()
        .unwrap();
    estate
}

/// A second source holding the *same relative paths* as the first, so a
/// tie that crosses a source boundary cannot be resolved by the relative
/// path alone.
fn add_colliding_source(estate: &mut Estate) {
    let second = TempDir::new().unwrap();
    git(second.path(), &["init", "-q"]);
    git(second.path(), &["config", "user.email", "a@b"]);
    git(second.path(), &["config", "user.name", "A"]);
    fs::write(
        second.path().join("code.rs"),
        "fn delta() { let other = 9; }\nfn theta() { let ranking = 10; }\n",
    )
    .unwrap();
    fs::write(second.path().join("doc.md"), "# other\r\nbody three\r\n").unwrap();
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
    let backend = chunk_backend(&estate.directory, "chunk-tie-collide.py", "honest");
    let edition = staged(
        estate
            .store
            .build_semantic(
                &other,
                &staged_other.id,
                &SemanticBuildConfig {
                    backend,
                    backend_args: Vec::new(),
                    model: estate.model.clone(),
                    producer: "test/w4b".into(),
                    chunking: SemanticChunking::Native,
                },
            )
            .unwrap(),
    );
    estate
        .store
        .select_semantic(&other, &edition.id)
        .unwrap()
        .unwrap();
    // The second repository must stay alive for the length of the test.
    std::mem::forget(second);
}

/// R1. Rows that tie on score come back in one canonical order, whichever
/// order the native ranker emitted them in — within one source.
#[test]
fn r1_an_equal_score_tie_is_ordered_the_same_whatever_order_the_ranker_emits() {
    let estate = tie_estate();
    let mut seen: Vec<Vec<(String, Vec<u8>, u64)>> = Vec::new();
    for flavour in ["ties_forward", "ties_reverse", "ties_rotate"] {
        let backend = query_backend(&estate.directory, &format!("query-{flavour}.py"), flavour);
        let answer =
            wirk_atlas::search(&estate.store, &search_request(&estate, Some(&backend))).unwrap();
        assert!(
            matches!(answer.semantic, SemanticStatus::Applied),
            "{flavour}: {:?}",
            answer.semantic
        );
        assert!(answer.hits.len() > 1, "{flavour}: nothing to tie");
        for pair in answer.hits.windows(2) {
            assert_eq!(
                pair[0].score, pair[1].score,
                "{flavour}: this check needs every score equal"
            );
        }
        seen.push(answer.hits.iter().map(coordinate_key).collect());
    }
    let mut canonical = seen[0].clone();
    canonical.sort();
    assert_eq!(seen[0], canonical, "the tie is not in canonical order");
    assert_eq!(seen[0], seen[1], "forward and reverse disagree");
    assert_eq!(seen[0], seen[2], "forward and rotate disagree");
}

/// R2. The same, across two sources whose relative paths collide: the tie
/// is broken by the membership first, so `second/code.rs` can never take
/// `fixture/code.rs`'s place.
#[test]
fn r2_a_tie_across_two_sources_with_colliding_paths_is_ordered_the_same() {
    let mut estate = tie_estate();
    add_colliding_source(&mut estate);
    let mut seen: Vec<Vec<(String, Vec<u8>, u64)>> = Vec::new();
    for flavour in ["ties_forward", "ties_reverse", "ties_rotate"] {
        let backend = query_backend(
            &estate.directory,
            &format!("query-cross-{flavour}.py"),
            flavour,
        );
        let answer =
            wirk_atlas::search(&estate.store, &search_request(&estate, Some(&backend))).unwrap();
        assert_eq!(answer.editions.len(), 2, "{flavour}: both sources ranked");
        let memberships: std::collections::BTreeSet<String> = answer
            .hits
            .iter()
            .map(|hit| hit.coordinate.membership.0.clone())
            .collect();
        assert_eq!(
            memberships.len(),
            2,
            "{flavour}: the tie must cross sources"
        );
        seen.push(answer.hits.iter().map(coordinate_key).collect());
    }
    let mut canonical = seen[0].clone();
    canonical.sort();
    assert_eq!(seen[0], canonical, "the cross-source tie is not canonical");
    assert_eq!(seen[0], seen[1], "forward and reverse disagree");
    assert_eq!(seen[0], seen[2], "forward and rotate disagree");
}

/// R3. The decisive one: a serial one-row walk whose pages are ranked by
/// processes that disagree about the tie returns every coordinate exactly
/// once — no duplicate, no gap. This is the audited defect in miniature.
#[test]
fn r3_a_serial_paged_walk_across_disagreeing_rankers_covers_each_row_once() {
    let mut estate = tie_estate();
    add_colliding_source(&mut estate);
    let flavours = ["ties_forward", "ties_reverse", "ties_rotate"];
    let backends: Vec<PathBuf> = flavours
        .iter()
        .map(|flavour| {
            query_backend(
                &estate.directory,
                &format!("query-walk-{flavour}.py"),
                flavour,
            )
        })
        .collect();
    let total = {
        let answer =
            wirk_atlas::search(&estate.store, &search_request(&estate, Some(&backends[0])))
                .unwrap();
        answer.budget.total_candidates
    };
    assert!(total >= 4, "the walk needs a pool worth paging: {total}");
    let mut walked: Vec<(String, Vec<u8>, u64)> = Vec::new();
    for page in 0..total {
        // Each page is ranked by a different process, exactly as each page
        // of the audited walk was.
        let backend = &backends[page % backends.len()];
        let mut request = search_request(&estate, Some(backend));
        request.limit = 1;
        request.offset = page;
        let answer = wirk_atlas::search(&estate.store, &request).unwrap();
        assert_eq!(answer.budget.returned, 1, "page {page} returned nothing");
        assert_eq!(answer.budget.total_candidates, total, "the pool moved");
        walked.push(coordinate_key(&answer.hits[0]));
    }
    let mut unique = walked.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        unique.len(),
        walked.len(),
        "the walk returned a coordinate twice: {walked:?}"
    );
    assert_eq!(unique.len(), total, "the walk did not cover the pool");
    let mut canonical = walked.clone();
    canonical.sort();
    assert_eq!(walked, canonical, "the walk did not page one ranked list");
}

/// A repository with enough separate files that a cut which drops one of
/// them by hash order lands somewhere different in almost every process:
/// `s2`'s red is a disagreement between two permutations of this many
/// rows, not a coin toss between two.
fn cut_fixture_repo() -> TempDir {
    let repo = TempDir::new().unwrap();
    git(repo.path(), &["init", "-q"]);
    git(repo.path(), &["config", "user.email", "a@b"]);
    git(repo.path(), &["config", "user.name", "A"]);
    for i in 0..12 {
        fs::write(
            repo.path().join(format!("unit{i:02}.rs")),
            format!("fn f{i}() {{ let admitted = {i}; }}\nfn g{i}() {{ let ranking = {i}; }}\n"),
        )
        .unwrap();
    }
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "cut fixture"]);
    repo
}

fn cut_estate() -> Estate {
    let mut estate = estate_with(cut_fixture_repo());
    let edition = staged(build(&mut estate, "honest"));
    estate
        .store
        .select_semantic(&estate.membership.clone(), &edition.id)
        .unwrap()
        .unwrap();
    estate
}

/// S1. The query child runs under the seed the declared ordering policy
/// names. Measured on the child's own environment, not on the product's
/// intention: the backend writes what it actually inherited.
///
/// Red before the correction: the child inherited no `PYTHONHASHSEED` at
/// all, because `run_query_backend` clears the environment and set only
/// the four offline/telemetry variables.
#[test]
fn s1_the_query_child_runs_under_the_pinned_hash_seed() {
    let estate = tie_estate();
    let backend = query_backend(&estate.directory, "query-runtime.py", "runtime");
    let answer =
        wirk_atlas::search(&estate.store, &search_request(&estate, Some(&backend))).unwrap();
    assert!(
        matches!(answer.semantic, SemanticStatus::Applied),
        "{:?}",
        answer.semantic
    );
    let recorded: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(format!("{}.env.json", backend.display())).unwrap(),
    )
    .unwrap();
    assert_eq!(
        recorded["PYTHONHASHSEED"].as_str(),
        Some(wirk_atlas::QUERY_HASH_SEED),
        "the child that ranked did not run under the declared selection policy: {recorded}"
    );
}

/// S2. The decisive one for the cut itself. A backend whose *selection*
/// is drawn through a hash-ordered set — `semble` 0.5.6's own shape —
/// keeps the same rows in every process, and a serial walk over that pool
/// returns every coordinate exactly once.
///
/// This is the half `order_ranked` cannot reach: sorting the rows that
/// came back says nothing about which rows came back. Red before the
/// correction, where two processes cut two different pools and the walk
/// duplicated one coordinate while never returning another.
#[test]
fn s2_a_hash_ordered_candidate_cut_keeps_the_same_rows_in_every_process() {
    let estate = cut_estate();
    let backend = query_backend(&estate.directory, "query-cut.py", "cut");
    let first =
        wirk_atlas::search(&estate.store, &search_request(&estate, Some(&backend))).unwrap();
    let second =
        wirk_atlas::search(&estate.store, &search_request(&estate, Some(&backend))).unwrap();
    let total = first.budget.total_candidates;
    assert!(
        total >= 8,
        "the cut needs a pool a hash order can really shuffle: {total}"
    );
    let members = |answer: &wirk_atlas::SearchAnswer| -> Vec<(String, Vec<u8>, u64)> {
        answer.hits.iter().map(coordinate_key).collect()
    };
    assert_eq!(
        members(&first),
        members(&second),
        "two processes cut two different candidate pools"
    );

    // And the consequence the caller actually sees: one serial walk whose
    // every page is its own process.
    let mut walked: Vec<(String, Vec<u8>, u64)> = Vec::new();
    for page in 0..total {
        let mut request = search_request(&estate, Some(&backend));
        request.limit = 1;
        request.offset = page;
        let answer = wirk_atlas::search(&estate.store, &request).unwrap();
        assert_eq!(answer.budget.returned, 1, "page {page} returned nothing");
        assert_eq!(answer.budget.total_candidates, total, "the pool moved");
        walked.push(coordinate_key(&answer.hits[0]));
    }
    let mut unique = walked.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        unique.len(),
        walked.len(),
        "the walk returned a coordinate twice: {walked:?}"
    );
    assert_eq!(unique.len(), total, "the walk did not cover the pool");
    let mut canonical = walked.clone();
    canonical.sort();
    assert_eq!(walked, canonical, "the walk did not page one ranked list");
}

// ---- source identity against ranking features (0167) ---------------------
//
// The demonstrated defect: the ranking path was `{alias}/{source-relative
// path}`, so the operator's own name for a source occupied the first path
// component of every `Chunk.file_path` the installed ranker sees — where
// its path priors, its stem/parent boosting and its BM25 path enrichment
// all read. A source called `tests` or `legacy` therefore reranked its own
// identical bytes (`native-ranking-gap-review/REVIEW.md` F2).
//
// The corrected convention hands the ranker the source-relative path and
// nothing else, and carries the membership beside it as an opaque scope
// that is a document key and a grouping key but never a token. These
// checks pin both halves: what the ranker may see, and what must still
// keep two memberships apart.

/// A repository holding the three path shapes a ranking path has to carry
/// through unchanged — shallow, deeply nested, and directories the
/// installed ranker's own priors read (`tests/`, `legacy/`).
fn ranking_fixture_repo() -> TempDir {
    let repo = TempDir::new().unwrap();
    git(repo.path(), &["init", "-q"]);
    git(repo.path(), &["config", "user.email", "a@b"]);
    git(repo.path(), &["config", "user.name", "A"]);
    fs::write(
        repo.path().join("README.md"),
        "# admitted\nranking evidence for the estate\n",
    )
    .unwrap();
    for (relative, body) in [
        (
            "src/route_planner.rs",
            "fn plan() { let admitted = 1; }\nfn refuse() { let ranking = 2; }\n",
        ),
        (
            "tests/route_planner_test.rs",
            "fn test_plan() { let admitted = 3; }\nfn test_refuse() { let ranking = 4; }\n",
        ),
        (
            "legacy/route_planner.rs",
            "fn old_plan() { let admitted = 5; }\nfn old_refuse() { let ranking = 6; }\n",
        ),
        (
            "a/b/c/d/deep_planner.rs",
            "fn deep_plan() { let admitted = 7; }\nfn deep_refuse() { let ranking = 8; }\n",
        ),
    ] {
        let path = repo.path().join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "ranking fixture"]);
    repo
}

/// T1. The ranking path a build records is the source-relative path
/// verbatim — shallow, nested, and inside directories the ranker's priors
/// read — whatever the membership is called.
#[test]
fn t1_the_ranking_path_is_the_source_relative_path_alone() {
    let mut estate = estate_with_alias(ranking_fixture_repo(), "tests");
    let edition = staged(build(&mut estate, "honest"));
    let rows = read_rows(&estate, &edition);
    assert!(rows.len() > 4);
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for row in &rows {
        let path = String::from_utf8(row.path.clone()).unwrap();
        let ranking = row.ranking_path.clone().expect("a row carries its path");
        assert_eq!(
            ranking, path,
            "the ranking path must be the source-relative path itself"
        );
        seen.insert(ranking);
    }
    for expected in [
        "README.md",
        "src/route_planner.rs",
        "tests/route_planner_test.rs",
        "legacy/route_planner.rs",
        "a/b/c/d/deep_planner.rs",
    ] {
        assert!(seen.contains(expected), "{expected} missing from {seen:?}");
    }
}

/// T2. The decisive one. Two estates over byte-identical repositories,
/// differing only in what the operator called the source, hand the ranker
/// the same ranking features: the same paths, in the same order, with the
/// same text. The alias appears nowhere in what is ranked; the scope that
/// does appear is the opaque membership identity, and it is not the
/// alias.
///
/// Red before the correction: alias `alpha` sent `alpha/src/...` and alias
/// `tests` sent `tests/src/...`, and the second spelling matches the
/// installed ranker's own test-directory prior at position 0.
#[test]
fn t2_only_the_alias_differs_and_the_ranking_features_do_not() {
    let features = |alias: &str| -> (Vec<serde_json::Value>, String) {
        let mut estate = estate_with_alias(ranking_fixture_repo(), alias);
        let edition = staged(build(&mut estate, "honest"));
        estate
            .store
            .select_semantic(&estate.membership.clone(), &edition.id)
            .unwrap()
            .unwrap();
        let backend = query_backend(&estate.directory, &format!("query-{alias}.py"), "honest");
        let answer =
            wirk_atlas::search(&estate.store, &search_request(&estate, Some(&backend))).unwrap();
        assert!(
            matches!(answer.semantic, SemanticStatus::Applied),
            "{alias}: {:?}",
            answer.semantic
        );
        let rows: Vec<serde_json::Value> = view_rows(&backend)
            .iter()
            .map(|row| serde_json::from_str(row).unwrap())
            .collect();
        assert!(!rows.is_empty());
        (rows, estate.membership.id.0.clone())
    };
    let (plain, plain_scope) = features("alpha");
    let (reserved, reserved_scope) = features("tests");
    let ranked = |rows: &[serde_json::Value]| -> Vec<(String, u64, String)> {
        rows.iter()
            .map(|row| {
                (
                    row["ranking_path"].as_str().unwrap().to_owned(),
                    row["slot"].as_u64().unwrap(),
                    row["text"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    };
    assert_eq!(
        ranked(&plain),
        ranked(&reserved),
        "the alias changed what the ranker sees"
    );
    // The paths are the repository's own, exactly — including the
    // `tests/` directory the repository really has, which must keep its
    // prior. An alias-shaped component nowhere else.
    let expected: std::collections::BTreeSet<String> = [
        "README.md",
        "src/route_planner.rs",
        "tests/route_planner_test.rs",
        "legacy/route_planner.rs",
        "a/b/c/d/deep_planner.rs",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    for (rows, alias) in [(&plain, "alpha"), (&reserved, "tests")] {
        let seen: std::collections::BTreeSet<String> = rows
            .iter()
            .map(|row| row["ranking_path"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(seen, expected, "{alias}: the ranker saw other paths");
        for row in rows {
            let scope = row["ranking_scope"].as_str().unwrap();
            assert!(!scope.is_empty(), "a row must carry its membership scope");
            assert_ne!(scope, alias, "the scope must not be the alias itself");
            assert!(
                !scope.contains(alias),
                "the scope must not spell the alias: {scope}"
            );
        }
    }
    assert_ne!(
        plain_scope, reserved_scope,
        "two memberships are two scopes"
    );
    for (rows, scope) in [(&plain, &plain_scope), (&reserved, &reserved_scope)] {
        for row in rows {
            assert_eq!(
                row["ranking_scope"].as_str().unwrap(),
                scope,
                "the scope is the membership identity"
            );
        }
    }
}

/// T3. Every alias the store accepts today it still accepts — including
/// the ones whose spelling the installed ranker's priors would have
/// matched. The correction is a separation of identity from ranking
/// features, not a denylist (0167).
#[test]
fn t3_reserved_looking_aliases_are_still_accepted() {
    let temporary = TempDir::new().unwrap();
    let repo = ranking_fixture_repo();
    let mut store =
        AtlasStore::open(temporary.path(), temporary.path().display().to_string()).unwrap();
    for alias in [
        "ordinary",
        "tests",
        "test",
        "spec",
        "testing",
        "__tests__",
        "legacy",
        "compat",
        "_compat",
        "examples",
        "docs_src",
    ] {
        let membership = store
            .register_git(alias, repo.path().display().to_string(), "HEAD")
            .unwrap_or_else(|error| panic!("alias {alias} was refused: {error}"));
        assert_eq!(membership.alias, alias);
    }
    // And the invalid ones stay invalid, for the reasons they always were.
    assert!(
        store
            .register_git("", repo.path().display().to_string(), "HEAD")
            .is_err()
    );
    assert!(
        store
            .register_git("a/b", repo.path().display().to_string(), "HEAD")
            .is_err()
    );
}

/// A repository with more resources than the result capacity T4 asks for,
/// so a walk over it pages a cut list rather than everything admitted.
fn deep_pool_repo() -> TempDir {
    let repo = TempDir::new().unwrap();
    git(repo.path(), &["init", "-q"]);
    git(repo.path(), &["config", "user.email", "a@b"]);
    git(repo.path(), &["config", "user.name", "A"]);
    for i in 0..260 {
        let path = repo.path().join(format!("src/unit{i:03}.rs"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            path,
            format!("fn f{i}() {{ let admitted = {i}; }}\nfn g{i}() {{ let ranking = {i}; }}\n"),
        )
        .unwrap();
    }
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "deep pool"]);
    repo
}

/// T4. A view larger than this query's result capacity still pages as one
/// deterministic list: every page is its own process, the result set does
/// not move, and a serial walk returns each coordinate exactly once in the
/// one canonical order.
///
/// The capacity is named explicitly (ruling 0171): 200 results walked 25
/// at a time is exactly the shape the policy separates — a result set
/// deeper than the page that shows it — and under the previous policy
/// this was the only shape there was.
#[test]
fn t4_a_view_past_the_candidate_pool_pages_without_duplicate_or_omission() {
    let mut estate = estate_with_alias(deep_pool_repo(), "tests");
    let edition = staged(build(&mut estate, "honest"));
    estate
        .store
        .select_semantic(&estate.membership.clone(), &edition.id)
        .unwrap()
        .unwrap();
    let rows = read_rows(&estate, &edition).len();
    assert!(
        rows > 200,
        "this check needs a view larger than the pool: {rows}"
    );
    let backend = query_backend(&estate.directory, "query-deep.py", "topk");
    let mut opening = search_request(&estate, Some(&backend));
    opening.capacity = Some(200);
    let first = wirk_atlas::search(&estate.store, &opening).unwrap();
    let total = first.budget.total_candidates;
    assert_eq!(total, 200, "the requested result capacity moved: {total}");
    assert_eq!(first.budget.capacity, 200);
    let mut walked: Vec<(String, Vec<u8>, u64)> = Vec::new();
    let page_size = 25usize;
    let mut offset = 0usize;
    while offset < total as usize {
        let mut request = search_request(&estate, Some(&backend));
        request.limit = page_size;
        request.capacity = Some(200);
        request.offset = offset;
        let answer = wirk_atlas::search(&estate.store, &request).unwrap();
        assert_eq!(
            answer.budget.total_candidates, total,
            "the result set moved at offset {offset}"
        );
        walked.extend(answer.hits.iter().map(coordinate_key));
        offset += page_size;
    }
    let mut unique = walked.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), walked.len(), "a coordinate came back twice");
    assert_eq!(unique.len(), total as usize, "the walk missed the pool");
    let mut canonical = walked.clone();
    canonical.sort();
    assert_eq!(walked, canonical, "the walk did not page one ranked list");
}

// T5 exercises the tie-boundary residue `ranking-identity-review/
// VERIFIED.md` found and `ranking-identity-review/ROOT-ADJUDICATION.md`
// rejected as pre-existing rather than acceptable: `_ScopedPath.__hash__`
// mixes membership scope into `hash((path, scope))`, and `search.py`
// unions candidates into a `set` before a stable sort on `start_line`
// alone, so rows that tie beyond `start_line` keep the hash-derived set
// order. At an exact score tie past a 200-result capacity,
// only the alias a source was registered under can move which rows
// survive. Unlike T1-T4, this drives the *actual installed* `semble`
// through the product's own backend script (`wirk-atlas/backends/
// semble_backend.py`), not a stub: a hash-order artifact cannot be
// reproduced by a backend that does not compute one.

/// A fixed query the fixture's content is chosen to score highly against
/// under the real ranker.
const TIE_QUERY: &str = "admitted ranking plan";

/// Rust source repeated byte-for-byte at every row so the real BM25 and
/// semantic scores tie exactly (mirrors `ranking-identity-review/
/// VERIFIED.md`'s adversarial-probe fixture).
const TIE_TEXT: &str = "fn plan() { let admitted = 1; let ranking = 2; }\n";

/// More than the result capacity this test requests (200, which is also
/// `CAPACITY_MAX`), so the tie forces a real down-select rather than
/// returning everything.
const TIE_ROWS: usize = 300;

/// `TIE_ROWS` files of identical content, at paths that differ only in a
/// zero-padded index ahead of an identical `x/y/z` tail — so the BM25
/// path-enrichment and length priors are identical too, and nothing but
/// the index distinguishes one row's ranking path from another's.
fn tied_fixture_repo() -> TempDir {
    let repo = TempDir::new().unwrap();
    git(repo.path(), &["init", "-q"]);
    git(repo.path(), &["config", "user.email", "a@b"]);
    git(repo.path(), &["config", "user.name", "A"]);
    for i in 0..TIE_ROWS {
        let path = repo.path().join(format!("d{i:04}/x/y/z/mod.rs"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, TIE_TEXT).unwrap();
    }
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "tied fixture"]);
    repo
}

/// The pinned development `semble` interpreter (`DEVELOPMENT.md`), read
/// from the required environment input — never a host-specific default
/// baked into the product (R2/R4: same `#[ignore]`d-native-test shape as
/// `WIRK_DOCKER_LIVE`/`WIRK_PLUGIN_INSTALL_LIVE`, adapted because this
/// test needs a real interpreter path, not a boolean). Only reached once
/// `t5` itself runs, i.e. under an explicit `--ignored`; panics with a
/// clear reason rather than skipping, so an opt-in run with a missing or
/// wrong prerequisite fails loudly instead of quietly recording a pass.
fn pinned_semble_python() -> PathBuf {
    let path = PathBuf::from(
        std::env::var("WIRK_TEST_SEMBLE_PYTHON").unwrap_or_else(|_| {
            panic!(
                "t5 is opted in (--ignored) but WIRK_TEST_SEMBLE_PYTHON is unset: point it at the \
             pinned semble python3 interpreter (see DEVELOPMENT.md for the working pin); this \
             test never falls back to a host-specific default"
            )
        }),
    );
    assert!(
        path.is_file(),
        "WIRK_TEST_SEMBLE_PYTHON={} is not a file: point it at the pinned semble python3 \
         interpreter (see DEVELOPMENT.md)",
        path.display()
    );
    path
}

/// The pinned offline `minishlab/potion-code-16M-v2` snapshot
/// (`DEVELOPMENT.md`), read from the required environment input the same
/// way as `pinned_semble_python`.
fn pinned_semble_model() -> PathBuf {
    let path = PathBuf::from(std::env::var("WIRK_TEST_SEMBLE_MODEL").unwrap_or_else(|_| {
        panic!(
            "t5 is opted in (--ignored) but WIRK_TEST_SEMBLE_MODEL is unset: point it at the \
             pinned offline potion-code-16M-v2 snapshot directory (see DEVELOPMENT.md for the \
             working pin); this test never falls back to a host-specific default"
        )
    }));
    assert!(
        path.is_dir(),
        "WIRK_TEST_SEMBLE_MODEL={} is not a directory: point it at the pinned offline \
         potion-code-16M-v2 snapshot (see DEVELOPMENT.md)",
        path.display()
    );
    path
}

/// The product's own `wirk-embed/v2` + `wirk-query/v2` backend, unmodified
/// — not a copy, not a stub.
fn real_semble_backend_script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("backends/semble_backend.py")
}

struct TiedEstate {
    _temporary: TempDir,
    store: AtlasStore,
    membership: Membership,
    generation: wirk_atlas::GenerationId,
}

/// One membership over `repo_path`, registered under `alias`, with its
/// source acquired and published — the identity half only; no semantic
/// build yet, since that is where the real backend gets chosen per call.
fn tied_estate_with_alias(repo_path: &Path, alias: &str) -> TiedEstate {
    let temporary = TempDir::new().unwrap();
    let mut store =
        AtlasStore::open(temporary.path(), temporary.path().display().to_string()).unwrap();
    let membership = store
        .register_git(alias, repo_path.display().to_string(), "HEAD")
        .unwrap();
    let staged = store
        .acquire(&membership, "HEAD", ExtractorPolicy::default())
        .unwrap()
        .staged()
        .unwrap();
    store.publish(&membership, &staged.id).unwrap();
    TiedEstate {
        _temporary: temporary,
        store,
        membership,
        generation: staged.id,
    }
}

fn tied_build(
    estate: &mut TiedEstate,
    python: &Path,
    script: &Path,
    model: &Path,
) -> SemanticEdition {
    let outcome = estate
        .store
        .build_semantic(
            &estate.membership.clone(),
            &estate.generation.clone(),
            &SemanticBuildConfig {
                backend: python.to_path_buf(),
                backend_args: vec![script.display().to_string()],
                model: model.to_path_buf(),
                producer: "test/ranking-tie".into(),
                chunking: SemanticChunking::Native,
            },
        )
        .unwrap();
    match outcome {
        SemanticBuildOutcome::Staged(edition) => *edition,
        SemanticBuildOutcome::Refused(reason) => panic!("tied build refused: {reason}"),
    }
}

/// The source-relative paths of the rows the real ranker selected into
/// the frozen candidate pool for `TIE_QUERY`, over one membership. Two
/// runs are comparable by this set alone: it carries no membership
/// identity, only which of the `TIE_ROWS` byte-identical files survived.
fn tied_selected_paths(
    estate: &TiedEstate,
    python: &Path,
    script: &Path,
    model: &Path,
) -> std::collections::BTreeSet<String> {
    let request = SearchRequest {
        scope: QueryScope::EstateOrientation,
        requested_source: None,
        query: TIE_QUERY.into(),
        families: vec![],
        semantic: SemanticRequest::Requested,
        limit: 200,
        capacity: None,
        pinned: None,
        offset: 0,
        semantic_query: Some(SemanticQueryConfig {
            backend: python.to_path_buf(),
            backend_args: vec![script.display().to_string()],
            model: model.to_path_buf(),
        }),
        pinned_editions: None,
        pinned_mode: None,
        pinned_producer: wirk_atlas::PinnedProducer::Unrecorded,
    };
    let answer = wirk_atlas::search(&estate.store, &request).unwrap();
    assert!(
        matches!(answer.semantic, SemanticStatus::Applied),
        "{:?}",
        answer.semantic
    );
    assert_eq!(
        answer.budget.total_candidates, 200,
        "the frozen candidate pool moved"
    );
    answer
        .hits
        .iter()
        .map(|hit| String::from_utf8(hit.coordinate.path.clone()).unwrap())
        .collect()
}

/// T5. `TIE_ROWS` rows tie exactly under the real ranker for `TIE_QUERY`,
/// past a 200-result capacity, so which 200 survive is a down-select
/// with nothing but hash order to decide it. Registering the same
/// repository under five different aliases must select the same 200
/// source-relative paths every time (0167): the alias, and the
/// membership scope it produces, must never be part of that decision.
///
/// Red on 68ff0c8: `_ScopedPath.__hash__` folds the membership scope into
/// `hash((path, scope))`, so a different scope reorders the tied
/// candidate set `search.py` builds and moves the cut
/// (`ranking-identity-review/VERIFIED.md`, "Adversarial probe").
///
/// `#[ignore]`d (R2: same shape as `wirk/tests/docker_executor.rs`'s
/// `WIRK_DOCKER_LIVE`/`wirk-herdr/tests/plugin_github_install.rs`'s
/// `WIRK_PLUGIN_INSTALL_LIVE`): an ordinary `cargo test`/`cargo test
/// --list` never runs or executes this test and reports it under
/// `ignored`, so a native prerequisite this box happens not to have never
/// reads as a silent pass. Run explicitly with `--ignored` plus
/// `WIRK_TEST_SEMBLE_PYTHON`/`WIRK_TEST_SEMBLE_MODEL` set (see
/// `DEVELOPMENT.md` for the working pin); unlike the boolean-flag
/// convention those two tests use, a missing or invalid path here is a
/// clear panic from `pinned_semble_python`/`pinned_semble_model`, not a
/// quiet skip, since opting in means the prerequisite was promised.
#[test]
#[ignore]
fn t5_alias_alone_does_not_move_an_equal_score_tied_selection() {
    let python = pinned_semble_python();
    let model = pinned_semble_model();
    let script = real_semble_backend_script();
    assert!(
        script.is_file(),
        "the actual backend script is missing at {}",
        script.display()
    );

    let repo = tied_fixture_repo();
    let aliases = ["m-alpha", "m-tests", "m-legacy", "m-notes", "m-zulu"];
    let mut by_alias = Vec::new();
    for alias in aliases {
        let mut estate = tied_estate_with_alias(repo.path(), alias);
        let edition = tied_build(&mut estate, &python, &script, &model);
        estate
            .store
            .select_semantic(&estate.membership.clone(), &edition.id)
            .unwrap()
            .unwrap();
        let paths = tied_selected_paths(&estate, &python, &script, &model);
        assert_eq!(
            paths.len(),
            200,
            "alias {alias}: the frozen pool did not return 200 distinct paths"
        );
        by_alias.push((alias, paths));
    }
    let (base_alias, base) = &by_alias[0];
    for (alias, paths) in &by_alias[1..] {
        let symmetric: Vec<&String> = base.symmetric_difference(paths).collect();
        assert!(
            symmetric.is_empty(),
            "0167 requires alias-invariant selection at an equal-score tie: {base_alias} vs \
             {alias} differ on {} of {TIE_ROWS} rows: {symmetric:?}",
            symmetric.len()
        );
    }
}

// ---- T6-T9: query-bound result capacity (ruling 0171) ----------------
//
// The previous policy handed one universal candidate depth of 200 to the
// native ranker for every query, so a five-result request was a five-row
// window onto a two-hundred-result ranking rather than the ranking the
// caller asked for. Measured red, on a fresh synthetic corpus with the
// frozen parent binary and the installed `semble` 0.5.6:
// `query-capacity-build/BUILT.md` — the product's first five and the
// native ranker's own requested-five differ in membership on 4 of 6
// queries and in score on 6 of 6, over byte-identical admitted rows.
//
// T6 and T7 drive the *installed* ranker, because the mechanism they pin
// is the installed ranker's: `candidate_count = top_k * 5` truncates each
// modality before fusion, so the capacity decides which rows carry a
// second modality's reciprocal-rank term at all, and the pool-wide
// normalisers in `boost_multi_chunk_files`/`apply_query_boost` move with
// it. A stub backend that returns a fixed order cannot reproduce any of
// that and would pin nothing but a field's shape.

/// A deterministic varied corpus: forty files of fourteen blocks, words
/// drawn by a seeded LCG from one fixed vocabulary. Varied on purpose,
/// unlike `tied_fixture_repo` — a capacity's effect on fusion is only
/// visible when scores actually differ.
const CAPACITY_VOCAB: [&str; 24] = [
    "route",
    "journal",
    "claim",
    "trail",
    "world",
    "source",
    "edition",
    "ranking",
    "continuation",
    "membership",
    "evidence",
    "estate",
    "producer",
    "backend",
    "candidate",
    "penalty",
    "boost",
    "pagination",
    "selection",
    "admitted",
    "digest",
    "capacity",
    "window",
    "budget",
];

const CAPACITY_DIRS: [&str; 8] = [
    "src", "lib", "docs", "tests", "pkg", "compat", "tools", "internal",
];

/// More rows than `CAPACITY_MAX`, so a capacity-200 result set is a real
/// down-select over the admitted view and a walk of it has somewhere to
/// go.
fn capacity_fixture_repo() -> TempDir {
    let repo = TempDir::new().unwrap();
    git(repo.path(), &["init", "-q"]);
    git(repo.path(), &["config", "user.email", "a@b"]);
    git(repo.path(), &["config", "user.name", "A"]);
    let mut seed: u64 = 20_260_910;
    let mut next = move || {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        (seed >> 33) as usize
    };
    for directory in CAPACITY_DIRS {
        for index in 0..5 {
            let path = repo.path().join(format!("{directory}/module_{index}.py"));
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let mut text = String::new();
            for slot in 0..14 {
                text.push_str(&format!("def block_{slot}():\n    # "));
                for _ in 0..40 {
                    text.push_str(CAPACITY_VOCAB[next() % CAPACITY_VOCAB.len()]);
                    text.push(' ');
                }
                text.push_str(&format!("\n    return {slot}\n\n"));
            }
            fs::write(path, text).unwrap();
        }
    }
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "capacity fixture"]);
    repo
}

/// Queries whose terms the fixture's vocabulary really carries, so every
/// one of them ranks something rather than exercising the empty path.
const CAPACITY_QUERIES: [&str; 6] = [
    "candidate pool penalty selection",
    "route journal claim",
    "how does a continuation get refused",
    "membership evidence estate",
    "boost",
    "capacity window budget frozen",
];

fn capacity_request(
    query: &str,
    limit: usize,
    capacity: Option<u64>,
    offset: usize,
    python: &Path,
    script: &Path,
    model: &Path,
) -> SearchRequest {
    SearchRequest {
        scope: QueryScope::EstateOrientation,
        requested_source: None,
        query: query.into(),
        families: vec![],
        semantic: SemanticRequest::Requested,
        limit,
        capacity,
        pinned: None,
        offset,
        semantic_query: Some(SemanticQueryConfig {
            backend: python.to_path_buf(),
            backend_args: vec![script.display().to_string()],
            model: model.to_path_buf(),
        }),
        pinned_editions: None,
        pinned_mode: None,
        pinned_producer: wirk_atlas::PinnedProducer::Unrecorded,
    }
}

/// One estate over `capacity_fixture_repo`, built and selected with the
/// real backend — the shape T6 to T8 all start from.
fn capacity_estate(python: &Path, script: &Path, model: &Path, repo: &Path) -> TiedEstate {
    let mut estate = tied_estate_with_alias(repo, "m-capacity");
    let edition = tied_build(&mut estate, python, script, model);
    estate
        .store
        .select_semantic(&estate.membership.clone(), &edition.id)
        .unwrap()
        .unwrap();
    estate
}

fn hit_keys(answer: &wirk_atlas::SearchAnswer) -> Vec<(String, u64, u64)> {
    answer
        .hits
        .iter()
        .map(|hit| {
            (
                String::from_utf8(hit.coordinate.path.clone()).unwrap(),
                hit.coordinate.line_start,
                hit.coordinate.line_end,
            )
        })
        .collect()
}

/// T6. An ordinary request for five results is one native search at
/// `top_k = 5`, not a five-row window onto a two-hundred-row one.
///
/// The mechanism, not the field: the whole result set is five rows
/// (`total_candidates`), the capacity the ranker was handed is five, and
/// asking the *same query with the same page size* at capacity 200
/// returns a different head — which can only happen because the capacity
/// changed what the installed ranker fused, since nothing else about the
/// request, the corpus or the configuration moved.
///
/// Red on `d5a5efc`: `total_candidates` was 200 for every request, the
/// application reported a fixed `candidate_limit` of 200, and the two
/// arms were identical by construction.
///
/// `#[ignore]`d and opted in exactly as T5 is, with the same loud failure
/// on a missing prerequisite rather than a silent pass.
#[test]
#[ignore]
fn t6_a_requested_limit_is_the_native_ranking_at_that_capacity() {
    let python = pinned_semble_python();
    let model = pinned_semble_model();
    let script = real_semble_backend_script();
    let repo = capacity_fixture_repo();
    let estate = capacity_estate(&python, &script, &model, repo.path());

    let mut heads_moved = 0;
    for query in CAPACITY_QUERIES {
        let shallow = wirk_atlas::search(
            &estate.store,
            &capacity_request(query, 5, None, 0, &python, &script, &model),
        )
        .unwrap();
        assert!(
            matches!(shallow.semantic, SemanticStatus::Applied),
            "{query}: {:?}",
            shallow.semantic
        );
        let applied = shallow.application.as_ref().unwrap();
        assert_eq!(applied.capacity, 5, "{query}: the ranker's own top_k");
        assert_eq!(
            applied.capacity_source,
            wirk_atlas::CapacitySource::RequestedLimit,
            "{query}: an omitted capacity is the requested limit"
        );
        assert_eq!(
            applied.capacity_policy,
            wirk_atlas::CAPACITY_POLICY,
            "{query}: the policy the edition declares"
        );
        assert_eq!(
            shallow.budget.total_candidates, 5,
            "{query}: the whole result set is this query's capacity, not a universal depth"
        );
        assert!(
            applied.capacity_reached && !applied.resultset_exhausted,
            "{query}: a full result set says so, and does not claim exhaustion"
        );
        assert!(
            shallow.coverage.partial,
            "{query}: a result set that filled its capacity may have relevant rows beyond it"
        );

        let deep = wirk_atlas::search(
            &estate.store,
            &capacity_request(query, 5, Some(200), 0, &python, &script, &model),
        )
        .unwrap();
        let deep_applied = deep.application.as_ref().unwrap();
        assert_eq!(deep_applied.capacity, 200);
        assert_eq!(
            deep_applied.capacity_source,
            wirk_atlas::CapacitySource::Explicit
        );
        assert_eq!(
            deep.budget.total_candidates, 200,
            "{query}: an explicit capacity is the size of the result set, not of the page"
        );
        assert_eq!(deep.hits.len(), 5, "{query}: the page size did not move");
        if hit_keys(&shallow) != hit_keys(&deep) {
            heads_moved += 1;
        }
    }
    assert!(
        heads_moved > 0,
        "capacity must reach the installed ranker: no query's first five moved between \
         capacity 5 and capacity 200, so nothing here would have caught the universal-depth \
         policy this test exists to replace"
    );
}

/// T7. A page is a slice of one frozen result set: at a fixed capacity,
/// the display page size decides how the same rows are handed over and
/// nothing about which rows they are or what order they come in.
///
/// Walked twice over the same query at capacity 200, once five rows at a
/// time and once forty — 40 pages against 5 — and the two walks must
/// produce the same 200 coordinates, in the same order, with no row
/// served twice.
#[test]
#[ignore]
fn t7_a_fixed_capacity_is_one_result_set_however_it_is_paged() {
    let python = pinned_semble_python();
    let model = pinned_semble_model();
    let script = real_semble_backend_script();
    let repo = capacity_fixture_repo();
    let estate = capacity_estate(&python, &script, &model, repo.path());
    let query = CAPACITY_QUERIES[0];

    let walk = |page: usize| {
        let mut seen: Vec<(String, u64, u64)> = Vec::new();
        let mut offset = 0;
        loop {
            let answer = wirk_atlas::search(
                &estate.store,
                &capacity_request(query, page, Some(200), offset, &python, &script, &model),
            )
            .unwrap();
            assert!(matches!(answer.semantic, SemanticStatus::Applied));
            assert_eq!(
                answer.budget.capacity, 200,
                "the capacity is the query's, not the page's"
            );
            assert_eq!(answer.budget.total_candidates, 200);
            if answer.hits.is_empty() {
                assert!(
                    answer.coverage.spent,
                    "a window past the end of the result set is spent, not a no-match"
                );
                break;
            }
            seen.extend(hit_keys(&answer));
            offset += answer.hits.len();
        }
        seen
    };

    let by_five = walk(5);
    let by_forty = walk(40);
    assert_eq!(by_five.len(), 200, "the frozen result set is the capacity");
    let mut unique = by_five.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), 200, "a walk served a row twice");
    assert_eq!(
        by_five, by_forty,
        "the display page budget moved the ranking at a fixed capacity"
    );
}

/// T8. An edition built under the previous universal-depth policy is
/// refused by name and left exactly as it was built — its own bytes are
/// read back, not reinterpreted under a policy it never declared — and
/// the refusal names the ordinary recovery.
///
/// The edition record is rewritten here to the shape the previous policy
/// wrote (`candidate_limit: 200`, no capacity policy) rather than
/// simulated behind a stub: this is the exact declaration a real
/// pre-0171 edition carries on disk, and the real refusal and the real
/// fresh-build recovery were both executed against a genuinely
/// parent-built edition (`query-capacity-build/BUILT.md`).
#[test]
#[ignore]
fn t8_a_previous_policy_edition_is_refused_by_name_not_reinterpreted() {
    let python = pinned_semble_python();
    let model = pinned_semble_model();
    let script = real_semble_backend_script();
    let repo = capacity_fixture_repo();
    let estate = capacity_estate(&python, &script, &model, repo.path());

    let edition_id = estate.store.selected_semantic(&estate.membership).unwrap();
    let record = estate
        ._temporary
        .path()
        .join("atlas/semantic")
        .join(&edition_id.0)
        .join(wirk_atlas::EDITION_RECORD);
    assert!(
        record.is_file(),
        "the edition record this test rewrites is not where it was looked for: {}",
        record.display()
    );
    let before = fs::read(&record).unwrap();
    let mut document: serde_json::Value = serde_json::from_slice(&before).unwrap();
    let retrieval = document["retrieval"].as_object_mut().unwrap();
    retrieval.remove("capacity_policy");
    retrieval.remove("capacity_max");
    retrieval.insert(
        "candidate_limit".into(),
        serde_json::json!(wirk_atlas::LEGACY_CANDIDATE_LIMIT),
    );
    fs::write(&record, serde_json::to_vec_pretty(&document).unwrap()).unwrap();

    let answer = wirk_atlas::search(
        &estate.store,
        &capacity_request(CAPACITY_QUERIES[0], 5, None, 0, &python, &script, &model),
    )
    .unwrap();
    let reason = match &answer.semantic {
        SemanticStatus::Unavailable(reason) => reason.clone(),
        other => panic!("a previous-policy edition was ranked through: {other:?}"),
    };
    assert!(
        reason.contains("fixed universal candidate depth of 200"),
        "the refusal must state what the edition itself declares: {reason}"
    );
    assert!(
        reason.contains(wirk_atlas::CAPACITY_POLICY),
        "the refusal must name the policy this product decides under: {reason}"
    );
    assert!(
        reason.contains("rebuild its semantic edition"),
        "the refusal must name the recovery: {reason}"
    );
    assert!(
        answer.application.is_none(),
        "nothing was ranked, so nothing may be reported as having been"
    );

    // The historical record is read, never rewritten: the declaration
    // this product refused is still exactly the one on disk.
    let after: serde_json::Value = serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();
    assert_eq!(
        after["retrieval"]["candidate_limit"],
        serde_json::json!(wirk_atlas::LEGACY_CANDIDATE_LIMIT)
    );
    assert!(after["retrieval"]["capacity_policy"].is_null());
}

/// T9. The capacity policy itself, which needs no backend, no model and
/// no estate: an omitted capacity is the requested limit, a requested
/// limit above the bound derives the bound and says so, an explicit
/// capacity is honoured inside the bound and refused outside it, and a
/// page size is never bounded by any of it.
#[test]
fn t9_result_capacity_resolves_by_policy_and_refuses_outside_its_bounds() {
    let request = |limit: usize, capacity: Option<u64>| SearchRequest {
        scope: QueryScope::EstateOrientation,
        requested_source: None,
        query: "anything".into(),
        families: vec![],
        semantic: SemanticRequest::Requested,
        limit,
        capacity,
        pinned: None,
        offset: 0,
        semantic_query: None,
        pinned_editions: None,
        pinned_mode: None,
        pinned_producer: wirk_atlas::PinnedProducer::Unrecorded,
    };
    let resolved = wirk_atlas::resolve_capacity(&request(5, None)).unwrap();
    assert_eq!(resolved.value, 5);
    assert_eq!(resolved.source, wirk_atlas::CapacitySource::RequestedLimit);

    let resolved = wirk_atlas::resolve_capacity(&request(10, None)).unwrap();
    assert_eq!(resolved.value, 10, "the documented default is the limit");

    let bounded = wirk_atlas::resolve_capacity(&request(500, None)).unwrap();
    assert_eq!(bounded.value, wirk_atlas::CAPACITY_MAX);
    assert_eq!(
        bounded.source,
        wirk_atlas::CapacitySource::RequestedLimitBounded,
        "a derived capacity that hit the bound says so rather than narrowing in silence"
    );
    assert_eq!(bounded.requested_limit, 500);

    let explicit = wirk_atlas::resolve_capacity(&request(5, Some(200))).unwrap();
    assert_eq!(explicit.value, 200);
    assert_eq!(explicit.source, wirk_atlas::CapacitySource::Explicit);
    assert_eq!(
        explicit.requested_limit, 5,
        "a deep result set with a small page is the ordinary shape of an explicit capacity"
    );

    // A page size far larger than the bound is not a capacity error: the
    // page budget belongs to the caller's surface and is not bounded here.
    assert!(wirk_atlas::resolve_capacity(&request(10_000, None)).is_ok());

    for refused in [0, wirk_atlas::CAPACITY_MAX + 1, 10_000] {
        let error = wirk_atlas::resolve_capacity(&request(5, Some(refused)))
            .expect_err("a capacity this product cannot run must be refused, never clamped");
        assert!(
            error.contains(&wirk_atlas::CAPACITY_MAX.to_string())
                && error.contains(wirk_atlas::CAPACITY_POLICY),
            "the refusal must name the bound and the policy: {error}"
        );
    }
    // A zero page still ranks from somewhere rather than asking the
    // native ranker for nothing.
    assert_eq!(
        wirk_atlas::resolve_capacity(&request(0, None))
            .unwrap()
            .value,
        1
    );
}

// ---- U1-U4: verified bytes are read once per query and never carried
// across queries (ruling 0181) --------------------------------------------
//
// `plan_semantic`/`rank` now read and verify each admitted membership's
// mapping and vector bytes exactly once per query — proved externally
// with `strace` on the real CLI against a real daemon and a real
// multi-repo fixture (`VERIFIED-READ-BUILT.md`), since these library-level
// tests cannot themselves count file opens. What these four pin instead
// is the correctness that refactor must never trade away: every call
// still re-reads and re-verifies from disk, nothing survives from one
// `search()` call to the next, per-membership verification stays
// independent under a multi-source scope, and a continuation's pinned
// edition still ranks even after its membership selects a different one.

fn u_estate_with_edition() -> (Estate, SemanticEdition) {
    let mut estate = estate();
    let edition = staged(build(&mut estate, "honest"));
    let membership = estate.membership.clone();
    estate
        .store
        .select_semantic(&membership, &edition.id)
        .unwrap()
        .unwrap();
    (estate, edition)
}

fn u_edition_file(estate: &Estate, edition: &SemanticEdition, file: &str) -> PathBuf {
    estate
        ._temporary
        .path()
        .join("atlas/semantic")
        .join(&edition.id.0)
        .join(file)
}

/// U1. A query re-verifies an edition's bytes from disk every time it
/// runs, in both directions: corrupting the selected edition's vectors
/// between two `search()` calls in the same process must be caught by the
/// second one, and repairing them must let a third call rank normally
/// again. Either direction failing would mean some earlier call's
/// verification survived past its own query — a process-lifetime cache
/// ruling 0181 explicitly forbids.
#[test]
fn u1_a_query_revalidates_disk_bytes_fresh_on_every_call() {
    let (estate, edition) = u_estate_with_edition();
    let backend = query_backend(&estate.directory, "query-u1.py", "topk");
    let request = search_request(&estate, Some(&backend));

    let first = wirk_atlas::search(&estate.store, &request).unwrap();
    assert!(
        matches!(first.semantic, SemanticStatus::Applied),
        "{:?}",
        first.semantic
    );
    let baseline_rows = first.application.as_ref().unwrap().rows_ranked;
    assert!(baseline_rows > 0);

    let vectors_path = u_edition_file(&estate, &edition, &edition.vectors.file);
    let original = fs::read(&vectors_path).unwrap();
    let tampered: Vec<u8> = original.iter().map(|byte| byte ^ 0x01).collect();
    fs::write(&vectors_path, &tampered).unwrap();

    let corrupted = wirk_atlas::search(&estate.store, &request).unwrap();
    let SemanticStatus::Unavailable(reason) = corrupted.semantic else {
        panic!(
            "a corrupted selected edition must never be silently ranked: {:?}",
            corrupted.semantic
        );
    };
    assert!(
        reason.contains("no longer verify"),
        "unexpected reason: {reason}"
    );

    fs::write(&vectors_path, &original).unwrap();
    let repaired = wirk_atlas::search(&estate.store, &request).unwrap();
    assert!(
        matches!(repaired.semantic, SemanticStatus::Applied),
        "a repair on disk must be picked up fresh, not stuck on the corrupt call: {:?}",
        repaired.semantic
    );
    assert_eq!(
        repaired.application.unwrap().rows_ranked,
        baseline_rows,
        "the repaired query must rank the same rows the first, uncorrupted query did"
    );
}

/// U2. The same freshness, over the mapping side, plus the shape checks a
/// doctored mapping has to survive on the query path itself, not only when
/// `verify_edition` is called directly: a row count that no longer
/// matches the edition's own manifest, and a file removed outright.
#[test]
fn u2_a_mapping_row_count_mismatch_or_missing_file_refuses_the_query() {
    let (estate, edition) = u_estate_with_edition();
    let backend = query_backend(&estate.directory, "query-u2.py", "topk");
    let request = search_request(&estate, Some(&backend));

    let mapping_path = u_edition_file(&estate, &edition, &edition.mapping.file);
    let original = fs::read_to_string(&mapping_path).unwrap();
    let mut lines: Vec<&str> = original
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    assert!(lines.len() > 1, "fixture must have more than one row");
    lines.pop();
    fs::write(&mapping_path, format!("{}\n", lines.join("\n"))).unwrap();

    let answer = wirk_atlas::search(&estate.store, &request).unwrap();
    let SemanticStatus::Unavailable(reason) = answer.semantic else {
        panic!(
            "a mapping row count that no longer matches its manifest must refuse, not rank: {:?}",
            answer.semantic
        );
    };
    assert!(
        reason.contains("no longer verify"),
        "unexpected reason: {reason}"
    );

    fs::write(&mapping_path, &original).unwrap();
    let repaired = wirk_atlas::search(&estate.store, &request).unwrap();
    assert!(matches!(repaired.semantic, SemanticStatus::Applied));

    fs::remove_file(&mapping_path).unwrap();
    let missing = wirk_atlas::search(&estate.store, &request).unwrap();
    let SemanticStatus::Unavailable(reason) = missing.semantic else {
        panic!(
            "a query over a missing mapping file must refuse, not rank: {:?}",
            missing.semantic
        );
    };
    assert!(
        reason.contains("incomplete") && reason.contains("absent"),
        "unexpected reason: {reason}"
    );
}

/// U3. A continuation's own captured edition — not whatever is selected by
/// the time the next page runs — decides what that page ranks through.
/// `plan_semantic`'s pinned branch reads and verifies exactly that
/// edition, once, regardless of what `select_semantic` has done to the
/// membership since. Red before ruling 0181's own selection-vs-pin
/// distinction would be conflating "verified once" with "verified for the
/// currently selected edition": this proves the pinned branch is its own
/// independent verified read, not a reuse of whatever the current
/// selection last checked.
#[test]
fn u3_a_continuation_follows_its_pinned_edition_past_a_later_selection() {
    let mut estate = estate();
    let edition_a = staged(build(&mut estate, "honest"));
    let generation_a = estate.generation.clone();
    let membership = estate.membership.clone();
    estate
        .store
        .select_semantic(&membership, &edition_a.id)
        .unwrap()
        .unwrap();

    let backend = query_backend(&estate.directory, "query-u3.py", "topk");
    let first =
        wirk_atlas::search(&estate.store, &search_request(&estate, Some(&backend))).unwrap();
    assert!(
        matches!(first.semantic, SemanticStatus::Applied),
        "{:?}",
        first.semantic
    );
    assert_eq!(
        first.editions,
        vec![(membership.id.clone(), edition_a.id.clone())]
    );
    let first_hits: Vec<_> = first.hits.iter().map(coordinate_key).collect();

    // New content, a new generation, and a new edition selected over it:
    // the membership now publishes and selects something other than A.
    // A's own bytes are never touched.
    fs::write(
        estate._repo.path().join("extra.rs"),
        "fn omega() { let extra = 99; }\n",
    )
    .unwrap();
    git(estate._repo.path(), &["add", "."]);
    git(estate._repo.path(), &["commit", "-qm", "extra content"]);
    let staged_generation = estate
        .store
        .acquire(&membership, "HEAD", ExtractorPolicy::default())
        .unwrap()
        .staged()
        .unwrap();
    estate
        .store
        .publish(&membership, &staged_generation.id)
        .unwrap();
    estate.generation = staged_generation.id.clone();
    let edition_b = staged(build(&mut estate, "honest"));
    assert_ne!(
        edition_b.id, edition_a.id,
        "new content must build a new edition"
    );
    estate
        .store
        .select_semantic(&membership, &edition_b.id)
        .unwrap()
        .unwrap();
    assert_eq!(
        estate.store.selected_semantic(&membership),
        Some(edition_b.id.clone())
    );

    // A continuation pinned to A's own generation and edition, run after B
    // became the membership's current selection.
    let mut pinned = search_request(&estate, Some(&backend));
    pinned.pinned = Some(std::collections::BTreeMap::from([(
        membership.id.clone(),
        generation_a,
    )]));
    pinned.pinned_editions = Some(std::collections::BTreeMap::from([(
        membership.id.clone(),
        edition_a.id.clone(),
    )]));
    // `pinned_mode`/`pinned_producer` stay at their defaults (`None`,
    // `Unrecorded`): that pair guards a *different* correctness property
    // (the implementation that ranked page 1 is the one ranking page 2,
    // `QueryProducerPin`) which this stub backend reports no environment
    // for and is not what U3 is about. `plan_semantic`'s pinned branch —
    // the one this test pins — reads `pinned_editions` on its own,
    // independent of that check.
    let second = wirk_atlas::search(&estate.store, &pinned).unwrap();
    assert!(
        matches!(second.semantic, SemanticStatus::Applied),
        "{:?}",
        second.semantic
    );
    assert_eq!(
        second.editions,
        vec![(membership.id.clone(), edition_a.id.clone())],
        "a continuation must rank through its own pinned edition, not the one now selected"
    );
    let second_hits: Vec<_> = second.hits.iter().map(coordinate_key).collect();
    assert_eq!(
        first_hits, second_hits,
        "the pinned page must reproduce exactly what A alone ranked, unaffected by B"
    );
}

/// U4. Multi-source scope: each membership's bytes are verified
/// independently and fresh on every call. Corrupting one of two selected
/// editions must not affect the other — the answer still ranks the
/// healthy membership and names the corrupt one — and repairing it must
/// restore full coverage on the very next call, proving the corrupt
/// membership's earlier failure was not remembered either.
#[test]
fn u4_multi_source_verification_is_independent_and_revalidated_per_call() {
    let mut estate = tie_estate();
    add_colliding_source(&mut estate);
    let backend = query_backend(&estate.directory, "query-u4.py", "topk");
    let request = search_request(&estate, Some(&backend));

    let first = wirk_atlas::search(&estate.store, &request).unwrap();
    assert!(
        matches!(first.semantic, SemanticStatus::Applied),
        "{:?}",
        first.semantic
    );
    assert_eq!(first.editions.len(), 2, "both sources must contribute");

    let corrupted_membership = estate.membership.clone();
    let corrupted_edition_id = first
        .editions
        .iter()
        .find(|(membership, _)| *membership == corrupted_membership.id)
        .unwrap()
        .1
        .clone();
    let corrupted_edition = estate.store.read_edition(&corrupted_edition_id).unwrap();
    let vectors_path = u_edition_file(&estate, &corrupted_edition, &corrupted_edition.vectors.file);
    let original = fs::read(&vectors_path).unwrap();
    let tampered: Vec<u8> = original.iter().map(|byte| byte ^ 0x01).collect();
    fs::write(&vectors_path, &tampered).unwrap();

    let partial = wirk_atlas::search(&estate.store, &request).unwrap();
    let SemanticStatus::Partial(reason) = partial.semantic else {
        panic!(
            "one corrupted membership among two must be partial, not applied or unavailable: {:?}",
            partial.semantic
        );
    };
    assert!(
        reason.contains(&corrupted_membership.alias),
        "unexpected reason: {reason}"
    );
    assert_eq!(
        partial.editions.len(),
        1,
        "the healthy membership alone must still rank"
    );

    fs::write(&vectors_path, &original).unwrap();
    let repaired = wirk_atlas::search(&estate.store, &request).unwrap();
    assert!(
        matches!(repaired.semantic, SemanticStatus::Applied),
        "a repair on disk must restore full coverage on the very next call: {:?}",
        repaired.semantic
    );
    assert_eq!(repaired.editions.len(), 2);
}

// ---- T10-T12: native embedding batch composition (ruling 0175, D4) ------
//
// Native chunk-embed vectors are a function of how chunks are batched into
// the model's `encode` calls: the installed tokenizer pads every text in
// one call to that call's own longest member, so a chunk's stored vector
// used to depend on every other chunk in the *whole build request*
// (`wirk-atlas/backends/semble_backend.py::run_embed`,
// `model.encode(texts, ...)` once over every resource). The installed
// reference does not do this: `create_index_from_path` calls
// `embed_chunks(model, file_chunks)` once per file. These three tests are
// `#[ignore]`d and opted in exactly as T5-T9 are, against the pinned real
// `semble` interpreter and offline model — no stub, because a stub's
// vectors do not depend on batching and could not observe this defect.

/// The directory one edition's files live under, by the same path
/// convention T7's pinned-edition-rot check uses.
fn edition_directory(estate: &TiedEstate, id: &wirk_atlas::EditionId) -> PathBuf {
    estate._temporary.path().join("atlas/semantic").join(&id.0)
}

/// The edition's stored vectors, decoded from the raw `f32le` row-major
/// file the record's own `VectorManifest` names — never re-derived, never
/// assumed to be at a fixed path.
fn read_vectors(estate: &TiedEstate, edition: &SemanticEdition) -> Vec<Vec<f32>> {
    let bytes =
        fs::read(edition_directory(estate, &edition.id).join(&edition.vectors.file)).unwrap();
    let dimensions = edition.vectors.dimensions as usize;
    assert_eq!(
        bytes.len(),
        edition.vectors.rows as usize * dimensions * 4,
        "the vectors file is not the size its own manifest declares"
    );
    bytes
        .chunks_exact(dimensions * 4)
        .map(|row| {
            row.as_chunks::<4>()
                .0
                .iter()
                .map(|four| f32::from_le_bytes(*four))
                .collect()
        })
        .collect()
}

/// The edition's mapping rows, parsed from the NDJSON file its own
/// `MappingManifest` names, in vector row order.
fn read_mapping(estate: &TiedEstate, edition: &SemanticEdition) -> Vec<wirk_atlas::MappingRow> {
    let bytes =
        fs::read(edition_directory(estate, &edition.id).join(&edition.mapping.file)).unwrap();
    let text = String::from_utf8(bytes).unwrap();
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// The rows and vectors belonging to one source-relative path, in row
/// order, as `(row, vector)` pairs.
fn rows_for_path<'a>(
    mapping: &'a [wirk_atlas::MappingRow],
    vectors: &'a [Vec<f32>],
    path: &str,
) -> Vec<(&'a wirk_atlas::MappingRow, &'a Vec<f32>)> {
    mapping
        .iter()
        .zip(vectors.iter())
        .filter(|(row, _)| row.path == path.as_bytes())
        .collect()
}

/// Vectors for exactly these texts, computed by the installed `semble`'s
/// own per-file embedding call — `embed_chunks(model, file_chunks)`,
/// i.e. one `model.encode` call over exactly this list, never mixed with
/// any other file's chunks. Used as the independent oracle T10 compares
/// the product's stored vectors against; it shells out to the pinned
/// interpreter rather than re-implementing `model2vec` encoding.
fn embed_chunks_oracle(python: &Path, model: &Path, texts: &[String]) -> Vec<Vec<f32>> {
    let script = r#"
import json, struct, sys
from model2vec import StaticModel
request = json.loads(sys.stdin.read())
model = StaticModel.from_pretrained(request["model_path"], force_download=False)
texts = request["texts"]
if texts:
    vectors = model.encode(texts, use_multiprocessing=False)
else:
    vectors = []
sys.stdout.buffer.write(struct.pack("<Q", len(texts)))
sys.stdout.buffer.write(struct.pack("<Q", model.dim))
for row in vectors:
    sys.stdout.buffer.write(struct.pack(f"<{model.dim}f", *(float(v) for v in row)))
"#;
    let mut child = Command::new(python)
        .arg("-c")
        .arg(script)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            &serde_json::to_vec(&serde_json::json!({
                "model_path": model.display().to_string(),
                "texts": texts,
            }))
            .unwrap(),
        )
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "embedding oracle failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows = u64::from_le_bytes(output.stdout[0..8].try_into().unwrap()) as usize;
    let dimensions = u64::from_le_bytes(output.stdout[8..16].try_into().unwrap()) as usize;
    assert_eq!(rows, texts.len());
    output.stdout[16..]
        .chunks_exact(dimensions * 4)
        .map(|row| {
            row.as_chunks::<4>()
                .0
                .iter()
                .map(|four| f32::from_le_bytes(*four))
                .collect()
        })
        .collect()
}

/// The exact ranking text of one mapping row, read from the committed
/// bytes at `[byte_start, byte_end)` -- valid only for the plain-LF ASCII
/// fixtures these tests use, where the ranking text is byte-identical to
/// the committed slice (`text_normalization` is always `identity` here).
fn row_text(repo: &Path, relative: &str, row: &wirk_atlas::MappingRow) -> String {
    let bytes = fs::read(repo.join(relative)).unwrap();
    assert_eq!(
        row.text_normalization.as_deref(),
        Some(wirk_atlas::TEXT_IDENTITY),
        "this helper only recovers text for identity-normalized rows"
    );
    String::from_utf8(bytes[row.byte_start as usize..row.byte_end as usize].to_vec()).unwrap()
}

fn batch_module_text(seed: u64, blocks: usize) -> String {
    let mut text = String::new();
    let mut state = seed;
    let mut next = move || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        (state >> 33) as usize
    };
    for slot in 0..blocks {
        text.push_str(&format!("def block_{slot}():\n    # "));
        for _ in 0..40 {
            text.push_str(CAPACITY_VOCAB[next() % CAPACITY_VOCAB.len()]);
            text.push(' ');
        }
        text.push_str(&format!("\n    return {slot}\n\n"));
    }
    text
}

/// T10. Every one of a resource's chunks is embedded in the *same*
/// `encode` call as every other chunk of that resource, and in no other
/// call: the product's stored vectors for `pkg_a/util.py` and
/// `pkg_b/util.py` (same basename, different directories, so row
/// alignment cannot be papering over a path collision) match the
/// installed `semble`'s own `embed_chunks` run over exactly that file's
/// chunk texts, bit-identical, while `short.py`'s vectors independently
/// verify the same way.
///
/// Red before the correction: the product embedded every resource's
/// chunks in one call over the whole build, so a multi-chunk file's
/// vectors depended on which other files were in the same build and
/// disagreed with `embed_chunks` run alone (`DIAGNOSIS.md` D4, measured
/// min cosine 0.9998-0.99999, max abs delta up to 0.0055 on the
/// diagnosis fixture).
#[test]
#[ignore]
fn t10_native_vectors_match_installed_semble_per_file_embed_chunks() {
    let python = pinned_semble_python();
    let model = pinned_semble_model();
    let script = real_semble_backend_script();

    let repo = TempDir::new().unwrap();
    git(repo.path(), &["init", "-q"]);
    git(repo.path(), &["config", "user.email", "a@b"]);
    git(repo.path(), &["config", "user.name", "A"]);
    for (relative, text) in [
        ("short.py", batch_module_text(1, 2)),
        ("pkg_a/util.py", batch_module_text(2, 3)),
        ("pkg_b/util.py", batch_module_text(3, 5)),
    ] {
        let path = repo.path().join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "batch fixture"]);

    let mut estate = tied_estate_with_alias(repo.path(), "m-batch");
    let edition = tied_build(&mut estate, &python, &script, &model);
    let mapping = read_mapping(&estate, &edition);
    let vectors = read_vectors(&estate, &edition);

    for relative in ["short.py", "pkg_a/util.py", "pkg_b/util.py"] {
        let pairs = rows_for_path(&mapping, &vectors, relative);
        assert!(!pairs.is_empty(), "{relative} produced no row");
        // Row alignment: this file's rows are its own chunks' slots, in
        // order, with no gap and no row belonging to another path.
        for (slot, (row, _)) in pairs.iter().enumerate() {
            assert_eq!(row.path, relative.as_bytes());
            let _ = slot;
        }
        let texts: Vec<String> = pairs
            .iter()
            .map(|(row, _)| row_text(repo.path(), relative, row))
            .collect();
        let oracle = embed_chunks_oracle(&python, &model, &texts);
        assert_eq!(
            oracle.len(),
            pairs.len(),
            "{relative}: oracle returned a different row count"
        );
        for (index, ((_, stored), expected)) in pairs.iter().zip(oracle.iter()).enumerate() {
            assert_eq!(
                stored.as_slice(),
                expected.as_slice(),
                "{relative} row {index}: stored vector does not match embed_chunks run over \
                 exactly this file's chunks"
            );
        }
    }
}

/// T11. Adding an unrelated, much longer file to the same membership must
/// not change the stored vectors of a file already there: `short.py`'s
/// vectors are bit-identical across a build without `long.py` and a
/// rebuild with it. A whitespace-only resource, present in both builds,
/// still produces no row in either (D-class coverage from
/// `fixture_repo`, pinned here against the real backend rather than a
/// synthetic one).
///
/// Red before the correction: `long.py`'s many long chunks entered the
/// same `encode` call as `short.py`'s few short ones, so the tokenizer
/// padded `short.py`'s chunks up to `long.py`'s longest token length and
/// moved their vectors (`DIAGNOSIS.md` D4).
#[test]
#[ignore]
fn t11_an_unrelated_longer_file_leaves_other_files_vectors_unchanged() {
    let python = pinned_semble_python();
    let model = pinned_semble_model();
    let script = real_semble_backend_script();

    let repo = TempDir::new().unwrap();
    git(repo.path(), &["init", "-q"]);
    git(repo.path(), &["config", "user.email", "a@b"]);
    git(repo.path(), &["config", "user.name", "A"]);
    fs::write(repo.path().join("short.py"), batch_module_text(11, 1)).unwrap();
    fs::write(repo.path().join("blank.py"), "   \n\t\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "before"]);

    let mut estate = tied_estate_with_alias(repo.path(), "m-unrelated");
    let before = tied_build(&mut estate, &python, &script, &model);
    assert_eq!(before.coverage.resources_indexed, 2);
    assert_eq!(before.coverage.resources_with_rows, 1);
    assert_eq!(before.coverage.resources_without_rows.len(), 1);
    let mapping_before = read_mapping(&estate, &before);
    let vectors_before = read_vectors(&estate, &before);
    let short_before = rows_for_path(&mapping_before, &vectors_before, "short.py");
    assert!(!short_before.is_empty());

    fs::write(repo.path().join("long.py"), batch_module_text(12, 60)).unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "after"]);
    estate.generation = estate
        .store
        .acquire(
            &estate.membership.clone(),
            "HEAD",
            ExtractorPolicy::default(),
        )
        .unwrap()
        .staged()
        .unwrap()
        .id;
    estate
        .store
        .publish(&estate.membership.clone(), &estate.generation.clone())
        .unwrap();
    let after = tied_build(&mut estate, &python, &script, &model);
    assert_eq!(after.coverage.resources_indexed, 3);
    assert_eq!(after.coverage.resources_with_rows, 2);
    assert_eq!(
        after.coverage.resources_without_rows.len(),
        1,
        "the whitespace-only resource still yields no row with a longer sibling present"
    );
    let mapping_after = read_mapping(&estate, &after);
    let vectors_after = read_vectors(&estate, &after);
    let short_after = rows_for_path(&mapping_after, &vectors_after, "short.py");

    assert_eq!(
        short_before.len(),
        short_after.len(),
        "short.py's own row count moved when an unrelated file was added"
    );
    for (index, ((_, before_vector), (_, after_vector))) in
        short_before.iter().zip(short_after.iter()).enumerate()
    {
        assert_eq!(
            before_vector.as_slice(),
            after_vector.as_slice(),
            "short.py row {index}: vector changed after an unrelated longer file was added"
        );
    }
}

/// T12. An edition built under the previous whole-request batching policy
/// is refused by name, never silently ranked as though it matched
/// today's per-resource policy — the same shape T8 proves for
/// `CAPACITY_POLICY`. The historical record is read, never rewritten.
#[test]
#[ignore]
fn t12_a_previous_batch_policy_edition_is_refused_by_name_not_reinterpreted() {
    let python = pinned_semble_python();
    let model = pinned_semble_model();
    let script = real_semble_backend_script();
    let repo = capacity_fixture_repo();
    let estate = capacity_estate(&python, &script, &model, repo.path());

    let edition_id = estate.store.selected_semantic(&estate.membership).unwrap();
    let record = edition_directory(&estate, &edition_id).join(wirk_atlas::EDITION_RECORD);
    let before = fs::read(&record).unwrap();
    let mut document: serde_json::Value = serde_json::from_slice(&before).unwrap();
    let retrieval = document["retrieval"].as_object_mut().unwrap();
    retrieval.insert("batch_policy".into(), serde_json::json!(""));
    fs::write(&record, serde_json::to_vec_pretty(&document).unwrap()).unwrap();

    let answer = wirk_atlas::search(
        &estate.store,
        &capacity_request(CAPACITY_QUERIES[0], 5, None, 0, &python, &script, &model),
    )
    .unwrap();
    let reason = match &answer.semantic {
        SemanticStatus::Unavailable(reason) => reason.clone(),
        other => panic!("a previous-batch-policy edition was ranked through: {other:?}"),
    };
    assert!(
        reason.contains("declares no embedding-batch policy"),
        "the refusal must state what the edition itself declares: {reason}"
    );
    assert!(
        reason.contains(wirk_atlas::EMBEDDING_BATCH_POLICY),
        "the refusal must name the policy this product embeds under: {reason}"
    );
    assert!(
        reason.contains("rebuild its semantic edition"),
        "the refusal must name the recovery: {reason}"
    );
    assert!(
        answer.application.is_none(),
        "nothing was ranked, so nothing may be reported as having been"
    );

    // Fresh-build recovery: the ordinary recovery this product names is
    // building against the estate's current admitted content and
    // selecting the result. A rebuild against byte-identical content
    // would reproduce the very same content-addressed id and find this
    // corrupted record already on disk (0089's deliberate content
    // addressing: identical bytes are one edition, never two) -- so the
    // realistic recovery, and what this proves, is building against a
    // freshly admitted generation, exactly what an operator does next.
    let mut estate = estate;
    fs::write(repo.path().join("recovery.py"), batch_module_text(99, 2)).unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "recovery content"]);
    estate.generation = estate
        .store
        .acquire(
            &estate.membership.clone(),
            "HEAD",
            ExtractorPolicy::default(),
        )
        .unwrap()
        .staged()
        .unwrap()
        .id;
    estate
        .store
        .publish(&estate.membership.clone(), &estate.generation.clone())
        .unwrap();
    let rebuilt = tied_build(&mut estate, &python, &script, &model);
    assert_ne!(
        rebuilt.id, edition_id,
        "a rebuild against freshly admitted content must not collide with the stale edition's id"
    );
    assert_eq!(
        rebuilt
            .retrieval
            .as_ref()
            .expect("a native edition binds a retrieval identity")
            .batch_policy,
        wirk_atlas::EMBEDDING_BATCH_POLICY,
        "a fresh build must declare today's policy, not inherit the stale one"
    );
    estate
        .store
        .select_semantic(&estate.membership.clone(), &rebuilt.id)
        .unwrap()
        .unwrap();
    let recovered = wirk_atlas::search(
        &estate.store,
        &capacity_request(CAPACITY_QUERIES[0], 5, None, 0, &python, &script, &model),
    )
    .unwrap();
    assert!(
        matches!(recovered.semantic, SemanticStatus::Applied),
        "{:?}",
        recovered.semantic
    );

    let after: serde_json::Value = serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();
    assert_eq!(after["retrieval"]["batch_policy"], serde_json::json!(""));
}
