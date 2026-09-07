//! P3 W4 A contract checks for the semantic edition lifecycle
//! (`W4-PUBLIC-LIFECYCLE-BUILD.md`, "Required proof and use").
//!
//! Every check here runs a *real* subprocess across the real argv/stdin
//! boundary the product uses in production: the backend is a small script
//! on disk, spawned, fed the real request and read back. Only the
//! embedding arithmetic is trivial, and deliberately so — these pin the
//! identity, refusal and atomicity contract, which is what a stub can
//! honestly pin (ruling 0040). They are not the proof that the feature
//! works: that is the recorded real run against the cached model and the
//! real repository, in `semantic-public-lifecycle-build/raw/`.
//!
//! Each refusal below was watched failing against the same code with its
//! own check removed before it was accepted; see
//! `raw/30-watched-refusals.log`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;
use wirk_atlas::{
    AcquireOutcome, AtlasStore, EditionId, ExtractorPolicy, Membership, SemanticBuildConfig,
    SemanticBuildOutcome, SemanticEdition, SemanticVerification,
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

fn fixture_repo() -> TempDir {
    let repo = TempDir::new().unwrap();
    git(repo.path(), &["init", "-q"]);
    git(repo.path(), &["config", "user.email", "a@b"]);
    git(repo.path(), &["config", "user.name", "A"]);
    fs::write(repo.path().join("code.rs"), "fn one() {}\nfn two() {}\n").unwrap();
    fs::write(repo.path().join("readme.md"), "# heading\nbody text\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "fixture"]);
    repo
}

/// A real `wirk-embed/v2` backend in its `embed` mode, on disk, executed
/// as a real child
/// process. `flavour` controls what it claims and writes so a defective
/// backend can be exercised as a defective *backend*, not as a mocked
/// return value inside the product.
fn backend_script(directory: &Path, name: &str, flavour: &str) -> PathBuf {
    let script = directory.join(name);
    fs::write(
        &script,
        format!(
            r#"#!/usr/bin/env python3
import hashlib, json, os, struct, sys
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

ARGV = sys.argv[1:]

def module_entry(name, path):
    body = open(path, "rb").read()
    return {{
        "name": name,
        "origin": path,
        "path": path,
        "digest": hashlib.sha256(body).hexdigest(),
        "byte_len": len(body),
        "claims": ["stubdist"],
    }}

def environment_block():
    """Emitted only when the caller passes `--report-environment <dist-info>`,
    so the argv boundary itself is what turns provenance reporting on and
    the test exercises a real non-file argv token.

    `--report-modules <file>...` adds the loaded-module list this
    correction introduced; without it the block is exactly the shape a
    pre-correction backend sent, which is how the "unmeasured, never
    complete" rule stays under test."""
    if "--report-environment" not in ARGV:
        return None
    where = ARGV[ARGV.index("--report-environment") + 1]
    record = open(os.path.join(where, "RECORD"), "rb").read()
    meta = open(os.path.join(where, "METADATA"), "rb").read()
    record_digest = hashlib.sha256(record).hexdigest()
    if "--lie-about-environment" in ARGV:
        record_digest = "0" * 64
    block = {{
        "kind": "python-distributions/v1",
        "root": os.path.dirname(where),
        "runtime": "stub/1.0",
        "executable": sys.executable,
        "distributions": [{{
            "name": "stubdist",
            "version": "1.0",
            "metadata_path": where,
            "record_digest": record_digest,
            "metadata_digest": hashlib.sha256(meta).hexdigest(),
            "declared_files": 2,
            "declared_byte_len": len(record) + len(meta),
            "files_checked": 2,
            "files_missing": 0,
            "files_mismatched": 0,
        }}],
    }}
    if "--report-modules" in ARGV:
        block["kind"] = "python-distributions/v2"
        modules = []
        index = ARGV.index("--report-modules") + 1
        while index < len(ARGV) and not ARGV[index].startswith("--"):
            name, _, path = ARGV[index].partition("=")
            modules.append(module_entry(name, path))
            index += 1
        if "--lie-about-modules" in ARGV and modules:
            modules[0]["digest"] = "0" * 64
        block["modules"] = modules
        block["undescribed_distributions"] = []
        block["unmeasured_modules"] = []
        if "--undescribed" in ARGV:
            block["undescribed_distributions"] = [
                {{"name": "otherdist", "reason": "no readable .dist-info directory"}}
            ]
        if "--unmeasured-module" in ARGV:
            block["unmeasured_modules"] = [
                {{"name": "stubdist._speedup", "reason": "no __file__: builtin"}}
            ]
    return block

header = json.loads(sys.stdin.readline())
if not os.path.isdir(header["model_path"]):
    print("model_path is not a directory", file=sys.stderr)
    raise SystemExit(2)
texts = [json.loads(line)["text"] for line in sys.stdin if line.strip()]
dimensions = 4
rows = len(texts)
if FLAVOUR == "short_output":
    rows = max(rows - 1, 0)
digest = model_digest(header["model_path"])
with open(header["output"], "wb") as handle:
    for text in texts[:rows]:
        # The model's own bytes are part of the vector, as they are for a
        # real embedder: a different model must produce different output,
        # or the "two editions differ" contract would be pinned by a stub
        # that cannot actually distinguish two models.
        if FLAVOUR == "model_blind":
            seed = hashlib.sha256(text.encode()).digest()
        elif FLAVOUR == "nondeterministic":
            seed = os.urandom(32)
        else:
            seed = hashlib.sha256(digest.encode() + b"\\x00" + text.encode()).digest()
        handle.write(struct.pack("<4f", *(b / 255.0 for b in seed[:4])))
if FLAVOUR == "wrong_model":
    digest = "0" * 64
reply = {{
    "protocol": "wirk-embed/v2",
    "backend": "test-backend/" + FLAVOUR,
    "model_path": header["model_path"],
    "model_digest": digest,
    "rows": len(texts),
    "dimensions": dimensions,
}}
if FLAVOUR == "wrong_model_path":
    reply["model_path"] = header["model_path"] + "-elsewhere"
environment = environment_block()
if environment is not None:
    reply["environment"] = environment
print(json.dumps(reply))
"#
        ),
    )
    .unwrap();
    let mut permissions = fs::metadata(&script).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    fs::set_permissions(&script, permissions).unwrap();
    script
}

fn model_dir(directory: &Path, name: &str, weights: &str) -> PathBuf {
    let model = directory.join(name);
    fs::create_dir_all(&model).unwrap();
    fs::write(model.join("config.json"), format!("{{\"m\":\"{name}\"}}")).unwrap();
    fs::write(model.join("model.bin"), weights).unwrap();
    model
}

struct Estate {
    _repo: TempDir,
    _home: TempDir,
    root: PathBuf,
    store: AtlasStore,
    membership: Membership,
    generation: wirk_atlas::GenerationId,
    backend: PathBuf,
    model: PathBuf,
    scratch: PathBuf,
}

fn estate() -> Estate {
    estate_with_backend("honest")
}

fn estate_with_backend(flavour: &str) -> Estate {
    let repo = fixture_repo();
    let home = TempDir::new().unwrap();
    let root = home.path().join("estate");
    fs::create_dir_all(&root).unwrap();
    let scratch = home.path().join("scratch");
    fs::create_dir_all(&scratch).unwrap();
    let mut store = AtlasStore::open(&root, root.display().to_string()).unwrap();
    let membership = store.register_git("fixture", repo.path(), "HEAD").unwrap();
    let AcquireOutcome::Staged(generation) = store
        .acquire(&membership, "HEAD", ExtractorPolicy::default())
        .unwrap()
    else {
        panic!("fixture acquisition is unavailable");
    };
    store.publish(&membership, &generation.id).unwrap();
    let backend = backend_script(&scratch, "backend.py", flavour);
    let model = model_dir(&scratch, "model-a", "weights-a");
    Estate {
        _repo: repo,
        _home: home,
        root,
        store,
        membership,
        generation: generation.id,
        backend,
        model,
        scratch,
    }
}

fn config(estate: &Estate, model: &Path) -> SemanticBuildConfig {
    config_with_args(estate, model, Vec::new())
}

fn config_with_args(estate: &Estate, model: &Path, args: Vec<String>) -> SemanticBuildConfig {
    SemanticBuildConfig {
        backend: estate.backend.clone(),
        backend_args: args,
        model: model.to_path_buf(),
        producer: "test/v1".into(),
        chunking: wirk_atlas::SemanticChunking::Units,
    }
}

fn build_with_args(estate: &mut Estate, model: &Path, args: &[&str]) -> SemanticBuildOutcome {
    let configuration = config_with_args(
        estate,
        model,
        args.iter().map(|arg| (*arg).to_owned()).collect(),
    );
    let membership = estate.membership.clone();
    let generation = estate.generation.clone();
    estate
        .store
        .build_semantic(&membership, &generation, &configuration)
        .unwrap()
}

/// A minimal installed-distribution metadata directory, in the shape the
/// backend contract reads: the installer's own `RECORD` and `METADATA`.
fn dist_info(directory: &Path, name: &str) -> PathBuf {
    let where_ = directory.join(name);
    fs::create_dir_all(&where_).unwrap();
    fs::write(
        where_.join("RECORD"),
        format!("{name}/__init__.py,sha256=abc,12\n{name}-1.0.dist-info/METADATA,,\n"),
    )
    .unwrap();
    fs::write(
        where_.join("METADATA"),
        format!("Metadata-Version: 2.1\nName: {name}\nVersion: 1.0\n"),
    )
    .unwrap();
    where_
}

fn build(estate: &mut Estate, model: &Path) -> SemanticBuildOutcome {
    let configuration = config(estate, model);
    let membership = estate.membership.clone();
    let generation = estate.generation.clone();
    estate
        .store
        .build_semantic(&membership, &generation, &configuration)
        .unwrap()
}

fn staged(outcome: SemanticBuildOutcome) -> SemanticEdition {
    match outcome {
        SemanticBuildOutcome::Staged(edition) => *edition,
        SemanticBuildOutcome::Refused(reason) => panic!("expected a staged edition: {reason}"),
    }
}

fn refusal(outcome: SemanticBuildOutcome) -> String {
    match outcome {
        SemanticBuildOutcome::Refused(reason) => reason,
        SemanticBuildOutcome::Staged(edition) => {
            panic!("expected a refusal, staged {}", edition.id.0)
        }
    }
}

fn edition_path(estate: &Estate, id: &EditionId, file: &str) -> PathBuf {
    estate
        .root
        .join("atlas")
        .join("semantic")
        .join(&id.0)
        .join(file)
}

// ---- lifecycle -----------------------------------------------------------

/// Building stages; it never publishes. The catalog is untouched until a
/// separate `select`, and `status`'s view of the estate says so.
#[test]
fn a_build_stages_and_does_not_select() {
    let mut estate = estate();
    let edition = staged({
        let m = estate.model.clone();
        build(&mut estate, &m)
    });

    assert!(estate.store.selected_semantic(&estate.membership).is_none());
    let states = estate.store.semantic_editions(&estate.membership).unwrap();
    assert_eq!(states.len(), 1);
    assert!(!states[0].selected);
    assert_eq!(states[0].verification, SemanticVerification::Verified);
    assert_eq!(states[0].edition.id, edition.id);
    // The edition binds the exact source generation identity, not a
    // re-derived one.
    assert_eq!(edition.generation, estate.generation);
    assert_eq!(edition.membership, estate.membership.id);
    assert_eq!(edition.vectors.rows, edition.mapping.rows);
    assert_eq!(
        edition.vectors.byte_len,
        edition.vectors.rows * edition.vectors.dimensions * 4
    );
}

/// One unchanged source generation, two models, two coexisting editions
/// with genuinely different vector bytes — and selecting the second never
/// rewrites the first (retained evidence and future continuation pins need
/// those identities intact).
#[test]
fn b_two_editions_coexist_for_one_generation() {
    let mut estate = estate();
    let first = staged({
        let m = estate.model.clone();
        build(&mut estate, &m)
    });
    let other = model_dir(
        estate.scratch.clone().as_path(),
        "model-b",
        "weights-b-different",
    );
    let second = staged(build(&mut estate, &other));

    assert_ne!(first.id, second.id);
    assert_eq!(first.generation, second.generation);
    // Same committed bytes, so the same mapping; different model, so
    // different vectors. Output identity, not recipe identity.
    assert_eq!(first.mapping.digest, second.mapping.digest);
    assert_ne!(first.vectors.digest, second.vectors.digest);
    assert_ne!(first.model.consumed.digest, second.model.consumed.digest);

    let before = fs::read(edition_path(&estate, &first.id, "vectors.bin")).unwrap();
    let membership = estate.membership.clone();
    estate
        .store
        .select_semantic(&membership, &first.id)
        .unwrap()
        .unwrap();
    estate
        .store
        .select_semantic(&membership, &second.id)
        .unwrap()
        .unwrap();
    assert_eq!(
        estate.store.selected_semantic(&membership),
        Some(second.id.clone())
    );
    assert_eq!(
        fs::read(edition_path(&estate, &first.id, "vectors.bin")).unwrap(),
        before
    );
    assert_eq!(
        estate.store.read_edition(&first.id).unwrap().vectors.digest,
        first.vectors.digest
    );
}

/// A selection survives the process that made it: reopening the store is
/// the same recovery path a daemon restart takes.
#[test]
fn c_selection_survives_reopen() {
    let mut estate = estate();
    let edition = staged({
        let m = estate.model.clone();
        build(&mut estate, &m)
    });
    let membership = estate.membership.clone();
    estate
        .store
        .select_semantic(&membership, &edition.id)
        .unwrap()
        .unwrap();

    let reopened = AtlasStore::open(&estate.root, estate.root.display().to_string()).unwrap();
    assert_eq!(
        reopened.selected_semantic(&membership),
        Some(edition.id.clone())
    );
    let states = reopened.semantic_editions(&membership).unwrap();
    assert!(states.iter().any(|state| state.selected));
}

/// A W3-era catalog — written before `semantic_selected` existed — still
/// opens, still resolves its generations, and simply reports nothing
/// selected. The migration is additive.
#[test]
fn d_old_catalog_without_semantic_field_still_opens() {
    let estate = estate();
    let catalog_path = estate.root.join("atlas").join("catalog.json");
    let mut catalog: serde_json::Value =
        serde_json::from_slice(&fs::read(&catalog_path).unwrap()).unwrap();
    assert!(catalog.get("semantic_selected").is_some());
    catalog
        .as_object_mut()
        .unwrap()
        .remove("semantic_selected")
        .unwrap();
    fs::write(&catalog_path, serde_json::to_vec_pretty(&catalog).unwrap()).unwrap();

    let reopened = AtlasStore::open(&estate.root, estate.root.display().to_string()).unwrap();
    assert!(reopened.selected_semantic(&estate.membership).is_none());
    // The W3 estate check and the old generation reference both survive.
    assert_eq!(
        reopened.current(&estate.membership).unwrap().unwrap().id,
        estate.generation
    );
}

// ---- refusals ------------------------------------------------------------

/// The backend says it loaded a model other than the one this build
/// consumed. Nothing is staged (ruling 0088's defect, from the product
/// side).
#[test]
fn e_refuses_a_backend_that_loaded_a_different_model() {
    let mut estate = estate_with_backend("wrong_model");
    let reason = refusal({
        let m = estate.model.clone();
        build(&mut estate, &m)
    });
    assert!(
        reason.contains("model digest"),
        "unexpected refusal: {reason}"
    );
    assert!(
        estate
            .store
            .semantic_editions(&estate.membership)
            .unwrap()
            .is_empty()
    );

    let mut estate = estate_with_backend("wrong_model_path");
    let reason = refusal({
        let m = estate.model.clone();
        build(&mut estate, &m)
    });
    assert!(
        reason.contains("backend loaded model"),
        "unexpected refusal: {reason}"
    );
}

/// A backend that writes fewer rows than it embedded produces no edition:
/// an incomplete artifact is never staged.
#[test]
fn f_refuses_an_incomplete_vector_output() {
    let mut estate = estate_with_backend("short_output");
    let reason = refusal({
        let m = estate.model.clone();
        build(&mut estate, &m)
    });
    assert!(
        reason.contains("vector bytes"),
        "unexpected refusal: {reason}"
    );
    assert!(
        estate
            .store
            .semantic_editions(&estate.membership)
            .unwrap()
            .is_empty()
    );
}

/// Vector bytes replaced by *the same number of different bytes* — the
/// case a length check alone would pass. Verification is by digest, so it
/// is corrupt, and selection refuses it.
#[test]
fn g_refuses_same_length_different_vector_bytes() {
    let mut estate = estate();
    let edition = staged({
        let m = estate.model.clone();
        build(&mut estate, &m)
    });
    let path = edition_path(&estate, &edition.id, "vectors.bin");
    let original = fs::read(&path).unwrap();
    let tampered: Vec<u8> = original.iter().map(|byte| byte ^ 0x01).collect();
    assert_eq!(original.len(), tampered.len());
    fs::write(&path, &tampered).unwrap();

    assert!(matches!(
        estate.store.verify_edition(&edition),
        SemanticVerification::Corrupt(_)
    ));
    let membership = estate.membership.clone();
    let reason = estate
        .store
        .select_semantic(&membership, &edition.id)
        .unwrap()
        .unwrap_err();
    assert!(reason.contains("does not verify"), "unexpected: {reason}");
    assert!(estate.store.selected_semantic(&membership).is_none());
}

/// The mapping's recorded content no longer describes the committed bytes.
/// Caught twice over: by the mapping digest, and — with the digest
/// recomputed by a more careful tamperer — by re-reading the real Git
/// blobs at the recorded coordinates.
#[test]
fn h_refuses_an_altered_mapping() {
    let mut estate = estate();
    let edition = staged({
        let m = estate.model.clone();
        build(&mut estate, &m)
    });
    let path = edition_path(&estate, &edition.id, "mapping.ndjson");
    let original = fs::read_to_string(&path).unwrap();

    let mut lines: Vec<String> = original.lines().map(str::to_owned).collect();
    let mut row: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
    row["content_digest"] = serde_json::json!("0".repeat(64));
    lines[0] = serde_json::to_string(&row).unwrap();
    fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();
    assert!(matches!(
        estate.store.verify_edition(&edition),
        SemanticVerification::Corrupt(_)
    ));

    // Now the harder case: the manifest is made self-consistent again, so
    // only the real repository can tell the truth.
    let tampered = fs::read(&path).unwrap();
    let mut record: serde_json::Value = serde_json::from_slice(
        &fs::read(edition_path(&estate, &edition.id, "edition.json")).unwrap(),
    )
    .unwrap();
    record["mapping"]["digest"] = serde_json::json!(sha256_hex(&tampered));
    record["mapping"]["byte_len"] = serde_json::json!(tampered.len());
    fs::write(
        edition_path(&estate, &edition.id, "edition.json"),
        serde_json::to_vec_pretty(&record).unwrap(),
    )
    .unwrap();
    // The record's own id no longer covers its contents, so it is refused
    // before any coordinate is consulted.
    assert!(estate.store.read_edition(&edition.id).is_err());

    let mut forged: SemanticEdition = serde_json::from_value(record).unwrap();
    forged.id = edition.id.clone();
    assert!(matches!(
        estate
            .store
            .verify_edition_coordinates(&estate.membership, &forged)
            .unwrap(),
        SemanticVerification::Corrupt(_)
    ));
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// An edition built for one source cannot be selected for another, even
/// when both are in the same estate and the artifact itself verifies.
#[test]
fn i_refuses_cross_membership_substitution() {
    let mut estate = estate();
    let edition = staged({
        let m = estate.model.clone();
        build(&mut estate, &m)
    });
    let second_repo = fixture_repo();
    let other = estate
        .store
        .register_git("other", second_repo.path(), "HEAD")
        .unwrap();
    let AcquireOutcome::Staged(generation) = estate
        .store
        .acquire(&other, "HEAD", ExtractorPolicy::default())
        .unwrap()
    else {
        panic!("second fixture is unavailable");
    };
    estate.store.publish(&other, &generation.id).unwrap();

    let reason = estate
        .store
        .select_semantic(&other, &edition.id)
        .unwrap()
        .unwrap_err();
    assert!(
        reason.contains("different estate, membership or source"),
        "unexpected: {reason}"
    );
    assert!(estate.store.selected_semantic(&other).is_none());

    // The same rule at build time: a generation belonging to another
    // source is refused before a single blob is read.
    let membership = estate.membership.clone();
    let configuration = {
        let m = estate.model.clone();
        config(&estate, &m)
    };
    let error = estate
        .store
        .build_semantic(&membership, &generation.id, &configuration)
        .unwrap_err();
    assert!(
        error.to_string().contains("does not belong"),
        "unexpected: {error}"
    );
}

/// An edition whose files are gone is `missing`, and selection refuses it.
#[test]
fn j_refuses_an_edition_with_missing_files() {
    let mut estate = estate();
    let edition = staged({
        let m = estate.model.clone();
        build(&mut estate, &m)
    });
    fs::remove_file(edition_path(&estate, &edition.id, "vectors.bin")).unwrap();

    assert!(matches!(
        estate.store.verify_edition(&edition),
        SemanticVerification::Missing(_)
    ));
    let membership = estate.membership.clone();
    assert!(
        estate
            .store
            .select_semantic(&membership, &edition.id)
            .unwrap()
            .is_err()
    );
}

/// Absent model, absent backend: a truthful named refusal, never a
/// download and never a partially staged artifact.
#[test]
fn k_refuses_an_absent_model_or_backend() {
    let mut estate = estate();
    let absent = estate.scratch.join("no-such-model");
    let reason = refusal(build(&mut estate, &absent));
    assert!(reason.contains("does not resolve"), "unexpected: {reason}");

    // A bare model name is refused explicitly: it resolves through a
    // shared mutable cache and names no fixed bytes.
    let reason = refusal(build(&mut estate, Path::new("minishlab/potion-code-16M")));
    assert!(
        reason.contains("not an absolute path"),
        "unexpected: {reason}"
    );

    let membership = estate.membership.clone();
    let generation = estate.generation.clone();
    let mut configuration = {
        let m = estate.model.clone();
        config(&estate, &m)
    };
    configuration.backend = estate.scratch.join("no-such-backend");
    let outcome = estate
        .store
        .build_semantic(&membership, &generation, &configuration)
        .unwrap();
    assert!(refusal(outcome).contains("does not resolve"));
    assert!(
        estate
            .store
            .semantic_editions(&estate.membership)
            .unwrap()
            .is_empty()
    );
}

/// Selecting an edition of a generation this source does not currently
/// publish is refused, and a *failed* replacement leaves the previously
/// selected edition exactly where it was.
#[test]
fn l_a_failed_replacement_preserves_the_previous_selection() {
    let mut estate = estate();
    let good = staged({
        let m = estate.model.clone();
        build(&mut estate, &m)
    });
    let membership = estate.membership.clone();
    estate
        .store
        .select_semantic(&membership, &good.id)
        .unwrap()
        .unwrap();
    let revision = estate.store.publication_revision();

    let other = model_dir(estate.scratch.clone().as_path(), "model-c", "weights-c");
    let replacement = staged(build(&mut estate, &other));
    fs::remove_file(edition_path(&estate, &replacement.id, "vectors.bin")).unwrap();
    let reason = estate
        .store
        .select_semantic(&membership, &replacement.id)
        .unwrap()
        .unwrap_err();
    assert!(reason.contains("does not verify"), "unexpected: {reason}");
    assert_eq!(
        estate.store.selected_semantic(&membership),
        Some(good.id.clone())
    );
    assert_eq!(estate.store.publication_revision(), revision);

    // And an unknown edition id is refused the same way.
    let unknown = EditionId(format!("e-{}", "1".repeat(64)));
    assert!(
        estate
            .store
            .select_semantic(&membership, &unknown)
            .unwrap()
            .is_err()
    );
    assert_eq!(estate.store.selected_semantic(&membership), Some(good.id));
}

/// Selection is refused when the edition's generation is not the one this
/// source currently publishes: the public semantic record must describe
/// the public source record.
#[test]
fn m_refuses_an_edition_of_an_unpublished_generation() {
    let mut estate = estate();
    let edition = staged({
        let m = estate.model.clone();
        build(&mut estate, &m)
    });
    let membership = estate.membership.clone();

    // Move the source on: a new commit, acquired and published.
    let repo = estate._repo.path().to_path_buf();
    fs::write(repo.join("code.rs"), "fn one() {}\nfn three() {}\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "second"]);
    let AcquireOutcome::Staged(next) = estate
        .store
        .acquire(&membership, "HEAD", ExtractorPolicy::default())
        .unwrap()
    else {
        panic!("second acquisition is unavailable");
    };
    estate.store.publish(&membership, &next.id).unwrap();

    let reason = estate
        .store
        .select_semantic(&membership, &edition.id)
        .unwrap()
        .unwrap_err();
    assert!(
        reason.contains("currently publishes"),
        "unexpected: {reason}"
    );
    assert!(estate.store.selected_semantic(&membership).is_none());
}

/// The edition id covers the bytes actually consumed, not the recipe
/// strings that name them. The backend here deliberately ignores the
/// model's contents, so both builds produce byte-identical output from
/// byte-identical configuration strings — only the model's *actual bytes*
/// differ, and the two editions must still be two editions.
#[test]
fn n_edition_identity_covers_consumed_bytes_not_recipe_strings() {
    let mut estate = estate_with_backend("model_blind");
    let first = staged({
        let m = estate.model.clone();
        build(&mut estate, &m)
    });
    fs::write(estate.model.join("model.bin"), "weights-a-changed").unwrap();
    let second = staged({
        let m = estate.model.clone();
        build(&mut estate, &m)
    });

    assert_eq!(
        first.model.consumed.canonical,
        second.model.consumed.canonical
    );
    assert_eq!(first.vectors.digest, second.vectors.digest);
    assert_eq!(first.mapping.digest, second.mapping.digest);
    assert_ne!(first.model.consumed.digest, second.model.consumed.digest);
    assert_ne!(first.id, second.id);
}

/// And it covers the bytes actually produced. This backend is genuinely
/// nondeterministic, so two builds share every input, every configuration
/// string and every digest of both — and still produce different vectors.
/// Recipe equality is not output identity (0078).
#[test]
fn n2_edition_identity_covers_produced_bytes() {
    let mut estate = estate_with_backend("nondeterministic");
    let first = staged({
        let m = estate.model.clone();
        build(&mut estate, &m)
    });
    let second = staged({
        let m = estate.model.clone();
        build(&mut estate, &m)
    });

    assert_eq!(first.model.consumed.digest, second.model.consumed.digest);
    assert_eq!(first.backend.program.digest, second.backend.program.digest);
    assert_eq!(first.generation, second.generation);
    assert_ne!(first.vectors.digest, second.vectors.digest);
    assert_ne!(first.id, second.id);
    assert_eq!(
        estate
            .store
            .semantic_editions(&estate.membership)
            .unwrap()
            .len(),
        2
    );
}

/// The converse of both: identical inputs, identical outputs, identical
/// configuration is *one* edition. A rebuild finds the immutable
/// directory already there and never mints a second identity or rewrites
/// the bytes.
#[test]
fn n3_an_identical_rebuild_is_the_same_edition() {
    let mut estate = estate();
    let first = staged({
        let m = estate.model.clone();
        build(&mut estate, &m)
    });
    let before = fs::read(edition_path(&estate, &first.id, "vectors.bin")).unwrap();
    let second = staged({
        let m = estate.model.clone();
        build(&mut estate, &m)
    });

    assert_eq!(first.id, second.id);
    assert_eq!(
        first.producer.built_at_unix_millis,
        second.producer.built_at_unix_millis
    );
    assert_eq!(
        fs::read(edition_path(&estate, &first.id, "vectors.bin")).unwrap(),
        before
    );
    assert_eq!(
        estate
            .store
            .semantic_editions(&estate.membership)
            .unwrap()
            .len(),
        1
    );
}

/// Every mapping row still resolves to the exact committed bytes it
/// recorded, read back out of the real repository — the exact-coordinate
/// control at the product boundary.
#[test]
fn o_mapping_rows_resolve_to_the_committed_bytes() {
    let mut estate = estate();
    let edition = staged({
        let m = estate.model.clone();
        build(&mut estate, &m)
    });
    assert_eq!(
        estate
            .store
            .verify_edition_coordinates(&estate.membership, &edition)
            .unwrap(),
        SemanticVerification::Verified
    );

    // Every row carries its own full identity, so a row can never be
    // reinterpreted against another source that holds the same path.
    let mapping = fs::read_to_string(edition_path(&estate, &edition.id, "mapping.ndjson")).unwrap();
    let rows: Vec<serde_json::Value> = mapping
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(rows.len() as u64, edition.mapping.rows);
    for (index, row) in rows.iter().enumerate() {
        assert_eq!(row["row"].as_u64().unwrap(), index as u64);
        assert_eq!(row["membership"].as_str().unwrap(), estate.membership.id.0);
        assert_eq!(row["generation"].as_str().unwrap(), estate.generation.0);
        assert!(!row["object_id"].as_str().unwrap().is_empty());
        assert!(row["byte_end"].as_u64().unwrap() > row["byte_start"].as_u64().unwrap());
    }
}

/// A build interrupted after its bytes are written but before they are
/// visible leaves nothing behind: no edition, no catalog change, and the
/// abandoned private temporary is cleaned on reopen. Exercised through a
/// real child process at a real process-level failpoint, not a simulated
/// store failure.
#[test]
fn p_an_interrupted_build_stages_nothing() {
    let estate = estate();
    let output = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("child_build_crash")
        .arg("--nocapture")
        .env("W4_CHILD_ROOT", &estate.root)
        .env("W4_CHILD_REPO", estate._repo.path())
        .env("W4_CHILD_BACKEND", &estate.backend)
        .env("W4_CHILD_MODEL", &estate.model)
        .env("WIRK_ATLAS_FAILPOINT", "semantic-edition-written")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(86), "{output:?}");

    let reopened = AtlasStore::open(&estate.root, estate.root.display().to_string()).unwrap();
    assert!(
        reopened
            .semantic_editions(&estate.membership)
            .unwrap()
            .is_empty()
    );
    assert!(reopened.selected_semantic(&estate.membership).is_none());
    let semantic = estate.root.join("atlas").join("semantic");
    if semantic.exists() {
        for entry in fs::read_dir(&semantic).unwrap() {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            assert!(!name.starts_with(".tmp-"), "abandoned temporary {name}");
        }
    }
}

/// A selection interrupted after the edition fully verified but before
/// the catalog advanced leaves the previously selected edition in place.
#[test]
fn q_an_interrupted_selection_preserves_the_previous_selection() {
    let mut estate = estate();
    let first = staged({
        let m = estate.model.clone();
        build(&mut estate, &m)
    });
    let other = model_dir(estate.scratch.clone().as_path(), "model-d", "weights-d");
    let second = staged(build(&mut estate, &other));
    let membership = estate.membership.clone();
    estate
        .store
        .select_semantic(&membership, &first.id)
        .unwrap()
        .unwrap();
    let revision = estate.store.publication_revision();

    let output = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("child_select_crash")
        .arg("--nocapture")
        .env("W4_CHILD_ROOT", &estate.root)
        .env("W4_CHILD_EDITION", &second.id.0)
        .env("WIRK_ATLAS_FAILPOINT", "semantic-selection-verified")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(86), "{output:?}");

    let reopened = AtlasStore::open(&estate.root, estate.root.display().to_string()).unwrap();
    assert_eq!(reopened.selected_semantic(&membership), Some(first.id));
    assert_eq!(reopened.publication_revision(), revision);
}

/// Runs only inside `p_an_interrupted_build_stages_nothing`'s child.
#[test]
fn child_build_crash() {
    let Ok(root) = std::env::var("W4_CHILD_ROOT") else {
        return;
    };
    let mut store = AtlasStore::open(&root, root.clone()).unwrap();
    let membership = store
        .register_git("fixture", std::env::var("W4_CHILD_REPO").unwrap(), "HEAD")
        .unwrap();
    let generation = store.current(&membership).unwrap().unwrap().id;
    let outcome = store
        .build_semantic(
            &membership,
            &generation,
            &SemanticBuildConfig {
                backend: PathBuf::from(std::env::var("W4_CHILD_BACKEND").unwrap()),
                backend_args: Vec::new(),
                model: PathBuf::from(std::env::var("W4_CHILD_MODEL").unwrap()),
                producer: "test/v1".into(),
                chunking: wirk_atlas::SemanticChunking::Units,
            },
        )
        .unwrap();
    panic!("the failpoint should have exited before this: {outcome:?}");
}

/// Runs only inside `q_an_interrupted_selection_preserves_the_previous_selection`'s child.
#[test]
fn child_select_crash() {
    let Ok(root) = std::env::var("W4_CHILD_ROOT") else {
        return;
    };
    let Ok(edition) = std::env::var("W4_CHILD_EDITION") else {
        return;
    };
    let mut store = AtlasStore::open(&root, root.clone()).unwrap();
    let membership = store
        .memberships()
        .find(|membership| membership.alias == "fixture")
        .cloned()
        .unwrap();
    let outcome = store
        .select_semantic(&membership, &EditionId(edition))
        .unwrap();
    panic!("the failpoint should have exited before this: {outcome:?}");
}

// ---- W4-LIFECYCLE-CORRECTION.md ------------------------------------------

/// Item 1. A source that republishes leaves its selection *retained* and
/// *intact*, and no longer *available*: the edition still verifies, its
/// bytes are untouched, the selection is not cleared, and every public
/// answer says it describes a generation this source no longer publishes.
/// Watched failing against the uncorrected candidate binary, which
/// reported `available true` / `selected verified` throughout
/// (`raw/10-red.log`).
#[test]
fn r_a_superseded_generation_is_retained_but_not_available() {
    let mut estate = estate();
    let edition = staged({
        let m = estate.model.clone();
        build(&mut estate, &m)
    });
    let membership = estate.membership.clone();
    estate
        .store
        .select_semantic(&membership, &edition.id)
        .unwrap()
        .unwrap();
    assert_eq!(
        estate.store.semantic_availability(&membership).unwrap(),
        wirk_atlas::SemanticAvailability::Available
    );

    // A genuinely new generation of the same source, published.
    fs::write(
        estate._repo.path().join("code.rs"),
        "fn one() {}\nfn three() {}\n",
    )
    .unwrap();
    git(estate._repo.path(), &["add", "."]);
    git(estate._repo.path(), &["commit", "-qm", "second"]);
    let AcquireOutcome::Staged(next) = estate
        .store
        .acquire(&membership, "HEAD", ExtractorPolicy::default())
        .unwrap()
    else {
        panic!("second acquisition is unavailable");
    };
    assert_ne!(next.id, estate.generation);
    estate.store.publish(&membership, &next.id).unwrap();

    let availability = estate.store.semantic_availability(&membership).unwrap();
    assert!(matches!(
        availability,
        wirk_atlas::SemanticAvailability::Superseded(_)
    ));
    assert!(!availability.is_available());
    assert_eq!(availability.selected_available(), Some(false));
    assert_eq!(availability.label(), "superseded");
    assert!(
        availability
            .detail()
            .unwrap()
            .contains(&edition.generation.0)
    );

    // Retained, not erased: the selection stands, the edition is still on
    // disk and still verifies as the bytes it committed to.
    assert_eq!(
        estate.store.selected_semantic(&membership),
        Some(edition.id.clone())
    );
    let states = estate.store.semantic_editions(&membership).unwrap();
    let state = states.iter().find(|s| s.edition.id == edition.id).unwrap();
    assert!(state.selected);
    assert_eq!(state.verification, SemanticVerification::Verified);
    assert!(!state.current);

    // Publishing the earlier generation again makes the same retained
    // selection current once more, because availability is derived and
    // never latched.
    let generation = estate.generation.clone();
    estate.store.publish(&membership, &generation).unwrap();
    assert_eq!(
        estate.store.semantic_availability(&membership).unwrap(),
        wirk_atlas::SemanticAvailability::Available
    );
    assert!(
        estate
            .store
            .semantic_editions(&membership)
            .unwrap()
            .iter()
            .find(|s| s.edition.id == edition.id)
            .unwrap()
            .current
    );
}

/// Item 1, the other half. Selected bytes that no longer verify cannot
/// make the selection available, whatever a record with that id says.
#[test]
fn s_corrupt_or_incomplete_selected_bytes_are_not_available() {
    let mut estate = estate();
    let edition = staged({
        let m = estate.model.clone();
        build(&mut estate, &m)
    });
    let membership = estate.membership.clone();
    estate
        .store
        .select_semantic(&membership, &edition.id)
        .unwrap()
        .unwrap();

    let vectors = edition_path(&estate, &edition.id, "vectors.bin");
    let mut bytes = fs::read(&vectors).unwrap();
    bytes[0] ^= 0xff;
    fs::write(&vectors, &bytes).unwrap();
    let availability = estate.store.semantic_availability(&membership).unwrap();
    assert!(matches!(
        availability,
        wirk_atlas::SemanticAvailability::Unusable(_)
    ));
    assert_eq!(availability.selected_available(), Some(false));

    // Missing, not merely altered, and the same answer.
    fs::remove_file(edition_path(&estate, &edition.id, "mapping.ndjson")).unwrap();
    assert_eq!(
        estate
            .store
            .semantic_availability(&membership)
            .unwrap()
            .selected_available(),
        Some(false)
    );

    // Nothing selected at all is a third, distinct answer — not `false`.
    let fresh = estate_with_backend("honest");
    assert_eq!(
        fresh
            .store
            .semantic_availability(&fresh.membership)
            .unwrap()
            .selected_available(),
        None
    );
}

/// Item 4. The producer record carries every argv token in its executed
/// position, so two builds handed genuinely different backend options are
/// different editions even when their output bytes are identical.
/// Watched failing against the uncorrected candidate, where the two
/// resolved to one edition id and the literal tokens were absent from the
/// record entirely (`raw/11-red-argv.log`).
#[test]
fn t_complete_argv_is_part_of_producer_identity() {
    let mut estate = estate();
    let model = estate.model.clone();
    let script = estate.backend.clone();
    let plain = staged(build_with_args(&mut estate, &model, &[]));
    let optioned = staged(build_with_args(
        &mut estate,
        &model,
        &["--precision", "high"],
    ));
    // The stub ignores its argv, so the produced bytes are identical...
    assert_eq!(plain.vectors.digest, optioned.vectors.digest);
    assert_eq!(plain.mapping.digest, optioned.mapping.digest);
    // ...and the editions are still distinct, because the configuration
    // that produced them was.
    assert_ne!(plain.id, optioned.id);

    assert!(plain.backend.argv.is_empty());
    let argv: Vec<&str> = optioned
        .backend
        .argv
        .iter()
        .map(|argument| argument.value())
        .collect();
    assert_eq!(argv, ["--precision", "high"]);
    assert!(optioned.backend.argv.iter().all(|a| a.file().is_none()));
    // Token boundaries are identity, not an artefact of joining.
    let joined = staged(build_with_args(&mut estate, &model, &["--precisionhigh"]));
    assert_ne!(joined.id, optioned.id);

    // A file-valued token keeps its separate content binding *and* its
    // place in the ordered argv.
    let with_file = staged(build_with_args(
        &mut estate,
        &model,
        &[script.to_str().unwrap(), "--precision", "high"],
    ));
    assert_eq!(with_file.backend.arguments.len(), 1);
    assert_eq!(with_file.backend.argv.len(), 3);
    assert!(with_file.backend.argv[0].file().is_some());
    assert_eq!(with_file.backend.argv[1].value(), "--precision");
    assert_ne!(with_file.id, optioned.id);
}

/// Item 3. The backend's own account of the implementation bytes it ran
/// on is re-measured by the product before it is recorded, is bound into
/// the edition identity, and its honest *absence* stays distinguishable
/// from a measured presence.
#[test]
fn u_backend_environment_is_re_measured_bound_and_honestly_absent() {
    let mut estate = estate();
    let model = estate.model.clone();
    let scratch = estate.scratch.clone();
    let where_ = dist_info(&scratch, "stubdist-1.0.dist-info");

    // No report: recorded as absent, and it says so rather than implying
    // provenance the record does not have.
    let bare = staged(build_with_args(&mut estate, &model, &[]));
    assert_eq!(
        bare.backend.environment,
        wirk_atlas::BackendEnvironment::Unreported
    );
    // The scheme this product writes now: `v4` bound the chunker, the
    // retrieval representation and the coverage on top of everything `v3`
    // bound, and `v5` adds the grammar libraries the boundaries actually
    // came out of. `v1`/`v2`/`v3` records still read back as themselves,
    // which `v_...` and `w_g_...` below pin.
    assert_eq!(bare.identity, wirk_atlas::IDENTITY_V5);

    let reported = staged(build_with_args(
        &mut estate,
        &model,
        &["--report-environment", where_.to_str().unwrap()],
    ));
    let wirk_atlas::BackendEnvironment::Reported(environment) = &reported.backend.environment
    else {
        panic!("expected a reported environment");
    };
    assert_eq!(environment.kind, "python-distributions/v1");
    assert_eq!(environment.distributions.len(), 1);
    assert_eq!(environment.distributions[0].name, "stubdist");
    // The digest recorded is the product's own reading of those bytes.
    assert_eq!(
        environment.distributions[0].record_digest,
        sha256_hex(&fs::read(where_.join("RECORD")).unwrap())
    );
    assert_ne!(bare.id, reported.id);

    // Editing the distribution's own metadata changes the identity, so
    // "which implementation built this" survives that file being edited.
    fs::write(
        where_.join("RECORD"),
        "stubdist/__init__.py,sha256=zzz,13\n",
    )
    .unwrap();
    let after = staged(build_with_args(
        &mut estate,
        &model,
        &["--report-environment", where_.to_str().unwrap()],
    ));
    assert_ne!(after.id, reported.id);

    // A backend that misreports what it ran on is refused, exactly as one
    // that misreports the model is: the product read the same bytes.
    let reason = refusal(build_with_args(
        &mut estate,
        &model,
        &[
            "--report-environment",
            where_.to_str().unwrap(),
            "--lie-about-environment",
        ],
    ));
    assert!(reason.contains("RECORD digest"), "{reason}");
}

/// Historical readability, with no invented producer proof. An edition
/// record written under the `v1` identity scheme — before the complete
/// argv and the backend environment were bound — still reads back under
/// its own scheme, is labelled `v1`, and reports its implementation
/// provenance as absent rather than as measured.
#[test]
fn v_a_v1_edition_record_still_reads_back_as_v1() {
    let mut estate = estate();
    let edition = staged({
        let m = estate.model.clone();
        build(&mut estate, &m)
    });
    let record = edition_path(&estate, &edition.id, "edition.json");
    let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();

    // Exactly the shape W4 A's first record had: no `identity`, no
    // `argv`, no `environment`.
    {
        let object = value.as_object_mut().unwrap();
        object.remove("identity");
        object["backend"].as_object_mut().unwrap().remove("argv");
        object["backend"]
            .as_object_mut()
            .unwrap()
            .remove("environment");
    }
    let legacy = EditionId(legacy_edition_id(&value, wirk_atlas::IDENTITY_V1));
    value
        .as_object_mut()
        .unwrap()
        .insert("id".into(), serde_json::json!(legacy.0));

    let directory = estate.root.join("atlas").join("semantic").join(&legacy.0);
    fs::create_dir_all(&directory).unwrap();
    fs::write(
        directory.join("edition.json"),
        serde_json::to_vec(&value).unwrap(),
    )
    .unwrap();
    for file in ["vectors.bin", "mapping.ndjson"] {
        fs::copy(
            edition_path(&estate, &edition.id, file),
            directory.join(file),
        )
        .unwrap();
    }

    let read = estate.store.read_edition(&legacy).unwrap();
    assert_eq!(read.identity, wirk_atlas::IDENTITY_V1);
    assert_eq!(
        read.backend.environment,
        wirk_atlas::BackendEnvironment::Unreported
    );
    assert!(read.backend.argv.is_empty());
    let states = estate.store.semantic_editions(&estate.membership).unwrap();
    let state = states.iter().find(|s| s.edition.id == legacy).unwrap();
    assert_eq!(state.verification, SemanticVerification::Verified);

    // And it is still selectable and still available: a historical record
    // is readable, not resurrected with provenance it never had.
    let membership = estate.membership.clone();
    estate
        .store
        .select_semantic(&membership, &legacy)
        .unwrap()
        .unwrap();
    assert_eq!(
        estate.store.semantic_availability(&membership).unwrap(),
        wirk_atlas::SemanticAvailability::Available
    );
}

/// The `v1` edition id, recomputed here from the record's own JSON — an
/// independent reimplementation of the scheme, in the test, rather than a
/// call into the code under test.
fn legacy_edition_id(value: &serde_json::Value, scheme: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    let absorb = |hasher: &mut Sha256, part: &[u8]| {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part);
    };
    absorb(&mut hasher, scheme.as_bytes());
    let string = |pointer: &str| -> String {
        value
            .pointer(pointer)
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("missing {pointer}"))
            .to_owned()
    };
    for pointer in [
        "/estate",
        "/membership",
        "/source",
        "/generation",
        "/generation_revision",
        "/generation_content",
        "/acquisition_policy",
        "/chunker/extractor_set",
        "/chunker/unitizer",
        "/model/consumed/canonical",
        "/model/consumed/digest",
        "/model/reported_path",
        "/model/reported_digest",
        "/backend/protocol",
        "/backend/program/canonical",
        "/backend/program/digest",
        "/backend/reported",
        "/vectors/format",
        "/vectors/digest",
        "/mapping/digest",
        "/producer/producer",
    ] {
        absorb(&mut hasher, string(pointer).as_bytes());
    }
    let arguments = value
        .pointer("/backend/arguments")
        .unwrap()
        .as_array()
        .unwrap();
    absorb(&mut hasher, &(arguments.len() as u64).to_be_bytes());
    for argument in arguments {
        absorb(
            &mut hasher,
            argument["canonical"].as_str().unwrap().as_bytes(),
        );
        absorb(&mut hasher, argument["digest"].as_str().unwrap().as_bytes());
    }
    if scheme != "wirk-semantic-edition/v1" {
        // The v2/v3 additions, in the order the product absorbs them.
        absorb(
            &mut hasher,
            string("/backend/program/configured").as_bytes(),
        );
        let argv = value.pointer("/backend/argv").unwrap().as_array().unwrap();
        absorb(&mut hasher, &(argv.len() as u64).to_be_bytes());
        for argument in argv {
            match argument["kind"].as_str().unwrap() {
                "literal" => {
                    absorb(&mut hasher, b"literal");
                    absorb(&mut hasher, argument["value"].as_str().unwrap().as_bytes());
                }
                _ => {
                    absorb(&mut hasher, b"file");
                    absorb(&mut hasher, argument["value"].as_str().unwrap().as_bytes());
                    absorb(
                        &mut hasher,
                        argument["file"]["canonical"].as_str().unwrap().as_bytes(),
                    );
                    absorb(
                        &mut hasher,
                        argument["file"]["digest"].as_str().unwrap().as_bytes(),
                    );
                }
            }
        }
        match value
            .pointer("/backend/environment/state")
            .unwrap()
            .as_str()
        {
            Some("reported") => {
                absorb(&mut hasher, b"environment-reported");
                absorb(
                    &mut hasher,
                    string("/backend/environment/detail/digest").as_bytes(),
                );
            }
            _ => absorb(&mut hasher, b"environment-unreported"),
        }
    }
    for pointer in [
        "/vectors/rows",
        "/vectors/dimensions",
        "/vectors/byte_len",
        "/mapping/rows",
        "/mapping/byte_len",
    ] {
        let number = value.pointer(pointer).unwrap().as_u64().unwrap();
        absorb(&mut hasher, &number.to_be_bytes());
    }
    format!(
        "e-{}",
        hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )
}

