//! P3 W3 (`source-orient/BUILD-BRIEF.md` "W3 — wirkd/CLI integration
//! and real two-estate scenario"): the public Atlas surface through
//! the real built `wirk` binary against real `wirkd` daemons and real
//! Git repositories — never a library call into `wirk_atlas` directly
//! (that is W1/W2's own scope, `wirk-atlas/tests/`). This file proves
//! the wirkd/CLI integration boundary: Work-derived admission, the
//! two-estate isolation the decisive scenario requires, staged-vs-
//! published coherence, historical-coordinate immutability across a
//! refresh, restart-identical state, and typed (non-crashing) failure
//! for a malformed or inadmissible request. The underlying acquisition/
//! coverage/extraction behavior is already the accepted W1/W2 donor's
//! own tested scope (`wirk-atlas/tests/`) and is not re-proven here.

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
    let exit_status = child.0.wait().expect("reap wirkd child");
    assert!(
        exit_status.success(),
        "wirkd did not exit clean: {exit_status:?}"
    );
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

/// A fresh real Git repository seeded with one committed file.
fn seed_repo(repo: &Path, filename: &str, contents: &str) {
    fs::create_dir_all(repo).expect("create repo dir");
    git(repo, &["init", "-q"]);
    git(
        repo,
        &["config", "user.email", "source-substrate@example.test"],
    );
    git(repo, &["config", "user.name", "source-substrate"]);
    fs::write(repo.join(filename), contents).expect("write seed file");
    git(repo, &["add", filename]);
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

fn submit_work(
    estate: &Path,
    repo_bindings: &[&str],
    execution_repo: &str,
    repo_path: &Path,
) -> String {
    submit_work_with_run(estate, repo_bindings, execution_repo, repo_path).0
}

fn submit_work_with_run(
    estate: &Path,
    repo_bindings: &[&str],
    execution_repo: &str,
    repo_path: &Path,
) -> (String, String) {
    let mut args = vec!["work", "submit", "--estate"];
    let estate = estate.to_str().expect("estate path is utf-8");
    args.push(estate);
    for binding in repo_bindings {
        args.push("--repo");
        args.push(binding);
    }
    args.push("--execution-repo");
    args.push(execution_repo);
    args.push("--base");
    args.push("HEAD");
    args.push("--source-basis");
    args.push("git");
    args.push("--repo-path");
    let repo_path = repo_path.to_str().expect("repo path is utf-8");
    args.push(repo_path);
    args.push("--route");
    args.push("smoke");
    let output = Command::new(wirk_bin())
        .args(&args)
        .output()
        .expect("work submit runs");
    assert!(
        output.status.success(),
        "work submit failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let words: Vec<&str> = stdout.split_whitespace().collect();
    let work_id = words
        .get(1)
        .expect("work submit prints work_id <id> ...")
        .to_string();
    let run_id = words
        .get(3)
        .expect("work submit prints ... run_id <id> ...")
        .to_string();
    (work_id, run_id)
}

/// Writes `report.md` into the deterministic Waypoint's own checkout
/// (`cwd` for a Git-basis Deterministic World is this Work's own
/// worktree, `<estate>/worktrees/<work_id>` — P3 execution-recovery
/// item 1, superseding this comment's own prior "`repo_path` itself")
/// and files a real `wirk claim` for it — the "real admitted producing
/// action" `atlas relate` now requires (ruling 0093, W3-CORRECTION.md
/// item 2) before a Work may assert a relationship.
fn claim_report(estate: &Path, work_id: &str, run_id: &str) {
    fs::write(
        estate.join("worktrees").join(work_id).join("report.md"),
        "produced by this Work\n",
    )
    .expect("write report.md");
    claim_report_existing(estate, work_id, run_id);
}

/// Claims a `report.md` the caller has already written — ruling 0095's
/// order: assert during the work, write the report that includes the
/// assertion's outcome, then Claim the finished report once.
fn claim_report_existing(estate: &Path, work_id: &str, run_id: &str) {
    let output = Command::new(wirk_bin())
        .arg("claim")
        .env("WIRK_ESTATE_ROOT", estate)
        .env("WIRK_WORK_ID", work_id)
        .env("WIRK_RUN_ID", run_id)
        .args(["--artifact", "report.md=report.md"])
        .output()
        .expect("wirk claim runs");
    assert!(
        output.status.success(),
        "claim report.md on {work_id} failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn install_smoke_route(estate: &Path) {
    let routes = estate.join("routes");
    fs::create_dir_all(&routes).expect("create routes dir");
    fs::write(
        routes.join("smoke.json"),
        r#"{"id":"smoke","waypoints":[{"id":"smoke/wp-1","kind":"Deterministic","intent":"noop","command":["true"],"declared_outputs":[{"name":"report.md","required":true}],"boundary":["**"]}]}"#,
    )
    .expect("write smoke route");
}

fn first_hit_coordinate(search_result: &serde_json::Value) -> String {
    search_result["hits"][0]["coordinate"]
        .as_str()
        .expect("search produced at least one hit with a coordinate")
        .to_string()
}

// ---- 1. real two-estate scenario: acquire/publish/search/resolve/relate ---

#[test]
fn two_estate_search_resolve_relate_round_trip_with_work_derived_admission() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate_a = dir.path().join("estate-a");
    fs::create_dir_all(&estate_a).unwrap();
    install_smoke_route(&estate_a);

    let wirk_repo = dir.path().join("wirk-repo");
    seed_repo(&wirk_repo, "lib.rs", "fn validate_claim() {}\n");
    let workspace_repo = dir.path().join("workspace-repo");
    seed_repo(&workspace_repo, "claim-contract.md", "# claim contract\n");

    let wirkd_a = start_wirkd(&estate_a);

    let (ok, acquired_wirk, err) = atlas(
        &estate_a,
        &[
            "acquire",
            "--source",
            "wirk",
            "--repository",
            wirk_repo.to_str().unwrap(),
            "--revision",
            "HEAD",
        ],
    );
    assert!(ok, "acquire wirk failed: {err}");
    assert_eq!(acquired_wirk["outcome"].as_str(), Some("staged"));
    let generation_wirk = acquired_wirk["generation"]["generation"]
        .as_str()
        .unwrap()
        .to_string();

    let (ok, acquired_workspace, err) = atlas(
        &estate_a,
        &[
            "acquire",
            "--source",
            "workspace",
            "--repository",
            workspace_repo.to_str().unwrap(),
            "--revision",
            "HEAD",
        ],
    );
    assert!(ok, "acquire workspace failed: {err}");
    let generation_workspace = acquired_workspace["generation"]["generation"]
        .as_str()
        .unwrap()
        .to_string();

    // A query before publish creates no visible generation.
    let (ok, status_before, err) = atlas(&estate_a, &["status"]);
    assert!(ok, "status before publish failed: {err}");
    for source in status_before["sources"].as_array().unwrap() {
        assert!(source["published_generation"].is_null());
    }

    let (ok, _, err) = atlas(
        &estate_a,
        &[
            "publish",
            "--source",
            "wirk",
            "--generation",
            &generation_wirk,
        ],
    );
    assert!(ok, "publish wirk failed: {err}");
    let (ok, _, err) = atlas(
        &estate_a,
        &[
            "publish",
            "--source",
            "workspace",
            "--generation",
            &generation_workspace,
        ],
    );
    assert!(ok, "publish workspace failed: {err}");

    let (work_id, run_id) = submit_work_with_run(
        &estate_a,
        &["wirk:write", "workspace:read"],
        "wirk",
        &wirk_repo,
    );

    let (ok, search_wirk, err) = atlas(
        &estate_a,
        &["search", "--work", &work_id, "--query", "validate_claim"],
    );
    assert!(ok, "search wirk failed: {err}");
    assert_eq!(search_wirk["admission"]["admitted"].as_u64(), Some(2));
    assert_eq!(search_wirk["hits"].as_array().unwrap().len(), 1);
    assert!(
        search_wirk["hits"][0]["path"]
            .as_str()
            .unwrap()
            .ends_with("lib.rs")
    );
    let wirk_coordinate = first_hit_coordinate(&search_wirk);

    let (ok, resolved, err) = atlas(
        &estate_a,
        &[
            "resolve",
            "--work",
            &work_id,
            "--coordinate",
            &wirk_coordinate,
        ],
    );
    assert!(ok, "resolve wirk coordinate failed: {err}");
    assert_eq!(resolved["outcome"].as_str(), Some("resolved"));
    assert_eq!(resolved["text"].as_str(), Some("fn validate_claim() {}\n"));
    assert_eq!(
        resolved["budget"]["total_bytes"].as_u64(),
        Some("fn validate_claim() {}\n".len() as u64),
        "resolve must disclose the full committed blob's own byte length, not just the returned span"
    );

    let (ok, search_workspace, err) = atlas(
        &estate_a,
        &[
            "search",
            "--work",
            &work_id,
            "--query",
            "claim contract",
            "--source",
            "workspace",
        ],
    );
    assert!(ok, "search workspace failed: {err}");
    assert_eq!(search_workspace["hits"].as_array().unwrap().len(), 1);
    let workspace_coordinate = first_hit_coordinate(&search_workspace);

    // A relationship needs a real admitted producing action (ruling
    // 0093, corrected by 0095): the Work's *current* Run and the World
    // it was reserved against — so the assertion is recorded here,
    // during the work, before the Work claims anything.
    let (ok, relationship, err) = atlas(
        &estate_a,
        &[
            "relate",
            "--work",
            &work_id,
            "--kind",
            "governed_by",
            "--from",
            &wirk_coordinate,
            "--to",
            &workspace_coordinate,
            "--evidence",
            &workspace_coordinate,
        ],
    );
    assert!(ok, "relate failed: {err}");
    assert_eq!(relationship["kind"].as_str(), Some("governed_by"));
    let producer = relationship["producer"].as_str().unwrap_or_default();
    assert_eq!(
        producer,
        format!(
            "explicit-admission/v1/work:{work_id}/run:{run_id}/world:{}",
            relationship["producing_action"]["world"].as_str().unwrap()
        ),
        "the producer identity must be this Work's own current journaled Run and World (ruling 0095), never a client-supplied string, a bare Work id, or a past Claim"
    );
    assert_eq!(
        relationship["producing_action"]["run"].as_str(),
        Some(run_id.as_str())
    );
    assert_eq!(
        relationship["from"].as_str(),
        Some(wirk_coordinate.as_str())
    );
    assert_eq!(
        relationship["to"].as_str(),
        Some(workspace_coordinate.as_str())
    );

    // Retrying the identical admission is idempotent (same relationship id).
    let (ok, relationship_again, err) = atlas(
        &estate_a,
        &[
            "relate",
            "--work",
            &work_id,
            "--kind",
            "governed_by",
            "--from",
            &wirk_coordinate,
            "--to",
            &workspace_coordinate,
            "--evidence",
            &workspace_coordinate,
        ],
    );
    assert!(ok, "repeat relate failed: {err}");
    assert_eq!(relationship_again["id"], relationship["id"]);

    // Ruling 0095's lifecycle, end to end: the Work now writes the final
    // report *including* what it asserted, and Claims it once. Nothing
    // has to be appended to the artifact after the Claim, so the claimed
    // digest still matches — the structural defect the correction-verify
    // VERDICT recorded as R1.
    fs::write(
        estate_a.join("worktrees").join(&work_id).join("report.md"),
        format!(
            "asserted {} under {}\n",
            relationship["id"].as_str().unwrap(),
            producer
        ),
    )
    .expect("write final report");
    claim_report_existing(&estate_a, &work_id, &run_id);

    stop_wirkd(&estate_a, wirkd_a);
}

// ---- 2. estate-B coordinate is refused before lookup, without disclosure --

#[test]
fn estate_b_coordinate_is_refused_before_lookup_without_disclosure() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate_a = dir.path().join("estate-a");
    let estate_b = dir.path().join("estate-b");
    fs::create_dir_all(&estate_a).unwrap();
    fs::create_dir_all(&estate_b).unwrap();
    install_smoke_route(&estate_a);

    let repo_a = dir.path().join("repo-a");
    seed_repo(&repo_a, "lib.rs", "fn validate_claim() {}\n");
    let repo_b = dir.path().join("repo-b");
    seed_repo(
        &repo_b,
        "lib.rs",
        "fn validate_claim() { /* estate B distractor, secret detail */ }\n",
    );

    let wirkd_a = start_wirkd(&estate_a);
    let wirkd_b = start_wirkd(&estate_b);

    let (ok, acquired_a, err) = atlas(
        &estate_a,
        &[
            "acquire",
            "--source",
            "wirk",
            "--repository",
            repo_a.to_str().unwrap(),
            "--revision",
            "HEAD",
        ],
    );
    assert!(ok, "acquire A failed: {err}");
    let generation_a = acquired_a["generation"]["generation"].as_str().unwrap();
    let (ok, _, err) = atlas(
        &estate_a,
        &["publish", "--source", "wirk", "--generation", generation_a],
    );
    assert!(ok, "publish A failed: {err}");

    let (ok, acquired_b, err) = atlas(
        &estate_b,
        &[
            "acquire",
            "--source",
            "wirk",
            "--repository",
            repo_b.to_str().unwrap(),
            "--revision",
            "HEAD",
        ],
    );
    assert!(ok, "acquire B failed: {err}");
    let generation_b = acquired_b["generation"]["generation"].as_str().unwrap();
    let (ok, _, err) = atlas(
        &estate_b,
        &["publish", "--source", "wirk", "--generation", generation_b],
    );
    assert!(ok, "publish B failed: {err}");

    // B is directly, honestly retrievable when explicitly addressed
    // (estate-wide orientation search, no --work).
    let (ok, search_b, err) = atlas(&estate_b, &["search", "--query", "validate_claim"]);
    assert!(ok, "orientation search on B failed: {err}");
    assert_eq!(search_b["hits"].as_array().unwrap().len(), 1);
    assert!(
        search_b["hits"][0]["snippet"]
            .as_str()
            .unwrap()
            .contains("secret detail"),
        "B's own query must genuinely see its own distractor content"
    );
    let coordinate_b = first_hit_coordinate(&search_b);

    let work_id = submit_work(&estate_a, &["wirk:write"], "wirk", &repo_a);

    // Estate A, addressed with B's own coordinate: refused before any
    // lookup or ranking, and the refusal discloses no path/content.
    let (ok, _value, err) = atlas(
        &estate_a,
        &["resolve", "--work", &work_id, "--coordinate", &coordinate_b],
    );
    assert!(!ok, "estate A must refuse estate B's coordinate");
    assert!(
        err.contains("InadmissibleEstate"),
        "expected InadmissibleEstate, got: {err}"
    );
    assert!(
        !err.contains("secret detail") && !err.contains("lib.rs"),
        "refusal must not disclose the hidden coordinate's path or content: {err}"
    );

    stop_wirkd(&estate_a, wirkd_a);
    stop_wirkd(&estate_b, wirkd_b);
}

// ---- 3. staged-vs-published coherence and historical immutability --------

#[test]
fn refresh_changes_no_answer_until_publish_and_old_coordinate_survives_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    install_smoke_route(&estate);

    let repo = dir.path().join("repo");
    seed_repo(&repo, "lib.rs", "fn validate_claim() {}\n");

    let wirkd_child = start_wirkd(&estate);

    let (ok, acquired, err) = atlas(
        &estate,
        &[
            "acquire",
            "--source",
            "wirk",
            "--repository",
            repo.to_str().unwrap(),
            "--revision",
            "HEAD",
        ],
    );
    assert!(ok, "acquire failed: {err}");
    let generation_1 = acquired["generation"]["generation"].as_str().unwrap();
    let (ok, _, err) = atlas(
        &estate,
        &["publish", "--source", "wirk", "--generation", generation_1],
    );
    assert!(ok, "publish 1 failed: {err}");

    let work_id = submit_work(&estate, &["wirk:write"], "wirk", &repo);
    let (ok, search_1, err) = atlas(
        &estate,
        &["search", "--work", &work_id, "--query", "validate_claim"],
    );
    assert!(ok, "search 1 failed: {err}");
    let coordinate_1 = first_hit_coordinate(&search_1);
    let revision_1 = search_1["publication_revision"].as_u64().unwrap();

    fs::write(
        repo.join("lib.rs"),
        "fn validate_claim() {} fn validate_claim_v2() {}\n",
    )
    .expect("write v2");
    git(&repo, &["add", "lib.rs"]);
    git(&repo, &["commit", "-q", "-m", "v2"]);

    let (ok, refreshed, err) = atlas(
        &estate,
        &["refresh", "--source", "wirk", "--revision", "HEAD"],
    );
    assert!(ok, "refresh failed: {err}");
    let generation_2 = refreshed["generation"]["generation"].as_str().unwrap();
    assert_ne!(generation_2, generation_1);

    // Staged, not published: the answer for the new text is unchanged.
    let (ok, search_after_refresh, err) = atlas(
        &estate,
        &["search", "--work", &work_id, "--query", "validate_claim_v2"],
    );
    assert!(ok, "search after refresh failed: {err}");
    assert_eq!(search_after_refresh["hits"].as_array().unwrap().len(), 0);
    assert_eq!(search_after_refresh["coverage"]["no_match"], true);
    assert_eq!(
        search_after_refresh["publication_revision"]
            .as_u64()
            .unwrap(),
        revision_1,
        "an unpublished refresh must not advance the publication revision a query observes"
    );

    let (ok, _, err) = atlas(
        &estate,
        &["publish", "--source", "wirk", "--generation", generation_2],
    );
    assert!(ok, "publish 2 failed: {err}");

    let (ok, search_after_publish, err) = atlas(
        &estate,
        &["search", "--work", &work_id, "--query", "validate_claim_v2"],
    );
    assert!(ok, "search after publish failed: {err}");
    assert_eq!(search_after_publish["hits"].as_array().unwrap().len(), 1);

    // The old, pre-refresh coordinate still resolves its original bytes.
    let (ok, resolved_old, err) = atlas(
        &estate,
        &["resolve", "--work", &work_id, "--coordinate", &coordinate_1],
    );
    assert!(ok, "resolve old coordinate failed: {err}");
    assert_eq!(resolved_old["outcome"].as_str(), Some("resolved"));
    assert_eq!(
        resolved_old["text"].as_str(),
        Some("fn validate_claim() {}\n")
    );

    stop_wirkd(&estate, wirkd_child);
}

