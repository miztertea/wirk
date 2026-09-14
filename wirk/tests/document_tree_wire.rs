//! The document-tree acquisition policy through the real public
//! CLI/daemon wire path — never a
//! library call into `wirk_atlas` directly (that is
//! `wirk-atlas/tests/document_tree.rs`'s own scope, mirroring
//! `source_substrate.rs`'s Git-path split of the same concern).
//! Exercises what only the wire path can: `--kind document-tree`
//! parsing and its default `--revision`, `atlas status`'s disclosed
//! `acquisition_policy`, non-mutating `search`, and `atlas remove`'s
//! corrected reply shape.

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[path = "../src/wirkd/mod.rs"]
mod wirkd;

use wirkd::WirkdPointer;

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
            "wirkd pointer file never appeared (readable) at {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Give this estate its own expensive-job host pool, unless the check
/// wrote its own `resources.json` first. The checks below that care
/// about the policy call `write_policy` before starting the daemon and
/// must keep winning outright: a chosen `max_host_expensive`, a chosen
/// pool, extra keys.
///
/// Every other check here fell through to the unset default,
/// `$XDG_RUNTIME_DIR/wirk/expensive` — this uid's own runtime
/// directory, shared with every other wirk job on this box, including
/// every other test estate a normal-parallel `cargo test` starts at
/// once. `atlas acquire` takes that pool's admission slot, so those
/// checks raced unrelated estates and lost to `HostExpensiveBusy`
/// (ruling 0291's class; `nested_harness.rs`, `evidence_locality.rs`
/// and `source_substrate.rs` carry the same correction, and this file
/// has its own `start_wirkd` so it did not inherit it). Nested under
/// the estate's own path: unique for the life of its tempdir, cleaned
/// up with it, and never the live default pool a real concurrent
/// `wirk` job on this host might be using.
fn ensure_isolated_host_pool(estate: &Path) {
    let wirk_dir = estate.join(".wirk");
    if wirk_dir.join("resources.json").exists() {
        return;
    }
    let pool = wirk_dir.join("host-pool");
    write_policy(estate, &pool, &[]);
}

fn start_wirkd(estate: &Path) -> KillOnDrop {
    ensure_isolated_host_pool(estate);
    let child = KillOnDrop(
        wirk_cli()
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
    let stop = wirk_cli()
        .args(["wirkd", "stop", "--estate"])
        .arg(estate)
        .output()
        .expect("wirkd stop runs");
    assert!(
        stop.status.success(),
        "wirkd stop failed: {}",
        String::from_utf8_lossy(&stop.stderr)
    );
    let exit_status = child.0.wait().expect("reap wirkd child");
    assert!(
        exit_status.success(),
        "wirkd did not exit clean: {exit_status:?}"
    );
}

/// Runs `wirk atlas <args> --estate <estate> --json`, exactly the
/// `source_substrate.rs` convention, so a reviewer reading both files
/// sees the same call shape for the Git and document-tree paths.
fn atlas(estate: &Path, args: &[&str]) -> (bool, serde_json::Value, String) {
    let mut full = vec!["atlas"];
    full.extend_from_slice(args);
    full.push("--estate");
    let estate = estate.to_str().expect("estate path is utf-8");
    full.push(estate);
    full.push("--json");
    let output = wirk_cli().args(&full).output().expect("wirk atlas runs");
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let value = serde_json::from_str(&stdout).unwrap_or(serde_json::Value::Null);
    (output.status.success(), value, stderr)
}

fn first_hit_coordinate(search_result: &serde_json::Value) -> String {
    search_result["hits"][0]["coordinate"]
        .as_str()
        .expect("search result has at least one hit with a coordinate")
        .to_string()
}

#[test]
fn kind_document_tree_defaults_revision_and_discloses_the_policy() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    let wirkd_child = start_wirkd(&estate);

    let docs = dir.path().join("docs");
    fs::create_dir_all(&docs).unwrap();
    fs::write(docs.join("brief.md"), "# Client brief\n\nHello.\n").unwrap();

    // No --revision at all: --kind document-tree supplies its own
    // sentinel client-side, never a made-up
    // Git-shaped default.
    let (ok, acquired, err) = atlas(
        &estate,
        &[
            "acquire",
            "--source",
            "docs",
            "--repository",
            docs.to_str().unwrap(),
            "--kind",
            "document-tree",
        ],
    );
    assert!(ok, "acquire --kind document-tree failed: {err}");
    assert_eq!(acquired["outcome"].as_str(), Some("staged"));
    assert_eq!(
        acquired["membership"]["acquisition_policy"].as_str(),
        Some("document-tree-policy/v1")
    );

    let (ok, status, err) = atlas(&estate, &["status", "--source", "docs"]);
    assert!(ok, "status --source docs failed: {err}");
    assert_eq!(
        status["sources"][0]["membership"]["acquisition_policy"].as_str(),
        Some("document-tree-policy/v1"),
        "status: {status}"
    );

    stop_wirkd(&estate, wirkd_child);
}

