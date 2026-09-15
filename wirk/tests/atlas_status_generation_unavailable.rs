//! `wirk atlas status` through the real CLI/daemon wire path, with one
//! admitted source's published generation no longer readable.
//!
//! `handle_atlas_status`'s per-membership walk returned out of the
//! whole handler the instant `atlas.current(membership)` failed for one
//! admitted membership, so a whole-estate `atlas status --json` with
//! one broken source produced no output at all — exit 2, `AtlasError`,
//! and a healthy sibling source's status never reached the caller.
//!
//! What these pin at the wire: a healthy source's status (and the CLI's
//! plain-text rendering of it) survives one broken sibling, the broken
//! source's own row says why it has nothing to report, every source
//! being broken at once is still an answer, and restoring the data
//! recovers the exact baseline. The last test orders the two separate
//! generation reads one `atlas status` makes per source, because
//! nothing holds the filesystem still between them.
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use wirk::wirkd;
use wirkd::WirkdPointer;

#[path = "support/read_barrier.rs"]
mod read_barrier;

fn wirk_bin() -> &'static str {
    env!("CARGO_BIN_EXE_wirk")
}

/// The `wirk` CLI with the test runner's own actor triple removed from
/// the child's environment (`document_tree_wire.rs`'s own convention):
/// a test process inherits whatever `WIRK_ESTATE_ROOT`/`WIRK_WORK_ID`/
/// `WIRK_RUN_ID` its runner had, and this suite can run from inside a
/// real actor pane, which would otherwise make every call here refuse
/// as a cross-estate read against its own temp estate.
fn wirk_cli() -> Command {
    let mut command = Command::new(wirk_bin());
    command
        .env_remove("WIRK_ESTATE_ROOT")
        .env_remove("WIRK_WORK_ID")
        .env_remove("WIRK_RUN_ID");
    command
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

/// Same isolation `document_tree_wire.rs` applies: an estate-local host
/// pool, never the shared default a concurrent normal-parallel `cargo
/// test` run on this box might also be using (ruling 0291's class).
fn ensure_isolated_host_pool(estate: &Path) {
    let wirk_dir = estate.join(".wirk");
    if wirk_dir.join("resources.json").exists() {
        return;
    }
    fs::create_dir_all(&wirk_dir).unwrap();
    fs::write(
        wirk_dir.join("resources.json"),
        serde_json::json!({
            "host_pool_dir": wirk_dir.join("host-pool").to_str().unwrap(),
        })
        .to_string(),
    )
    .unwrap();
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

struct AtlasOutput {
    ok: bool,
    exit_code: Option<i32>,
    json: serde_json::Value,
    stdout: String,
    stderr: String,
}

fn atlas(estate: &Path, args: &[&str]) -> AtlasOutput {
    let mut json_args = vec![];
    json_args.extend_from_slice(args);
    json_args.push("--estate");
    let estate_str = estate.to_str().unwrap();
    json_args.push(estate_str);
    json_args.push("--json");
    let output = wirk_cli()
        .args(&json_args)
        .output()
        .expect("wirk atlas runs");
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let json = serde_json::from_str(&stdout).unwrap_or(serde_json::Value::Null);
    AtlasOutput {
        ok: output.status.success(),
        exit_code: output.status.code(),
        json,
        stdout,
        stderr,
    }
}

/// The plain-text rendering, deliberately without `--json` -- what a
/// human running this actually reads, exercising the CLI's own
/// `generation_error` branch in `wirk/src/atlas.rs`.
fn atlas_text(estate: &Path, args: &[&str]) -> (bool, String, String) {
    let mut full = vec![];
    full.extend_from_slice(args);
    full.push("--estate");
    let estate_str = estate.to_str().unwrap();
    full.push(estate_str);
    let output = wirk_cli().args(&full).output().expect("wirk atlas runs");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).trim().to_string(),
        String::from_utf8_lossy(&output.stderr).trim().to_string(),
    )
}

fn publish_document_tree(
    estate: &Path,
    alias: &str,
    docs: &Path,
    file: &str,
    body: &str,
) -> String {
    fs::create_dir_all(docs).unwrap();
    fs::write(docs.join(file), body).unwrap();
    let acquired = atlas(
        estate,
        &[
            "atlas",
            "acquire",
            "--source",
            alias,
            "--repository",
            docs.to_str().unwrap(),
            "--kind",
            "document-tree",
        ],
    );
    assert!(acquired.ok, "acquire {alias} failed: {}", acquired.stderr);
    let generation = acquired.json["generation"]["generation"]
        .as_str()
        .expect("acquired generation id")
        .to_string();
    let published = atlas(
        estate,
        &[
            "atlas",
            "publish",
            "--source",
            alias,
            "--generation",
            &generation,
        ],
    );
    assert!(published.ok, "publish {alias} failed: {}", published.stderr);
    generation
}

