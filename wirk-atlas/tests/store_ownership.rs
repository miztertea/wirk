//! B1 (P4.5, ruling 0237): `AtlasStore` lifetime ownership.
//!
//! Every check here runs real processes against a real on-disk
//! `wirk-embed/v2` backend and pins the interleaving with a **fifo**, not
//! a sleep: the build is held inside `finish_build` — after
//! `semantic.rs`'s `.tmp-<ULID>` staging directory exists and while the
//! store still owns it — until this test writes the fifo. What a second
//! `AtlasStore::open` does in that window is the whole question.
//!
//! Watched red at `bf163696343b9f76cc13b20cda29b7b013bbd7b7`: the second
//! `open` succeeded and `store.rs`'s sweep `remove_dir_all`'d the live
//! staging directory, so the build's own `rename` failed `ENOENT` and the
//! expensive work was lost. "Abandoned" (`store.rs:49`) was an
//! assumption, not a liveness check.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;
use wirk_atlas::{
    AcquireOutcome, AtlasStore, ExtractorPolicy, Membership, SemanticBuildConfig,
    SemanticBuildOutcome,
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
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// Deliberately tiny: two files, a handful of units. This check is about
/// the ownership window, not about embedding cost.
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

fn model_dir(directory: &Path, name: &str) -> PathBuf {
    let model = directory.join(name);
    fs::create_dir_all(&model).unwrap();
    fs::write(model.join("config.json"), format!("{{\"m\":\"{name}\"}}")).unwrap();
    fs::write(model.join("model.bin"), b"weights").unwrap();
    model
}

/// A real `wirk-embed/v2` backend that first announces it has started by
/// creating `started`, then **blocks reading `gate`** (a fifo) before
/// producing anything. `run_backend_v2` `env_clear()`s, so both paths are
/// baked into the script's own text rather than passed through the
/// environment.
fn gated_backend(directory: &Path, started: &Path, gate: &Path) -> PathBuf {
    let script = directory.join("gated-backend.py");
    fs::write(
        &script,
        format!(
            r#"#!/usr/bin/env python3
import hashlib, json, os, struct, sys

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
texts = [json.loads(line)["text"] for line in sys.stdin if line.strip()]
# The staging directory now exists and this build owns it.
open({started:?}, "w").close()
# Block until the test opens the other end. No sleep anywhere.
open({gate:?}, "r").read()
digest = model_digest(header["model_path"])
with open(header["output"], "wb") as handle:
    for text in texts:
        seed = hashlib.sha256(digest.encode() + b"\x00" + text.encode()).digest()
        handle.write(struct.pack("<4f", *(b / 255.0 for b in seed[:4])))
print(json.dumps({{
    "protocol": "wirk-embed/v2",
    "backend": "gated-backend/1",
    "model_path": header["model_path"],
    "model_digest": digest,
    "rows": len(texts),
    "dimensions": 4,
}}))
"#,
            started = started.display().to_string(),
            gate = gate.display().to_string(),
        ),
    )
    .unwrap();
    let mut permissions = fs::metadata(&script).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    fs::set_permissions(&script, permissions).unwrap();
    script
}

fn mkfifo(path: &Path) {
    let status = Command::new("mkfifo").arg(path).status().unwrap();
    assert!(status.success(), "mkfifo {}", path.display());
}

struct Fixture {
    _repo: TempDir,
    _home: TempDir,
    root: PathBuf,
    store: AtlasStore,
    membership: Membership,
    generation: wirk_atlas::GenerationId,
    scratch: PathBuf,
}

fn fixture() -> Fixture {
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
    Fixture {
        _repo: repo,
        _home: home,
        root,
        store,
        membership,
        generation: generation.id,
        scratch,
    }
}

/// Bounded: never spin longer than this for a real event the other side
/// is actually producing. A failure here is a real failure, not a slow box.
fn wait_for(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while std::time::Instant::now() < deadline {
        if ready() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("timed out waiting for {what}");
}

fn staging_entries(root: &Path) -> Vec<PathBuf> {
    let semantic = root.join("atlas").join("semantic");
    let Ok(entries) = fs::read_dir(&semantic) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(".tmp-"))
        .map(|entry| entry.path())
        .collect()
}

/// **The decisive check.** A second opener must not delete a live build's
/// staging directory, and the build it did not disturb must still finish.
#[test]
fn a_second_opener_is_refused_and_live_staging_survives() {
    let mut fixture = fixture();
    let started = fixture.scratch.join("started");
    let gate = fixture.scratch.join("gate");
    mkfifo(&gate);
    let backend = gated_backend(&fixture.scratch, &started, &gate);
    let model = model_dir(&fixture.scratch, "model-a");
    let root = fixture.root.clone();
    let membership = fixture.membership.clone();
    let generation = fixture.generation.clone();

    let build = std::thread::spawn(move || {
        let configuration = SemanticBuildConfig {
            backend,
            backend_args: Vec::new(),
            model,
            producer: "test/v1".into(),
            chunking: wirk_atlas::SemanticChunking::Units,
        };
        let outcome = fixture
            .store
            .build_semantic(&membership, &generation, &configuration);
        (fixture, outcome)
    });

    wait_for("the gated backend to reach its gate", || started.exists());
    let live = staging_entries(&root);
    assert_eq!(
        live.len(),
        1,
        "exactly one live staging directory must exist while the build is gated, saw {live:?}"
    );
    let live = live.into_iter().next().unwrap();

    // The window. At base this `open` succeeded and swept `live` away.
    let second = AtlasStore::open(&root, root.display().to_string());
    let refusal = match second {
        Ok(_) => panic!(
            "a second AtlasStore::open succeeded while the first store was live and owned \
             staging at {} — 'abandoned' is an assumption, not a liveness check",
            live.display()
        ),
        Err(err) => err.to_string(),
    };
    assert!(
        refusal.contains("already owned") || refusal.contains("in use"),
        "the refusal must name the contention, got: {refusal}"
    );
    assert!(
        live.exists(),
        "the live staging directory {} must survive a refused second open",
        live.display()
    );

    // Release the gate and let the build it never disturbed finish.
    fs::write(&gate, b"go").unwrap();
    let (fixture, outcome) = build.join().unwrap();
    let outcome = outcome.expect("the undisturbed build must not error");
    let SemanticBuildOutcome::Staged(edition) = outcome else {
        panic!("the undisturbed build must stage an edition, got {outcome:?}");
    };
    assert!(
        !live.exists(),
        "staging must have been renamed away once the build finished"
    );
    let verification = fixture.store.verify_edition(&edition);
    assert!(
        matches!(verification, wirk_atlas::SemanticVerification::Verified),
        "the edition the second opener did not destroy must verify, got {verification:?}"
    );
}

/// Negative control: ownership must not break the ordinary sequential
/// case. A store that has been dropped releases its claim, and the next
/// `open` — the normal restart — succeeds and still sweeps the
/// temporaries that really are abandoned.
#[test]
fn a_released_store_reopens_and_still_sweeps_genuinely_abandoned_temporaries() {
    let fixture = fixture();
    let root = fixture.root.clone();
    drop(fixture);

    let abandoned = root.join("atlas").join("semantic");
    fs::create_dir_all(&abandoned).unwrap();
    let stale = abandoned.join(".tmp-01ABANDONED");
    fs::create_dir_all(&stale).unwrap();
    fs::write(stale.join("vectors.bin"), b"partial").unwrap();

    let reopened = AtlasStore::open(&root, root.display().to_string())
        .expect("a released store must reopen; the lock is a lifetime, not a tombstone");
    assert!(
        !stale.exists(),
        "a genuinely abandoned temporary must still be swept once ownership is held"
    );
    drop(reopened);

    AtlasStore::open(&root, root.display().to_string())
        .expect("and again: dropping the store releases ownership every time");
}

/// An ungated backend of the same protocol, for the "the estate still
/// works afterwards" half of the cancellation check.
fn plain_backend(directory: &Path) -> PathBuf {
    let script = directory.join("plain-backend.py");
    fs::write(
        &script,
        r#"#!/usr/bin/env python3
import hashlib, json, os, struct, sys

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
texts = [json.loads(line)["text"] for line in sys.stdin if line.strip()]
digest = model_digest(header["model_path"])
with open(header["output"], "wb") as handle:
    for text in texts:
        seed = hashlib.sha256(digest.encode() + b"\x00" + text.encode()).digest()
        handle.write(struct.pack("<4f", *(b / 255.0 for b in seed[:4])))
print(json.dumps({
    "protocol": "wirk-embed/v2",
    "backend": "plain-backend/1",
    "model_path": header["model_path"],
    "model_digest": digest,
    "rows": len(texts),
    "dimensions": 4,
}))
"#,
    )
    .unwrap();
    let mut permissions = fs::metadata(&script).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    fs::set_permissions(&script, permissions).unwrap();
    script
}

/// **The decisive cancellation check** (P4.5 B2 correction).
///
/// A build is blocked inside its real backend child, holding the store,
/// with its staging directory live — exactly the state a cancellation
/// has to reach. The cancel goes through the registry handle taken
/// *before* the store moved, which is what the daemon's cancel verb
/// does: it never takes the atlas lock, so it does not queue behind the
/// build it is stopping.
///
/// The gate is **never written**. If the child only ended because the
/// fifo was released, this would prove nothing; it ends because it was
/// killed.
#[test]
fn a_cancellation_reaches_a_running_build_that_holds_the_store() {
    let mut fixture = fixture();
    let started = fixture.scratch.join("started");
    let gate = fixture.scratch.join("gate");
    mkfifo(&gate);
    let backend = gated_backend(&fixture.scratch, &started, &gate);
    let model = model_dir(&fixture.scratch, "model-a");
    let plain = plain_backend(&fixture.scratch);
    let root = fixture.root.clone();
    let membership = fixture.membership.clone();
    let generation = fixture.generation.clone();

    // The handle an operator's cancellation reaches the job through,
    // taken before the store is moved out of reach behind the build.
    let registry = fixture.store.job_registry();

    let build = std::thread::spawn({
        let membership = membership.clone();
        let generation = generation.clone();
        move || {
            let configuration = SemanticBuildConfig {
                backend,
                backend_args: Vec::new(),
                model,
                producer: "test/v1".into(),
                chunking: wirk_atlas::SemanticChunking::Units,
            };
            let outcome = fixture
                .store
                .build_semantic(&membership, &generation, &configuration);
            (fixture, outcome)
        }
    });

    wait_for("the gated backend to reach its gate", || started.exists());
    assert_eq!(
        staging_entries(&root).len(),
        1,
        "the build owns live staging while it is gated"
    );

    // The job is visible, and it names what it is working on so a
    // cancellation can be aimed rather than blunt.
    wait_for("the running job to appear in the registry", || {
        !registry.list().is_empty()
    });
    let running = registry.list();
    assert_eq!(running.len(), 1, "one job is running, saw {running:?}");
    assert_eq!(running[0].verb, "semantic build backend");
    assert_eq!(
        running[0].scope, membership.alias,
        "the job names its source alias, which is the cancellation target"
    );

    // Aim at the source, not at everything.
    let acknowledged = registry.cancel(
        &wirk_core::jobs::JobSelector::Scope(membership.alias.clone()),
        "was cancelled by an operator (test)",
    );
    assert_eq!(acknowledged.len(), 1, "the running build was reached");

    // The gate is deliberately never written.
    let (fixture, outcome) = build.join().unwrap();
    let outcome = outcome.expect("a cancelled build is an outcome, not an error");
    let SemanticBuildOutcome::Refused(reason) = outcome else {
        panic!("a cancelled build must refuse, not stage: {outcome:?}");
    };
    assert!(
        reason.contains("cancelled by an operator"),
        "the refusal names the deliberate act rather than a backend failure: {reason}"
    );
    assert!(
        reason.contains("not a backend failure"),
        "build refusal wording is preserved: {reason}"
    );

    // Scoped cleanup: the cancelled build's staging is gone, and the job
    // left the registry so "still running" stays a truthful answer.
    assert!(
        staging_entries(&root).is_empty(),
        "a cancelled build leaves no staging behind: {:?}",
        staging_entries(&root)
    );
    assert!(
        registry.list().is_empty(),
        "the cancelled job left the registry"
    );

    // And the estate still works. Under the single sticky store-wide
    // token this build died on arrival without running at all.
    let mut fixture = fixture;
    let after = fixture
        .store
        .build_semantic(
            &membership,
            &generation,
            &SemanticBuildConfig {
                backend: plain,
                backend_args: Vec::new(),
                model: model_dir(&fixture.scratch, "model-a"),
                producer: "test/v1".into(),
                chunking: wirk_atlas::SemanticChunking::Units,
            },
        )
        .expect("legitimate work after a cancellation must run");
    let SemanticBuildOutcome::Staged(edition) = after else {
        panic!("the estate must be usable after a cancellation, got {after:?}");
    };
    assert!(matches!(
        fixture.store.verify_edition(&edition),
        wirk_atlas::SemanticVerification::Verified
    ));
}