// ---- W4-PRODUCER-PROVENANCE-CORRECTION.md --------------------------------
//
// The lifecycle correction bound the *installer's* account of the backend
// environment: `RECORD` and `METADATA` at each reported `.dist-info`, plus
// the backend's counts of how many declared files still matched. That is
// not the identity of the bytes that ran, and the independent review found
// two ordinary ways it comes apart — neither of them needing a hostile
// backend, a compromised upstream or a modified adapter. These tests pin
// the corrected contract: the loaded file's own bytes, measured by the
// product, and coverage that reads as what it is.

/// A distribution whose `RECORD` really declares a module file that really
/// exists, so "declared" can be a verified answer rather than an assumed
/// one. Returns the `.dist-info` directory and the module file.
fn dist_with_module(directory: &Path, distribution: &str, body: &str) -> (PathBuf, PathBuf) {
    let package = directory.join(distribution);
    fs::create_dir_all(&package).unwrap();
    let module = package.join("__init__.py");
    fs::write(&module, body).unwrap();
    let where_ = directory.join(format!("{distribution}-1.0.dist-info"));
    fs::create_dir_all(&where_).unwrap();
    fs::write(
        where_.join("RECORD"),
        format!(
            "{distribution}/__init__.py,sha256=abc,12\n\
             {distribution}/extra.py,sha256=def,12\n\
             {distribution}-1.0.dist-info/METADATA,,\n"
        ),
    )
    .unwrap();
    fs::write(
        where_.join("METADATA"),
        format!("Metadata-Version: 2.1\nName: {distribution}\nVersion: 1.0\n"),
    )
    .unwrap();
    (where_, module)
}