fn generation_dir(estate: &Path, generation: &str) -> PathBuf {
    estate.join("atlas").join("generations").join(generation)
}

fn source_row<'a>(status: &'a serde_json::Value, alias: &str) -> &'a serde_json::Value {
    status["sources"]
        .as_array()
        .unwrap_or_else(|| panic!("status has no sources array: {status}"))
        .iter()
        .find(|source| source["membership"]["alias"].as_str() == Some(alias))
        .unwrap_or_else(|| panic!("no source row for {alias} in {status}"))
}

#[test]
fn whole_estate_status_discloses_one_unavailable_generation_beside_a_healthy_one() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    let wirkd_child = start_wirkd(&estate);

    let sources = dir.path().join("sources");
    publish_document_tree(
        &estate,
        "alpha",
        &sources.join("docs-a"),
        "alpha.txt",
        "alpha content\n",
    );
    let beta_generation = publish_document_tree(
        &estate,
        "beta",
        &sources.join("docs-b"),
        "beta.txt",
        "beta content\n",
    );

    // ---- baseline: whole-estate status, both healthy ------------------
    let baseline = atlas(&estate, &["atlas", "status"]);
    assert!(baseline.ok, "baseline status failed: {}", baseline.stderr);
    assert_eq!(baseline.json["sources_total"].as_u64(), Some(2));
    assert!(source_row(&baseline.json, "alpha")["published_generation"]["generation"].is_string());
    assert!(source_row(&baseline.json, "beta")["published_generation"]["generation"].is_string());
    assert!(
        baseline.json["sources"][0]
            .get("generation_error")
            .is_none()
    );

    // ---- fault injection: beta's published generation directory moved
    // aside, on this test's own owned estate directory only.
    let generation_dir = estate
        .join("atlas")
        .join("generations")
        .join(&beta_generation);
    let backup_dir = estate.join("beta-generation-backup");
    assert!(
        generation_dir.is_dir(),
        "fixture must find beta's real generation directory at {}",
        generation_dir.display()
    );
    fs::rename(&generation_dir, &backup_dir).unwrap();

    // ---- whole-estate status must not abort and lose alpha's healthy
    // status alongside beta's.
    let after_fault = atlas(&estate, &["atlas", "status"]);
    assert!(
        after_fault.ok,
        "an unavailable sibling generation must not abort whole-estate status; \
         exit={:?} stdout={:?} stderr={}",
        after_fault.exit_code, after_fault.stdout, after_fault.stderr
    );
    assert_eq!(
        after_fault.json["sources_total"].as_u64(),
        Some(2),
        "both admitted sources are still counted: {}",
        after_fault.json
    );
    let alpha_row = source_row(&after_fault.json, "alpha");
    assert!(
        alpha_row["published_generation"]["generation"].is_string(),
        "alpha's healthy status must be unaffected by beta's state: {alpha_row}"
    );
    assert!(alpha_row.get("generation_error").is_none());
    let beta_row = source_row(&after_fault.json, "beta");
    assert!(
        beta_row["published_generation"].is_null(),
        "beta has no readable published generation: {beta_row}"
    );
    let beta_error = beta_row["generation_error"]
        .as_str()
        .unwrap_or_else(|| panic!("beta's row must carry generation_error: {beta_row}"));
    assert!(
        beta_error.contains(&beta_generation),
        "the disclosed reason must name the actual unreadable generation, not a generic \
         message: {beta_error}"
    );

    // ---- a call scoped to the broken source by name also now succeeds,
    // carrying the same explicit reason, instead of the whole-call
    // AtlasError the unscoped call used to produce.
    let beta_scoped = atlas(&estate, &["atlas", "status", "--source", "beta"]);
    assert!(
        beta_scoped.ok,
        "a status call scoped to the broken source must still answer, with the reason \
         disclosed, not abort: {}",
        beta_scoped.stderr
    );
    assert_eq!(beta_scoped.json["admitted"].as_bool(), None);
    assert_eq!(beta_scoped.json["registered"].as_bool(), Some(true));
    assert!(
        source_row(&beta_scoped.json, "beta")["generation_error"]
            .as_str()
            .is_some()
    );

    // ---- explicit alpha-only status is unaffected either way ----------
    let alpha_scoped = atlas(&estate, &["atlas", "status", "--source", "alpha"]);
    assert!(alpha_scoped.ok, "{}", alpha_scoped.stderr);
    assert!(
        source_row(&alpha_scoped.json, "alpha")["published_generation"]["generation"].is_string()
    );

    // ---- plain-text rendering distinguishes "unavailable" from "none" -
    let (text_ok, text_stdout, text_stderr) = atlas_text(&estate, &["atlas", "status"]);
    assert!(text_ok, "{text_stderr}");
    assert!(
        text_stdout.contains("beta published unavailable:"),
        "human output must say the generation is unavailable, not silently print \
         `published none` as if beta had simply never published: {text_stdout}"
    );
    assert!(
        text_stdout
            .lines()
            .any(|line| line.trim_start().starts_with("alpha published ")
                && !line.contains("unavailable")),
        "alpha's line must render its real generation, unaffected: {text_stdout}"
    );

    // ---- restoring the directory recovers the exact baseline ----------
    fs::rename(&backup_dir, &generation_dir).unwrap();
    let recovered = atlas(&estate, &["atlas", "status"]);
    assert!(recovered.ok, "{}", recovered.stderr);
    assert_eq!(recovered.json["sources_total"].as_u64(), Some(2));
    assert!(
        source_row(&recovered.json, "beta")
            .get("generation_error")
            .is_none()
    );
    assert_eq!(
        source_row(&recovered.json, "beta")["published_generation"]["generation"].as_str(),
        Some(beta_generation.as_str()),
        "recovery must republish the exact same generation this test staged, not rewrite it"
    );

    stop_wirkd(&estate, wirkd_child);
}