// ---- 4. restart reproduces identical state --------------------------------

#[test]
fn restart_reproduces_identical_status() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();

    let repo = dir.path().join("repo");
    seed_repo(&repo, "lib.rs", "fn validate_claim() {}\n");

    let wirkd_child = start_wirkd(&estate);
    let (ok, acquired, err) = atlas(
        &estate,
        &[
            "acquire",
            "--source",
            "wirk",
            "--repository",
            repo.to_str().unwrap(),
            "--revision",
            "HEAD",
        ],
    );
    assert!(ok, "acquire failed: {err}");
    let generation = acquired["generation"]["generation"].as_str().unwrap();
    let (ok, _, err) = atlas(
        &estate,
        &["publish", "--source", "wirk", "--generation", generation],
    );
    assert!(ok, "publish failed: {err}");
    let (ok, status_before, err) = atlas(&estate, &["status"]);
    assert!(ok, "status before restart failed: {err}");
    stop_wirkd(&estate, wirkd_child);

    let wirkd_child = start_wirkd(&estate);
    let (ok, status_after, err) = atlas(&estate, &["status"]);
    assert!(ok, "status after restart failed: {err}");
    assert_eq!(
        status_before, status_after,
        "restart must reproduce identical Atlas state"
    );
    stop_wirkd(&estate, wirkd_child);
}