fn environment_of(edition: &SemanticEdition) -> &wirk_atlas::EnvironmentIdentity {
    match &edition.backend.environment {
        wirk_atlas::BackendEnvironment::Reported(identity) => identity,
        wirk_atlas::BackendEnvironment::Unreported => panic!("expected a reported environment"),
    }
}

/// Item 3, the root's counterexample. Two *different* implementation
/// mutations, with the distribution's `RECORD` and `METADATA` byte-identical,
/// the backend's file counts identical, and the produced vectors and mapping
/// byte-identical, must not collapse to one identity.
///
/// This is the test that fails against the lifecycle candidate: there the
/// only thing an implementation edit moved was `files_mismatched`, a
/// *count*, so two edits that both change one file are the same number and
/// therefore the same edition. A count is not the identity of the changed
/// bytes.
#[test]
fn w_a_two_implementation_mutations_with_identical_counts_are_distinct_editions() {
    let mut estate = estate();
    let model = estate.model.clone();
    let scratch = estate.scratch.clone();
    let (where_, module) = dist_with_module(&scratch, "stubdist", "VERSION = 1\n");
    let record_before = fs::read(where_.join("RECORD")).unwrap();
    let metadata_before = fs::read(where_.join("METADATA")).unwrap();

    let with = |estate: &mut Estate| {
        staged(build_with_args(
            estate,
            &model,
            &[
                "--report-environment",
                where_.to_str().unwrap(),
                "--report-modules",
                &format!("stubdist={}", module.display()),
            ],
        ))
    };

    let a = with(&mut estate);
    // Same length, different bytes: nothing a size or a count can see.
    fs::write(&module, "VERSION = 2\n").unwrap();
    let b = with(&mut estate);
    fs::write(&module, "VERSION = 3\n").unwrap();
    let c = with(&mut estate);

    // Everything the previous record bound is unchanged...
    assert_eq!(record_before, fs::read(where_.join("RECORD")).unwrap());
    assert_eq!(metadata_before, fs::read(where_.join("METADATA")).unwrap());
    for pair in [(&a, &b), (&a, &c)] {
        let (left, right) = (environment_of(pair.0), environment_of(pair.1));
        assert_eq!(left.distributions, right.distributions);
        assert_eq!(pair.0.vectors.digest, pair.1.vectors.digest);
        assert_eq!(pair.0.mapping.digest, pair.1.mapping.digest);
    }
    // ...and all three editions are distinct, by the loaded file's bytes.
    assert_ne!(a.id, b.id);
    assert_ne!(a.id, c.id);
    assert_ne!(b.id, c.id);

    // The digest recorded is the product's own reading of that file, not
    // the backend's claim about it.
    let measured = environment_of(&c);
    assert_eq!(measured.modules.len(), 1);
    assert_eq!(
        measured.modules[0].digest,
        sha256_hex(&fs::read(&module).unwrap())
    );
    assert_eq!(measured.modules[0].byte_len, 12);
    assert_eq!(
        measured.modules[0].attribution,
        wirk_atlas::ModuleAttribution::Declared("stubdist".into())
    );
    assert_eq!(measured.coverage, wirk_atlas::EnvironmentCoverage::Complete);
    assert_eq!(c.identity, wirk_atlas::IDENTITY_V5);
}