/// Every admitted source broken at once, by two different faults: one
/// generation directory moved aside, one `manifest.json` left in place
/// but no longer parseable. Status has no healthy row left to preserve,
/// so what is left to get right is that it still answers at all, counts
/// both admitted sources honestly, and reports each as unavailable
/// rather than as having published nothing.
#[test]
fn every_admitted_generation_unavailable_still_answers_status() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    let wirkd_child = start_wirkd(&estate);

    let sources = dir.path().join("sources");
    let alpha_generation = publish_document_tree(
        &estate,
        "alpha",
        &sources.join("docs-a"),
        "alpha.txt",
        "alpha content\n",
    );
    let beta_generation = publish_document_tree(
        &estate,
        "beta",
        &sources.join("docs-b"),
        "beta.txt",
        "beta content\n",
    );

    let alpha_dir = generation_dir(&estate, &alpha_generation);
    let alpha_backup = estate.join("alpha-generation-backup");
    fs::rename(&alpha_dir, &alpha_backup).unwrap();
    let beta_manifest = generation_dir(&estate, &beta_generation).join("manifest.json");
    let beta_manifest_bytes = fs::read(&beta_manifest).unwrap();
    fs::write(&beta_manifest, b"{ not json at all").unwrap();

    let status = atlas(&estate, &["atlas", "status"]);
    assert!(
        status.ok,
        "nothing readable is still an answer: exit={:?} stdout={:?} stderr={}",
        status.exit_code, status.stdout, status.stderr
    );
    assert_eq!(
        status.json["sources_total"].as_u64(),
        Some(2),
        "both sources are still admitted and must still be counted: {}",
        status.json
    );
    for alias in ["alpha", "beta"] {
        let row = source_row(&status.json, alias);
        assert!(
            row["published_generation"].is_null(),
            "{alias} has no readable published generation: {row}"
        );
        assert!(
            row["generation_error"].as_str().is_some(),
            "{alias}'s row must say why, rather than look like a source that never published: \
             {row}"
        );
        assert!(
            row["semantic"].is_null(),
            "with no readable generation there is no semantic state to assert: {row}"
        );
    }

    // A malformed manifest is a different failure from a missing
    // directory, and the caller is told which one it has.
    assert!(
        source_row(&status.json, "beta")["generation_error"]
            .as_str()
            .is_some_and(|error| error.contains("malformed")),
        "beta's manifest parsed as nothing; the reason must say so: {}",
        status.json
    );

    let (text_ok, text_stdout, text_stderr) = atlas_text(&estate, &["atlas", "status"]);
    assert!(text_ok, "{text_stderr}");
    assert_eq!(
        text_stdout
            .lines()
            .filter(|line| line.contains("published unavailable:"))
            .count(),
        2,
        "a human reading this must see both sources as unavailable: {text_stdout}"
    );

    // ---- restoration returns ordinary operation ----------------------
    fs::rename(&alpha_backup, &alpha_dir).unwrap();
    fs::write(&beta_manifest, &beta_manifest_bytes).unwrap();
    let recovered = atlas(&estate, &["atlas", "status"]);
    assert!(recovered.ok, "{}", recovered.stderr);
    for (alias, generation) in [("alpha", &alpha_generation), ("beta", &beta_generation)] {
        let row = source_row(&recovered.json, alias);
        assert!(row.get("generation_error").is_none(), "{row}");
        assert_eq!(
            row["published_generation"]["generation"].as_str(),
            Some(generation.as_str()),
            "recovery must restore the exact generations this test published, not new ones: \
             {row}"
        );
    }

    stop_wirkd(&estate, wirkd_child);
}