// ---- 5. malformed and inadmissible requests are typed failures, not crashes

#[test]
fn malformed_coordinate_and_unknown_work_are_typed_failures() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    let wirkd_child = start_wirkd(&estate);

    let (ok, _, err) = atlas(&estate, &["resolve", "--coordinate", "not-hex-json"]);
    assert!(!ok, "a malformed coordinate must be refused, not accepted");
    assert!(
        err.contains("MalformedCoordinate"),
        "expected MalformedCoordinate, got: {err}"
    );

    let (ok, _, err) = atlas(
        &estate,
        &[
            "search",
            "--work",
            "work-does-not-exist",
            "--query",
            "anything",
        ],
    );
    assert!(!ok, "an unknown Work id must be refused, not accepted");
    assert!(err.contains("NotFound"), "expected NotFound, got: {err}");

    // wirkd is still alive and answering after both refusals — no crash.
    let (ok, _, err) = atlas(&estate, &["status"]);
    assert!(
        ok,
        "wirkd must still be serving after typed refusals: {err}"
    );

    stop_wirkd(&estate, wirkd_child);
}

// ---- 6. continuation pins the exact generation vector (ruling 0093) -------

#[test]
fn continuation_pins_generations_across_refresh_publish_and_restart() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    install_smoke_route(&estate);

    // Two separate files, not one file with two lines: the default (`v4`)
    // extractor packs consecutive short lines of one file into a single
    // unit up to its 65536-byte budget (`wirk-atlas/src/extract.rs`), and
    // `atlas search` returns at most one hit per unit — one file with both
    // needle lines would derive one unit and one hit, not two. Two
    // distinct resources still derive two distinct units and two hits
    // regardless of packing, which is what this continuation/pinning
    // check needs.
    let repo = dir.path().join("repo");
    seed_repo(&repo, "alpha.rs", "// needle marker alpha\nfn one() {}\n");
    fs::write(repo.join("beta.rs"), "// needle marker beta\nfn two() {}\n").expect("write beta");
    git(&repo, &["add", "beta.rs"]);
    git(&repo, &["commit", "-q", "-m", "beta"]);

    let mut wirkd_child = start_wirkd(&estate);
    let (ok, acquired, err) = atlas(
        &estate,
        &[
            "acquire",
            "--source",
            "wirk",
            "--repository",
            repo.to_str().unwrap(),
            "--revision",
            "HEAD",
        ],
    );
    assert!(ok, "acquire failed: {err}");
    let generation_1 = acquired["generation"]["generation"].as_str().unwrap();
    let (ok, _, err) = atlas(
        &estate,
        &["publish", "--source", "wirk", "--generation", generation_1],
    );
    assert!(ok, "publish 1 failed: {err}");

    let work_id = submit_work(&estate, &["wirk:write"], "wirk", &repo);

    let (ok, page_1, err) = atlas(
        &estate,
        &[
            "search", "--work", &work_id, "--query", "needle", "--limit", "1",
        ],
    );
    assert!(ok, "page 1 failed: {err}");
    assert_eq!(page_1["hits"].as_array().unwrap().len(), 1);
    assert!(
        page_1["truncated"].as_bool().unwrap(),
        "one of two hits must be truncated at limit 1"
    );
    let token = page_1["continuation"]
        .as_str()
        .expect("a truncated answer must carry a continuation token")
        .to_string();
    let first_hit_path_line = (
        page_1["hits"][0]["path"].as_str().unwrap().to_string(),
        page_1["hits"][0]["line_start"].as_u64().unwrap(),
    );

    // Refresh and publish a genuinely new generation before continuing:
    // add a third file with its own needle, so the refreshed generation
    // has three needle-bearing units.
    fs::write(
        repo.join("gamma.rs"),
        "// needle marker gamma\nfn three() {}\n",
    )
    .expect("write v2");
    git(&repo, &["add", "gamma.rs"]);
    git(&repo, &["commit", "-q", "-m", "v2"]);
    let (ok, refreshed, err) = atlas(
        &estate,
        &["refresh", "--source", "wirk", "--revision", "HEAD"],
    );
    assert!(ok, "refresh failed: {err}");
    let generation_2 = refreshed["generation"]["generation"].as_str().unwrap();
    let (ok, _, err) = atlas(
        &estate,
        &["publish", "--source", "wirk", "--generation", generation_2],
    );
    assert!(ok, "publish 2 failed: {err}");

    // A fresh (non-continued) query now sees three needles.
    let (ok, fresh, err) = atlas(
        &estate,
        &[
            "search", "--work", &work_id, "--query", "needle", "--limit", "10",
        ],
    );
    assert!(ok, "fresh query failed: {err}");
    assert_eq!(
        fresh["hits"].as_array().unwrap().len(),
        3,
        "fresh query must see the new publication"
    );

    // The continuation still reads the *pinned* (old, two-needle)
    // generation, not the one just published.
    let (ok, page_2, err) = atlas(
        &estate,
        &[
            "search",
            "--work",
            &work_id,
            "--query",
            "needle",
            "--limit",
            "1",
            "--continue",
            &token,
        ],
    );
    assert!(ok, "continued page failed: {err}");
    let hits_2 = page_2["hits"].as_array().unwrap();
    assert_eq!(
        hits_2.len(),
        1,
        "the pinned two-needle generation has exactly one more hit"
    );
    assert!(
        !page_2["truncated"].as_bool().unwrap(),
        "the second of two pinned hits must not itself be truncated"
    );
    let second_hit_path_line = (
        hits_2[0]["path"].as_str().unwrap().to_string(),
        hits_2[0]["line_start"].as_u64().unwrap(),
    );
    assert_ne!(
        first_hit_path_line, second_hit_path_line,
        "continuation must not re-return the same hit its own first page already returned"
    );
    assert_eq!(
        page_2["generations"], page_1["generations"],
        "a continued answer must read the exact generation vector its own token captured"
    );

    // A continuation cannot be reused with a different query.
    let (ok, _, err) = atlas(
        &estate,
        &[
            "search",
            "--work",
            &work_id,
            "--query",
            "something-else",
            "--limit",
            "1",
            "--continue",
            &token,
        ],
    );
    assert!(
        !ok,
        "a continuation must be refused when the restated query does not match"
    );
    assert!(
        err.contains("ContinuationMismatch"),
        "expected ContinuationMismatch, got: {err}"
    );

    // The continuation survives a daemon restart (it is self-contained,
    // never held in server-side session state).
    stop_wirkd(&estate, wirkd_child);
    wirkd_child = start_wirkd(&estate);
    let (ok, page_2_after_restart, err) = atlas(
        &estate,
        &[
            "search",
            "--work",
            &work_id,
            "--query",
            "needle",
            "--limit",
            "1",
            "--continue",
            &token,
        ],
    );
    assert!(ok, "continuation after restart failed: {err}");
    assert_eq!(
        page_2_after_restart["hits"], page_2["hits"],
        "the same continuation token must reproduce the identical page after a restart"
    );

    stop_wirkd(&estate, wirkd_child);
}