/// Item 1. A module that loads from somewhere the claiming distribution's
/// `RECORD` does not declare — a package earlier on `sys.path` than the
/// installed one — is recorded as unattributed, not as the installed
/// distribution running clean.
///
/// The lifecycle candidate answered this case with a record that positively
/// asserted `files_mismatched: 0` for code that did not run, and produced
/// the *identical* edition id. Membership is verified here against the
/// `RECORD` the product read itself; a top-level name is never enough.
#[test]
fn w_b_a_module_loaded_outside_its_distribution_is_not_attributed_to_it() {
    let mut estate = estate();
    let model = estate.model.clone();
    let scratch = estate.scratch.clone();
    let (where_, declared) = dist_with_module(&scratch, "stubdist", "VERSION = 1\n");
    // Byte-identical content, in a directory the RECORD says nothing about.
    let elsewhere = scratch.join("shadow");
    fs::create_dir_all(&elsewhere).unwrap();
    let shadow = elsewhere.join("__init__.py");
    fs::write(&shadow, "VERSION = 1\n").unwrap();

    let with = |estate: &mut Estate, path: &Path| {
        staged(build_with_args(
            estate,
            &model,
            &[
                "--report-environment",
                where_.to_str().unwrap(),
                "--report-modules",
                &format!("stubdist={}", path.display()),
            ],
        ))
    };
    let installed = with(&mut estate, &declared);
    let shadowed = with(&mut estate, &shadow);

    // The installer's account is identical in both — that is the point.
    assert_eq!(
        environment_of(&installed).distributions,
        environment_of(&shadowed).distributions
    );
    assert_eq!(installed.vectors.digest, shadowed.vectors.digest);
    // The loaded bytes are identical too, so only the *origin* differs.
    assert_eq!(
        environment_of(&installed).modules[0].digest,
        environment_of(&shadowed).modules[0].digest
    );
    assert_ne!(installed.id, shadowed.id);

    assert_eq!(
        environment_of(&installed).modules[0].attribution,
        wirk_atlas::ModuleAttribution::Declared("stubdist".into())
    );
    let wirk_atlas::ModuleAttribution::Undeclared(detail) =
        &environment_of(&shadowed).modules[0].attribution
    else {
        panic!("a module loaded outside the distribution must not be attributed to it");
    };
    assert!(detail.contains("stubdist"), "{detail}");
    assert!(detail.contains("shadow"), "{detail}");
    assert!(detail.contains("does not declare"), "{detail}");
    // And it is a coverage gap, not a silent difference: a reader is told.
    let wirk_atlas::EnvironmentCoverage::Partial(reason) = &environment_of(&shadowed).coverage
    else {
        panic!("an unattributed loaded module makes the coverage partial");
    };
    assert!(
        reason.contains("declared by no reported RECORD"),
        "{reason}"
    );
    assert!(reason.contains("stubdist: 1"), "{reason}");
    assert_eq!(
        environment_of(&installed).coverage,
        wirk_atlas::EnvironmentCoverage::Complete
    );
}