#[test]
fn kind_document_tree_refuses_an_explicit_git_shaped_revision() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    let wirkd_child = start_wirkd(&estate);

    let docs = dir.path().join("docs");
    fs::create_dir_all(&docs).unwrap();
    fs::write(docs.join("a.md"), "# a\n").unwrap();

    let (ok, reply, err) = atlas(
        &estate,
        &[
            "acquire",
            "--source",
            "docs",
            "--repository",
            docs.to_str().unwrap(),
            "--kind",
            "document-tree",
            "--revision",
            "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
        ],
    );
    // The CLI passes the caller's value through unchanged; the daemon
    // is the one place that can express what this policy actually
    // observed, and it refuses by name rather than silently recording
    // an honoured-looking value it never checked.
    assert!(
        !ok,
        "an arbitrary revision must be refused for a document-tree source, got: {reply}"
    );
    assert!(
        err.contains("current"),
        "refusal should name the only revision this policy honours; stderr={err}"
    );

    stop_wirkd(&estate, wirkd_child);
}

#[test]
fn document_tree_publish_search_resolve_round_trip_and_search_does_not_mutate() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    let wirkd_child = start_wirkd(&estate);

    let docs = dir.path().join("docs");
    fs::create_dir_all(&docs).unwrap();
    fs::write(
        docs.join("brief.md"),
        "# Client brief\n\nvalidate_claim lives here.\n",
    )
    .unwrap();

    let (ok, acquired, err) = atlas(
        &estate,
        &[
            "acquire",
            "--source",
            "docs",
            "--repository",
            docs.to_str().unwrap(),
            "--kind",
            "document-tree",
        ],
    );
    assert!(ok, "acquire failed: {err}");
    let generation = acquired["generation"]["generation"].as_str().unwrap();
    let (ok, _, err) = atlas(
        &estate,
        &["publish", "--source", "docs", "--generation", generation],
    );
    assert!(ok, "publish failed: {err}");

    let (ok, status_before, err) = atlas(&estate, &["status"]);
    assert!(ok, "status before search failed: {err}");
    let revision_before = status_before["publication_revision"].as_u64().unwrap();

    let (ok, search_result, err) = atlas(&estate, &["search", "--query", "validate_claim"]);
    assert!(ok, "search failed: {err}");
    assert_eq!(search_result["hits"].as_array().unwrap().len(), 1);
    let coordinate = first_hit_coordinate(&search_result);

    let (ok, status_after, err) = atlas(&estate, &["status"]);
    assert!(ok, "status after search failed: {err}");
    assert_eq!(
        status_after["publication_revision"].as_u64().unwrap(),
        revision_before,
        "search must never advance the catalog's own publication revision"
    );

    let (ok, resolved, err) = atlas(&estate, &["resolve", "--coordinate", &coordinate]);
    assert!(ok, "resolve failed: {err}");
    assert_eq!(resolved["outcome"].as_str(), Some("resolved"));
    assert!(
        resolved["text"]
            .as_str()
            .unwrap_or_default()
            .contains("validate_claim")
    );

    stop_wirkd(&estate, wirkd_child);
}