// ---- 7. relate requires a real producing action, not a borrowed Work id --

#[test]
fn relate_binds_the_producer_to_the_current_run_and_world_not_a_past_claim() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    install_smoke_route(&estate);

    let repo = dir.path().join("repo");
    seed_repo(&repo, "lib.rs", "fn validate_claim() {}\n");

    let wirkd_child = start_wirkd(&estate);
    let (ok, acquired, err) = atlas(
        &estate,
        &[
            "acquire",
            "--source",
            "wirk",
            "--repository",
            repo.to_str().unwrap(),
            "--revision",
            "HEAD",
        ],
    );
    assert!(ok, "acquire failed: {err}");
    let generation = acquired["generation"]["generation"].as_str().unwrap();
    let (ok, _, err) = atlas(
        &estate,
        &["publish", "--source", "wirk", "--generation", generation],
    );
    assert!(ok, "publish failed: {err}");

    // A Work with real read grants and a real open Run — the ordinary
    // shape of an actor part-way through an investigation, with nothing
    // claimed yet (ruling 0095).
    // The Work writes into its own output checkout and holds only
    // `wirk:read` on the evidence source — ruling 0077's shape: a Read
    // knowledge source stays usable for a real investigation, and the
    // assertion never needs Write on it.
    let out_repo = dir.path().join("out-repo");
    seed_repo(&out_repo, "seed.txt", "output repository\n");
    let (work_id, run_id) =
        submit_work_with_run(&estate, &["out:write", "wirk:read"], "out", &out_repo);
    let other = seed_other_work(&estate, &out_repo);
    let (ok, search, err) = atlas(
        &estate,
        &["search", "--work", &work_id, "--query", "validate_claim"],
    );
    assert!(ok, "search failed: {err}");
    assert_eq!(search["hits"].as_array().unwrap().len(), 1);
    let coordinate = first_hit_coordinate(&search);

    let relate = |work: &str, extra: Vec<&str>| {
        let mut args = vec![
            "relate",
            "--work",
            work,
            "--kind",
            "governed_by",
            "--from",
            &coordinate,
            "--to",
            &coordinate,
            "--evidence",
            &coordinate,
        ];
        args.extend(extra);
        atlas(&estate, &args)
    };

    // Positive: asserting *during* the work, before any Claim exists.
    let (ok, relationship, err) = relate(&work_id, vec![]);
    assert!(ok, "an open Run is a real producing action: {err}");
    let world = relationship["producing_action"]["world"]
        .as_str()
        .expect("the receipt names the World the Run was reserved against")
        .to_string();
    assert_eq!(
        relationship["producer"].as_str(),
        Some(format!("explicit-admission/v1/work:{work_id}/run:{run_id}/world:{world}").as_str())
    );

    // Stating the receipt is checked, not believed.
    let (ok, _, err) = relate(&work_id, vec!["--run", &other.run]);
    assert!(
        !ok,
        "another Work's Run must not be adopted as the producer"
    );
    assert!(err.contains("ProducingActionMismatch"), "got: {err}");
    let (ok, _, err) = relate(&work_id, vec!["--run", "run-forged-9"]);
    assert!(!ok, "an invented Run id must not be adopted");
    assert!(err.contains("ProducingActionMismatch"), "got: {err}");
    let (ok, _, err) = relate(&work_id, vec!["--run", &run_id, "--world", &"0".repeat(64)]);
    assert!(
        !ok,
        "the right Run with the wrong World must not be adopted"
    );
    assert!(err.contains("ProducingActionMismatch"), "got: {err}");

    // Negative that ruling 0095 turns on: once the producing action is
    // spent, the Claim it produced is history, not standing authority to
    // assert something new.
    claim_report(&estate, &work_id, &run_id);
    let (ok, _, err) = relate(&work_id, vec![]);
    assert!(
        !ok,
        "a completed Work's past Claim must not confer perpetual assertion authority"
    );
    assert!(
        err.contains("NoAdmittedProducingAction"),
        "expected NoAdmittedProducingAction, got: {err}"
    );

    // And a Work id that never named a real Work is still refused
    // outright, before any producing-action question is reached.
    let (ok, _, err) = relate("work-forged-9", vec![]);
    assert!(!ok, "a forged Work id must not resolve");
    assert!(err.contains("NotFound"), "got: {err}");

    stop_wirkd(&estate, wirkd_child);
}