/// Item 2. A distribution the run imported but could not describe, and a
/// module the interpreter could name no file for, are both counted *and*
/// named, and the coverage reads `partial` rather than the same word a
/// complete measurement gets.
#[test]
fn w_c_partial_provenance_is_named_not_silently_skipped() {
    let mut estate = estate();
    let model = estate.model.clone();
    let scratch = estate.scratch.clone();
    let (where_, module) = dist_with_module(&scratch, "stubdist", "VERSION = 1\n");
    let base = [
        "--report-environment".to_owned(),
        where_.display().to_string(),
        "--report-modules".to_owned(),
        format!("stubdist={}", module.display()),
    ];
    let with = |estate: &mut Estate, extra: &[&str]| {
        let mut args: Vec<String> = base.to_vec();
        args.extend(extra.iter().map(|value| (*value).to_owned()));
        let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
        staged(build_with_args(estate, &model, &borrowed))
    };

    let complete = with(&mut estate, &[]);
    let short = with(&mut estate, &["--undescribed"]);
    let blind = with(&mut estate, &["--unmeasured-module"]);

    assert_eq!(
        environment_of(&complete).coverage,
        wirk_atlas::EnvironmentCoverage::Complete
    );
    assert!(
        environment_of(&complete)
            .undescribed_distributions
            .is_empty()
    );

    let wirk_atlas::EnvironmentCoverage::Partial(reason) = &environment_of(&short).coverage else {
        panic!("a distribution that could not be described makes the coverage partial");
    };
    // Which dependency is unknown, not merely how many.
    assert!(reason.contains("otherdist"), "{reason}");
    assert!(reason.contains("no readable .dist-info"), "{reason}");
    assert_eq!(environment_of(&short).undescribed_distributions.len(), 1);
    assert_eq!(
        environment_of(&short).undescribed_distributions[0].name,
        "otherdist"
    );

    let wirk_atlas::EnvironmentCoverage::Partial(reason) = &environment_of(&blind).coverage else {
        panic!("a module that could not be read makes the coverage partial");
    };
    assert!(reason.contains("stubdist._speedup"), "{reason}");
    assert_eq!(environment_of(&blind).unmeasured_modules.len(), 1);

    // Three different coverages, three different identities: the gap is
    // part of what the edition is, not a note beside it.
    assert_ne!(complete.id, short.id);
    assert_ne!(complete.id, blind.id);
    assert_ne!(short.id, blind.id);
    // The scope of the measurement is written into the record itself.
    assert_eq!(
        environment_of(&complete).scope,
        wirk_atlas::ENVIRONMENT_SCOPE_V2
    );
}