#[test]
fn document_tree_resolve_survives_an_unrelated_edit_and_discloses_unavailable_after_the_resolved_file_changes()
 {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    let wirkd_child = start_wirkd(&estate);

    let docs = dir.path().join("docs");
    fs::create_dir_all(&docs).unwrap();
    fs::write(docs.join("stable.md"), "# stable\n").unwrap();
    fs::write(docs.join("other.md"), "# v1\n").unwrap();

    let (ok, acquired, err) = atlas(
        &estate,
        &[
            "acquire",
            "--source",
            "docs",
            "--repository",
            docs.to_str().unwrap(),
            "--kind",
            "document-tree",
        ],
    );
    assert!(ok, "acquire failed: {err}");
    let generation = acquired["generation"]["generation"].as_str().unwrap();
    let (ok, _, err) = atlas(
        &estate,
        &["publish", "--source", "docs", "--generation", generation],
    );
    assert!(ok, "publish failed: {err}");

    let (ok, search_result, err) = atlas(&estate, &["search", "--query", "stable"]);
    assert!(ok, "search failed: {err}");
    let stable_coordinate = first_hit_coordinate(&search_result);

    // An edit to a different file must not disturb the unchanged one.
    fs::write(docs.join("other.md"), "# v2, changed\n").unwrap();
    let (ok, resolved, err) = atlas(&estate, &["resolve", "--coordinate", &stable_coordinate]);
    assert!(ok, "resolve after unrelated edit failed: {err}");
    assert_eq!(resolved["outcome"].as_str(), Some("resolved"));

    // Now edit the resolved file itself and refresh/publish: the *old*
    // coordinate discloses Unavailable rather than stale or wrong bytes.
    fs::write(docs.join("stable.md"), "# stable v2\n").unwrap();
    let (ok, refreshed, err) = atlas(
        &estate,
        &["refresh", "--source", "docs", "--revision", "current"],
    );
    assert!(ok, "refresh failed: {err}");
    let refreshed_generation = refreshed["generation"]["generation"].as_str().unwrap();
    let (ok, _, err) = atlas(
        &estate,
        &[
            "publish",
            "--source",
            "docs",
            "--generation",
            refreshed_generation,
        ],
    );
    assert!(ok, "publish refreshed generation failed: {err}");

    let (ok, stale, err) = atlas(&estate, &["resolve", "--coordinate", &stable_coordinate]);
    // Deliberately *not* exit 0. Ruling 0093 (`call_expecting_outcome`)
    // made an unresolvable `resolve` exit non-zero precisely because
    // "failed operations return exit 0" was the defect: a caller that
    // only checks the exit status must not read an unavailable
    // coordinate as a resolved one. The full JSON is still printed —
    // the diagnostic is never withheld — which is what carries the
    // disclosure below.
    assert!(
        !ok,
        "an unavailable coordinate must not exit 0 (ruling 0093): {stale} / {err}"
    );
    assert_eq!(
        stale["outcome"].as_str(),
        Some("unavailable"),
        "a coordinate whose own file changed must disclose unavailable, not resolve stale bytes: {stale}"
    );

    stop_wirkd(&estate, wirkd_child);
}

#[test]
fn atlas_remove_unregisters_and_names_the_estate_clean_reclaim_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    let wirkd_child = start_wirkd(&estate);

    let docs = dir.path().join("docs");
    fs::create_dir_all(&docs).unwrap();
    fs::write(docs.join("a.md"), "# original\n").unwrap();

    let (ok, acquired, err) = atlas(
        &estate,
        &[
            "acquire",
            "--source",
            "docs",
            "--repository",
            docs.to_str().unwrap(),
            "--kind",
            "document-tree",
        ],
    );
    assert!(ok, "acquire failed: {err}");
    let generation = acquired["generation"]["generation"]
        .as_str()
        .unwrap()
        .to_string();
    let (ok, _, err) = atlas(
        &estate,
        &["publish", "--source", "docs", "--generation", &generation],
    );
    assert!(ok, "publish failed: {err}");

    let (ok, removed, err) = atlas(&estate, &["remove", "--source", "docs"]);
    assert!(ok, "remove failed: {err}");
    assert_eq!(removed["outcome"].as_str(), Some("removed"));
    assert_eq!(
        removed["released_generation"].as_str(),
        Some(generation.as_str())
    );
    assert!(
        removed["reclaim"]
            .as_str()
            .unwrap_or_default()
            .contains("wirk estate clean"),
        "the reply must name the actual reclaim path, not silently claim the bytes are gone: {removed}"
    );

    let (ok, status, err) = atlas(&estate, &["status", "--source", "docs"]);
    assert!(ok, "status after remove failed: {err}");
    assert_eq!(
        status["registered"].as_bool(),
        Some(false),
        "docs must no longer be a registered source: {status}"
    );

    // The original file is, as always, completely untouched.
    assert_eq!(
        fs::read_to_string(docs.join("a.md")).unwrap(),
        "# original\n"
    );

    stop_wirkd(&estate, wirkd_child);
}