/// A second, unrelated Work in the same estate, used as the source of a
/// *foreign* Run id for the producing-action negatives.
struct OtherWork {
    run: String,
}

fn seed_other_work(estate: &Path, repo: &Path) -> OtherWork {
    let (_, run) = submit_work_with_run(estate, &["out:write", "wirk:read"], "out", repo);
    OtherWork { run }
}

// ---- 8. status --work never discloses a denied source's locator ----------

#[test]
fn status_work_scope_hides_a_denied_sources_locator() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    install_smoke_route(&estate);

    let wirk_repo = dir.path().join("wirk-repo");
    seed_repo(&wirk_repo, "lib.rs", "fn validate_claim() {}\n");
    let workspace_repo = dir.path().join("workspace-repo");
    seed_repo(&workspace_repo, "claim-contract.md", "# claim contract\n");

    let wirkd_child = start_wirkd(&estate);
    for (source, repo) in [("wirk", &wirk_repo), ("workspace", &workspace_repo)] {
        let (ok, acquired, err) = atlas(
            &estate,
            &[
                "acquire",
                "--source",
                source,
                "--repository",
                repo.to_str().unwrap(),
                "--revision",
                "HEAD",
            ],
        );
        assert!(ok, "acquire {source} failed: {err}");
        let generation = acquired["generation"]["generation"].as_str().unwrap();
        let (ok, _, err) = atlas(
            &estate,
            &["publish", "--source", source, "--generation", generation],
        );
        assert!(ok, "publish {source} failed: {err}");
    }

    // Unscoped (estate-wide administration): both sources, both locators.
    let (ok, unscoped, err) = atlas(&estate, &["status"]);
    assert!(ok, "unscoped status failed: {err}");
    assert_eq!(unscoped["sources_total"].as_u64(), Some(2));
    assert_eq!(unscoped["sources"].as_array().unwrap().len(), 2);
    assert!(!unscoped["work_scoped"].as_bool().unwrap());

    // A Work granted only `wirk` must not see `workspace`'s locator at all.
    let work_id = submit_work(&estate, &["wirk:read"], "wirk", &wirk_repo);
    let (ok, scoped, err) = atlas(&estate, &["status", "--work", &work_id]);
    assert!(ok, "scoped status failed: {err}");
    assert!(scoped["work_scoped"].as_bool().unwrap());
    assert_eq!(
        scoped["sources_total"].as_u64(),
        Some(1),
        "ruling 0095: a Work-scoped total counts only what this scope admits — counting denied aliases made the total itself an existence oracle"
    );
    let shown = scoped["sources"].as_array().unwrap();
    assert_eq!(shown.len(), 1, "only the admitted source may be disclosed");
    assert_eq!(shown[0]["membership"]["alias"].as_str(), Some("wirk"));
    let scoped_text = scoped.to_string();
    assert!(
        !scoped_text.contains(workspace_repo.to_str().unwrap()),
        "a denied source's real locator must never be disclosed through Work-scoped status"
    );

    // Ruling 0095: a denied alias and an alias that does not exist must
    // be indistinguishable through a Work-scoped call. `registered` —
    // which answered `true` for the former and `false` for the latter —
    // is not part of the scoped answer at all.
    let (ok, denied_named, err) = atlas(
        &estate,
        &["status", "--work", &work_id, "--source", "workspace"],
    );
    assert!(ok, "scoped status of a denied source failed: {err}");
    let (ok, absent_named, err) = atlas(
        &estate,
        &["status", "--work", &work_id, "--source", "no-such-alias"],
    );
    assert!(ok, "scoped status of an unknown source failed: {err}");
    assert_eq!(denied_named["admitted"].as_bool(), Some(false));
    assert_eq!(absent_named["admitted"].as_bool(), Some(false));
    assert!(denied_named["registered"].is_null());
    assert!(absent_named["registered"].is_null());
    assert_eq!(
        denied_named["sources_total"], absent_named["sources_total"],
        "the two answers must be indistinguishable"
    );
    assert_eq!(denied_named["sources"], absent_named["sources"]);

    // The explicit administrative surface (no `--work`) is unchanged and
    // still answers the catalog question directly (ruling 0093).
    let (ok, admin, err) = atlas(&estate, &["status", "--source", "workspace"]);
    assert!(ok, "administrative status failed: {err}");
    assert_eq!(admin["registered"].as_bool(), Some(true));
    let (ok, admin_absent, err) = atlas(&estate, &["status", "--source", "no-such-alias"]);
    assert!(ok, "administrative status failed: {err}");
    assert_eq!(admin_absent["registered"].as_bool(), Some(false));

    stop_wirkd(&estate, wirkd_child);
}

// ---- 9. denial and a fresh estate are distinct from a genuine no_match ----