/// One `atlas status` reads each admitted source's published generation
/// **twice**: once for the source's own row, and again inside
/// `semantic_status_record`, which derives semantic availability from
/// whatever generation that source publishes now. Nothing holds the
/// filesystem still between the two — the store's lock excludes another
/// `AtlasStore` in another process, not an operator, a crash or a
/// cleanup job removing a generation directory mid-call — so the second
/// read can fail where the first succeeded, and that must be as
/// source-local as the first.
///
/// A sleep or a swap loop would prove a thread ran, not that the
/// decisive interleaving happened. This **orders** it with the existing
/// FIFO scheduler (`support/read_barrier.rs`): beta's `manifest.json` is
/// replaced by a named pipe, so the daemon's first read of it blocks in
/// this test's own hands. The test supplies the real manifest bytes,
/// renames a malformed file over the path, and only then releases — so
/// the rewrite happens-before the first read returns, and therefore
/// before the second read can open the path. The second read is
/// guaranteed the malformed bytes; it is an order, not a window.
///
/// The fixture arranges one thing the product would otherwise have
/// built: the estate's `atlas/semantic` directory. `semantic_editions`
/// returns early when it is absent, and with it absent the second read
/// never happens at all — so this test would pass vacuously on any
/// estate that has never staged a semantic edition. Nothing else about
/// the semantic surface is simulated: the directory is empty and the
/// daemon reads it as the empty edition set it really is.
#[test]
fn a_generation_that_stops_reading_between_the_two_status_reads_stays_source_local() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    let wirkd_child = start_wirkd(&estate);

    let sources = dir.path().join("sources");
    publish_document_tree(
        &estate,
        "alpha",
        &sources.join("docs-a"),
        "alpha.txt",
        "alpha content\n",
    );
    let beta_generation = publish_document_tree(
        &estate,
        "beta",
        &sources.join("docs-b"),
        "beta.txt",
        "beta content\n",
    );
    fs::create_dir_all(estate.join("atlas").join("semantic")).unwrap();

    let baseline = atlas(&estate, &["atlas", "status"]);
    assert!(baseline.ok, "baseline status: {}", baseline.stderr);
    assert!(source_row(&baseline.json, "beta")["semantic"].is_object());

    let manifest = generation_dir(&estate, &beta_generation).join("manifest.json");
    let manifest_bytes = fs::read(&manifest).unwrap();
    let malformed = estate.join("malformed-manifest");
    fs::write(&malformed, b"{ not json at all").unwrap();

    let barrier = read_barrier::ReadBarrier::arm(&manifest);
    let status_estate = estate.clone();
    let status = std::thread::spawn(move || atlas(&status_estate, &["atlas", "status"]));

    // Returns exactly when the daemon has opened beta's manifest for its
    // first read of it, and it cannot proceed past that read until the
    // hold is released.
    let mut held = barrier.park("the daemon reads beta's published generation for its status row");
    // 1. the real bytes: the first read succeeds, exactly as it would
    //    have on a healthy estate.
    held.supply(&manifest_bytes);
    // 2. the fault, applied at the barrier. Atomic: the daemon's open
    //    file description still names the pipe, and the path now names a
    //    whole file that is not JSON.
    fs::rename(&malformed, &manifest).unwrap();
    // 3. only now can the first read reach EOF and return.
    held.release();

    let status = status.join().expect("status thread");
    assert!(
        status.ok,
        "the second read of one source's generation failing must not erase the whole status: \
         exit={:?} stdout={:?} stderr={}",
        status.exit_code, status.stdout, status.stderr
    );
    assert_eq!(
        status.json["sources_total"].as_u64(),
        Some(2),
        "{}",
        status.json
    );
    let alpha_row = source_row(&status.json, "alpha");
    assert!(
        alpha_row["published_generation"]["generation"].is_string()
            && alpha_row.get("generation_error").is_none(),
        "alpha was never touched and its row must be ordinary: {alpha_row}"
    );
    let beta_row = source_row(&status.json, "beta");
    assert!(
        beta_row["generation_error"].as_str().is_some(),
        "beta's generation stopped reading back mid-call; its own row must say so: {beta_row}"
    );
    assert!(
        beta_row["published_generation"].is_null() && beta_row["semantic"].is_null(),
        "a generation that no longer reads back publishes nothing and supports no semantic \
         claim; neither may be carried over from the earlier read: {beta_row}"
    );

    fs::write(&manifest, &manifest_bytes).unwrap();
    let recovered = atlas(&estate, &["atlas", "status"]);
    assert!(recovered.ok, "{}", recovered.stderr);
    let beta_row = source_row(&recovered.json, "beta");
    assert_eq!(
        beta_row["published_generation"]["generation"].as_str(),
        Some(beta_generation.as_str()),
        "{beta_row}"
    );
    assert!(beta_row["semantic"].is_object(), "{beta_row}");

    stop_wirkd(&estate, wirkd_child);
}