/// Source-neutral budget disclosure on a document resolve.
///
/// `resolve` discloses how large the whole resource is, so a caller can
/// tell a deliberately narrow span from the entire thing. For a Git
/// source that length comes from the object store. A document collection
/// has no object store and no repository to run `git` in, so asking Git
/// for it produced a failed process and a `null` length — the disclosure
/// silently absent for exactly the source kind that has the file sitting
/// right there. The length is recorded on the generation's own resource
/// record at acquisition, and that is what must come back.
#[test]
fn a_document_resolve_discloses_the_real_total_bytes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    let wirkd_child = start_wirkd(&estate);

    let docs = dir.path().join("docs");
    fs::create_dir_all(&docs).unwrap();
    let body = "# Ledger\n\nThe reconciliation token is quiddington.\n";
    fs::write(docs.join("ledger.md"), body).unwrap();

    let (ok, acquired, err) = atlas(
        &estate,
        &[
            "acquire",
            "--source",
            "docs",
            "--repository",
            docs.to_str().unwrap(),
            "--kind",
            "document-tree",
        ],
    );
    assert!(ok, "acquire failed: {err}");
    let generation = acquired["generation"]["generation"].as_str().unwrap();
    let (ok, _, err) = atlas(
        &estate,
        &["publish", "--source", "docs", "--generation", generation],
    );
    assert!(ok, "publish failed: {err}");

    let (ok, search_result, err) = atlas(&estate, &["search", "--query", "quiddington"]);
    assert!(ok, "search failed: {err}");
    let coordinate = first_hit_coordinate(&search_result);

    let (ok, resolved, err) = atlas(&estate, &["resolve", "--coordinate", &coordinate]);
    assert!(ok, "resolve failed: {err}");
    assert_eq!(
        resolved["budget"]["total_bytes"].as_u64(),
        Some(body.len() as u64),
        "a document resolve must disclose the real file length, not null: {resolved}"
    );
    let returned = resolved["budget"]["returned_bytes"]
        .as_u64()
        .expect("returned_bytes");
    assert!(
        returned <= body.len() as u64,
        "the returned span cannot exceed the whole resource: {resolved}"
    );

    stop_wirkd(&estate, wirkd_child);
}

/// `refresh` on a document source needs no `--revision`.
///
/// `acquire` has always been able to default it, because `--kind` tells
/// it what sort of source is being registered. `refresh` never
/// re-registers and so was never told a kind, which left a document
/// collection having to be refreshed by typing the literal string the
/// help text itself describes as not a revision. The membership already
/// records the policy; the daemon reads it.
#[test]
fn refresh_needs_no_revision_for_a_document_source() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    let wirkd_child = start_wirkd(&estate);

    let docs = dir.path().join("docs");
    fs::create_dir_all(&docs).unwrap();
    fs::write(docs.join("a.md"), "# one\n").unwrap();

    let (ok, acquired, err) = atlas(
        &estate,
        &[
            "acquire",
            "--source",
            "docs",
            "--repository",
            docs.to_str().unwrap(),
            "--kind",
            "document-tree",
        ],
    );
    assert!(ok, "acquire failed: {err}");
    let first = acquired["generation"]["generation"]
        .as_str()
        .unwrap()
        .to_string();

    fs::write(docs.join("a.md"), "# two\n").unwrap();
    let (ok, refreshed, err) = atlas(&estate, &["refresh", "--source", "docs"]);
    assert!(ok, "refresh with no --revision failed: {err}");
    let second = refreshed["generation"]["generation"].as_str().unwrap();
    assert_ne!(
        first, second,
        "an edited collection must capture as a different generation"
    );

    stop_wirkd(&estate, wirkd_child);
}

// ---------------------------------------------------------------------
// Document work is registered, cancellable and bounded — publicly
// ---------------------------------------------------------------------
//
// A document collection is walked, read and extracted **in this
// process**, under the daemon's single atlas mutex. So the job has to
// announce itself in the registry `atlas cancel` reads — which is held
// beside that mutex precisely so a cancellation does not queue behind
// the work it means to stop — and the work has to look at its own stop
// condition often enough to act on one. Both halves are exercised here
// through the public CLI only.

/// The estate's own host pool, so these fixtures do not bound, or get
/// bounded by, unrelated work on this box.
fn write_policy(estate: &Path, pool: &Path, extra: &[(&str, serde_json::Value)]) {
    fs::create_dir_all(estate.join(".wirk")).unwrap();
    let mut policy = serde_json::Map::new();
    policy.insert(
        "host_pool_dir".to_string(),
        serde_json::Value::String(pool.to_string_lossy().into_owned()),
    );
    for (key, value) in extra {
        policy.insert((*key).to_string(), value.clone());
    }
    fs::write(
        estate.join(".wirk").join("resources.json"),
        serde_json::to_string(&serde_json::Value::Object(policy)).unwrap(),
    )
    .unwrap();
}

fn start_wirkd_with(estate: &Path, envs: &[(&str, String)]) -> KillOnDrop {
    ensure_isolated_host_pool(estate);
    let mut command = wirk_cli();
    command
        .args(["wirkd", "start", "--estate"])
        .arg(estate)
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    for (key, value) in envs {
        command.env(key, value);
    }
    let child = KillOnDrop(command.spawn().expect("spawn wirkd"));
    wait_for_pointer(estate);
    child
}