#[test]
fn denied_and_fresh_estate_are_distinct_from_no_match() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    install_smoke_route(&estate);
    let wirkd_child = start_wirkd(&estate);

    // A fresh estate: no membership registered at all.
    let (ok, fresh, err) = atlas(&estate, &["search", "--query", "anything"]);
    assert!(ok, "fresh estate search failed: {err}");
    assert!(fresh["coverage"]["no_sources"].as_bool().unwrap());
    assert!(!fresh["coverage"]["no_match"].as_bool().unwrap());

    let repo = dir.path().join("repo");
    seed_repo(&repo, "lib.rs", "fn validate_claim() {}\n");
    let (ok, acquired, err) = atlas(
        &estate,
        &[
            "acquire",
            "--source",
            "wirk",
            "--repository",
            repo.to_str().unwrap(),
            "--revision",
            "HEAD",
        ],
    );
    assert!(ok, "acquire failed: {err}");
    let generation = acquired["generation"]["generation"].as_str().unwrap();
    let (ok, _, err) = atlas(
        &estate,
        &["publish", "--source", "wirk", "--generation", generation],
    );
    assert!(ok, "publish failed: {err}");

    // A registered-but-ungranted source: the Work's scope denies it.
    let work_id = submit_work(&estate, &["some-other-name:read"], "some-other-name", &repo);
    let (ok, denied, err) = atlas(
        &estate,
        &["search", "--work", &work_id, "--query", "validate_claim"],
    );
    assert!(ok, "denied search failed: {err}");
    assert!(denied["coverage"]["denied"].as_bool().unwrap());
    assert!(!denied["coverage"]["no_match"].as_bool().unwrap());
    assert_eq!(denied["admission"]["admitted"].as_u64(), Some(0));

    // A genuinely admitted, fully searched, zero-hit query.
    let admitted_work_id = submit_work(&estate, &["wirk:read"], "wirk", &repo);
    let (ok, no_match, err) = atlas(
        &estate,
        &[
            "search",
            "--work",
            &admitted_work_id,
            "--query",
            "no_such_identifier_anywhere",
        ],
    );
    assert!(ok, "no-match search failed: {err}");
    assert!(no_match["coverage"]["no_match"].as_bool().unwrap());
    assert!(!no_match["coverage"]["denied"].as_bool().unwrap());
    assert!(!no_match["coverage"]["no_sources"].as_bool().unwrap());

    stop_wirkd(&estate, wirkd_child);
}

// ---- 10. failed acquire/refresh/resolve exit non-zero, with full JSON ----

#[test]
fn failed_atlas_operations_exit_nonzero_with_full_diagnostics() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    let wirkd_child = start_wirkd(&estate);

    let repo = dir.path().join("repo");
    seed_repo(&repo, "lib.rs", "fn validate_claim() {}\n");
    let (ok, acquired, err) = atlas(
        &estate,
        &[
            "acquire",
            "--source",
            "wirk",
            "--repository",
            repo.to_str().unwrap(),
            "--revision",
            "HEAD",
        ],
    );
    assert!(ok, "acquire failed: {err}");
    assert_eq!(acquired["outcome"].as_str(), Some("staged"));

    // A refresh at an unresolvable ref genuinely fails (outcome
    // "unavailable") - the exit code must say so even though the JSON
    // itself is a well-formed, successful *reply*.
    let (ok, refreshed, _err) = atlas(
        &estate,
        &[
            "refresh",
            "--source",
            "wirk",
            "--revision",
            "not-a-real-ref",
        ],
    );
    assert!(!ok, "a failed refresh must exit non-zero");
    assert_eq!(refreshed["outcome"].as_str(), Some("unavailable"));

    stop_wirkd(&estate, wirkd_child);
}

// ---- 10. a continuation must be an answer THIS estate actually issued ----

/// Ruling 0095 / W3-SECOND-CORRECTION.md item 1, from the
/// source-public-w3-correction-verify VERDICT §3 X1: `handle_atlas_search`
/// used to copy a token's `generations`/`offset` into its own comparison
/// object, so those two fields — the ones that decide which immutable
/// blobs get read — were never checked against anything. A client that
/// had seen one honest answer could rewrite the pinned generation to a
/// source its own scope *denies* and read that source's path, revision,
/// generation id and snippet bytes, with the answer calling itself
/// `complete`.
///
/// Two independent things are pinned here, because either alone leaves a
/// hole. The token now carries an HMAC under a secret only the daemon
/// holds, so a token nobody was issued is not a token; and
/// `wirk_atlas::search` binds a pinned generation to its own membership's
/// source before reading anything (`wirk-atlas/tests/correction_contract.rs`
/// pins that half at the library boundary, where a forged token cannot
/// reach).
#[test]
fn a_forged_continuation_cannot_read_a_denied_sources_content() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    install_smoke_route(&estate);

    // One repository, two revisions, registered under two aliases: the
    // ordinary shape that makes the leak reachable — the denied source's
    // blobs live in a repository the Work does have an admitted
    // membership on.
    let repo = dir.path().join("repo");
    seed_repo(&repo, "public.md", "alpha public note\n");
    fs::write(repo.join("secret.md"), "alpha SECRET-CANARY-9271\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "rev2 adds the canary"]);
    let rev2 = git(&repo, &["rev-parse", "HEAD"]);
    let rev1 = git(&repo, &["rev-parse", "HEAD~1"]);

    let wirkd_child = start_wirkd(&estate);
    let mut generations = Vec::new();
    for (alias, revision) in [("mutable", &rev1), ("secret", &rev2)] {
        let (ok, acquired, err) = atlas(
            &estate,
            &[
                "acquire",
                "--source",
                alias,
                "--repository",
                repo.to_str().unwrap(),
                "--revision",
                revision,
            ],
        );
        assert!(ok, "acquire {alias} failed: {err}");
        let generation = acquired["generation"]["generation"]
            .as_str()
            .unwrap()
            .to_string();
        let (ok, _, err) = atlas(
            &estate,
            &["publish", "--source", alias, "--generation", &generation],
        );
        assert!(ok, "publish {alias} failed: {err}");
        generations.push(generation);
    }
    let denied_generation = generations[1].clone();

    let out_repo = dir.path().join("out-repo");
    seed_repo(&out_repo, "seed.txt", "output repository\n");
    let work_id = submit_work(&estate, &["out:write", "mutable:read"], "out", &out_repo);

    // The Work's honest view: the canary is simply not there.
    let (ok, honest, err) = atlas(
        &estate,
        &["search", "--work", &work_id, "--query", "CANARY"],
    );
    assert!(ok, "honest search failed: {err}");
    assert!(honest["hits"].as_array().unwrap().is_empty());
    assert!(honest["coverage"]["no_match"].as_bool().unwrap());

    // One real answer, and its real token.
    let (ok, page, err) = atlas(&estate, &["search", "--work", &work_id, "--query", "alpha"]);
    assert!(ok, "search failed: {err}");
    let token = page["continuation"].as_str().expect("a token").to_string();
    let admitted_membership = page["generations"][0]["membership"]
        .as_str()
        .unwrap()
        .to_string();

    // Forge: same work, same query, the pinned generation swapped for the
    // denied source's.
    let body = serde_json::json!({
        "work": work_id,
        "query": "CANARY",
        "source": serde_json::Value::Null,
        "families": [],
        "semantic": serde_json::Value::Null,
        "limit": 10,
        "offset": 0,
        "generations": [[admitted_membership, denied_generation]],
    });
    let forged_body = hex_of(&serde_json::to_vec(&body).unwrap());
    let mut candidates = vec![
        // A wholly client-minted token: every field is one this caller
        // legitimately knows, and no tag at all. This is the shape the
        // pre-correction code accepted outright.
        ("unsigned", forged_body.clone()),
    ];
    if let Some((_, tag)) = token.split_once('.') {
        // The same body wearing another answer's tag.
        candidates.push(("someone else's tag", format!("{forged_body}.{tag}")));
    }
    for (label, candidate) in candidates {
        let (ok, value, err) = atlas(
            &estate,
            &[
                "search",
                "--work",
                &work_id,
                "--query",
                "CANARY",
                "--continue",
                &candidate,
            ],
        );
        assert!(!ok, "{label}: a forged continuation must be refused");
        assert!(
            err.contains("ForgedContinuation") || err.contains("MalformedContinuation"),
            "{label}: expected a forgery refusal, got: {err}"
        );
        assert!(
            !value.to_string().contains("SECRET-CANARY-9271"),
            "{label}: the denied source's bytes must never be returned"
        );
    }

    // The estate's own token still works, unchanged.
    let (ok, second, err) = atlas(
        &estate,
        &[
            "search",
            "--work",
            &work_id,
            "--query",
            "alpha",
            "--continue",
            &token,
        ],
    );
    assert!(ok, "the estate's own token must still be honoured: {err}");
    assert_eq!(
        second["generations"][0]["generation"].as_str(),
        Some(generations[0].as_str()),
        "and it must still read the generation it captured"
    );

    stop_wirkd(&estate, wirkd_child);
}