/// A backend that reported distributions but no loaded modules — every
/// backend written before this correction — records `unmeasured`, and never
/// `complete`. Manufacturing coverage for a build that never looked is the
/// same defect as minting historical producer proof.
#[test]
fn w_d_a_report_without_modules_is_unmeasured_never_complete() {
    let mut estate = estate();
    let model = estate.model.clone();
    let scratch = estate.scratch.clone();
    let (where_, module) = dist_with_module(&scratch, "stubdist", "VERSION = 1\n");

    let old = staged(build_with_args(
        &mut estate,
        &model,
        &["--report-environment", where_.to_str().unwrap()],
    ));
    assert_eq!(
        environment_of(&old).coverage,
        wirk_atlas::EnvironmentCoverage::Unmeasured
    );
    assert!(environment_of(&old).modules.is_empty());
    // No scope claim either: that record measured nothing to scope.
    assert!(environment_of(&old).scope.is_empty());

    let new = staged(build_with_args(
        &mut estate,
        &model,
        &[
            "--report-environment",
            where_.to_str().unwrap(),
            "--report-modules",
            &format!("stubdist={}", module.display()),
        ],
    ));
    assert_eq!(
        environment_of(&new).coverage,
        wirk_atlas::EnvironmentCoverage::Complete
    );
    assert_ne!(old.id, new.id);
}