/// A deadline that has already passed stops document work at its first
/// checkpoint, and **nothing else moves**: what was published stays
/// published, and the estate does the same work successfully as soon as
/// the deadline is a usable one.
///
/// The deadline is zero rather than short on purpose. There is no
/// elapsed time to wait for and no timing race in the control: a zero
/// deadline has passed by the first checkpoint, which is exactly what
/// it already means for a bounded child process.
#[test]
fn a_passed_deadline_refuses_document_work_preserves_the_publication_and_lets_a_later_job_run() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    let pool = dir.path().join("host-pool");
    fs::create_dir_all(&estate).unwrap();
    let docs = dir.path().join("docs");
    fs::create_dir_all(&docs).unwrap();
    fs::write(docs.join("brief.md"), "# Client brief\n\nHello.\n").unwrap();

    // First, with an ordinary deadline: acquire and publish for real.
    write_policy(&estate, &pool, &[]);
    let wirkd_child = start_wirkd(&estate);
    let (ok, acquired, err) = atlas(
        &estate,
        &[
            "acquire",
            "--source",
            "docs",
            "--repository",
            docs.to_str().unwrap(),
            "--kind",
            "document-tree",
        ],
    );
    assert!(ok, "acquire failed: {err}");
    let published_generation = acquired["generation"]["generation"]
        .as_str()
        .expect("staged generation id")
        .to_string();
    let (ok, _, err) = atlas(
        &estate,
        &[
            "publish",
            "--source",
            "docs",
            "--generation",
            &published_generation,
        ],
    );
    assert!(ok, "publish failed: {err}");
    stop_wirkd(&estate, wirkd_child);

    // Now with a deadline that has already passed. wirkd reads its
    // policy once, at startup, so this is written before it restarts.
    write_policy(
        &estate,
        &pool,
        &[("job_deadline_secs", serde_json::json!(0))],
    );
    let wirkd_child = start_wirkd(&estate);

    let (ok, refused, err) = atlas(&estate, &["refresh", "--source", "docs"]);
    assert!(
        !ok,
        "a refresh whose deadline has passed is refused, not quietly completed: {refused} {err}"
    );
    // A refusal is rendered to stderr by `render_refusal`, code first.
    assert!(
        err.contains("JobStopped"),
        "and it is reported as a stopped job, not as a broken estate or a bad request: {err}"
    );

    // The publication is untouched: a refused refresh stages nothing and
    // publishes nothing.
    let (ok, status, err) = atlas(&estate, &["status", "--source", "docs"]);
    assert!(ok, "status failed: {err}");
    assert_eq!(
        status["sources"][0]["published_generation"]["generation"].as_str(),
        Some(published_generation.as_str()),
        "the generation published before the stop is still the published one: {status}"
    );
    stop_wirkd(&estate, wirkd_child);

    // And the estate is not poisoned by the stop: the same verb on the
    // same source succeeds as soon as the bound is a usable one.
    write_policy(&estate, &pool, &[]);
    let wirkd_child = start_wirkd(&estate);
    let (ok, staged_again, err) = atlas(&estate, &["refresh", "--source", "docs"]);
    assert!(ok, "the later job is refused too: {staged_again} {err}");
    assert_eq!(staged_again["outcome"].as_str(), Some("staged"));
    stop_wirkd(&estate, wirkd_child);
}