// ---- 11. a spent continuation window is not a corpus no-match ------------

/// VERDICT §3 X2: `query.rs` guarded `no_match` against `truncated` and
/// unavailable sources, but not against an `offset` at or past
/// `total_candidates` — so the page *after* the last page said
/// `no_match: true` beside a `budget.total_candidates` of 12. Both
/// renderings are checked: `--json` is the acceptance surface, and the
/// plain-text summary must not disagree with it.
#[test]
fn an_exhausted_continuation_window_is_spent_not_a_no_match() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    install_smoke_route(&estate);

    let repo = dir.path().join("repo");
    seed_repo(&repo, "a.md", "alpha one\n");
    for name in ["b.md", "c.md"] {
        fs::write(repo.join(name), "alpha more\n").unwrap();
    }
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "more"]);

    let wirkd_child = start_wirkd(&estate);
    let (ok, acquired, err) = atlas(
        &estate,
        &[
            "acquire",
            "--source",
            "wirk",
            "--repository",
            repo.to_str().unwrap(),
            "--revision",
            "HEAD",
        ],
    );
    assert!(ok, "acquire failed: {err}");
    let generation = acquired["generation"]["generation"].as_str().unwrap();
    let (ok, _, err) = atlas(
        &estate,
        &["publish", "--source", "wirk", "--generation", generation],
    );
    assert!(ok, "publish failed: {err}");

    let (ok, page, err) = atlas(&estate, &["search", "--query", "alpha", "--limit", "3"]);
    assert!(ok, "search failed: {err}");
    let total = page["budget"]["total_candidates"].as_u64().unwrap();
    assert_eq!(page["budget"]["returned"].as_u64(), Some(total));
    assert!(page["coverage"]["complete"].as_bool().unwrap());
    let token = page["continuation"].as_str().unwrap().to_string();

    let (ok, spent, err) = atlas(
        &estate,
        &[
            "search",
            "--query",
            "alpha",
            "--limit",
            "3",
            "--continue",
            &token,
        ],
    );
    assert!(ok, "a spent window is not an error: {err}");
    assert_eq!(spent["budget"]["returned"].as_u64(), Some(0));
    assert_eq!(spent["budget"]["total_candidates"].as_u64(), Some(total));
    assert!(
        spent["coverage"]["spent"].as_bool().unwrap(),
        "the window is spent"
    );
    assert!(
        !spent["coverage"]["no_match"].as_bool().unwrap(),
        "and must not also claim the corpus held nothing it just returned {total} candidates from"
    );

    // P3 native closeout item 3. The spent page returned zero rows, so
    // the offset a token would carry is the offset it already used and
    // the next request would be byte-identical to this one. It hands
    // back no continuation, so the walk ends here.
    //
    // Red before the change, and the shape observed live in
    // `p3-sources/source-coverage-verify/raw/p4-walk.txt`: this page
    // carried a fresh token, and following it produced this same page
    // again, forever.
    assert!(
        spent["continuation"].is_null(),
        "a page that returned nothing must not hand back a token that repeats it: {spent}"
    );

    // The walk itself, driven to its end rather than described: from the
    // first page, follow every token handed back. It must stop, and it
    // must stop having seen every candidate — a walk that ends early
    // would be this change trading a loop for lost coverage.
    let mut seen = 0u64;
    let mut next = Some(
        atlas(&estate, &["search", "--query", "alpha", "--limit", "1"])
            .1
            .clone(),
    );
    let mut pages = 0;
    while let Some(page) = next.take() {
        pages += 1;
        assert!(pages <= 20, "the walk did not terminate: {page}");
        seen += page["budget"]["returned"].as_u64().unwrap();
        if let Some(token) = page["continuation"].as_str() {
            let token = token.to_string();
            let (ok, following, err) = atlas(
                &estate,
                &[
                    "search",
                    "--query",
                    "alpha",
                    "--limit",
                    "1",
                    "--continue",
                    &token,
                ],
            );
            assert!(ok, "following the walk failed: {err}");
            next = Some(following);
        }
    }
    assert_eq!(
        seen, total,
        "the walk must end having returned every candidate, not early: saw {seen} of {total}"
    );

    // A genuine no-match over the same corpus is still a no-match.
    let (ok, none, err) = atlas(&estate, &["search", "--query", "zzzznothing"]);
    assert!(ok, "search failed: {err}");
    assert!(none["coverage"]["no_match"].as_bool().unwrap());
    assert!(!none["coverage"]["spent"].as_bool().unwrap());

    stop_wirkd(&estate, wirkd_child);
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ---- 12. the completed admitted corpus, through the public surface -------

