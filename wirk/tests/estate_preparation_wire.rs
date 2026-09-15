//! P6.3-A/B through the real public CLI/daemon wire path (card
//! 10302881774, todo 10297970868): `atlas status`'s plain-text
//! rendering discloses a source's real coverage gap, and
//! `atlas acquire --dry-run` classifies a source without staging,
//! embedding or writing anything. Mirrors `document_tree_wire.rs`'s
//! own fixture and process style.

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use wirk::wirkd;
use wirkd::WirkdPointer;

fn wirk_bin() -> &'static str {
    env!("CARGO_BIN_EXE_wirk")
}

/// `.env_remove`s the actor triple: this process may itself be running
/// as a Wirk actor (`WIRK_ESTATE_ROOT`/`WIRK_WORK_ID`/`WIRK_RUN_ID` set
/// in its own environment), which every spawned `wirk` child would
/// otherwise inherit and be scoped by — exactly what `resolve_scope`
/// exists to enforce, and exactly wrong for a fixture that wants to
/// address its own throwaway estate as a plain operator. Same fix
/// `document_tree_wire.rs`'s own `wirk_cli` already applies.
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

/// Same isolation `document_tree_wire.rs` gives every estate here — its
/// own expensive-job host pool, so this file's `atlas acquire`/`--dry-run`
/// calls never race an unrelated concurrent estate for the shared
/// default admission pool (ruling 0291's class).
fn ensure_isolated_host_pool(estate: &Path) {
    let wirk_dir = estate.join(".wirk");
    if wirk_dir.join("resources.json").exists() {
        return;
    }
    let pool = wirk_dir.join("host-pool");
    fs::create_dir_all(&wirk_dir).unwrap();
    fs::write(
        wirk_dir.join("resources.json"),
        serde_json::json!({"host_pool_dir": pool.to_str().unwrap()}).to_string(),
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

fn atlas_json(estate: &Path, args: &[&str]) -> (bool, serde_json::Value, String) {
    let mut full = vec!["atlas"];
    full.extend_from_slice(args);
    full.push("--estate");
    let estate_str = estate.to_str().expect("estate path is utf-8");
    full.push(estate_str);
    full.push("--json");
    let output = wirk_cli().args(&full).output().expect("wirk atlas runs");
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let value = serde_json::from_str(&stdout).unwrap_or(serde_json::Value::Null);
    (output.status.success(), value, stderr)
}

/// The plain-text rendering a human actually reads — no `--json`.
fn atlas_text(estate: &Path, args: &[&str]) -> (bool, String, String) {
    let mut full = vec!["atlas"];
    full.extend_from_slice(args);
    full.push("--estate");
    let estate_str = estate.to_str().expect("estate path is utf-8");
    full.push(estate_str);
    let output = wirk_cli().args(&full).output().expect("wirk atlas runs");
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    (output.status.success(), stdout, stderr)
}

/// **P6.3-A, meaningful red before the change**: `atlas status`'s plain
/// text printed nothing distinguishing a source with real
/// `unsupported`/`error` coverage from a fully-indexed one — the exact
/// gap this preparation's own live use found (`knowledge/evidence/work/
/// p6-estate-flow-prepare/CHECKS.json`). **Green**: the same command,
/// same source, now prints the coverage split, and `--json`'s own
/// shape (`published_generation.coverage`) is unchanged — no schema
/// break, this is a rendering-only fix.
#[test]
fn status_text_discloses_a_sources_real_coverage_gap() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    let wirkd_child = start_wirkd(&estate);

    let docs = dir.path().join("docs");
    fs::create_dir_all(&docs).unwrap();
    // One recognized, indexable input and one with no configured
    // extractor family — a real, honest coverage gap, not a
    // manufactured one.
    fs::write(docs.join("brief.md"), "# Client brief\n\nHello.\n").unwrap();
    fs::write(docs.join("photo.png"), [0x89u8, b'P', b'N', b'G', 0, 0, 0]).unwrap();

    let (ok, acquired, err) = atlas_json(
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
    assert_eq!(
        acquired["generation"]["coverage"]["indexed"].as_u64(),
        Some(1)
    );
    assert_eq!(
        acquired["generation"]["coverage"]["unsupported"].as_u64(),
        Some(1)
    );
    let generation = acquired["generation"]["generation"].as_str().unwrap();
    let (ok, _, err) = atlas_json(
        &estate,
        &["publish", "--source", "docs", "--generation", generation],
    );
    assert!(ok, "publish failed: {err}");

    let (ok, text, err) = atlas_text(&estate, &["status", "--source", "docs"]);
    assert!(ok, "status --source docs failed: {err}");
    assert!(
        text.contains("coverage indexed 1")
            && text.contains("unsupported 1")
            && text.contains("of 2 total"),
        "plain-text status must disclose the real indexed/unsupported split, got:\n{text}"
    );

    // `--json`'s own coverage shape is unchanged: still the same
    // fields `generation_json` always produced.
    let (ok, status_json, err) = atlas_json(&estate, &["status", "--source", "docs"]);
    assert!(ok, "status --json --source docs failed: {err}");
    let coverage = &status_json["sources"][0]["published_generation"]["coverage"];
    assert_eq!(coverage["indexed"].as_u64(), Some(1));
    assert_eq!(coverage["unsupported"].as_u64(), Some(1));
    assert_eq!(coverage["total"].as_u64(), Some(2));

    stop_wirkd(&estate, wirkd_child);
}

/// **P6.3-B, meaningful red before the change**: `--dry-run` did not
/// exist; `atlas acquire` had no way to classify a source without
/// staging it. **Green**: `--dry-run` reports the same
/// candidate/excluded/unsupported split a real acquisition's own
/// coverage would show, and — the decisive non-mutation check —
/// `atlas status` afterward shows no source registered at all: no
/// membership, no generation, nothing staged. A subsequent *real*
/// acquisition of the identical tree then does register and stage,
/// proving the preview and the real acquisition are actually the same
/// walk, not two different implementations that happen to agree.
#[test]
fn dry_run_previews_without_mutation_then_real_acquire_stages_the_same_split() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    let wirkd_child = start_wirkd(&estate);

    let docs = dir.path().join("docs");
    fs::create_dir_all(&docs).unwrap();
    fs::write(docs.join("brief.md"), "# Client brief\n\nHello.\n").unwrap();
    fs::write(docs.join("photo.png"), [0x89u8, b'P', b'N', b'G', 0, 0, 0]).unwrap();

    let (ok, previewed, err) = atlas_json(
        &estate,
        &[
            "acquire",
            "--source",
            "docs",
            "--repository",
            docs.to_str().unwrap(),
            "--kind",
            "document-tree",
            "--dry-run",
        ],
    );
    assert!(ok, "acquire --dry-run failed: {err}");
    assert_eq!(previewed["outcome"].as_str(), Some("previewed"));
    assert_eq!(previewed["preview"]["candidate"]["count"].as_u64(), Some(1));
    assert_eq!(
        previewed["preview"]["unsupported"]["count"].as_u64(),
        Some(1)
    );
    assert_eq!(previewed["preview"]["total"]["count"].as_u64(), Some(2));

    // Decisive non-mutation: no source was registered by the preview.
    let (ok, status, err) = atlas_json(&estate, &["status"]);
    assert!(ok, "status after dry-run failed: {err}");
    assert_eq!(
        status["sources_total"].as_u64(),
        Some(0),
        "a dry-run must register no membership: {status}"
    );

    // The identical tree, acquired for real, stages the split the
    // preview named.
    let (ok, acquired, err) = atlas_json(
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
    assert!(ok, "real acquire after preview failed: {err}");
    assert_eq!(acquired["outcome"].as_str(), Some("staged"));
    assert_eq!(
        acquired["generation"]["coverage"]["indexed"].as_u64(),
        Some(1)
    );
    assert_eq!(
        acquired["generation"]["coverage"]["unsupported"].as_u64(),
        Some(1)
    );

    let (ok, status, err) = atlas_json(&estate, &["status"]);
    assert!(ok, "status after real acquire failed: {err}");
    assert_eq!(status["sources_total"].as_u64(), Some(1));

    stop_wirkd(&estate, wirkd_child);
}

/// A `--dry-run` over a Git source classifies from `git ls-tree`
/// metadata and stages nothing, exactly like the document-tree case
/// above — pinned separately because Git's preview reads no blob at
/// all (`wirk_atlas::git::preview`'s own doc), a materially different
/// cost shape than document-tree's.
#[test]
fn dry_run_over_a_git_source_previews_without_mutation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    let wirkd_child = start_wirkd(&estate);

    let code = dir.path().join("code");
    fs::create_dir_all(&code).unwrap();
    let run = |args: &[&str]| {
        let status = Command::new("git")
            .arg("-C")
            .arg(&code)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    };
    run(&["init", "-q"]);
    run(&["config", "user.email", "t@example.test"]);
    run(&["config", "user.name", "t"]);
    fs::write(code.join("main.rs"), "fn main() {}\n").unwrap();
    run(&["add", "."]);
    run(&["commit", "-qm", "one"]);

    let (ok, previewed, err) = atlas_json(
        &estate,
        &[
            "acquire",
            "--source",
            "code",
            "--repository",
            code.to_str().unwrap(),
            "--revision",
            "HEAD",
            "--dry-run",
        ],
    );
    assert!(ok, "acquire --dry-run over git failed: {err}");
    assert_eq!(previewed["outcome"].as_str(), Some("previewed"));
    assert_eq!(previewed["preview"]["kind"].as_str(), Some("git"));
    assert_eq!(previewed["preview"]["candidate"]["count"].as_u64(), Some(1));
    assert_eq!(
        previewed["preview"]["content_sniffed"].as_bool(),
        Some(false)
    );

    let (ok, status, err) = atlas_json(&estate, &["status"]);
    assert!(ok, "status after dry-run failed: {err}");
    assert_eq!(status["sources_total"].as_u64(), Some(0));

    stop_wirkd(&estate, wirkd_child);
}