/// Two independent measurements, on the module list as much as on `RECORD`:
/// a backend whose claimed module digest is not what the file digests to is
/// refused, and no edition is written.
#[test]
fn w_e_a_backend_that_misreports_a_loaded_module_is_refused() {
    let mut estate = estate();
    let model = estate.model.clone();
    let scratch = estate.scratch.clone();
    let (where_, module) = dist_with_module(&scratch, "stubdist", "VERSION = 1\n");
    let reason = refusal(build_with_args(
        &mut estate,
        &model,
        &[
            "--report-environment",
            where_.to_str().unwrap(),
            "--report-modules",
            &format!("stubdist={}", module.display()),
            "--lie-about-modules",
        ],
    ));
    assert!(reason.contains("module stubdist"), "{reason}");
    assert!(reason.contains("digest"), "{reason}");

    // A module naming a file that is not there is refused too, rather than
    // being quietly dropped from the list.
    fs::remove_file(&module).unwrap();
    let reason = refusal(build_with_args(
        &mut estate,
        &model,
        &[
            "--report-environment",
            where_.to_str().unwrap(),
            "--report-modules",
            &format!("stubdist={}", module.display()),
        ],
    ));
    assert!(reason.contains("stubdist"), "{reason}");
}

/// Item 4. A selected edition whose own record is gone is not a source
/// whose generation is gone. The state was already right; the two sentences
/// a caller actually reads described a different condition entirely.
#[test]
fn w_f_a_missing_edition_record_names_the_edition_not_the_generation() {
    let mut estate = estate();
    let edition = staged({
        let m = estate.model.clone();
        build(&mut estate, &m)
    });
    let membership = estate.membership.clone();
    estate
        .store
        .select_semantic(&membership, &edition.id)
        .unwrap()
        .unwrap();
    assert!(
        estate
            .store
            .semantic_availability(&membership)
            .unwrap()
            .is_available()
    );

    fs::remove_file(edition_path(&estate, &edition.id, "edition.json")).unwrap();
    let availability = estate.store.semantic_availability(&membership).unwrap();
    let wirk_atlas::SemanticAvailability::Unreadable(detail) = &availability else {
        panic!("expected unreadable, got {availability:?}");
    };
    assert!(detail.contains("edition"), "{detail}");
    assert!(
        !detail.contains("generation is incomplete or absent"),
        "the edition record is what is missing, not the source generation: {detail}"
    );
    // The selection itself is untouched: nothing is erased to hide this.
    assert_eq!(
        estate.store.selected_semantic(&membership),
        Some(edition.id)
    );
}