/// P3 W3 extractor completion (`W3-EXTRACTOR-COMPLETION.md`, under ruling
/// 0095). The second-correction HANDOFF measured the gap and reported it;
/// this pins it closed. The families are the installed `semble` 0.5.2
/// reference's own (`semble.index.files.get_extensions()` for
/// code/docs/config), so the construction control compares like with
/// like instead of two different admitted sets.
///
/// The negatives matter as much as the positives: the reference's own
/// `_DATA_LANGUAGES` (`.json`, `.csv`) are claimed by no content type and
/// must stay unsupported, and the secret-like exclusions must survive the
/// widening — "do not index secret fixtures merely to match counts".
#[test]
fn the_admitted_corpus_covers_code_docs_and_config_without_widening_exclusions() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    install_smoke_route(&estate);

    let repo = dir.path().join("repo");
    seed_repo(&repo, "seed.txt", "seed\n");
    for (rel, body) in [
        ("src/lib.rs", "fn spandex_marker() {}\n"),
        ("docs/guide.md", "# spandex_marker\n"),
        ("notes/readme.rst", "spandex_marker in rst\n"),
        ("tools/analyze.py", "def spandex_marker():\n    return 1\n"),
        ("tools/build.sh", "# spandex_marker\n"),
        ("web/app.js", "function spandex_marker() {}\n"),
        ("native/core.c", "int spandex_marker(void) { return 1; }\n"),
        ("Cargo.toml", "name = \"spandex_marker\"\n"),
        ("ci/pipeline.yml", "spandex_marker: true\n"),
        ("conf/app.ini", "spandex_marker = 1\n"),
        // reference data languages: claimed by no content type
        ("data/blob.json", "{\"spandex_marker\": 1}\n"),
        ("data/rows.csv", "spandex_marker,1\n"),
        // exclusions that must survive the widening
        ("fixtures/secret.pem", "spandex_marker\n"),
        ("fixtures/id.key", "spandex_marker\n"),
        (".env", "TOKEN=spandex_marker\n"),
        ("secret/creds.toml", "token = \"spandex_marker\"\n"),
    ] {
        let path = repo.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }
    git(&repo, &["add", "-A", "-f"]);
    git(&repo, &["commit", "-q", "-m", "corpus"]);
    let rev = git(&repo, &["rev-parse", "HEAD"]);

    let wirkd_child = start_wirkd(&estate);
    let (ok, acquired, err) = atlas(
        &estate,
        &[
            "acquire",
            "--source",
            "corpus",
            "--repository",
            repo.to_str().unwrap(),
            "--revision",
            &rev,
        ],
    );
    assert!(ok, "acquire failed: {err}");
    let generation = acquired["generation"]["generation"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        acquired["generation"]["extractor_set"].as_str(),
        Some("text-multiline-chunks/utf8-multiline-chunks-65536+semble-0.5.2-content-families/v4"),
        "the identity must name the edition and say plainly that it is text line chunks, not syntax"
    );
    let (ok, _, err) = atlas(
        &estate,
        &["publish", "--source", "corpus", "--generation", &generation],
    );
    assert!(ok, "publish failed: {err}");

    let paths_for = |family: Option<&str>| {
        let mut args = vec!["search", "--query", "spandex_marker", "--limit", "50"];
        if let Some(family) = family {
            args.push("--family");
            args.push(family);
        }
        let (ok, value, err) = atlas(&estate, &args);
        assert!(ok, "search failed: {err}");
        let mut paths: Vec<String> = value["hits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|hit| hit["path"].as_str().unwrap().to_string())
            .collect();
        paths.sort();
        paths
    };

    assert_eq!(
        paths_for(Some("code")),
        vec![
            "native/core.c",
            "src/lib.rs",
            "tools/analyze.py",
            "tools/build.sh",
            "web/app.js"
        ]
    );
    assert_eq!(
        paths_for(Some("knowledge")),
        vec!["docs/guide.md", "notes/readme.rst"]
    );
    assert_eq!(
        paths_for(Some("config")),
        vec!["Cargo.toml", "ci/pipeline.yml", "conf/app.ini"]
    );

    let all = paths_for(None);
    for excluded in [
        "fixtures/secret.pem",
        "fixtures/id.key",
        ".env",
        "secret/creds.toml",
    ] {
        assert!(
            !all.contains(&excluded.to_string()),
            "{excluded} must stay excluded: widening the vocabulary must not widen disclosure"
        );
    }
    for data_language in ["data/blob.json", "data/rows.csv"] {
        assert!(
            !all.contains(&data_language.to_string()),
            "{data_language} is a reference data language no content type claims"
        );
    }

    // Exact byte/path/span provenance on a newly admitted file: the hit's
    // own coordinate must resolve to the exact bytes the real committed
    // file holds at that span.
    let (ok, search, err) = atlas(
        &estate,
        &[
            "search",
            "--query",
            "spandex_marker",
            "--family",
            "code",
            "--limit",
            "50",
        ],
    );
    assert!(ok, "search failed: {err}");
    let hit = search["hits"]
        .as_array()
        .unwrap()
        .iter()
        .find(|hit| hit["path"].as_str() == Some("tools/analyze.py"))
        .expect("the Python file is admitted");
    assert_eq!(hit["revision"].as_str(), Some(rev.as_str()));
    let (ok, resolved, err) = atlas(
        &estate,
        &[
            "resolve",
            "--coordinate",
            hit["coordinate"].as_str().unwrap(),
        ],
    );
    assert!(ok, "resolve failed: {err}");
    assert_eq!(resolved["outcome"].as_str(), Some("resolved"));
    // The default (`v4`) extractor packs both of this file's short lines
    // into one unit (well under its 65536-byte budget), so the hit's
    // coordinate spans the whole file, not just its first line.
    assert_eq!(
        resolved["text"].as_str(),
        Some("def spandex_marker():\n    return 1\n"),
        "the span must be the real committed bytes, not a re-render"
    );

    // A dirty working tree is not the corpus.
    fs::write(repo.join("tools/analyze.py"), "# TAMPERED\n").unwrap();
    fs::write(repo.join("tools/untracked.py"), "# TAMPERED\n").unwrap();
    let (ok, tampered, err) = atlas(&estate, &["search", "--query", "TAMPERED"]);
    assert!(ok, "search failed: {err}");
    assert!(tampered["hits"].as_array().unwrap().is_empty());
    assert!(tampered["coverage"]["no_match"].as_bool().unwrap());
    let (ok, reacquired, err) = atlas(
        &estate,
        &[
            "acquire",
            "--source",
            "corpus",
            "--repository",
            repo.to_str().unwrap(),
            "--revision",
            &rev,
        ],
    );
    assert!(ok, "re-acquire failed: {err}");
    assert_eq!(
        reacquired["generation"]["generation"].as_str(),
        Some(generation.as_str()),
        "the same revision acquired over a dirty checkout is the same generation"
    );

    stop_wirkd(&estate, wirkd_child);
}

// ---- 13. an unsupported CLI flag fails instead of being ignored ---------

/// The second-correction HANDOFF's limit 6: both native actors passed
/// `--source wirk` to `wirk atlas relate`, which has no such flag. It was
/// silently dropped, and their reports read as though it had applied.
/// A flag that is not understood must say so.
#[test]
fn an_unknown_atlas_flag_is_a_typed_failure_not_a_silent_drop() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    install_smoke_route(&estate);
    let repo = dir.path().join("repo");
    seed_repo(&repo, "lib.rs", "fn marker() {}\n");
    let wirkd_child = start_wirkd(&estate);
    let (ok, acquired, err) = atlas(
        &estate,
        &[
            "acquire",
            "--source",
            "s",
            "--repository",
            repo.to_str().unwrap(),
            "--revision",
            "HEAD",
        ],
    );
    assert!(ok, "acquire failed: {err}");
    let generation = acquired["generation"]["generation"].as_str().unwrap();
    let (ok, _, err) = atlas(
        &estate,
        &["publish", "--source", "s", "--generation", generation],
    );
    assert!(ok, "publish failed: {err}");

    for (label, args) in [
        (
            "misspelled --family",
            vec!["search", "--query", "marker", "--famly", "code"],
        ),
        ("unknown flag on status", vec!["status", "--nonsense", "x"]),
        // the exact mistake the previous stage's actors made
        (
            "relate --source",
            vec![
                "relate",
                "--work",
                "w",
                "--kind",
                "governed_by",
                "--from",
                "x",
                "--to",
                "x",
                "--evidence",
                "x",
                "--source",
                "s",
            ],
        ),
        ("value-less flag", vec!["search", "--query"]),
        (
            "stray positional",
            vec!["search", "--query", "marker", "oops"],
        ),
    ] {
        let (ok, _, err) = atlas(&estate, &args);
        assert!(!ok, "{label}: must not succeed");
        assert!(
            err.contains("unknown flag")
                || err.contains("unexpected argument")
                || err.contains("requires a value"),
            "{label}: expected a typed flag failure, got: {err}"
        );
    }

    // The same request without the bad flag still works, so the check
    // rejects only what it should.
    let (ok, value, err) = atlas(
        &estate,
        &["search", "--query", "marker", "--family", "code"],
    );
    assert!(ok, "the well-formed command must still work: {err}");
    assert_eq!(value["hits"].as_array().unwrap().len(), 1);

    stop_wirkd(&estate, wirkd_child);
}