/// `atlas cancel --source` reaches a document capture **while the atlas
/// is busy with it**, and the capture stops.
///
/// Two things are being pinned, and the first is the one a slot-only
/// admission could never satisfy: the running capture is visible to
/// `atlas cancel --list` at the moment it holds the atlas mutex. If the
/// job were not registered, or the registry were behind that mutex, this
/// call would either see nothing or block until the capture it is trying
/// to stop had finished.
///
/// The capture is held at the real entry-classified window rather than
/// made slow, so there is no sleep anywhere in the control and no
/// dependence on how fast this box walks a directory.
#[test]
fn atlas_cancel_reaches_a_running_document_capture_and_a_later_one_still_succeeds() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    let pool = dir.path().join("host-pool");
    fs::create_dir_all(&estate).unwrap();
    let docs = dir.path().join("docs");
    fs::create_dir_all(&docs).unwrap();
    // Exactly one entry reaches the window, so the one thread the
    // barrier arms is parked on the capture and nothing else.
    fs::write(docs.join("brief.md"), "# Client brief\n\nHello.\n").unwrap();

    let barrier = dir.path().join("barrier");
    fs::create_dir_all(&barrier).unwrap();
    let listener =
        std::os::unix::net::UnixListener::bind(barrier.join(wirk_atlas::BARRIER_RELEASE_SOCKET))
            .unwrap();
    listener.set_nonblocking(true).unwrap();

    write_policy(&estate, &pool, &[]);
    let wirkd_child = start_wirkd_with(
        &estate,
        &[(
            "WIRK_ATLAS_BARRIER",
            format!("{}={}", wirk_atlas::DOCTREE_OPEN_WINDOW, barrier.display()),
        )],
    );
    fs::write(barrier.join("arm"), b"").unwrap();

    // The acquisition runs as its own process so this one can keep
    // talking to the daemon while it is parked.
    let mut acquiring = wirk_cli()
        .args([
            "atlas",
            "acquire",
            "--source",
            "docs",
            "--repository",
            docs.to_str().unwrap(),
            "--kind",
            "document-tree",
            "--estate",
        ])
        .arg(&estate)
        .arg("--json")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    // The gate has no notion of elapsed time; every bound below is this
    // controller's own, and an exhausted one reports a state that was
    // never observed rather than a verdict.
    let supervision = Duration::from_secs(60);
    let deadline = Instant::now() + supervision;
    let parked = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("accept on the release socket failed: {error}"),
        }
        assert!(
            Instant::now() < deadline,
            "the capture never reached the entry-classified window"
        );
        std::thread::sleep(Duration::from_millis(10));
    };

    // The capture is holding the atlas mutex right now. This is the
    // call a slot-only admission cannot answer.
    let (ok, listed, err) = atlas(&estate, &["cancel", "--list", "--admin"]);
    assert!(ok, "cancel --list failed while the atlas was busy: {err}");
    let running = listed["running"]
        .as_array()
        .expect("a running job list")
        .clone();
    assert!(
        running
            .iter()
            .any(|job| job["scope"].as_str() == Some("docs")
                && job["verb"].as_str() == Some("atlas acquire")),
        "the running document capture is addressable by the source an operator would name: \
         {listed}"
    );

    let (ok, cancelled, err) = atlas(&estate, &["cancel", "--source", "docs", "--admin"]);
    assert!(ok, "cancel --source docs failed: {err}");
    assert_eq!(
        cancelled["outcome"].as_str(),
        Some("signalled"),
        "signalling is acknowledged, and is deliberately not reported as stopping: {cancelled}"
    );

    drop(parked);

    // Bounded, because the failure being ruled out is a capture that
    // never returns.
    let deadline = Instant::now() + supervision;
    let acquired = loop {
        if let Some(status) = acquiring.try_wait().unwrap() {
            let mut stdout = String::new();
            let mut stderr = String::new();
            std::io::Read::read_to_string(acquiring.stdout.as_mut().unwrap(), &mut stdout).unwrap();
            std::io::Read::read_to_string(acquiring.stderr.as_mut().unwrap(), &mut stderr).unwrap();
            break (status, stdout, stderr);
        }
        if Instant::now() >= deadline {
            let _ = acquiring.kill();
            let _ = acquiring.wait();
            panic!(
                "the cancelled capture was never observed to finish: a cancellation that does \
                 not reach the work is not a cancellation"
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(
        !acquired.0.success(),
        "a cancelled acquisition does not report success: {:?} {}",
        acquired.1,
        acquired.2
    );
    assert!(
        acquired.2.contains("JobStopped"),
        "and it is reported as a stopped job rather than a source or estate failure: {} {}",
        acquired.1,
        acquired.2
    );

    // Nothing was published, and nothing is left registered.
    let (ok, status, err) = atlas(&estate, &["status", "--source", "docs"]);
    assert!(ok, "status failed: {err}");
    assert!(
        status["sources"][0]["published_generation"].is_null(),
        "a cancelled acquisition publishes nothing: {status}"
    );
    let (ok, after, err) = atlas(&estate, &["cancel", "--list", "--admin"]);
    assert!(ok, "cancel --list failed after the stop: {err}");
    assert!(
        after["running"].as_array().is_some_and(Vec::is_empty),
        "the registration is released on the way out, so \"still listed\" keeps meaning \"still \
         running\": {after}"
    );

    // The arm was consumed by the first arrival, so this capture runs
    // straight through: the cancellation was the job's, not the
    // estate's.
    let (ok, again, err) = atlas(
        &estate,
        &[
            "acquire",
            "--source",
            "docs",
            "--repository",
            docs.to_str().unwrap(),
            "--kind",
            "document-tree",
        ],
    );
    assert!(
        ok,
        "the next capture of the same source is refused after a cancellation: {again} {err}"
    );
    assert_eq!(again["outcome"].as_str(), Some("staged"));

    stop_wirkd(&estate, wirkd_child);
}

// ---------------------------------------------------------------------
// A document collection through the semantic lifecycle, for real
// ---------------------------------------------------------------------
//
// `hydrate`'s document arm is reached by the edition build and by
// edition verification, and nothing anywhere exercised either of them
// over a document-tree generation: every existing semantic control is
// over a Git source. A fake backend would pin the shape of the protocol
// and prove nothing about the hydration, so this drives the product's
// own `semble` backend against the pinned interpreter and the pinned
// offline model.
//
// Opted in (`--ignored`) for the same reason every other native
// semantic control is: it needs those two prerequisites on the box. It
// takes both from the environment and panics when they are missing
// rather than skipping, so an opt-in run with a wrong prerequisite
// fails loudly instead of quietly recording a pass. Neither has a
// host-specific default anywhere in the product.

fn pinned_semble_python() -> std::path::PathBuf {
    let path = std::path::PathBuf::from(std::env::var("WIRK_TEST_SEMBLE_PYTHON").unwrap_or_else(
        |_| {
            panic!(
                "this test is opted in (--ignored) but WIRK_TEST_SEMBLE_PYTHON is unset: point \
                 it at the pinned semble python3 interpreter (see DEVELOPMENT.md); this test \
                 never falls back to a host-specific default"
            )
        },
    ));
    assert!(
        path.is_file(),
        "WIRK_TEST_SEMBLE_PYTHON={} is not a file",
        path.display()
    );
    path
}

fn pinned_semble_model() -> std::path::PathBuf {
    let path =
        std::path::PathBuf::from(std::env::var("WIRK_TEST_SEMBLE_MODEL").unwrap_or_else(|_| {
            panic!(
                "this test is opted in (--ignored) but WIRK_TEST_SEMBLE_MODEL is unset: point it \
                 at the pinned offline model snapshot directory (see DEVELOPMENT.md); this test \
                 never falls back to a host-specific default"
            )
        }));
    assert!(
        path.is_dir(),
        "WIRK_TEST_SEMBLE_MODEL={} is not a directory",
        path.display()
    );
    path
}

/// The product's own backend script, unmodified — not a copy and not a
/// stub.
fn real_semble_backend_script() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../wirk-atlas/backends/semble_backend.py")
}

/// Build, select, search and reclaim a semantic edition over a
/// **document** collection, end to end, through the public CLI.
///
/// The decisive part is that the build has to read the collection's
/// bytes at all: an edition over a document generation is built from
/// files on disk, through `hydrate`'s document arm, not from a Git
/// object store. Before that dispatch existed the build reached for
/// `git` and found nothing. A search that returns the document's own
/// text through a semantic edition is the only thing that proves the
/// whole path.
#[test]
#[ignore = "needs the pinned semble interpreter and offline model on this box"]
fn a_document_collection_builds_selects_searches_and_reclaims_a_semantic_edition() {
    let python = pinned_semble_python();
    let model = pinned_semble_model();
    let script = real_semble_backend_script();

    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    let pool = dir.path().join("host-pool");
    fs::create_dir_all(&estate).unwrap();
    write_policy(&estate, &pool, &[]);

    let docs = dir.path().join("docs");
    fs::create_dir_all(&docs).unwrap();
    fs::write(
        docs.join("retention.md"),
        "# Retention\n\nA published generation is retained by the estate's own records while a \
         non-terminal Work's delivered World still names it. Removal is refused by name until \
         that evidence is settled or terminal.\n",
    )
    .unwrap();
    fs::write(
        docs.join("capture.md"),
        "# Capture\n\nOne walk, one bounded read per file. The traversal budget counts every \
         examined entry, and an overrun refuses the whole capture visibly rather than reporting \
         a partial collection as complete.\n",
    )
    .unwrap();

    let wirkd_child = start_wirkd(&estate);

    let (ok, acquired, err) = atlas(
        &estate,
        &[
            "acquire",
            "--source",
            "clientdocs",
            "--repository",
            docs.to_str().unwrap(),
            "--kind",
            "document-tree",
        ],
    );
    assert!(ok, "acquire failed: {err}");
    let generation = acquired["generation"]["generation"]
        .as_str()
        .expect("generation id")
        .to_string();
    let (ok, _, err) = atlas(
        &estate,
        &[
            "publish",
            "--source",
            "clientdocs",
            "--generation",
            &generation,
        ],
    );
    assert!(ok, "publish failed: {err}");

    // The build reads the collection's bytes through the document
    // hydration arm. This is the call that found nothing before that
    // arm existed.
    let (ok, built, err) = atlas(
        &estate,
        &[
            "semantic",
            "build",
            "--source",
            "clientdocs",
            "--generation",
            &generation,
            "--backend",
            python.to_str().unwrap(),
            "--backend-arg",
            script.to_str().unwrap(),
            "--model",
            model.to_str().unwrap(),
        ],
    );
    assert!(
        ok,
        "semantic build over a document collection failed: {err}"
    );
    let edition = built["edition"]["edition"]
        .as_str()
        .expect("staged edition id")
        .to_string();
    assert!(
        built["edition"]["vectors"]["rows"].as_u64().unwrap_or(0) > 0,
        "an edition with no vector rows read no document bytes: {built}"
    );

    let (ok, _, err) = atlas(
        &estate,
        &[
            "semantic",
            "select",
            "--source",
            "clientdocs",
            "--edition",
            &edition,
        ],
    );
    assert!(ok, "semantic select failed: {err}");

    // And a semantic search answers from the documents themselves,
    // which needs edition verification to hydrate the same bytes again.
    let (ok, found, err) = atlas(
        &estate,
        &[
            "search",
            "--source",
            "clientdocs",
            "--query",
            "when is removing a published generation refused",
            "--semantic",
            "requested",
            "--semantic-backend",
            python.to_str().unwrap(),
            "--semantic-backend-arg",
            script.to_str().unwrap(),
            "--semantic-model",
            model.to_str().unwrap(),
        ],
    );
    assert!(ok, "semantic search failed: {err}");
    assert_eq!(
        found["semantic"]["status"].as_str(),
        Some("applied"),
        "the ranked query must actually run, not fall back to lexical: {found}"
    );
    assert_eq!(
        found["ranking"]["mode"].as_str(),
        Some("semantic"),
        "and the answer must be the semantic ranking's, not a lexical one beside it: {found}"
    );
    assert!(
        found["ranking"]["application"]["rows_ranked"]
            .as_u64()
            .unwrap_or(0)
            > 0,
        "with rows the native ranker actually ranked: {found}"
    );
    let hits = found["hits"].as_array().expect("hits").clone();
    assert!(
        !hits.is_empty(),
        "no hits from the document edition: {found}"
    );
    assert!(
        hits.iter()
            .any(|hit| hit["path"].as_str() == Some("retention.md")),
        "the document whose own text answers the question is among the hits: {found}"
    );

    // Reclamation, through the one cleanup owner. The selected edition
    // is retained; once the source is removed it is not, and the
    // originals are untouched either way.
    let (ok, retained, err) = atlas(&estate, &["remove", "--source", "clientdocs"]);
    assert!(ok, "remove failed: {err}");
    assert_eq!(retained["outcome"].as_str(), Some("removed"), "{retained}");

    let clean = wirk_cli()
        .args([
            "estate",
            "clean",
            "--class",
            "atlas-editions",
            "--all-unreferenced",
            "--admin",
            "--json",
            "--estate",
        ])
        .arg(&estate)
        .output()
        .expect("estate clean runs");
    assert!(
        clean.status.success(),
        "estate clean --class atlas-editions failed: {}",
        String::from_utf8_lossy(&clean.stderr)
    );
    let reclaimed: serde_json::Value =
        serde_json::from_slice(&clean.stdout).expect("clean reports json");
    assert!(
        reclaimed.to_string().contains(&edition),
        "the edition built over this collection is the one reclaimed: {reclaimed}"
    );

    // The user's own files, byte for byte, after everything above.
    assert!(docs.join("retention.md").is_file());
    assert!(docs.join("capture.md").is_file());
    assert!(
        fs::read_to_string(docs.join("capture.md"))
            .unwrap()
            .starts_with("# Capture"),
        "nothing in this lifecycle touches the originals"
    );

    stop_wirkd(&estate, wirkd_child);
}

/// The `wirk` CLI with the *test runner's own* actor triple removed from
/// the child's environment.
///
/// `resolve_scope` reads `WIRK_ESTATE_ROOT`/`WIRK_WORK_ID`/`WIRK_RUN_ID`
/// to decide whether a call is an actor's own or an operator's, and a
/// test process inherits whatever its runner had. This suite is run from
/// inside a real actor pane often enough that an inherited triple makes
/// a fixture's administrative call against its own temp estate refuse as
/// a cross-estate read — so the fixture has to say which it is rather
/// than depend on who started it.
///
/// Sites that mean to act *as* an actor set the three back explicitly on
/// the returned command; a later `env` overrides this removal.
fn wirk_cli() -> Command {
    let mut command = Command::new(wirk_bin());
    command
        .env_remove("WIRK_ESTATE_ROOT")
        .env_remove("WIRK_WORK_ID")
        .env_remove("WIRK_RUN_ID");
    command
}