/// Migration, both directions of honesty. A `v2` record — written by the
/// lifecycle candidate, which bound the complete argv and the installer's
/// account of the environment but never looked at a loaded module — still
/// verifies against the identity it already carries, still reads back
/// labelled `v2`, and reports its module coverage as `unmeasured`.
///
/// Nothing is recomputed and nothing is upgraded: the environment digest
/// the record stores is the one its identity was built over, which is why
/// a scheme change never rewrites a prior edition's bytes.
#[test]
fn w_g_a_v2_edition_record_reads_back_as_v2_with_unmeasured_coverage() {
    let mut estate = estate();
    let model = estate.model.clone();
    let scratch = estate.scratch.clone();
    let (where_, module) = dist_with_module(&scratch, "stubdist", "VERSION = 1\n");
    let edition = staged(build_with_args(
        &mut estate,
        &model,
        &[
            "--report-environment",
            where_.to_str().unwrap(),
            "--report-modules",
            &format!("stubdist={}", module.display()),
        ],
    ));
    let record = edition_path(&estate, &edition.id, "edition.json");
    let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();
    {
        let object = value.as_object_mut().unwrap();
        object["identity"] = serde_json::json!(wirk_atlas::IDENTITY_V2);
        let environment = object["backend"]["environment"]["detail"]
            .as_object_mut()
            .unwrap();
        for field in [
            "modules",
            "undescribed_distributions",
            "unmeasured_modules",
            "scope",
            "coverage",
        ] {
            environment.remove(field);
        }
    }
    // A `v2` record is genuinely a different edition — the scheme name is
    // absorbed — so this is written as the `v2` edition it is, at the id
    // the `v2` computation gives, rather than a `v3` record relabelled.
    let legacy = EditionId(legacy_edition_id(&value, wirk_atlas::IDENTITY_V2));
    value.as_object_mut().unwrap()["id"] = serde_json::json!(legacy.0);
    let directory = estate.root.join("atlas").join("semantic").join(&legacy.0);
    fs::create_dir_all(&directory).unwrap();
    let record = directory.join("edition.json");
    fs::write(&record, serde_json::to_vec(&value).unwrap()).unwrap();

    // Reads back, and verifies: the stored environment digest is what its
    // identity was computed over, and that has not moved.
    let read = estate.store.read_edition(&legacy).unwrap();
    assert_eq!(read.id, legacy);
    assert_eq!(read.identity, wirk_atlas::IDENTITY_V2);
    let environment = environment_of(&read);
    assert_eq!(environment.digest, environment_of(&edition).digest);
    assert_eq!(
        environment.distributions,
        environment_of(&edition).distributions
    );
    // The part it never measured reads as never measured.
    assert_eq!(
        environment.coverage,
        wirk_atlas::EnvironmentCoverage::Unmeasured
    );
    assert!(environment.modules.is_empty());
    assert!(environment.scope.is_empty());

    // And the record on disk is unchanged by having been read.
    let after: serde_json::Value = serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();
    assert_eq!(after, value);
}
