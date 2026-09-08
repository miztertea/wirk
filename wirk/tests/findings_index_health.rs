//! Real-daemon, real-Git, real-Atlas proof that the derived Findings
//! index never answers as though it were complete when it is not, and
//! that one mutation costs one index rewrite rather than one per row.
//!
//! Ruling 0116 accepted the assertion-visibility component and recorded
//! two executed limits against it, both of them here:
//!
//! 1. **A failed reconciliation was invisible to every client.** The
//!    sweep's only report was `eprintln!` on the daemon's own stderr.
//!    The mutating caller got exit 0 and a complete reply; a later
//!    `atlas findings` answered from a silently stale index with no
//!    signal at all that it was behind. The journal stayed canonical, so
//!    nothing was lost — but nothing said anything was missing either,
//!    and "the search came back clean" is exactly the sentence that must
//!    never be produced by an index quietly missing rows.
//! 2. **The sweep was quadratic.** It offered every journaled row on
//!    every mutation and appended them one at a time, each append
//!    re-reading and atomically rewriting the whole file with its own
//!    `fsync` pair — O(rows²) bytes and O(rows) `fsync`s per `assert`.
//!
//! What is *not* changed, and is asserted here as much as the repairs
//! are: the journal remains the record, a mutation whose projection
//! failed still succeeds, a query never writes and never re-scans the
//! estate, and a scoped requester learns the projection's health without
//! learning one thing about the rows it may not see.

#[path = "support/nested_harness.rs"]
mod harness;
#[path = "support/route_fixture.rs"]
mod route_fixture;
#[path = "../src/wirkd/mod.rs"]
mod wirkd;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::Command;

use harness::*;
use serde_json::Value;

fn wirk_bin() -> &'static str {
    env!("CARGO_BIN_EXE_wirk")
}

fn atlas(estate: &Path, args: &[&str]) -> (Option<i32>, Value, String) {
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
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    (
        output.status.code(),
        serde_json::from_str(&stdout).unwrap_or(Value::Null),
        stderr,
    )
}

/// The same call **without** `--json`: the plain-text surface an
/// operator actually reads at a terminal, which is the surface the
/// erasure this component repairs was invisible on.
fn atlas_text(estate: &Path, args: &[&str]) -> (Option<i32>, String, String) {
    let mut full = vec!["atlas"];
    full.extend_from_slice(args);
    full.push("--estate");
    let estate_str = estate.to_str().unwrap();
    full.push(estate_str);
    let output = Command::new(wirk_bin())
        .args(&full)
        .output()
        .expect("wirk atlas runs");
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).trim().to_string(),
        String::from_utf8_lossy(&output.stderr).trim().to_string(),
    )
}

fn finding_cli(estate: &Path, args: &[&str]) -> (Option<i32>, Value, String) {
    let mut full = vec!["finding"];
    full.extend_from_slice(args);
    full.push("--estate");
    let estate_str = estate.to_str().unwrap();
    full.push(estate_str);
    full.push("--json");
    let output = Command::new(wirk_bin())
        .args(&full)
        .output()
        .expect("wirk finding runs");
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    (
        output.status.code(),
        serde_json::from_str(&stdout).unwrap_or(Value::Null),
        stderr,
    )
}

fn raise_cli(
    estate: &Path,
    work_id: &str,
    run_id: &str,
    args: &[&str],
) -> (Option<i32>, Value, String) {
    let mut full = vec!["finding", "raise"];
    full.extend_from_slice(args);
    full.push("--json");
    let output = Command::new(wirk_bin())
        .args(&full)
        .env("WIRK_ESTATE_ROOT", estate)
        .env("WIRK_WORK_ID", work_id)
        .env("WIRK_RUN_ID", run_id)
        .output()
        .expect("wirk finding raise runs");
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    (
        output.status.code(),
        serde_json::from_str(&stdout).unwrap_or(Value::Null),
        stderr,
    )
}

/// `disclosure.rs`'s own `publish_and_locate_as`, reused verbatim
/// (which is itself `findings.rs`'s): commit, acquire and publish `repo`
/// under `source`, and return the exact coordinate of the unit carrying
/// `marker`.
fn publish_and_locate_as(estate: &Path, repo: &Path, source: &str, marker: &str) -> String {
    let git = |args: &[&str]| {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(repo)
                .status()
                .unwrap()
                .success()
        );
    };
    git(&["add", "."]);
    git(&[
        "-c",
        "user.email=wb@example.test",
        "-c",
        "user.name=wb",
        "commit",
        "-q",
        "-m",
        "sources",
    ]);
    let (code, acquired, err) = atlas(
        estate,
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
    assert_eq!(code, Some(0), "{err}");
    let generation = acquired["generation"]["generation"]
        .as_str()
        .unwrap()
        .to_string();
    let (code, _, err) = atlas(
        estate,
        &["publish", "--source", source, "--generation", &generation],
    );
    assert_eq!(code, Some(0), "{err}");
    let (code, search, err) = atlas(estate, &["search", "--source", source, "--query", marker]);
    assert_eq!(code, Some(0), "{err}");
    search["hits"][0]["coordinate"]
        .as_str()
        .unwrap_or_else(|| panic!("no hit for {marker} in {source}: {search}"))
        .to_string()
}

/// One estate, one daemon, a parent Work and a helper child narrowed off
/// the parent's own `closed` source — the smallest fixture that can
/// answer both "is the projection reported honestly" and "is it reported
/// without disclosing what the requester may not see".
struct Estate {
    #[allow(dead_code)]
    dir: tempfile::TempDir,
    estate: PathBuf,
    wirkd: Option<KillOnDrop>,
    parent: Submitted,
    child: Submitted,
    open_coordinate: String,
    closed_secrets: Vec<String>,
}

impl Estate {
    fn atlas_dir(&self) -> PathBuf {
        self.estate.join("atlas")
    }

    fn index_path(&self) -> PathBuf {
        self.atlas_dir().join("findings.ndjson")
    }

    fn set_atlas_writable(&self, writable: bool) {
        let mode = if writable { 0o755 } else { 0o555 };
        fs::set_permissions(self.atlas_dir(), fs::Permissions::from_mode(mode)).unwrap();
    }

    /// Execute but **not** read (`0o111`): every path inside the atlas
    /// directory still opens by name — the index file included — and
    /// only a *listing* of the directory is denied. That is the one
    /// operation the preserved-copy question is asked through, so this
    /// isolates "the question could not be answered" from "the answer
    /// is none".
    fn set_atlas_listable(&self, listable: bool) {
        let mode = if listable { 0o755 } else { 0o111 };
        fs::set_permissions(self.atlas_dir(), fs::Permissions::from_mode(mode)).unwrap();
    }

    /// Write and execute but **not** read (`0o311`): a real kernel
    /// denial of exactly one syscall — `File::open` of the atlas
    /// directory itself, which is the directory `fsync`
    /// `AtlasStore::rewrite_rows` ends with.
    ///
    /// Every other step of a real index write still succeeds: the
    /// temporary file is created and `fsync`ed (write + execute), and
    /// the atomic `rename` that makes its rows visible to a fresh reader
    /// lands (write + execute). So what fails is the one call the
    /// product reports as `DurabilityUncertain` — rows visible, their
    /// directory entry unconfirmed — reached through the real code path
    /// with no fault injected into the product and nothing faked. A
    /// listing of the directory is denied too (it needs read), so while
    /// this is in force the preserved-copy question is also unanswerable;
    /// both facts are the estate's, and the assertions below read the
    /// record after the mode is restored for exactly that reason.
    fn set_atlas_directory_syncable(&self, syncable: bool) {
        let mode = if syncable { 0o755 } else { 0o311 };
        fs::set_permissions(self.atlas_dir(), fs::Permissions::from_mode(mode)).unwrap();
    }

    fn journal_lines(&self, work_id: &str) -> usize {
        fs::read_to_string(
            self.estate
                .join("works")
                .join(work_id)
                .join("journal.ndjson"),
        )
        .map(|text| text.lines().filter(|line| !line.trim().is_empty()).count())
        .unwrap_or(0)
    }

    fn journal_path(&self, work_id: &str) -> PathBuf {
        self.estate
            .join("works")
            .join(work_id)
            .join("journal.ndjson")
    }

    /// A real kernel denial on one canonical Work journal — the estate
    /// a restore, a `chown` or a wrongly-umasked operator leaves. The
    /// atlas, the catalog and every other Work's journal stay readable,
    /// so what is being tested is exactly the walk's own blind spot and
    /// not a broken estate.
    fn set_journal_readable(&self, work_id: &str, readable: bool) {
        let mode = if readable { 0o600 } else { 0o000 };
        fs::set_permissions(self.journal_path(work_id), fs::Permissions::from_mode(mode)).unwrap();
    }

    fn index_bytes(&self) -> Vec<u8> {
        fs::read(self.index_path()).unwrap_or_default()
    }

    /// Blocks until a Work's journal holds `lines` durable events — the
    /// real observable a concurrent mutation produces before it can
    /// reach the index at all. The deadline is a test failure, never a
    /// decision.
    fn wait_for_journal_lines(&self, work_id: &str, lines: usize) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while self.journal_lines(work_id) < lines {
            assert!(
                std::time::Instant::now() < deadline,
                "{work_id} never reached {lines} journal lines"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    fn index_rows_on_disk(&self) -> usize {
        fs::read_to_string(self.index_path())
            .map(|text| text.lines().filter(|line| !line.trim().is_empty()).count())
            .unwrap_or(0)
    }

    fn rebuild(&self) -> (Option<i32>, Value, String) {
        atlas(&self.estate, &["findings", "--admin", "--rebuild"])
    }

    fn retire_preserved(&self) -> (Option<i32>, Value, String) {
        atlas(
            &self.estate,
            &["findings", "--admin", "--retire-preserved-index"],
        )
    }

    /// A malformed **final** line, which is what a crash between a
    /// write and its `fsync` actually leaves and the exact condition
    /// `--rebuild` exists to repair. Every row already in the file is
    /// untouched.
    fn corrupt_index_tail(&self) {
        let mut text = fs::read_to_string(self.index_path()).unwrap();
        text.push_str("{\"id\": not json at all\n");
        fs::write(self.index_path(), text).unwrap();
    }

    /// A malformed line with **valid rows after it**. The all-or-nothing
    /// read refuses at this line and never sees the rest; a recovery
    /// read must, because those later rows are published evidence a
    /// replacement is about to be judged against.
    fn corrupt_index_line(&self, line: usize) {
        let text = fs::read_to_string(self.index_path()).unwrap();
        let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
        assert!(
            lines.len() >= line,
            "the index has no line {line}: {lines:?}"
        );
        lines[line - 1] = "{\"id\": not json at all".to_string();
        fs::write(self.index_path(), format!("{}\n", lines.join("\n"))).unwrap();
    }

    /// Every line unparsable: the case where the salvage recovers
    /// nothing at all and the estate's own journals are the only
    /// evidence left.
    fn make_index_wholly_unparsable(&self) {
        fs::write(self.index_path(), "not json\nnor this\n").unwrap();
    }

    fn set_index_readable(&self, readable: bool) {
        let mode = if readable { 0o600 } else { 0o000 };
        fs::set_permissions(self.index_path(), fs::Permissions::from_mode(mode)).unwrap();
    }

    fn atlas_names_with_prefix(&self, prefix: &str) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.atlas_dir())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(prefix))
            .collect();
        names.sort();
        names
    }

    fn preserved_copies(&self) -> Vec<String> {
        self.atlas_names_with_prefix("findings.ndjson.unreadable-")
    }

    fn retired_copies(&self) -> Vec<String> {
        self.atlas_names_with_prefix("findings.ndjson.retired-")
    }

    /// Raise an estate-local finding on the child, so the index holds
    /// rows from two different Works and a walk that loses one of them
    /// is visible as a loss rather than as an empty estate.
    fn raise_on_child(&self, claim: &str) -> String {
        let (code, raised, stderr) = raise_cli(
            &self.estate,
            &self.child.work_id,
            &self.child.run_id,
            &[
                "--kind",
                "gap",
                "--scope",
                "estate_local",
                "--claim",
                claim,
                "--evidence",
                &self.open_coordinate,
            ],
        );
        assert_eq!(code, Some(0), "{stderr}");
        raised["id"].as_str().unwrap().to_string()
    }

    fn assert_as_child(&self, finding: &str, reason: &str) -> (Option<i32>, Value, String) {
        finding_cli(
            &self.estate,
            &[
                "assert",
                "--finding",
                finding,
                "--decision",
                "deferred",
                "--by",
                "a reviewer",
                "--reason",
                reason,
                "--requesting-work",
                &self.child.work_id,
            ],
        )
    }

    /// Raise an estate-local finding on the parent, citing the source
    /// the whole family holds.
    fn raise(&self, claim: &str) -> String {
        let (code, raised, stderr) = raise_cli(
            &self.estate,
            &self.parent.work_id,
            &self.parent.run_id,
            &[
                "--kind",
                "gap",
                "--scope",
                "estate_local",
                "--claim",
                claim,
                "--evidence",
                &self.open_coordinate,
            ],
        );
        assert_eq!(code, Some(0), "{stderr}");
        raised["id"].as_str().unwrap().to_string()
    }

    fn assert_on(&self, finding: &str, reason: &str) -> (Option<i32>, Value, String) {
        finding_cli(
            &self.estate,
            &[
                "assert",
                "--finding",
                finding,
                "--decision",
                "deferred",
                "--by",
                "a reviewer",
                "--reason",
                reason,
                "--requesting-work",
                &self.parent.work_id,
            ],
        )
    }

    fn admin_index(&self) -> Value {
        let (code, value, stderr) = atlas(&self.estate, &["findings", "--admin"]);
        assert_eq!(code, Some(0), "{stderr}");
        value
    }

    /// The same, asked by an explicitly named Work rather than always by
    /// the child: a probe that removes the child's own journal cannot
    /// then ask a question as the child.
    fn scoped_index_as(&self, work_id: &str) -> (Value, String) {
        let (code, value, stderr) =
            atlas(&self.estate, &["findings", "--requesting-work", work_id]);
        assert_eq!(code, Some(0), "{stderr}");
        (value, stderr)
    }

    /// Move a Work's canonical journal out of the estate, keeping every
    /// byte of it: the estate a restore, a stray `mv` or a half-finished
    /// backup leaves. Nothing is ever deleted here — the point of the
    /// probe is that the daemon must not destroy the last derived trace
    /// of a record whose canonical bytes an operator may still have.
    fn hold_journal_aside(&self, work_id: &str) -> PathBuf {
        let held = self.dir.path().join(format!("held-{work_id}.ndjson"));
        fs::rename(self.journal_path(work_id), &held).unwrap();
        held
    }

    fn restore_journal(&self, work_id: &str, held: &Path) {
        let path = self.journal_path(work_id);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::copy(held, &path).unwrap();
    }

    fn scoped_index(&self) -> (Value, String) {
        let (code, value, stderr) = atlas(
            &self.estate,
            &["findings", "--requesting-work", &self.child.work_id],
        );
        assert_eq!(code, Some(0), "{stderr}");
        (value, stderr)
    }

    fn restart(&mut self, env: &[(&str, &str)]) {
        if let Some(child) = self.wirkd.take() {
            stop_wirkd(&self.estate, child);
        }
        let (child, _) = start_wirkd_with_env(&self.estate, env);
        self.wirkd = Some(child);
    }

    fn stop(&mut self) {
        if let Some(child) = self.wirkd.take() {
            stop_wirkd(&self.estate, child);
        }
    }
}

fn build_estate() -> Estate {
    let dir = tempfile::tempdir().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    route_fixture::install_route_fixture(&estate, "wa_simple_leaf");
    route_fixture::write_route(
        &estate,
        "health_container",
        r#"{"id":"health-container","waypoints":[
            {"id":"outer","kind":"Container",
             "declared_outputs":[{"name":"a.md","required":true}],
             "required_child_outcomes":[{"role":"helper","required":true}],
             "leaves":[
               {"id":"outer/leaf-a","kind":"Deterministic",
                "command":["sh","-c","echo a > a.md"],
                "declared_outputs":[{"name":"a.md","required":true}]}
             ]}
        ]}"#,
    );
    let (wirkd, _pointer) = start_wirkd(&estate);

    let closed_repo = dir.path().join("embargo-repo");
    init_repo(&closed_repo);
    write_file(
        &closed_repo,
        "embargoed.md",
        "embargomarker: the embargoed basis\n",
    );
    let closed_coordinate =
        publish_and_locate_as(&estate, &closed_repo, "embargo", "embargomarker");

    let open_repo = dir.path().join("open-repo");
    init_repo(&open_repo);
    write_file(&open_repo, "shared.md", "openmarker: the shared basis\n");
    let open_coordinate = publish_and_locate_as(&estate, &open_repo, "open", "openmarker");

    let parent_repo = dir.path().join("parent-repo");
    init_repo(&parent_repo);
    let parent = submit(
        &estate,
        "health_container",
        &parent_repo,
        &[
            "embargo:write",
            "open:read",
            "helper:write",
            "scratch:write",
        ],
        None,
    )
    .unwrap();

    let child_repo = dir.path().join("child-repo");
    init_repo(&child_repo);
    let child = submit(
        &estate,
        "health_container",
        &child_repo,
        &["scratch:write", "open:read", "helper:write"],
        Some(ParentRef {
            work: &parent.work_id,
            waypoint: "outer",
            run: &parent.run_id,
            role: "helper",
            attempt: None,
        }),
    )
    .unwrap();

    Estate {
        dir,
        estate,
        wirkd: Some(wirkd),
        parent,
        child,
        open_coordinate,
        closed_secrets: vec![
            "embargo".to_string(),
            "embargoed.md".to_string(),
            "embargomarker".to_string(),
            closed_coordinate,
        ],
    }
}

fn all_strings(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(text) => out.push(text.clone()),
        Value::Array(items) => items.iter().for_each(|item| all_strings(item, out)),
        Value::Object(map) => map.iter().for_each(|(key, item)| {
            out.push(key.clone());
            all_strings(item, out);
        }),
        _ => {}
    }
}

fn assert_discloses_nothing(what: &str, value: &Value, stderr: &str, needles: &[String]) {
    let mut strings = Vec::new();
    all_strings(value, &mut strings);
    strings.push(stderr.to_string());
    for needle in needles {
        for text in &strings {
            assert!(
                !text.contains(needle.as_str()),
                "{what} disclosed {needle:?} in {text:?}"
            );
        }
    }
}

fn row_ids(index: &Value) -> Vec<String> {
    index["rows"]
        .as_array()
        .expect("rows is a list")
        .iter()
        .map(|row| row["id"].as_str().unwrap().to_string())
        .collect()
}

/// Whether the index holds any row for `finding`.
fn indexes_finding(index: &Value, finding: &str) -> bool {
    index["rows"]
        .as_array()
        .expect("rows is a list")
        .iter()
        .any(|row| row["finding"]["id"] == finding)
}

/// Whether an estate-wide `finding list` reply names `finding`. This
/// reads the *journals*, through the sweep at §10, and not the derived
/// index — which is what makes it evidence about the walk rather than
/// about the projection.
fn lists_finding(listed: &Value, finding: &str) -> bool {
    listed["findings"]
        .as_array()
        .map(|findings| findings.iter().any(|value| value["id"] == finding))
        .unwrap_or(false)
}

fn assert_no_duplicate_rows(index: &Value) {
    let ids = row_ids(index);
    let mut unique = ids.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), ids.len(), "a row is indexed twice: {ids:?}");
}

// ---- 1. A projection that fell behind says so, everywhere -------------

/// The whole of ruling 0116's first carried limit, executed against a
/// real denial: the estate's `atlas/` directory is made read-only
/// mid-flight, so the index write fails for the reason a real one would.
///
/// What must hold, in order:
///
/// - the assertion is **accepted**, because it is journaled and durable
///   and a derived projection is not the record;
/// - its own reply says the projection is behind rather than reading
///   exactly like a synchronized one;
/// - a later `atlas findings`, admin and scoped alike, says the same —
///   this is the "the search came back clean" case, and the rows it
///   returns really are a subset;
/// - `finding list`, which reads the journal, still has the record;
/// - and when the denial is lifted, the next mutation's own sweep
///   repairs the index with no restart and no rebuild, recovering the
///   row written during the failure **and** the new one, each exactly
///   once.
#[test]
fn an_index_write_failure_is_told_to_the_mutating_caller_and_to_every_later_query() {
    let mut estate = build_estate();
    let finding = estate.raise("the first record, indexed while the estate is healthy");

    let (code, reply, stderr) = estate.assert_on(&finding, "asserted while healthy");
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        reply["index"]["projection"], "synchronized",
        "a healthy estate reports a synchronized projection: {reply}"
    );
    assert_eq!(reply["index"]["complete"], true);
    let healthy_rows = row_ids(&estate.admin_index());
    assert_eq!(healthy_rows.len(), 1, "one asserted row so far");

    // A real denial, at the real write. Nothing is stubbed: the daemon
    // meets `EACCES` from the kernel exactly as it would on a full or
    // wrongly-owned estate.
    estate.set_atlas_writable(false);
    let journal_before = estate.journal_lines(&estate.parent.work_id);
    let (code, reply, stderr) =
        estate.assert_on(&finding, "asserted while the index is unwritable");
    assert_eq!(
        code,
        Some(0),
        "the journal append succeeded, so the assertion is accepted: {stderr}"
    );
    assert_eq!(
        estate.journal_lines(&estate.parent.work_id),
        journal_before + 1,
        "the journal fact stands"
    );
    assert_eq!(
        reply["index"]["projection"], "behind",
        "the mutating caller is told its row did not reach the index: {reply}"
    );
    assert_eq!(reply["index"]["complete"], false);
    assert!(
        reply["index"]["recovery"].is_string(),
        "and is told how it recovers: {reply}"
    );
    // Scoped: the state, and not one thing about what is missing.
    assert!(
        reply["index"]["pending_rows"].is_null() && reply["index"]["detail"].is_null(),
        "a scoped reply carries no count and no error text: {reply}"
    );
    assert!(
        stderr.contains("subset"),
        "and the operator is told on stderr, in --json mode too: {stderr:?}"
    );

    // The dependent query is the surface that used to answer as though
    // nothing had happened.
    let admin = estate.admin_index();
    assert_eq!(admin["index"]["complete"], false, "{admin}");
    assert_eq!(admin["index"]["projection"], "behind");
    assert_eq!(
        admin["index"]["pending_rows"], 1,
        "administration is told how many rows are missing: {}",
        admin["index"]
    );
    assert!(
        admin["index"]["detail"]
            .as_str()
            .unwrap()
            .contains("denied"),
        "and why: {}",
        admin["index"]
    );
    assert_eq!(
        row_ids(&admin),
        healthy_rows,
        "the answer really is stale — that is why it must not look complete"
    );

    let (scoped, scoped_stderr) = estate.scoped_index();
    assert_eq!(scoped["index"]["complete"], false, "{scoped}");
    assert!(
        scoped["index"]["pending_rows"].is_null() && scoped["index"]["detail"].is_null(),
        "a narrowed reader learns the projection is behind, not what is behind it: {scoped}"
    );
    assert_discloses_nothing(
        "the scoped index health block",
        &scoped,
        &scoped_stderr,
        &estate.closed_secrets,
    );

    // The record itself was never at risk: the journal has it.
    let (code, listed, stderr) = finding_cli(
        &estate.estate,
        &["list", "--requesting-work", &estate.parent.work_id],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let assertions = listed["findings"]
        .as_array()
        .and_then(|list| list.iter().find(|row| row["id"] == finding.as_str()))
        .map(|row| row["assertions"].as_array().map(|a| a.len()).unwrap_or(0))
        .unwrap_or(0);
    assert_eq!(
        assertions, 2,
        "both assertions are in the journal, the failed projection notwithstanding: {listed}"
    );

    // Recovery: no restart, no rebuild, just the next successful sweep.
    estate.set_atlas_writable(true);
    let (code, reply, stderr) = estate.assert_on(&finding, "asserted after the denial was lifted");
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        reply["index"]["projection"], "synchronized",
        "the repaired projection says so: {reply}"
    );
    let admin = estate.admin_index();
    assert_eq!(admin["index"]["complete"], true, "{}", admin["index"]);
    assert_eq!(admin["index"]["pending_rows"], 0);
    assert_eq!(
        row_ids(&admin).len(),
        3,
        "the row written during the denial is recovered alongside the new one: {}",
        admin["rows"]
    );
    assert_no_duplicate_rows(&admin);

    estate.stop();
}

// ---- 2. A query neither writes nor re-scans ---------------------------

/// The health block is the daemon's own last reconciliation outcome, and
/// reading the index is still a pure read: no journal walk, no index
/// rewrite, no Atlas state created (`W3-CORRECTION.md` item 3). Pinned
/// on the file itself — same inode, same mtime, same bytes across
/// repeated admin and scoped queries — because "we did not turn every
/// read into an estate scan" is a claim that has to be checked, not
/// asserted in a comment.
#[test]
fn reading_the_index_reports_health_without_writing_or_rescanning() {
    let mut estate = build_estate();
    let finding = estate.raise("a record to project");
    let (code, _reply, stderr) = estate.assert_on(&finding, "the only assertion");
    assert_eq!(code, Some(0), "{stderr}");

    let identity = || {
        let meta = fs::metadata(estate.index_path()).unwrap();
        use std::os::unix::fs::MetadataExt;
        (
            meta.ino(),
            meta.mtime(),
            meta.mtime_nsec(),
            fs::read(estate.index_path()).unwrap(),
        )
    };
    let before = identity();

    for _ in 0..3 {
        let admin = estate.admin_index();
        assert_eq!(admin["index"]["projection"], "synchronized");
        assert_eq!(admin["index"]["complete"], true);
        let (scoped, _) = estate.scoped_index();
        assert_eq!(scoped["index"]["complete"], true);
        // A scoped reader is told the state and the standing caveat, and
        // gets no administrative detail even when everything is healthy.
        assert!(scoped["index"]["pending_rows"].is_null());
        assert!(
            scoped["index"]["canonical"]
                .as_str()
                .unwrap()
                .contains("journal"),
            "every reply says the journal is the record: {}",
            scoped["index"]
        );
    }

    assert_eq!(
        before,
        identity(),
        "six queries left the index file byte-identical, same inode, same mtime"
    );
    estate.stop();
}

// ---- 3. A crash between the journal and the index ---------------------

/// A **real** crash at the real write boundary: the daemon runs with
/// `WIRK_ATLAS_FAILPOINT=findings-file-synced`, so it dies by
/// `process::exit` between the durable journal append and the index
/// rename, exactly where a power loss would land.
///
/// Nothing is lost and nothing is invented: the journal keeps the
/// assertion, the index simply does not have its row yet, and the next
/// daemon start re-projects it from the journals — append history,
/// recorded identities and prior rows all preserved, no duplicate row,
/// and no automatic destruction of anything that was already there.
#[test]
fn a_crash_between_the_journal_and_the_index_is_repaired_at_the_next_start() {
    let mut estate = build_estate();
    let first = estate.raise("a record indexed before the crash");
    let (code, _reply, stderr) = estate.assert_on(&first, "indexed cleanly");
    assert_eq!(code, Some(0), "{stderr}");
    let before_crash = row_ids(&estate.admin_index());
    assert_eq!(before_crash.len(), 1);

    // The daemon that will die at its own index write.
    estate.restart(&[("WIRK_ATLAS_FAILPOINT", "findings-file-synced")]);
    let second = estate.raise("a record whose index write is interrupted");
    let journal_before = estate.journal_lines(&estate.parent.work_id);
    let (code, _reply, _stderr) = estate.assert_on(&second, "asserted into a dying daemon");
    assert_ne!(
        code,
        Some(0),
        "the daemon died mid-handler, so the client's call did not complete"
    );
    assert_eq!(
        estate.journal_lines(&estate.parent.work_id),
        journal_before + 1,
        "the append happened before the index write, and it is durable"
    );
    // The crashed daemon is gone; drop the handle without a clean stop.
    estate.wirkd = None;
    assert_eq!(
        fs::read_to_string(estate.index_path())
            .unwrap()
            .lines()
            .count(),
        1,
        "the index never took the row: the crash landed before the rename"
    );

    // The next start re-projects from the journals.
    let (child, _pointer) = start_wirkd(&estate.estate);
    estate.wirkd = Some(child);
    let admin = estate.admin_index();
    assert_eq!(
        admin["index"]["projection"], "synchronized",
        "startup reconciliation ran and says so: {}",
        admin["index"]
    );
    let after = row_ids(&admin);
    assert_eq!(
        after.len(),
        2,
        "the interrupted row is recovered: {after:?}"
    );
    assert!(
        after.contains(&before_crash[0]),
        "and the row that was already indexed is still there, unchanged"
    );
    assert_no_duplicate_rows(&admin);

    estate.stop();
}

/// The window the crash proof above was actually failing in, made into
/// a test of its own.
///
/// `a_crash_between_the_journal_and_the_index_is_repaired_at_the_next_start`
/// failed intermittently at
/// `wirkd client I/O error: Connection refused (os error 111)` on the
/// admin read immediately after its second `start_wirkd`, and the cause
/// is not in the product: **`wirkd` binds its listener before it writes
/// its pointer**, so a pointer naming a daemon is always a daemon that
/// is already bound. What the daemon does *not* do is remove its pointer
/// or its socket when it dies without a `wirkd stop` — the failpoint
/// crash in that test is exactly such a death — so both files are still
/// in the estate when the next daemon is started, and a readiness wait
/// keyed on "a pointer file exists and parses" was satisfied by the
/// corpse's, before the replacement had bound anything. The request that
/// followed went to a socket file with nothing listening on it.
///
/// This asserts each link of that on its own: the leftovers a crash
/// really leaves, that connecting to them really is refused, and that
/// the harness now hands back **this start's** daemon.
#[test]
fn a_start_after_an_unclean_death_waits_for_the_new_daemon_and_not_the_corpse() {
    let mut estate = build_estate();
    let pointer_path = estate.estate.join(".wirk").join("wirkd.json");
    let dead: wirkd::WirkdPointer =
        serde_json::from_slice(&fs::read(&pointer_path).unwrap()).unwrap();

    // Death without a clean stop: `KillOnDrop` sends `SIGKILL` and reaps,
    // so nothing runs any shutdown path — the same leftovers the
    // `WIRK_ATLAS_FAILPOINT` crash leaves, produced without needing the
    // crash.
    estate.wirkd = None;

    let left: wirkd::WirkdPointer =
        serde_json::from_slice(&fs::read(&pointer_path).unwrap()).unwrap();
    assert_eq!(
        left.pid, dead.pid,
        "the dead daemon's pointer file is still in the estate, naming it"
    );
    assert!(
        left.socket.exists(),
        "and so is its socket file: {}",
        left.socket.display()
    );
    let refused = UnixStream::connect(&left.socket).expect_err(
        "a socket file whose listener is gone is not a daemon, and connecting to it must fail",
    );
    assert_eq!(
        refused.kind(),
        std::io::ErrorKind::ConnectionRefused,
        "and it fails the exact way the flake did: {refused}"
    );

    // The replacement. What the harness returns must be this child's
    // pointer, which — because the pointer is written only after
    // `bind_socket` returns — is proof the listener is bound.
    let (child, pointer) = start_wirkd(&estate.estate);
    let started = child.id();
    estate.wirkd = Some(child);
    assert_ne!(
        pointer.pid, dead.pid,
        "readiness must not be satisfied by the dead daemon's pointer"
    );
    assert_eq!(
        pointer.pid, started,
        "it is this start's daemon that the wait returned on"
    );

    // And the first request after it completes, with no wait, no retry
    // and no second attempt of any kind.
    let admin = estate.admin_index();
    assert_eq!(admin["index"]["projection"], "synchronized");
    estate.stop();
}

/// A daemon that starts while the index cannot be written must never
/// present itself as clean. The reconciliation it runs before accepting
/// its first connection fails, and the very first query says so.
#[test]
fn a_daemon_that_starts_unable_to_project_never_reports_a_clean_index() {
    let mut estate = build_estate();
    let finding = estate.raise("a record the restarted daemon cannot project");

    let (code, _reply, stderr) = estate.assert_on(&finding, "one journaled assertion");
    assert_eq!(code, Some(0), "{stderr}");
    estate.stop();

    // Remove the index entirely, then deny writes: on the next start the
    // journals hold a row the file does not, and the file cannot be
    // written — the state a restored-from-backup or wrongly-owned estate
    // is genuinely in.
    fs::remove_file(estate.index_path()).unwrap();
    estate.set_atlas_writable(false);
    let (child, _pointer) = start_wirkd(&estate.estate);
    estate.wirkd = Some(child);

    let admin = estate.admin_index();
    assert_eq!(
        admin["index"]["complete"], false,
        "an index that could not be projected at startup is not clean: {}",
        admin["index"]
    );
    assert_eq!(admin["index"]["projection"], "behind");
    assert_eq!(
        admin["rows"].as_array().unwrap().len(),
        0,
        "and it really is empty — which is precisely why it must say so"
    );

    estate.set_atlas_writable(true);
    estate.stop();
}

// ---- 4. Concurrency ---------------------------------------------------

/// Simultaneous mutations and queries against the live daemon: every
/// query sees a whole, parseable index and never a torn or duplicated
/// one, every assertion lands exactly once, and the projection is
/// synchronized when the storm is over.
#[test]
fn concurrent_mutation_and_query_never_show_a_torn_or_duplicated_index() {
    let mut estate = build_estate();
    let findings: Vec<String> = (0..6)
        .map(|n| estate.raise(&format!("concurrent record {n}")))
        .collect();

    let estate_root = estate.estate.clone();
    let parent = estate.parent.work_id.clone();
    let child = estate.child.work_id.clone();
    std::thread::scope(|scope| {
        for (n, finding) in findings.iter().enumerate() {
            let estate_root = estate_root.clone();
            let parent = parent.clone();
            scope.spawn(move || {
                let (code, reply, stderr) = finding_cli(
                    &estate_root,
                    &[
                        "assert",
                        "--finding",
                        finding,
                        "--decision",
                        "deferred",
                        "--by",
                        "a reviewer",
                        "--reason",
                        &format!("concurrent assertion {n}"),
                        "--requesting-work",
                        &parent,
                    ],
                );
                assert_eq!(code, Some(0), "{stderr}");
                assert!(reply["index"]["complete"].is_boolean(), "{reply}");
            });
        }
        for _ in 0..4 {
            let estate_root = estate_root.clone();
            let child = child.clone();
            scope.spawn(move || {
                for _ in 0..4 {
                    let (code, value, stderr) = atlas(&estate_root, &["findings", "--admin"]);
                    assert_eq!(
                        code,
                        Some(0),
                        "a concurrent read never sees a torn index: {stderr}"
                    );
                    assert_no_duplicate_rows(&value);
                    let (code, scoped, stderr) =
                        atlas(&estate_root, &["findings", "--requesting-work", &child]);
                    assert_eq!(code, Some(0), "{stderr}");
                    assert!(scoped["index"]["complete"].is_boolean(), "{scoped}");
                }
            });
        }
    });

    let admin = estate.admin_index();
    assert_eq!(
        admin["index"]["projection"], "synchronized",
        "{}",
        admin["index"]
    );
    assert_eq!(
        row_ids(&admin).len(),
        findings.len(),
        "every concurrent assertion landed exactly once: {}",
        admin["rows"]
    );
    assert_no_duplicate_rows(&admin);
    estate.stop();
}

// ---- 5. One broken estate is not every estate -------------------------

/// Projection health is per-estate, held by that estate's own daemon and
/// measured by that daemon's own sweeps. A denial on one estate must not
/// make a second, healthy estate's index report itself behind — nor the
/// reverse.
#[test]
fn one_estates_broken_projection_does_not_touch_another_estate() {
    let mut broken = build_estate();
    let mut healthy = build_estate();

    let broken_finding = broken.raise("a record in the estate that will break");
    let healthy_finding = healthy.raise("a record in the estate that stays well");

    broken.set_atlas_writable(false);
    let (code, reply, stderr) = broken.assert_on(&broken_finding, "into an unwritable index");
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(reply["index"]["complete"], false, "{reply}");

    let (code, reply, stderr) = healthy.assert_on(&healthy_finding, "into a healthy index");
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        reply["index"]["projection"], "synchronized",
        "the healthy estate is unaffected: {reply}"
    );
    let admin = healthy.admin_index();
    assert_eq!(admin["index"]["complete"], true, "{}", admin["index"]);
    assert_eq!(row_ids(&admin).len(), 1);
    assert_eq!(
        broken.admin_index()["index"]["complete"],
        false,
        "and the broken one is still broken"
    );

    broken.set_atlas_writable(true);
    broken.stop();
    healthy.stop();
}

// ---- 6. A canonical journal the walk could not read -------------------
//
// Ruling 0125's case 1, executed. The daemon's own journal walk dropped
// every `read_dir`, `Journal::open` and `replay` error on the floor, so
// an estate whose canonical record could not be read produced a *short*
// row set indistinguishable from a complete one — and `--rebuild`
// replaced the whole index with it, deleted valid rows, and reported
// `synchronized`, `complete: true`, `pending_rows: 0`, exit 0, empty
// stderr. Two real causes with nothing hostile in either: a kernel
// `EACCES` on one Work's journal, and the torn final line a crash
// between `write_all` and `sync_all` leaves, which `Journal` fails
// closed on by design.

/// The whole of it, in the order an operator would meet it: a mutation
/// while one journal is unreadable, the queries after it, the rebuild
/// that used to destroy, a restart with the denial still in force, and
/// the recovery when it is lifted.
#[test]
fn a_canonical_journal_that_cannot_be_read_is_never_a_synchronized_projection() {
    let mut estate = build_estate();
    let parent_finding = estate.raise("a parent record, indexed while the estate is healthy");
    let (code, _reply, stderr) = estate.assert_on(&parent_finding, "the parent's assertion");
    assert_eq!(code, Some(0), "{stderr}");
    let child_finding =
        estate.raise_on_child("a child record, indexed while the estate is healthy");
    let (code, reply, stderr) = estate.assert_as_child(&child_finding, "the child's assertion");
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(reply["index"]["projection"], "synchronized", "{reply}");

    let healthy_rows = row_ids(&estate.admin_index());
    assert_eq!(
        healthy_rows.len(),
        2,
        "two Works, one indexed row each: {healthy_rows:?}"
    );
    let parent_journal_digest = fs::read(estate.journal_path(&estate.parent.work_id)).unwrap();

    // A real kernel denial on one canonical journal. Nothing else in the
    // estate is touched.
    estate.set_journal_readable(&estate.parent.work_id, false);

    // 6.1 — a mutation whose sweep walks past an unreadable journal.
    // The append is additive so nothing is lost, but the walk that
    // produced it was short and the reply must say so rather than
    // report a complete projection it did not observe.
    let (code, reply, stderr) =
        estate.assert_as_child(&child_finding, "asserted while a journal is unreadable");
    assert_eq!(
        code,
        Some(0),
        "the journal append succeeded, so the assertion is accepted: {stderr}"
    );
    assert_eq!(
        reply["index"]["projection"], "behind",
        "a successful append of a partial walk is not a synchronized index: {reply}"
    );
    assert_eq!(reply["index"]["complete"], false);
    assert!(reply["index"]["recovery"].is_string(), "{reply}");
    assert!(
        reply["index"]["pending_rows"].is_null() && reply["index"]["detail"].is_null(),
        "a scoped reply carries no count and no error text: {reply}"
    );
    assert!(
        stderr.contains("subset"),
        "and the operator is told on stderr: {stderr:?}"
    );

    // 6.2 — every dependent query says the same, and administration is
    // told the count is *unknown* rather than zero: the rows the walk
    // never read were never counted.
    let admin = estate.admin_index();
    assert_eq!(admin["index"]["complete"], false, "{}", admin["index"]);
    assert_eq!(admin["index"]["projection"], "behind");
    assert!(
        admin["index"]["pending_rows"].is_null(),
        "how far behind a partial walk left the index is unknown, not zero: {}",
        admin["index"]
    );
    assert!(
        admin["index"]["detail"]
            .as_str()
            .unwrap()
            .contains("could not be read completely"),
        "and administration is told why: {}",
        admin["index"]
    );
    let (scoped, scoped_stderr) = estate.scoped_index();
    assert_eq!(scoped["index"]["complete"], false, "{scoped}");
    assert!(
        scoped["index"]["pending_rows"].is_null() && scoped["index"]["detail"].is_null(),
        "a narrowed reader learns the state, not the estate's paths or counts: {scoped}"
    );
    // The rows a scoped requester receives legitimately name the Works
    // on its own lineage; what it must never receive is the *health*
    // block's administrative half. Scanned on the block itself, and on
    // the stderr the CLI prints from it.
    let mut needles = estate.closed_secrets.clone();
    needles.push(estate.parent.work_id.clone());
    needles.push(estate.estate.display().to_string());
    needles.push("Permission denied".to_string());
    needles.push("os error 13".to_string());
    needles.push("could not be read completely".to_string());
    needles.push("unreadable".to_string());
    assert_discloses_nothing(
        "the scoped index health block over an unreadable canonical journal",
        &scoped["index"],
        &scoped_stderr,
        &needles,
    );

    // 6.3 — `--rebuild` over a partial walk is a refusal, not an
    // overwrite. This is the destructive case: the walk cannot see the
    // parent's journal, and replacing the whole index with what it
    // *could* see deletes a valid, correctly indexed row.
    let before = estate.index_bytes();
    let (code, _reply, stderr) = estate.rebuild();
    assert_eq!(
        code,
        Some(2),
        "an administrator who asked for a rebuild that did not happen is told by exit code: {stderr}"
    );
    assert!(
        stderr.contains("IndexScanIncomplete"),
        "and by name: {stderr:?}"
    );
    assert_eq!(
        estate.index_bytes(),
        before,
        "the index is left exactly as it was rather than replaced from a partial walk"
    );
    let admin = estate.admin_index();
    let survived = row_ids(&admin);
    assert_eq!(
        survived.len(),
        3,
        "the refused rebuild deleted nothing: the two healthy rows and the one the additive sweep added under the denial: {survived:?}"
    );
    for row in &healthy_rows {
        assert!(
            survived.contains(row),
            "the refused rebuild erased {row}: {survived:?}"
        );
    }
    assert_eq!(admin["index"]["complete"], false, "{}", admin["index"]);

    // 6.4 — a restart with the denial still in force. Startup
    // reconciliation is the path the recovery sentence itself
    // recommends, and it used to report a clean index with an empty
    // stderr.
    estate.restart(&[]);
    let admin = estate.admin_index();
    assert_eq!(
        admin["index"]["complete"], false,
        "a daemon that started on a partial walk is not clean: {}",
        admin["index"]
    );
    assert_eq!(admin["index"]["projection"], "behind");
    let after_restart = row_ids(&admin);
    assert_eq!(
        after_restart, survived,
        "and the additive startup sweep destroyed nothing: {}",
        admin["rows"]
    );

    // 6.5 — the canonical record was never at risk, and recovery is the
    // next ordinary mutation.
    estate.set_journal_readable(&estate.parent.work_id, true);
    assert_eq!(
        fs::read(estate.journal_path(&estate.parent.work_id)).unwrap(),
        parent_journal_digest,
        "the denied journal is byte-identical: only its mode was ever changed"
    );
    let (code, reply, stderr) =
        estate.assert_as_child(&child_finding, "asserted after the denial was lifted");
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        reply["index"]["projection"], "synchronized",
        "a complete walk with a successful append is synchronized again: {reply}"
    );
    let admin = estate.admin_index();
    assert_eq!(admin["index"]["complete"], true, "{}", admin["index"]);
    assert_eq!(admin["index"]["pending_rows"], 0);
    let recovered = row_ids(&admin);
    assert_eq!(
        recovered.len(),
        4,
        "every row of both Works, the two written during the denial included: {recovered:?}"
    );
    for row in &healthy_rows {
        assert!(recovered.contains(row), "row {row} was lost: {recovered:?}");
    }
    assert_no_duplicate_rows(&admin);

    // And a rebuild over a complete walk still does its job.
    let (code, _reply, stderr) = estate.rebuild();
    assert_eq!(code, Some(0), "{stderr}");
    let admin = estate.admin_index();
    assert_eq!(admin["index"]["projection"], "synchronized");
    assert_eq!(row_ids(&admin).len(), 4, "{}", admin["rows"]);

    estate.stop();
}

/// The same failure with no permissions involved at all: the torn final
/// line a crash between `write_all` and `sync_all` leaves. `Journal`
/// fails closed on it by design, so it reaches the walk as an ordinary
/// `Err` — and the walk used to treat that exactly as it treats a Work
/// with no findings.
#[test]
fn a_torn_journal_tail_is_a_failed_scan_and_not_an_empty_one() {
    let mut estate = build_estate();
    let parent_finding = estate.raise("a parent record before the crash residue");
    let (code, _reply, stderr) = estate.assert_on(&parent_finding, "indexed cleanly");
    assert_eq!(code, Some(0), "{stderr}");
    let child_finding = estate.raise_on_child("a child record before the crash residue");
    let (code, _reply, stderr) = estate.assert_as_child(&child_finding, "indexed cleanly too");
    assert_eq!(code, Some(0), "{stderr}");
    let healthy_rows = row_ids(&estate.admin_index());
    assert_eq!(healthy_rows.len(), 2);

    // The exact residue a power loss between the write and the fsync
    // leaves behind: a final line that is not a whole envelope.
    let path = estate.journal_path(&estate.parent.work_id);
    let intact = fs::read_to_string(&path).unwrap();
    fs::write(&path, format!("{intact}{{\"seq\":99,\"event\":{{\"kind\":")).unwrap();

    let before = estate.index_bytes();
    let (code, _reply, stderr) = estate.rebuild();
    assert_eq!(
        code,
        Some(2),
        "a rebuild whose walk hit crash residue is refused: {stderr}"
    );
    assert_eq!(
        estate.index_bytes(),
        before,
        "and the rows the residue hid are still in the index"
    );
    let admin = estate.admin_index();
    assert_eq!(row_ids(&admin), healthy_rows, "{}", admin["rows"]);
    assert_eq!(admin["index"]["complete"], false, "{}", admin["index"]);

    // The record's own repair is truncating the partial line, which is
    // what an operator does; the next sweep then reports clean.
    fs::write(&path, &intact).unwrap();
    let (code, reply, stderr) =
        estate.assert_as_child(&child_finding, "after the tail was trimmed");
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(reply["index"]["projection"], "synchronized", "{reply}");
    let (code, _reply, stderr) = estate.rebuild();
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        row_ids(&estate.admin_index()).len(),
        3,
        "and the rebuild now writes every row of both Works"
    );

    estate.stop();
}

/// The positive controls the two above must not swallow. An estate with
/// nothing in it is **complete and empty**, not broken; and the things
/// the estate's own layout says are not Work journals are skipped
/// without making the walk partial. "Empty means broken" would be a
/// worse defect than the one being closed.
#[test]
fn a_legally_empty_estate_and_irrelevant_entries_are_a_complete_observation() {
    // An estate that has never submitted anything: no `works/` at all.
    let dir = tempfile::tempdir().unwrap();
    let empty = dir.path().join("empty-estate");
    fs::create_dir_all(&empty).unwrap();
    let (empty_daemon, _pointer) = start_wirkd(&empty);
    let (code, value, stderr) = atlas(&empty, &["findings", "--admin"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        value["rows"].as_array().unwrap().len(),
        0,
        "an estate with no Work has no rows"
    );
    assert_eq!(
        value["index"]["projection"], "synchronized",
        "and an empty projection of an empty estate really is complete: {}",
        value["index"]
    );
    assert_eq!(value["index"]["complete"], true);
    assert_eq!(value["index"]["pending_rows"], 0);
    let (code, _value, stderr) = atlas(&empty, &["findings", "--admin", "--rebuild"]);
    assert_eq!(
        code,
        Some(0),
        "a rebuild of an empty estate succeeds: {stderr}"
    );
    stop_wirkd(&empty, empty_daemon);

    // A populated estate with residue under `works/` that is not a Work
    // journal: a stray file, and a directory with no events in it.
    let mut estate = build_estate();
    let finding = estate.raise("the one real record");
    let (code, _reply, stderr) = estate.assert_on(&finding, "indexed cleanly");
    assert_eq!(code, Some(0), "{stderr}");
    let rows_before = row_ids(&estate.admin_index());

    fs::write(estate.estate.join("works").join("README"), "not a Work\n").unwrap();
    fs::create_dir_all(estate.estate.join("works").join("work-with-no-events")).unwrap();

    let (code, reply, stderr) = estate.assert_on(&finding, "after the residue appeared");
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        reply["index"]["projection"], "synchronized",
        "neither a stray file nor an empty Work directory makes the walk partial: {reply}"
    );
    let (code, _reply, stderr) = estate.rebuild();
    assert_eq!(
        code,
        Some(0),
        "and a rebuild is not refused either: {stderr}"
    );
    let admin = estate.admin_index();
    assert_eq!(admin["index"]["complete"], true, "{}", admin["index"]);
    let after = row_ids(&admin);
    for row in &rows_before {
        assert!(after.contains(row), "the rebuild dropped {row}: {after:?}");
    }
    estate.stop();
}

// ---- 7. One index observation, one reply -----------------------------
//
// Ruling 0125's case 2, executed. Three windows, each between two real
// operations the daemon already performs, each parked with the estate's
// own verifier gate (`AtlasStore::checkpoint`, whose crash half test 3
// above already uses). The gate never fabricates a reply, a row, an
// outcome or an error: it holds exactly one real thread at a real
// instant so the other can be driven past it.
//
// Every park below is a socket rendezvous with this controller and every
// release is this controller's end of it going away. Nothing in the
// product times anything (ruling 0044 D134, final); no sleep decides any
// assertion; and the fourth test in this section is the gate's own
// negative control.

/// One armed window of the daemon's own verifier gate, driven from this
/// side by a real socket rendezvous.
///
/// Ruling 0044 D134, final: nothing in the product is decided or paced by
/// time, so the gate holds a real thread on a blocking read of a socket
/// this controller owns and has no notion of how long it has waited.
/// `accept` returning here *is* the parked thread's arrival — the gate
/// connects only after it has taken the arm — and dropping the accepted
/// connection is the release, the peer's disappearance observed by the
/// parked read.
///
/// The only durations anywhere in this arrangement are this controller's
/// own termination bounds, and each one's exhaustion is a test failure
/// reporting a state that was never observed. Nothing on either side
/// resumes on a clock, and nothing on either side reports a schedule it
/// did not have.
struct Window {
    dir: PathBuf,
    listener: UnixListener,
    parked: Option<UnixStream>,
    /// Threads parked at this same window earlier and deliberately kept
    /// held while a later one is parked behind them, so a test can
    /// arrange an order the arm file alone cannot: the barrier admits
    /// exactly one parked caller per arm, so two real walks held at once
    /// means arming twice and moving the first one aside. Released
    /// last-in-first-out by `release_aside`, and dropped — which is a
    /// release — if a test panics before it gets there.
    aside: Vec<UnixStream>,
}

impl Window {
    fn new(root: &Path, window: &str) -> Window {
        let dir = root.join(format!("barrier-{window}"));
        fs::create_dir_all(&dir).unwrap();
        // Bound before the daemon is ever armed, so a gate that reaches
        // this window always finds the socket it is told to park on.
        let listener = UnixListener::bind(dir.join(wirk_atlas::BARRIER_RELEASE_SOCKET)).unwrap();
        listener.set_nonblocking(true).unwrap();
        Window {
            dir,
            listener,
            parked: None,
            aside: Vec::new(),
        }
    }

    /// The daemon's environment. Set for the daemon's whole life and
    /// inert until `arm` — the gate takes the arm file by an atomic
    /// rename, so with no arm file every thread runs straight through.
    fn env(&self, window: &str) -> String {
        format!("{window}={}", self.dir.display())
    }

    fn arm(&self) {
        fs::write(self.dir.join("arm"), b"").unwrap();
    }

    /// Whether a real thread has parked here yet. Never used to decide
    /// anything but a test assertion.
    fn parked_now(&mut self) -> bool {
        if self.parked.is_some() {
            return true;
        }
        match self.listener.accept() {
            Ok((stream, _)) => {
                self.parked = Some(stream);
                true
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => false,
            Err(error) => panic!("accept on {} failed: {error}", self.dir.display()),
        }
    }

    /// Blocks until a real thread is provably parked at the window. The
    /// bound is this controller's, and its exhaustion is "no thread ever
    /// reached the window" — a failure, never a decision, and nothing
    /// downstream of it runs.
    fn wait_parked(&mut self) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !self.parked_now() {
            assert!(
                std::time::Instant::now() < deadline,
                "no thread was ever observed to reach the window at {}",
                self.dir.display()
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// The release, and the only one there is: this end of the
    /// connection goes away and the parked read sees its peer gone.
    fn release(&mut self) {
        assert!(
            self.parked.take().is_some(),
            "released a window no thread was ever observed to park at: {}",
            self.dir.display()
        );
    }

    /// Moves the currently parked thread aside — still parked, still
    /// holding its real operation — so the window can be armed again and
    /// a second real thread parked at the same instant. Nothing is
    /// released here.
    fn park_aside(&mut self) {
        let held = self.parked.take().unwrap_or_else(|| {
            panic!(
                "moved aside a window no thread was ever observed to park at: {}",
                self.dir.display()
            )
        });
        self.aside.push(held);
    }

    /// Releases the most recently set-aside thread, the same way
    /// `release` does: this end of the connection goes away.
    fn release_aside(&mut self) {
        assert!(
            self.aside.pop().is_some(),
            "released a set-aside thread at a window that has none: {}",
            self.dir.display()
        );
    }
}

// `Window`'s own drop releases whatever is parked on it and closes the
// listener, so a failing assertion never leaves a real daemon thread
// held: a panic between `wait_parked` and `release` frees the daemon on
// the way out. Both fields do that by themselves; there is no `Drop`
// impl to get wrong.

/// **2A.** A reply's `rows` and its `index` block must describe the same
/// index observation. They were read under two separate locks with the
/// whole scoped-disclosure pass — a journal replay, a lineage walk and a
/// per-row scoping loop — in between, so a repair that landed inside
/// that window produced a reply carrying a one-row subset and telling
/// its requester, in the same object, that the projection was complete.
/// One of the rows it omitted was the requester's own Work's.
#[test]
fn a_replys_rows_and_its_health_are_one_index_observation() {
    let mut estate = build_estate();
    let mut window = Window::new(estate.dir.path(), "read");
    estate.restart(&[("WIRK_ATLAS_BARRIER", &window.env("findings-index-read"))]);

    // A genuinely behind projection: a durable, journaled row the index
    // does not hold, made by a real denied write and then left behind
    // when the denial is lifted without a sweep.
    let finding = estate.raise("a record the index will be missing");
    let (code, _reply, stderr) = estate.assert_on(&finding, "indexed while healthy");
    assert_eq!(code, Some(0), "{stderr}");
    estate.set_atlas_writable(false);
    let (code, reply, stderr) = estate.assert_on(&finding, "durable but unprojected");
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(reply["index"]["complete"], false, "{reply}");
    estate.set_atlas_writable(true);
    let captured_rows = estate.index_rows_on_disk();
    assert_eq!(captured_rows, 1, "the index really is a subset right now");

    window.arm();
    let estate_root = estate.estate.clone();
    let child = estate.child.work_id.clone();
    let query =
        std::thread::spawn(move || atlas(&estate_root, &["findings", "--requesting-work", &child]));
    window.wait_parked();

    // A real successful repair lands while that query is parked.
    let (code, repair, stderr) = estate.assert_on(&finding, "the repair that lands mid-reply");
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(repair["index"]["projection"], "synchronized", "{repair}");
    assert_eq!(
        estate.index_rows_on_disk(),
        3,
        "the file the parked reply is *not* reading now holds every row"
    );

    window.release();
    let (code, reply, stderr) = query.join().unwrap();
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        reply["rows"].as_array().unwrap().len(),
        captured_rows,
        "the reply carries the rows it captured: {reply}"
    );
    assert_eq!(
        reply["index"]["complete"], false,
        "and its health describes those rows, not the file at release: {}",
        reply["index"]
    );
    assert_eq!(reply["index"]["projection"], "behind");
    assert!(
        reply["index"]["observed"]
            .as_str()
            .unwrap()
            .contains("read together"),
        "and says so: {}",
        reply["index"]
    );

    // Positive control: the identical schedule with nothing landing in
    // the window returns the same pairing, so what is being measured is
    // the interleaved repair and not the barrier.
    let mut window = Window::new(estate.dir.path(), "read-control");
    estate.restart(&[("WIRK_ATLAS_BARRIER", &window.env("findings-index-read"))]);
    let rows_now = estate.index_rows_on_disk();
    window.arm();
    let estate_root = estate.estate.clone();
    let child = estate.child.work_id.clone();
    let query =
        std::thread::spawn(move || atlas(&estate_root, &["findings", "--requesting-work", &child]));
    window.wait_parked();
    window.release();
    let (code, reply, stderr) = query.join().unwrap();
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(reply["rows"].as_array().unwrap().len(), rows_now);
    assert_eq!(
        reply["index"]["complete"], true,
        "a paused query over a healthy, unchanging index is complete: {}",
        reply["index"]
    );

    estate.stop();
}

/// **The gate itself, negatively.** The three windows above are only
/// worth anything if the gate is inert unless *this exact* window is
/// armed, and if a window that is never released never releases itself.
/// The shape this closes did both wrong on the second count: it polled a
/// release file every 5 ms and, after 120 s, resumed the real operation
/// and let its caller report a schedule it had never been held for — so
/// a window whose release never came produced a *passing* test instead
/// of a failing one, and a hardcoded duration inside the product decided
/// when a real operation continued (ruling 0044 D134, final: nothing in
/// the product is decided or paced by time; a termination bound may
/// exist only in a test controller and only as "state X was never
/// observed").
///
/// Three controls, and each one fails closed: an armed name no window
/// answers to, a real window with no arm, and a real window that is
/// reached and then held. The last one's bound is measured against this
/// same estate's own unparked round trip and its exhaustion is reported
/// as "never observed to finish".
#[test]
fn an_armed_window_is_inert_elsewhere_and_never_releases_itself() {
    let mut estate = build_estate();
    let finding = estate.raise("a record the verifier gate must not disturb");
    let (code, _reply, stderr) = estate.assert_on(&finding, "indexed before any window exists");
    assert_eq!(code, Some(0), "{stderr}");

    // 1. A window name nothing in the daemon reaches, armed. Every real
    //    operation runs straight through it and the arm is never taken.
    let mut absent = Window::new(estate.dir.path(), "absent");
    estate.restart(&[("WIRK_ATLAS_BARRIER", &absent.env("no-such-window"))]);
    absent.arm();
    let (code, reply, stderr) = estate.assert_on(&finding, "past a window that does not exist");
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        reply["index"]["projection"], "synchronized",
        "a mutation is untouched by an armed name no window answers to: {reply}"
    );
    let admin = estate.admin_index();
    assert_eq!(admin["index"]["complete"], true, "{}", admin["index"]);
    assert!(
        !absent.parked_now(),
        "no thread parked at a window the daemon has no such name for"
    );
    assert!(
        absent.dir.join("arm").exists() && !absent.dir.join("arrived").exists(),
        "and the arm was never taken"
    );

    // 2. A window the daemon really does reach, named, but never armed.
    //    The gate claims nothing, so nothing parks.
    let mut unarmed = Window::new(estate.dir.path(), "unarmed");
    estate.restart(&[("WIRK_ATLAS_BARRIER", &unarmed.env("findings-index-read"))]);
    let admin = estate.admin_index();
    assert_eq!(
        admin["index"]["complete"], true,
        "an unarmed window decides nothing: {}",
        admin["index"]
    );
    assert!(
        !unarmed.parked_now(),
        "no thread parked at a window that was never armed"
    );

    // 3. Armed, reached, and never released. The unparked round trip
    //    against this same daemon calibrates the bound, so the bound is
    //    a measured multiple of this estate's own cost and not a number
    //    chosen by hand.
    let mut held = Window::new(estate.dir.path(), "held");
    estate.restart(&[("WIRK_ATLAS_BARRIER", &held.env("findings-index-read"))]);
    let started = std::time::Instant::now();
    let admin = estate.admin_index();
    let unparked = started.elapsed();
    assert_eq!(admin["index"]["complete"], true, "{}", admin["index"]);
    let rows_before = row_ids(&admin);

    held.arm();
    let estate_root = estate.estate.clone();
    let query = std::thread::spawn(move || atlas(&estate_root, &["findings", "--admin"]));
    held.wait_parked();

    let grace = std::cmp::max(std::time::Duration::from_secs(2), unparked * 20);
    std::thread::sleep(grace);
    assert!(
        !query.is_finished(),
        "a parked query finished with no release, so something other than this controller \
         decided when a real operation continued (it took {unparked:?} unparked, and this \
         waited {grace:?} without ever observing it finish)"
    );

    // And the release — this controller's end of the connection going
    // away, the only exit there is — really is what ends it.
    held.release();
    let (code, reply, stderr) = query.join().unwrap();
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        reply["index"]["complete"], true,
        "and the released query answers normally: {}",
        reply["index"]
    );
    assert_eq!(row_ids(&reply), rows_before);

    estate.stop();
}

/// **2C.** An index publication and the health record that describes it
/// are one operation. Held apart, an older attempt that appended first
/// and recorded last overwrote a newer attempt's genuine failure with
/// its own stale success, and the daemon then told every later caller
/// that a demonstrably incomplete index was complete.
#[test]
fn an_older_reconciliation_cannot_overwrite_a_newer_failure() {
    let mut estate = build_estate();
    let mut window = Window::new(estate.dir.path(), "published");
    estate.restart(&[(
        "WIRK_ATLAS_BARRIER",
        &window.env("findings-index-published"),
    )]);

    let first = estate.raise("the record the older attempt indexes");
    let second = estate.raise("the record the newer attempt cannot index");

    // The positive control, run first and against this same daemon: an
    // ordinary denied assertion, unparked, from the same estate. It
    // reports `behind` correctly on its own, and how long it takes
    // calibrates the grace below — so the grace is a measured multiple
    // of this estate's own round trip and not a number picked by hand.
    let control = estate.raise("the record the control attempt cannot index");
    estate.set_atlas_writable(false);
    let started = std::time::Instant::now();
    let (code, reply, stderr) = estate.assert_on(&control, "the unparked control");
    let unparked = started.elapsed();
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        reply["index"]["projection"], "behind",
        "a denied sweep reports behind on its own: {reply}"
    );
    estate.set_atlas_writable(true);
    let (code, _reply, stderr) = estate.assert_on(&control, "the control's own repair");
    assert_eq!(code, Some(0), "{stderr}");
    let grace = std::cmp::max(std::time::Duration::from_secs(2), unparked * 20);

    window.arm();
    let estate_root = estate.estate.clone();
    let parent = estate.parent.work_id.clone();
    let older = first.clone();
    let older_call = std::thread::spawn(move || {
        finding_cli(
            &estate_root,
            &[
                "assert",
                "--finding",
                &older,
                "--decision",
                "deferred",
                "--by",
                "a reviewer",
                "--reason",
                "the older attempt, parked after its own successful append",
                "--requesting-work",
                &parent,
            ],
        )
    });
    window.wait_parked();

    // With the older attempt parked between its publication and its
    // record, a newer one really fails.
    estate.set_atlas_writable(false);
    let journal_before = estate.journal_lines(&estate.parent.work_id);
    let estate_root = estate.estate.clone();
    let parent = estate.parent.work_id.clone();
    let newer = second.clone();
    let newer_call = std::thread::spawn(move || {
        finding_cli(
            &estate_root,
            &[
                "assert",
                "--finding",
                &newer,
                "--decision",
                "deferred",
                "--by",
                "a reviewer",
                "--reason",
                "the newer attempt, whose index write is denied",
                "--requesting-work",
                &parent,
            ],
        )
    });

    // The decisive scheduling fact, and it is a fact about exclusion
    // rather than about elapsed time: with the older attempt parked
    // between its own publication and its own health record, no other
    // attempt can get past that pair. The newer one is genuinely in
    // flight — its event is already durable in the parent's journal —
    // and it still cannot finish. On the shape this closes it finished
    // here, in about `unparked`, and its correct `behind` was then
    // overwritten by the older success.
    estate.wait_for_journal_lines(&estate.parent.work_id, journal_before + 1);
    std::thread::sleep(grace);
    assert!(
        !newer_call.is_finished(),
        "the newer attempt completed while an older publication was parked mid-record, \
         so the two are not one operation (it took {unparked:?} unparked, and this waited {grace:?})"
    );

    window.release();
    let (code, _older_reply, stderr) = older_call.join().unwrap();
    assert_eq!(code, Some(0), "{stderr}");
    let (code, newer_reply, stderr) = newer_call.join().unwrap();
    assert_eq!(code, Some(0), "the newer assertion is journaled: {stderr}");
    assert_eq!(
        newer_reply["index"]["projection"], "behind",
        "and its own reply is correct: {newer_reply}"
    );

    // The decisive check: the daemon's standing health is the newer
    // failure, not the older success that finished recording after it.
    let admin = estate.admin_index();
    assert_eq!(
        admin["index"]["complete"], false,
        "an older success must not clear a newer failure: {}",
        admin["index"]
    );
    assert_eq!(admin["index"]["projection"], "behind");
    assert!(
        !indexes_finding(&admin, &second),
        "and the index really is missing the newer attempt's row, which is why it must say so: {}",
        admin["rows"]
    );

    // The record is untouched and the next healthy sweep repairs both.
    estate.set_atlas_writable(true);
    let (code, reply, stderr) = estate.assert_on(&second, "after the denial was lifted");
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(reply["index"]["projection"], "synchronized", "{reply}");
    let admin = estate.admin_index();
    assert_eq!(admin["index"]["complete"], true, "{}", admin["index"]);
    assert_no_duplicate_rows(&admin);

    estate.stop();
}

/// **2B.** An administrative whole-file replacement cannot discard a row
/// that was durably journaled *and* successfully indexed while it was
/// walking. The walk ran before the lock, so a real assertion could land
/// and be acknowledged `synchronized` inside the window, and the rebuild
/// then overwrote the index with its own pre-lock snapshot and recorded
/// `Synchronized` over the deletion.
#[test]
fn a_rebuild_cannot_delete_a_row_indexed_while_it_was_walking() {
    let mut estate = build_estate();
    let mut window = Window::new(estate.dir.path(), "walked");
    estate.restart(&[("WIRK_ATLAS_BARRIER", &window.env("findings-index-walked"))]);

    let settled = estate.raise("a record indexed before the rebuild walks");
    // Timed only to calibrate the exclusion bound below against this
    // same estate's own cost; nothing here is decided by it.
    let started = std::time::Instant::now();
    let (code, _reply, stderr) = estate.assert_on(&settled, "indexed before the walk");
    let unparked = started.elapsed();
    assert_eq!(code, Some(0), "{stderr}");
    let racing = estate.raise("a record that lands while the rebuild is walking");
    let before = row_ids(&estate.admin_index());
    assert_eq!(before.len(), 1);

    let journal_before = estate.journal_lines(&estate.parent.work_id);
    window.arm();
    let estate_root = estate.estate.clone();
    let rebuild =
        std::thread::spawn(move || atlas(&estate_root, &["findings", "--admin", "--rebuild"]));
    window.wait_parked();

    // A real assertion inside the rebuild's window, released by a
    // durable fact of its own — its event reaching the parent's journal,
    // which every path it can take does before it touches the index at
    // all — and never by a sleep. On the shape this closes it went
    // further than that unaided: it indexed its row and was acknowledged
    // `synchronized` inside the window, and the rebuild deleted it.
    let estate_root = estate.estate.clone();
    let parent = estate.parent.work_id.clone();
    let landing = racing.clone();
    let assertion = std::thread::spawn(move || {
        finding_cli(
            &estate_root,
            &[
                "assert",
                "--finding",
                &landing,
                "--decision",
                "deferred",
                "--by",
                "a reviewer",
                "--reason",
                "landed inside the rebuild's window",
                "--requesting-work",
                &parent,
            ],
        )
    });
    estate.wait_for_journal_lines(&estate.parent.work_id, journal_before + 1);

    // The decisive scheduling fact, and it is about exclusion rather
    // than about elapsed time: the racing assertion is genuinely in
    // flight — its event is already durable in the parent's journal —
    // and while the rebuild is parked between its walk and its
    // replacement it cannot reach the index at all, because the walk and
    // the replacement are inside one hold of the Atlas lock. On the
    // shape this closes it got all the way through here, was told
    // `synchronized`, and was then erased by the released rebuild's
    // pre-lock snapshot. Without this the erasure is left to the
    // scheduler; with it, it is a property.
    let grace = std::cmp::max(std::time::Duration::from_secs(2), unparked * 20);
    std::thread::sleep(grace);
    assert!(
        !assertion.is_finished(),
        "a racing assertion reached the index while a rebuild was parked between its walk \
         and its replacement, so the two are not one operation (it took {unparked:?} \
         unparked, and this waited {grace:?} without ever observing it finish)"
    );

    window.release();
    let (code, reply, stderr) = assertion.join().unwrap();
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        reply["index"]["projection"], "synchronized",
        "the assertion was told its row reached the index: {reply}"
    );
    let (code, _reply, stderr) = rebuild.join().unwrap();
    assert_eq!(code, Some(0), "{stderr}");

    let admin = estate.admin_index();
    let after = row_ids(&admin);
    assert_eq!(
        after.len(),
        2,
        "a row acknowledged as indexed is not deleted by a rebuild that never saw it: {after:?}"
    );
    for row in &before {
        assert!(after.contains(row), "{row} was erased: {after:?}");
    }
    assert_eq!(admin["index"]["complete"], true, "{}", admin["index"]);
    assert_no_duplicate_rows(&admin);

    // Positive control: the same parked rebuild with nothing landing in
    // its window leaves exactly the rows it walked.
    let mut window = Window::new(estate.dir.path(), "walked-control");
    estate.restart(&[("WIRK_ATLAS_BARRIER", &window.env("findings-index-walked"))]);
    let before = row_ids(&estate.admin_index());
    window.arm();
    let estate_root = estate.estate.clone();
    let rebuild =
        std::thread::spawn(move || atlas(&estate_root, &["findings", "--admin", "--rebuild"]));
    window.wait_parked();
    window.release();
    let (code, _reply, stderr) = rebuild.join().unwrap();
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(row_ids(&estate.admin_index()), before);

    estate.stop();
}

// ---- 8. One observation order for the whole lifecycle ----------------
//
// Ruling 0125 closed "an older outcome cannot overwrite a newer
// publication" at the *publication* boundary: `{append, record}` is one
// critical section, so an attempt that appended first and recorded last
// can no longer land its record after a newer attempt's. The
// index-health-reverify review then executed the same class one step
// earlier, at the **scan** boundary, and it survived:
//
//   1. `A` is asserted and indexed. The index is synchronized.
//   2. An older sweep completes its whole walk and is parked before it
//      takes the Atlas lock.
//   3. A healthy sweep runs straight through and indexes everything the
//      parked walk was carrying, so the parked walk now has nothing left
//      to offer.
//   4. The Atlas directory is denied. A newer sweep journals `D`
//      durably, fails its index write on a real `EACCES`, and records
//      the failure. The standing health says `behind`.
//   5. The older walk resumes with the denial still in force, offers
//      rows that all already exist, gets `Ok(0)` from
//      `append_finding_rows` — **writing nothing at all** — and records
//      `Synchronized` over the newer, known, unrepaired failure.
//
// `D` is still durably journaled and still absent from the index file,
// which is byte-for-byte what it was when the failure was known, and the
// daemon now answers `complete: true` to every later caller. The
// existing `an_older_reconciliation_cannot_overwrite_a_newer_failure`
// orders publication against record and cannot see this, because here
// there is no publication at all.
//
// The repair is ruling 0125's own rule read from the **walk** instead of
// from the append: every reconciliation takes an observation number
// before its walk (the rebuild takes its own inside the Atlas hold it
// already has, where its walk lives), and a record whose observation is
// older than the standing one is discarded — except that an older
// observation may still make the standing projection *less* complete,
// never more. Nothing new is locked, no walk moves under a lock, and an
// observation number is an order rather than a duration or a budget
// (ruling 0044 D134).

/// The reported schedule, executed: an older complete walk with nothing
/// left to offer must not clear a newer failure the daemon has already
/// recorded and not repaired — and the older caller's own reply must not
/// assert the outcome that was discarded.
#[test]
fn an_older_complete_scan_cannot_clear_a_newer_known_failure() {
    let mut estate = build_estate();
    let mut window = Window::new(estate.dir.path(), "scanned");
    estate.restart(&[("WIRK_ATLAS_BARRIER", &window.env("findings-index-scanned"))]);

    // 1. A healthy, synchronized starting point.
    let a = estate.raise("the record the estate starts synchronized on");
    let (code, reply, stderr) = estate.assert_on(&a, "asserted while healthy");
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(reply["index"]["projection"], "synchronized", "{reply}");

    let b = estate.raise("the record the older walk is carrying");
    let c = estate.raise("the record a healthy sweep indexes inside the window");
    let d = estate.raise("the record the newer sweep cannot index");

    // 2. The older sweep: parked after its complete walk, before the
    // Atlas lock. Arrival is this controller's `accept` returning, a
    // real handshake and not a wait.
    window.arm();
    let estate_root = estate.estate.clone();
    let parent = estate.parent.work_id.clone();
    let older = b.clone();
    let older_call = std::thread::spawn(move || {
        finding_cli(
            &estate_root,
            &[
                "assert",
                "--finding",
                &older,
                "--decision",
                "deferred",
                "--by",
                "a reviewer",
                "--reason",
                "the older sweep, parked after its own complete walk",
                "--requesting-work",
                &parent,
            ],
        )
    });
    window.wait_parked();

    // 3. A healthy sweep runs straight through the claimed window — the
    // arm is taken by exactly one caller — and indexes everything,
    // including the row the parked walk is carrying. The parked walk now
    // has nothing left to offer, which is what makes its append a
    // no-op and its `Ok(0)` a success it never earned.
    let (code, reply, stderr) = estate.assert_on(&c, "the healthy sweep inside the window");
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(reply["index"]["projection"], "synchronized", "{reply}");
    let admin = estate.admin_index();
    assert!(indexes_finding(&admin, &b), "{}", admin["rows"]);
    assert!(indexes_finding(&admin, &c), "{}", admin["rows"]);

    // 4. A real kernel denial, and a newer sweep that really fails.
    estate.set_atlas_writable(false);
    let (code, newer_reply, stderr) = estate.assert_on(&d, "the newer sweep, denied");
    assert_eq!(code, Some(0), "the assertion is journaled: {stderr}");
    assert_eq!(
        newer_reply["index"]["projection"], "behind",
        "the newer sweep reports its own failure: {newer_reply}"
    );
    let failed_health = estate.admin_index();
    assert_eq!(failed_health["index"]["complete"], false);
    assert!(
        !indexes_finding(&failed_health, &d),
        "the newer row really is missing: {}",
        failed_health["rows"]
    );
    let index_when_the_failure_was_known = estate.index_bytes();

    // 5. The older walk resumes, with the denial still in force and
    // nothing repaired.
    window.release();
    let (code, older_reply, stderr) = older_call.join().unwrap();
    assert_eq!(code, Some(0), "the older assertion is journaled: {stderr}");

    // The decisive check. The index file has not changed a byte — the
    // older sweep's append wrote nothing — so any reply calling the
    // projection complete is describing an index that is demonstrably
    // missing a durably journaled row.
    assert_eq!(
        estate.index_bytes(),
        index_when_the_failure_was_known,
        "the older sweep wrote nothing, so nothing it could say repaired anything"
    );
    let admin = estate.admin_index();
    assert_eq!(
        admin["index"]["complete"], false,
        "an older walk cleared a newer KNOWN failure without repairing it: {}, while {d} is \
         durably journaled and absent from the index",
        admin["index"]
    );
    assert_eq!(admin["index"]["projection"], "behind");
    assert!(!indexes_finding(&admin, &d), "{}", admin["rows"]);
    // And the older caller does not assert the outcome that was
    // discarded: its reply renders the standing observation, which is
    // the newer failure.
    assert_eq!(
        older_reply["index"]["projection"], "behind",
        "the older caller asserted an outcome that was discarded: {older_reply}"
    );
    assert_eq!(older_reply["index"]["complete"], false);

    // The journal is the record throughout, and one ordinary mutation
    // after the denial is lifted still recovers everything — the
    // ordering never wedges a `behind` shut.
    let (code, listed, stderr) = finding_cli(
        &estate.estate,
        &["list", "--requesting-work", &estate.parent.work_id],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let listed = serde_json::to_string(&listed).unwrap();
    assert!(listed.contains(&d), "the journal still holds the record");

    estate.set_atlas_writable(true);
    let (code, reply, stderr) = estate.assert_on(&d, "the ordinary repair");
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        reply["index"]["projection"], "synchronized",
        "a later observation still recovers: {reply}"
    );
    let admin = estate.admin_index();
    assert_eq!(admin["index"]["complete"], true, "{}", admin["index"]);
    assert!(indexes_finding(&admin, &d), "{}", admin["rows"]);
    assert_no_duplicate_rows(&admin);

    estate.stop();
}

/// Positive control 1: the identical schedule with **nothing failing**
/// inside the window. The older sweep resumes, finds nothing missing and
/// reports synchronized — and that is correct, so the ordering must not
/// turn it into a failure. What is being measured above is the
/// interleaving, not the barrier.
#[test]
fn an_older_scan_with_nothing_failing_in_its_window_still_reports_synchronized() {
    let mut estate = build_estate();
    let mut window = Window::new(estate.dir.path(), "scanned-control");
    estate.restart(&[("WIRK_ATLAS_BARRIER", &window.env("findings-index-scanned"))]);

    let a = estate.raise("the record the estate starts synchronized on");
    let (code, _reply, stderr) = estate.assert_on(&a, "asserted while healthy");
    assert_eq!(code, Some(0), "{stderr}");
    let b = estate.raise("the record the older walk is carrying");
    let c = estate.raise("the record a healthy sweep indexes inside the window");

    window.arm();
    let estate_root = estate.estate.clone();
    let parent = estate.parent.work_id.clone();
    let older = b.clone();
    let older_call = std::thread::spawn(move || {
        finding_cli(
            &estate_root,
            &[
                "assert",
                "--finding",
                &older,
                "--decision",
                "deferred",
                "--by",
                "a reviewer",
                "--reason",
                "the older sweep, parked with nothing failing behind it",
                "--requesting-work",
                &parent,
            ],
        )
    });
    window.wait_parked();

    let (code, _reply, stderr) = estate.assert_on(&c, "the healthy sweep inside the window");
    assert_eq!(code, Some(0), "{stderr}");

    window.release();
    let (code, older_reply, stderr) = older_call.join().unwrap();
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        older_reply["index"]["projection"], "synchronized",
        "an older walk with nothing failing behind it is still healthy: {older_reply}"
    );
    let admin = estate.admin_index();
    assert_eq!(admin["index"]["complete"], true, "{}", admin["index"]);
    assert!(indexes_finding(&admin, &b), "{}", admin["rows"]);
    assert!(indexes_finding(&admin, &c), "{}", admin["rows"]);
    assert_no_duplicate_rows(&admin);

    estate.stop();
}

/// Positive control 2: the identical denied sweep with **no older walk
/// parked** at all. `behind` stands, and stands across later reads — so
/// what the first test observes is the parked walk and not the denial.
#[test]
fn a_newer_failure_with_no_older_scan_parked_stands() {
    let mut estate = build_estate();
    let a = estate.raise("the record the estate starts synchronized on");
    let (code, _reply, stderr) = estate.assert_on(&a, "asserted while healthy");
    assert_eq!(code, Some(0), "{stderr}");
    let d = estate.raise("the record the newer sweep cannot index");

    estate.set_atlas_writable(false);
    let (code, reply, stderr) = estate.assert_on(&d, "the newer sweep, denied");
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(reply["index"]["projection"], "behind", "{reply}");
    for _ in 0..3 {
        let admin = estate.admin_index();
        assert_eq!(admin["index"]["complete"], false, "{}", admin["index"]);
    }
    estate.set_atlas_writable(true);
    estate.stop();
}

/// Rebuild order, the other half of the lifecycle: `--rebuild` walks
/// **inside** the Atlas hold, so its observation is taken there. An
/// ordinary sweep whose walk ran before that hold cannot clear the
/// rebuild's recorded failure either.
#[test]
fn an_older_scan_cannot_clear_a_rebuilds_newer_failure() {
    let mut estate = build_estate();
    let mut window = Window::new(estate.dir.path(), "scanned-rebuild");
    estate.restart(&[("WIRK_ATLAS_BARRIER", &window.env("findings-index-scanned"))]);

    let a = estate.raise("the record the estate starts synchronized on");
    let (code, _reply, stderr) = estate.assert_on(&a, "asserted while healthy");
    assert_eq!(code, Some(0), "{stderr}");
    let b = estate.raise("the record the older walk is carrying");
    let c = estate.raise("the record a healthy sweep indexes inside the window");

    window.arm();
    let estate_root = estate.estate.clone();
    let parent = estate.parent.work_id.clone();
    let older = b.clone();
    let older_call = std::thread::spawn(move || {
        finding_cli(
            &estate_root,
            &[
                "assert",
                "--finding",
                &older,
                "--decision",
                "deferred",
                "--by",
                "a reviewer",
                "--reason",
                "the older sweep, parked before an administrative rebuild",
                "--requesting-work",
                &parent,
            ],
        )
    });
    window.wait_parked();

    // Everything the parked walk carries is indexed by a sweep that runs
    // straight through, so the parked walk has nothing left to offer.
    let (code, _reply, stderr) = estate.assert_on(&c, "the healthy sweep inside the window");
    assert_eq!(code, Some(0), "{stderr}");

    // A real administrative rebuild that really fails its write.
    estate.set_atlas_writable(false);
    let (code, _reply, stderr) = estate.rebuild();
    assert_eq!(
        code,
        Some(2),
        "a denied rebuild is a refusal with a reason: {stderr}"
    );
    let admin = estate.admin_index();
    assert_eq!(admin["index"]["complete"], false, "{}", admin["index"]);

    window.release();
    let (code, older_reply, stderr) = older_call.join().unwrap();
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        older_reply["index"]["projection"], "behind",
        "the older caller must not assert an outcome the rebuild's own \
         newer observation replaced: {older_reply}"
    );
    let admin = estate.admin_index();
    assert_eq!(
        admin["index"]["complete"], false,
        "an older sweep cleared a rebuild's newer failure: {}",
        admin["index"]
    );

    // And the rebuild the operator runs next, once the denial is lifted,
    // still clears it: the order is an order, not a latch.
    estate.set_atlas_writable(true);
    let (code, _reply, stderr) = estate.rebuild();
    assert_eq!(code, Some(0), "{stderr}");
    let admin = estate.admin_index();
    assert_eq!(admin["index"]["complete"], true, "{}", admin["index"]);
    assert!(indexes_finding(&admin, &b), "{}", admin["rows"]);
    assert!(indexes_finding(&admin, &c), "{}", admin["rows"]);

    estate.stop();
}

/// The order is an order and not "the worst outcome wins": a newer
/// observation that succeeds clears an older parked sweep's outcome, and
/// a rebuild that runs while a walk is parked is not undone by it.
#[test]
fn a_newer_successful_observation_clears_what_an_older_parked_walk_reports() {
    let mut estate = build_estate();
    let mut window = Window::new(estate.dir.path(), "scanned-recovery");
    estate.restart(&[("WIRK_ATLAS_BARRIER", &window.env("findings-index-scanned"))]);

    let a = estate.raise("the record the estate starts synchronized on");
    let (code, _reply, stderr) = estate.assert_on(&a, "asserted while healthy");
    assert_eq!(code, Some(0), "{stderr}");
    let b = estate.raise("the record the older walk is carrying");

    // The older walk parks while one canonical journal is unreadable, so
    // what it is carrying is a genuinely partial scan.
    estate.set_journal_readable(&estate.child.work_id, false);
    window.arm();
    let estate_root = estate.estate.clone();
    let parent = estate.parent.work_id.clone();
    let older = b.clone();
    let older_call = std::thread::spawn(move || {
        finding_cli(
            &estate_root,
            &[
                "assert",
                "--finding",
                &older,
                "--decision",
                "deferred",
                "--by",
                "a reviewer",
                "--reason",
                "the older sweep, parked on a partial walk",
                "--requesting-work",
                &parent,
            ],
        )
    });
    window.wait_parked();

    // The estate is repaired and a newer, complete sweep runs straight
    // through and records a real success.
    estate.set_journal_readable(&estate.child.work_id, true);
    let c = estate.raise("the record the newer healthy sweep indexes");
    let (code, reply, stderr) = estate.assert_on(&c, "the newer healthy sweep");
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(reply["index"]["projection"], "synchronized", "{reply}");

    // The older walk resumes. Its own scan really was partial, so the
    // conservative direction is open to it — but it must not be able to
    // do anything else, and a later observation must clear it.
    window.release();
    let (code, _older_reply, stderr) = older_call.join().unwrap();
    assert_eq!(code, Some(0), "{stderr}");

    let (code, reply, stderr) = estate.assert_on(&b, "the next ordinary observation");
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        reply["index"]["projection"], "synchronized",
        "the next complete observation clears whatever the older walk left: {reply}"
    );
    let admin = estate.admin_index();
    assert_eq!(admin["index"]["complete"], true, "{}", admin["index"]);
    assert!(indexes_finding(&admin, &b), "{}", admin["rows"]);
    assert!(indexes_finding(&admin, &c), "{}", admin["rows"]);
    assert_no_duplicate_rows(&admin);

    estate.stop();
}

/// **The stale walk's own, genuinely later directory `fsync` failure.**
///
/// `record_index_projection` used to discard an older observation
/// *whole* — its projection and the directory fact it was carrying
/// alike. Those are two facts on two different clocks, and only one of
/// them is the walk's.
///
/// A walk's ticket is taken before the walk and orders the walk. An
/// `fsync` is not a walk: every `fsync` of the atlas directory this
/// daemon makes happens under the Atlas mutex, so the order of the
/// critical sections is the order the syncs really happened in. A walk
/// with an **older** ticket still writes, and still `fsync`s, whenever
/// it holds a row a newer walk never read — `append_finding_rows`
/// returns `Ok(0)` without opening a file only when the index already
/// holds every offered row — and a newer walk is short exactly when a
/// canonical journal has gone unreadable under it. So the older ticket
/// belonged to a failure that happened *later* than the standing
/// record, and dropping it dropped a real `EIO`-class outcome of a real
/// syscall on a real directory.
///
/// Executed here with no fault injected into the product: the atlas
/// directory is left writable and executable but not readable, so the
/// temporary file is written and `fsync`ed, the atomic rename lands and
/// makes the rows visible, and the one call that fails is
/// `rewrite_rows`' closing `File::open` of the directory it just
/// renamed into — which is what `DurabilityUncertain` names.
///
/// The consequence is not a hidden field. With the estate repaired, an
/// administrative call that appends **nothing** — a retirement with no
/// preserved copy to retire, whose sweep finds every row already
/// indexed and so writes not one byte — used to certify the projection
/// `synchronized` / `complete: true` over a directory entry whose
/// `fsync` had returned `EACCES`. That is the D3 outcome from a third
/// side, reachable by ordinary mutations alone.
///
/// Both directions are asserted: the no-op sweep must **not** resolve
/// the window (ruling 0130 — a sweep that writes nothing has learned
/// nothing), and a later write whose directory `fsync` genuinely
/// succeeds **must**.
#[test]
fn a_stale_walks_own_later_directory_failure_is_not_discarded_with_its_walk() {
    let mut estate = build_estate();
    let mut window = Window::new(estate.dir.path(), "stale-directory-failure");
    estate.restart(&[("WIRK_ATLAS_BARRIER", &window.env("findings-index-scanned"))]);

    // One record on each Work. `older` lives in the parent's journal —
    // the journal that goes unreadable — so the complete walk below is
    // the only one that ever sees it.
    let older = estate.raise("the row only the older, complete walk ever reads");
    let newer = estate.raise_on_child("the row the newer, short walk indexes");

    // ---- B: the older ticket, a COMPLETE walk, parked ---------------
    window.arm();
    let estate_root = estate.estate.clone();
    let parent_work = estate.parent.work_id.clone();
    let b_finding = older.clone();
    let b_call = std::thread::spawn(move || {
        finding_cli(
            &estate_root,
            &[
                "assert",
                "--finding",
                &b_finding,
                "--decision",
                "deferred",
                "--by",
                "a reviewer",
                "--reason",
                "the older walk, complete, parked before anything broke",
                "--requesting-work",
                &parent_work,
            ],
        )
    });
    window.wait_parked();
    // Held, still parked, while a second real walk is parked behind it.
    window.park_aside();

    // ---- A: the newer ticket, a SHORT walk, parked -------------------
    //
    // The parent's canonical journal is denied *after* B's walk read it
    // and *before* A's walk runs, so A is genuinely short by exactly the
    // rows B is carrying — which is the only condition under which B's
    // own append has anything to write at all.
    estate.set_journal_readable(&estate.parent.work_id, false);
    window.arm();
    let estate_root = estate.estate.clone();
    let child_work = estate.child.work_id.clone();
    let a_finding = newer.clone();
    let a_call = std::thread::spawn(move || {
        finding_cli(
            &estate_root,
            &[
                "assert",
                "--finding",
                &a_finding,
                "--decision",
                "deferred",
                "--by",
                "a reviewer",
                "--reason",
                "the newer walk, short, parked behind the older one",
                "--requesting-work",
                &child_work,
            ],
        )
    });
    window.wait_parked();

    // ---- A lands first: a newer ticket, a short walk, a real, --------
    // ---- SUCCESSFUL directory fsync ----------------------------------
    window.release();
    let (code, a_reply, stderr) = a_call.join().unwrap();
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        a_reply["index"]["projection"], "behind",
        "the newer walk really was short: {}",
        a_reply["index"]
    );
    let rows_after_a = estate.index_rows_on_disk();

    // ---- B lands second, and its own directory fsync FAILS -----------
    //
    // Armed only now: A's own directory `fsync` had already succeeded,
    // so the failure below belongs to B's critical section and to no
    // other.
    estate.set_atlas_directory_syncable(false);
    window.release_aside();
    let (code, _b_reply, stderr) = b_call.join().unwrap();
    assert_eq!(code, Some(0), "{stderr}");

    // B really did write — the whole scenario depends on it, so it is
    // measured rather than assumed. The rows it appended are the ones
    // the short walk never read.
    estate.set_atlas_directory_syncable(true);
    let rows_after_b = estate.index_rows_on_disk();
    assert!(
        rows_after_b > rows_after_a,
        "the older walk appended nothing, so it never reached the directory fsync at all: \
         {rows_after_a} rows before it, {rows_after_b} after"
    );

    // ---- the estate is repaired, and a sweep that writes NOTHING -----
    // ---- must not certify the entry that failed ----------------------
    estate.set_journal_readable(&estate.parent.work_id, true);
    let (code, retire_reply, stderr) = estate.retire_preserved();
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        retire_reply["retired_index_copies"]
            .as_array()
            .map(Vec::len),
        Some(0),
        "this call must rename nothing, so it syncs no directory of its own: {retire_reply}"
    );
    assert_eq!(
        estate.index_rows_on_disk(),
        rows_after_b,
        "the sweep this call ran must have appended nothing, or its own successful fsync — \
         not the standing record — is what the assertion below would be reading"
    );

    let admin = estate.admin_index();
    assert_eq!(
        admin["index"]["projection"], "durability_unconfirmed",
        "a complete sweep that wrote not one byte certified an entry whose own fsync failed: {}",
        admin["index"]
    );
    assert_eq!(admin["index"]["complete"], false, "{}", admin["index"]);
    let detail = admin["index"]["detail"].as_str().unwrap_or_default();
    assert!(
        detail.contains("directory sync failed"),
        "the window must name the failure that opened it: {detail}"
    );

    // ---- and a genuinely later successful sync DOES resolve it -------
    let resolving = estate.raise("the row whose write really does sync the directory");
    let (code, reply, stderr) = estate.assert_on(&resolving, "a write whose directory fsync lands");
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        reply["index"]["projection"], "synchronized",
        "a real, later, successful directory fsync must clear the window it is the only cure for: {}",
        reply["index"]
    );
    let admin = estate.admin_index();
    assert_eq!(admin["index"]["complete"], true, "{}", admin["index"]);
    assert!(indexes_finding(&admin, &older), "{}", admin["rows"]);
    assert!(indexes_finding(&admin, &newer), "{}", admin["rows"]);
    assert!(indexes_finding(&admin, &resolving), "{}", admin["rows"]);
    assert_no_duplicate_rows(&admin);

    estate.stop();
}

// ---- 9. The scanner reads canonical journals, and only reads them ----
//
// `Journal::open` is the estate's one *write* path: it `create_dir_all`s
// the directory, opens `journal.ndjson` with `create(true).append(true)`,
// and hands back a handle that can append. The findings walk is a pure
// read and used it anyway, with two executed consequences
// (index-health-reverify, "`Journal::open` on the read path"):
//
// 1. A `works/` entry with no journal got a **zero-byte
//    `journal.ndjson` created by the scan itself** — a read path
//    inventing a canonical file.
// 2. A journal that is perfectly readable but not writable (`0444`: a
//    restored backup, an archived tree, a `chmod -R a-w` snapshot) was
//    reported as `journal io error: Permission denied`, so the estate
//    was permanently `behind` and could never be rebuilt.
//
// The repair is the estate's existing replay format opened read-only,
// scoped to this scanner. Nothing else changes: a journal that cannot be
// read is still `unreadable`, a torn tail is still a failed scan, and a
// legally empty estate is still complete.

/// A canonical journal that is readable but not writable is read, and
/// the projection built from it is complete.
#[test]
fn a_readable_but_unwritable_canonical_journal_is_a_complete_scan() {
    let mut estate = build_estate();
    let finding = estate.raise("the record indexed from a read-only journal");
    let (code, _reply, stderr) = estate.assert_on(&finding, "indexed while healthy");
    assert_eq!(code, Some(0), "{stderr}");
    let on_child = estate.raise_on_child("the child's record, on a read-only journal");
    let (code, _reply, stderr) = estate.assert_as_child(&on_child, "indexed while healthy");
    assert_eq!(code, Some(0), "{stderr}");

    let child_journal = estate.journal_path(&estate.child.work_id);
    let before = fs::read(&child_journal).unwrap();
    fs::set_permissions(&child_journal, fs::Permissions::from_mode(0o444)).unwrap();

    // A read-only canonical journal, an ordinary writable derived index.
    let (code, reply, stderr) = estate.assert_on(&finding, "with one journal read-only");
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        reply["index"]["projection"], "synchronized",
        "a journal every reader can read is a journal this walk read: {reply}"
    );
    assert_eq!(reply["index"]["complete"], true);
    let admin = estate.admin_index();
    assert_eq!(admin["index"]["complete"], true, "{}", admin["index"]);
    assert!(indexes_finding(&admin, &on_child), "{}", admin["rows"]);

    let (code, _reply, stderr) = estate.rebuild();
    assert_eq!(
        code,
        Some(0),
        "and a rebuild is not refused by a read-only journal: {stderr}"
    );
    let admin = estate.admin_index();
    assert_eq!(admin["index"]["complete"], true, "{}", admin["index"]);
    assert!(indexes_finding(&admin, &on_child), "{}", admin["rows"]);

    // The walk read it and wrote nothing to it.
    assert_eq!(
        fs::read(&child_journal).unwrap(),
        before,
        "the walk changed a canonical journal"
    );

    // The negative control is unchanged: a journal that genuinely
    // cannot be read is still a failed scan.
    fs::set_permissions(&child_journal, fs::Permissions::from_mode(0o000)).unwrap();
    let (code, reply, stderr) = estate.assert_on(&finding, "with one journal unreadable");
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        reply["index"]["projection"], "behind",
        "an unreadable journal is still a failed scan: {reply}"
    );
    fs::set_permissions(&child_journal, fs::Permissions::from_mode(0o600)).unwrap();
    estate.stop();
}

/// A scan never creates a canonical journal. A `works/` entry with no
/// journal is not a Work — the same rule `journal_for` already applies
/// to every other read — and the file the estate does not have does not
/// appear merely because something looked.
#[test]
fn a_scan_never_creates_a_canonical_journal() {
    let mut estate = build_estate();
    let finding = estate.raise("the one real record");
    let (code, _reply, stderr) = estate.assert_on(&finding, "indexed while healthy");
    assert_eq!(code, Some(0), "{stderr}");

    let bare = estate.estate.join("works").join("work-with-no-journal");
    fs::create_dir_all(&bare).unwrap();

    let (code, reply, stderr) = estate.assert_on(&finding, "after the bare directory appeared");
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        reply["index"]["projection"], "synchronized",
        "a directory that is not a Work does not make the walk partial: {reply}"
    );
    assert!(
        !bare.join("journal.ndjson").exists(),
        "the scan invented a canonical journal at {}",
        bare.display()
    );

    let (code, _reply, stderr) = estate.rebuild();
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        !bare.join("journal.ndjson").exists(),
        "the rebuild's walk invented a canonical journal at {}",
        bare.display()
    );
    let admin = estate.admin_index();
    assert_eq!(admin["index"]["complete"], true, "{}", admin["index"]);
    assert!(indexes_finding(&admin, &finding), "{}", admin["rows"]);

    estate.stop();
}

// ---- 9. Evidence the estate already published is never certified away

/// **The mandatory probe (ruling 0125, and `index-health-order-verify`'s
/// executed rejection).** A real Work with a real published, indexed
/// finding, whose own canonical journal is gone.
///
/// Nothing raises an error anywhere: the `works/` entry is a directory,
/// it simply has no journal in it, and the estate's own layout rule says
/// a directory with no journal is not a Work. That rule is right for the
/// *read* it was written for and wrong as a completeness attestation —
/// the walk then calls itself a complete observation of the estate, and
/// `--rebuild` replaces the whole index from it, deleting a published
/// Work's rows at exit 0 while reporting `synchronized`.
///
/// The filesystem alone genuinely cannot tell a Work that never existed
/// from a Work whose journal is gone. The index can: it already names
/// the origin Work of every row it durably holds, it is already read
/// under the same hold, and the Finding lifecycle has no retraction and
/// no journal rewrite — so a walk that did not reproduce a standing row
/// did not observe the whole estate. That is evidence of absence, never
/// authority to invent what is absent: the refusal leaves the index
/// exactly as it found it and reconstructs nothing.
#[test]
fn a_known_works_missing_journal_is_never_complete_and_never_rebuilt_over() {
    let mut estate = build_estate();
    let parent_finding = estate.raise("a parent record, indexed while the estate is healthy");
    let (code, _reply, stderr) = estate.assert_on(&parent_finding, "the parent's assertion");
    assert_eq!(code, Some(0), "{stderr}");
    let child_finding = estate.raise_on_child("a child record, published and indexed");
    let (code, reply, stderr) = estate.assert_as_child(&child_finding, "the child's assertion");
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(reply["index"]["projection"], "synchronized", "{reply}");
    let healthy_rows = row_ids(&estate.admin_index());
    assert_eq!(healthy_rows.len(), 2, "{healthy_rows:?}");
    assert!(indexes_finding(&estate.admin_index(), &child_finding));

    // Only the child's journal moves, and it moves rather than dies.
    // The Work directory stays exactly where it was.
    let held = estate.hold_journal_aside(&estate.child.work_id);
    let held_bytes = fs::read(&held).unwrap();
    assert!(
        estate
            .estate
            .join("works")
            .join(&estate.child.work_id)
            .is_dir(),
        "the probe removes a journal, not a Work directory"
    );

    // 9.1 — an ordinary sweep is additive, so it loses nothing; what it
    // must not do is call a walk that came back short of published
    // evidence a complete observation of the estate.
    let (code, reply, stderr) =
        estate.assert_on(&parent_finding, "swept while the journal is gone");
    assert_eq!(
        code,
        Some(0),
        "the mutation is journaled and durable, so it is accepted: {stderr}"
    );
    assert_eq!(
        reply["index"]["projection"], "behind",
        "a walk that did not reproduce a published row is not synchronized: {reply}"
    );
    assert_eq!(reply["index"]["complete"], false, "{reply}");
    assert!(reply["index"]["recovery"].is_string(), "{reply}");
    assert!(
        stderr.contains("subset"),
        "and the operator is told on stderr: {stderr:?}"
    );
    let admin = estate.admin_index();
    assert!(
        admin["index"]["pending_rows"].is_null(),
        "how far behind is unknown, not zero: {}",
        admin["index"]
    );
    let detail = admin["index"]["detail"].as_str().unwrap();
    assert!(
        detail.contains("no longer account for rows this index already holds")
            && detail.contains(&estate.child.work_id),
        "and administration is told which Work to repair: {detail:?}"
    );
    assert!(
        row_ids(&admin).contains(&healthy_rows[1]) && row_ids(&admin).contains(&healthy_rows[0]),
        "the additive sweep preserved every existing row: {}",
        admin["rows"]
    );

    // The administrative half of that stays administrative.
    let (scoped, scoped_stderr) = estate.scoped_index_as(&estate.parent.work_id);
    assert_eq!(scoped["index"]["complete"], false, "{scoped}");
    assert!(
        scoped["index"]["pending_rows"].is_null() && scoped["index"]["detail"].is_null(),
        "a narrowed reader learns the state, not the estate's Works or counts: {scoped}"
    );
    let mut needles = estate.closed_secrets.clone();
    needles.push(estate.child.work_id.clone());
    needles.push(estate.estate.display().to_string());
    needles.push("unaccounted".to_string());
    needles.push("did not reproduce".to_string());
    assert_discloses_nothing(
        "the scoped health block over a Work whose journal is gone",
        &scoped["index"],
        &scoped_stderr,
        &needles,
    );

    // 9.2 — the destructive path. This is the executed erasure: it used
    // to exit 0, report `synchronized`, and delete the child's rows.
    let before = estate.index_bytes();
    let (code, _reply, stderr) = estate.rebuild();
    assert_eq!(
        code,
        Some(2),
        "a rebuild from a walk that lost a published Work is refused: {stderr}"
    );
    assert!(stderr.contains("IndexScanIncomplete"), "{stderr:?}");
    assert_eq!(
        estate.index_bytes(),
        before,
        "and the index file is byte-identical: nothing was replaced"
    );
    let admin = estate.admin_index();
    assert!(
        indexes_finding(&admin, &child_finding),
        "the child's published finding is still in the index: {}",
        admin["rows"]
    );
    assert_eq!(admin["index"]["complete"], false, "{}", admin["index"]);

    // 9.3 — across a restart. This branch was written tolerant, because
    // at the time the startup sweeps still opened every `works/` entry
    // through `Journal::open` and re-created the absent journal as a
    // zero-byte file; the property it pins is that missing known
    // material does not become healthy merely because some other path
    // made an empty file, and that property is unchanged. What has
    // changed is upstream: no startup path invents the file any more
    // (§10), so the tolerated case is now a defect and is asserted
    // against rather than allowed for.
    estate.restart(&[]);
    let recreated = estate.journal_path(&estate.child.work_id);
    assert!(
        !recreated.exists(),
        "a startup sweep re-created the absent canonical journal at {}",
        recreated.display()
    );
    let admin = estate.admin_index();
    assert_eq!(
        admin["index"]["complete"], false,
        "an empty file where a published Work's journal was is not a healthy estate: {}",
        admin["index"]
    );
    let before = estate.index_bytes();
    let (code, _reply, stderr) = estate.rebuild();
    assert_eq!(code, Some(2), "and the rebuild is refused again: {stderr}");
    assert_eq!(estate.index_bytes(), before, "still byte-identical");
    assert!(indexes_finding(&estate.admin_index(), &child_finding));

    // 9.4 — recovery, which is the whole reason the rows were kept: the
    // operator restores the exact canonical bytes and one ordinary
    // mutation re-projects everything, with no duplicate rows.
    estate.restore_journal(&estate.child.work_id, &held);
    assert_eq!(
        fs::read(estate.journal_path(&estate.child.work_id)).unwrap(),
        held_bytes,
        "the held-aside journal is byte-identical: it was moved, never edited"
    );
    let (code, reply, stderr) = estate.assert_on(&parent_finding, "after the journal came back");
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        reply["index"]["projection"], "synchronized",
        "the next genuine reconciliation clears the conservative behind: {reply}"
    );
    let admin = estate.admin_index();
    assert_eq!(admin["index"]["complete"], true, "{}", admin["index"]);
    assert_eq!(admin["index"]["pending_rows"], 0);
    assert!(indexes_finding(&admin, &child_finding), "{}", admin["rows"]);
    assert!(
        indexes_finding(&admin, &parent_finding),
        "{}",
        admin["rows"]
    );
    assert_no_duplicate_rows(&admin);

    // And the destructive path works again, because the walk is whole.
    let (code, _reply, stderr) = estate.rebuild();
    assert_eq!(code, Some(0), "{stderr}");
    let admin = estate.admin_index();
    assert_eq!(admin["index"]["projection"], "synchronized");
    assert!(indexes_finding(&admin, &child_finding), "{}", admin["rows"]);
    assert_no_duplicate_rows(&admin);

    estate.stop();
}

/// The same absence one level up: not the journal but the **whole Work
/// directory**. `read_dir` never lists it, so a walk that tracked only
/// the entries it skipped would see nothing at all to track — which is
/// exactly why the accounting is anchored on the rows the index holds
/// and not on the names the walk stepped over.
#[test]
fn a_known_works_missing_directory_is_never_complete_and_never_rebuilt_over() {
    let mut estate = build_estate();
    let parent_finding = estate.raise("a parent record, indexed while healthy");
    let (code, _reply, stderr) = estate.assert_on(&parent_finding, "the parent's assertion");
    assert_eq!(code, Some(0), "{stderr}");
    let child_finding = estate.raise_on_child("a child record, published and indexed");
    let (code, _reply, stderr) = estate.assert_as_child(&child_finding, "the child's assertion");
    assert_eq!(code, Some(0), "{stderr}");

    // Held aside whole, never deleted.
    let child_dir = estate.estate.join("works").join(&estate.child.work_id);
    let held = estate.dir.path().join("held-child-work");
    fs::rename(&child_dir, &held).unwrap();

    let (code, reply, stderr) = estate.assert_on(&parent_finding, "swept with the Work gone");
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        reply["index"]["projection"], "behind",
        "an absent Work directory is missing evidence, not an empty estate: {reply}"
    );
    let before = estate.index_bytes();
    let (code, _reply, stderr) = estate.rebuild();
    assert_eq!(code, Some(2), "{stderr}");
    assert!(stderr.contains("IndexScanIncomplete"), "{stderr:?}");
    assert_eq!(estate.index_bytes(), before);
    assert!(indexes_finding(&estate.admin_index(), &child_finding));

    // Restored whole, and the estate is healthy again on the next sweep.
    fs::rename(&held, &child_dir).unwrap();
    let (code, reply, stderr) = estate.assert_on(&parent_finding, "after the Work came back");
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(reply["index"]["projection"], "synchronized", "{reply}");
    let (code, _reply, stderr) = estate.rebuild();
    assert_eq!(code, Some(0), "{stderr}");
    let admin = estate.admin_index();
    assert!(indexes_finding(&admin, &child_finding), "{}", admin["rows"]);
    assert_no_duplicate_rows(&admin);

    estate.stop();
}

/// A canonical history that is **valid and shorter than it was**: every
/// line whole, the sequence contiguous from 1, nothing for `Journal` to
/// fail closed on — and the event that published an indexed finding no
/// longer in it.
///
/// This is the case a skipped-name or an open-failure rule cannot see at
/// all: the journal opens, replays cleanly, and simply holds less than
/// the estate already published. The Finding lifecycle has four events
/// (raised, settled, asserted, applied) and no retraction, withdrawal or
/// expiry of any of them, and nothing in the product removes a journal
/// line — so a row that was there and is not is unexplained evidence,
/// and unexplained evidence is not something a rebuild may quietly
/// discard. Row *content* is deliberately not compared, only the row's
/// identity, so a re-serialisation is not corruption.
#[test]
fn a_shortened_canonical_history_that_drops_a_published_row_is_never_rebuilt_over() {
    let mut estate = build_estate();
    let parent_finding = estate.raise("a parent record, indexed while healthy");
    let (code, _reply, stderr) = estate.assert_on(&parent_finding, "the parent's assertion");
    assert_eq!(code, Some(0), "{stderr}");

    let child_finding = estate.raise_on_child("a child record about to be published");
    let before_assertion = fs::read_to_string(estate.journal_path(&estate.child.work_id)).unwrap();
    let (code, _reply, stderr) = estate.assert_as_child(&child_finding, "the child's assertion");
    assert_eq!(code, Some(0), "{stderr}");
    let admin = estate.admin_index();
    assert!(
        indexes_finding(&admin, &child_finding),
        "the assertion is published and indexed: {}",
        admin["rows"]
    );

    // Roll the child's journal back to exactly the bytes it held before
    // the assertion, keeping the longer history aside. Contiguous seq,
    // whole lines: a valid journal that says less than the estate did.
    let held = estate.dir.path().join("held-longer-history.ndjson");
    fs::copy(estate.journal_path(&estate.child.work_id), &held).unwrap();
    fs::write(
        estate.journal_path(&estate.child.work_id),
        &before_assertion,
    )
    .unwrap();

    let (code, reply, stderr) = estate.assert_on(&parent_finding, "swept over a shortened history");
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        reply["index"]["projection"], "behind",
        "a valid journal that no longer supports a published row is missing evidence: {reply}"
    );
    let before = estate.index_bytes();
    let (code, _reply, stderr) = estate.rebuild();
    assert_eq!(
        code,
        Some(2),
        "and the rebuild does not silently discard it: {stderr}"
    );
    assert_eq!(estate.index_bytes(), before);
    assert!(indexes_finding(&estate.admin_index(), &child_finding));

    // Restored, and the estate reconciles clean.
    fs::copy(&held, estate.journal_path(&estate.child.work_id)).unwrap();
    let (code, reply, stderr) = estate.assert_on(&parent_finding, "after the history came back");
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(reply["index"]["projection"], "synchronized", "{reply}");
    let (code, _reply, stderr) = estate.rebuild();
    assert_eq!(code, Some(0), "{stderr}");
    assert_no_duplicate_rows(&estate.admin_index());

    estate.stop();
}

/// The positive control the three above must not swallow, stated as one
/// estate rather than inferred: a `works/` entry that never was a Work
/// has no rows in the index, so it can never be an unaccounted one — the
/// whole reason the accounting is anchored on the index and not on the
/// filesystem. Held beside a Work that *is* published and indexed, so
/// what is being measured is the discrimination and not an empty estate.
#[test]
fn an_unused_directory_beside_a_published_work_is_still_a_complete_observation() {
    let mut estate = build_estate();
    let parent_finding = estate.raise("a parent record, indexed while healthy");
    let (code, _reply, stderr) = estate.assert_on(&parent_finding, "the parent's assertion");
    assert_eq!(code, Some(0), "{stderr}");
    let child_finding = estate.raise_on_child("a child record, published and indexed");
    let (code, _reply, stderr) = estate.assert_as_child(&child_finding, "the child's assertion");
    assert_eq!(code, Some(0), "{stderr}");
    let indexed = row_ids(&estate.admin_index());
    assert_eq!(indexed.len(), 2, "{indexed:?}");

    // Three things the estate's layout says are not Works, none of which
    // the index has ever held a row for.
    fs::create_dir_all(estate.estate.join("works").join("work-never-used")).unwrap();
    fs::create_dir_all(estate.estate.join("works").join("work-empty-journal")).unwrap();
    fs::write(
        estate
            .estate
            .join("works")
            .join("work-empty-journal")
            .join("journal.ndjson"),
        "",
    )
    .unwrap();
    fs::write(estate.estate.join("works").join("README"), "not a Work\n").unwrap();

    let (code, reply, stderr) = estate.assert_on(&parent_finding, "beside the residue");
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        reply["index"]["projection"], "synchronized",
        "nothing the index never held a row for makes a walk incomplete: {reply}"
    );
    let (code, _reply, stderr) = estate.rebuild();
    assert_eq!(code, Some(0), "and a rebuild is not refused: {stderr}");
    let admin = estate.admin_index();
    assert_eq!(admin["index"]["complete"], true, "{}", admin["index"]);
    assert_eq!(admin["index"]["pending_rows"], 0);
    for row in &indexed {
        assert!(
            row_ids(&admin).contains(row),
            "the rebuild dropped {row}: {}",
            admin["rows"]
        );
    }
    assert_no_duplicate_rows(&admin);

    estate.stop();
}

/// The health a mutating verb renders says which *record* it is, not
/// which attempt produced it.
///
/// The asymmetry in `weakens` is deliberate and safe — an older
/// attempt's failure may stand over a newer success, never the reverse —
/// but it means the projection a caller reads is not always its own
/// sweep's, so the surface must not describe it as one. What the ticket
/// proves is that this call's sweep was *offered* to the record; it
/// proves nothing about the provenance of what is rendered.
#[test]
fn a_mutating_replys_health_does_not_claim_to_be_this_calls_own_scan() {
    let mut estate = build_estate();
    let finding = estate.raise("the record the sentence is read beside");
    let (code, reply, stderr) = estate.assert_on(&finding, "a healthy mutation");
    assert_eq!(code, Some(0), "{stderr}");
    let observed = reply["index"]["observed"].as_str().unwrap().to_string();
    assert!(
        !observed.contains("at or after this call's own sweep"),
        "the health a mutation renders is not certified as this call's own scan: {observed:?}"
    );
    assert!(
        observed.contains("most recently recorded reconciliation outcome")
            && observed.contains("was offered to"),
        "it says what it is instead: {observed:?}"
    );
    assert!(
        observed.contains("does not re-scan the estate"),
        "and keeps what was already true of it: {observed:?}"
    );

    // A reply that carries rows keeps its own, stronger sentence: those
    // rows and this health really were read together (ruling 0125, 2A).
    let paired = estate.admin_index();
    assert!(
        paired["index"]["observed"]
            .as_str()
            .unwrap()
            .contains("read together as one snapshot"),
        "{}",
        paired["index"]
    );

    estate.stop();
}

// ---- 10. The other estate sweeps read canonical journals too --------
//
// §9 cured the findings walk. It did not cure the rest of the estate,
// and `index-canonical-preserve/HANDOFF.md` recorded exactly that as a
// named limit and as the cause of a flaky gate: six other sweeps in
// `server.rs` walk `works/` purely to *discover* what exists, and every
// one of them opened each entry with `Journal::open` — the estate's one
// **write** path, which `create_dir_all`s the directory and creates
// `journal.ndjson` with `create(true).append(true)`.
//
// The six, and the public operation each one is reached by:
//
// | sweep | reached by |
// |---|---|
// | `remove_owned_containers` | `wirkd stop` |
// | `open_deterministic_runs` | daemon start (docker Run recovery) |
// | `reevaluate_waiting_works` | daemon start |
// | `settle_ready_findings` | daemon start |
// | `find_finding_owner` | `wirk finding assert` / `settle` / `applied` |
// | admin estate-wide list | `wirk finding list --admin` |
//
// Two executed consequences, and they are worse here than they were in
// §9 because these run at startup, at shutdown and on ordinary mutating
// verbs rather than only inside the index walk:
//
// 1. A `works/` entry with no journal gets a **zero-byte
//    `journal.ndjson` created by the sweep itself**. This is why a
//    restart turned an absent journal into an empty one (§9.3), and it
//    is the direct cause of the pre-existing flake in
//    `a_scan_never_creates_a_canonical_journal`: that test's own
//    mutating verb runs `find_finding_owner`, which stops at the first
//    directory holding the finding, so whether it reached the bare
//    directory first came down to `read_dir` order.
// 2. A journal that is readable but not writable (`0444`) fails to open
//    at all, so a real, intact, published Work is **silently skipped**
//    by container cleanup, run recovery, held-work re-evaluation,
//    settlement and the admin listing — its findings simply are not
//    there, at exit 0, with nothing said.
//
// The repair is the same one §9 used and nothing more: `JournalReader`,
// the read-only half of the same journal, behind one named helper
// (`discovery_events`). No writer path moves — `journal_for`,
// `create_journal_for` and `Journal::append` are untouched, and each of
// these sweeps still performs its real intended mutation afterwards
// through them. The positive controls for that live where they already
// live (`docker_executor.rs` for container cleanup and Deterministic
// Run recovery, `nested*` and `w4_*` for held-parent re-evaluation,
// `finding assert`/`settle` throughout this file for settlement); the
// tests below are the ones that did not exist.
//
// Each test drives a public operation that walks the **whole** estate
// with no early return, so none of them depends on `read_dir` order.

/// One deterministic, unambiguous defect site per public operation.
/// A `works/` entry that is not a Work — a bare directory, the estate a
/// half-finished `mkdir`, an interrupted submit or an operator leaves —
/// stays bare across a real daemon's whole life.
#[test]
fn no_estate_sweep_creates_a_canonical_journal_in_a_bare_directory() {
    let mut estate = build_estate();
    let finding = estate.raise("the one real record");
    let (code, _reply, stderr) = estate.assert_on(&finding, "indexed while healthy");
    assert_eq!(code, Some(0), "{stderr}");

    let bare = estate.estate.join("works").join("work-with-no-journal");
    fs::create_dir_all(&bare).unwrap();
    let journal = bare.join("journal.ndjson");
    let still_bare = |what: &str| {
        assert!(
            !journal.exists(),
            "{what} created a canonical journal at {}",
            journal.display()
        );
    };

    // `find_finding_owner`, walked to exhaustion: a `--finding` no Work
    // holds. The walk cannot stop early, so this is the site's causal
    // path and not a lucky ordering of it. The refusal itself is the
    // pre-existing behaviour and is asserted so the walk is known to
    // have actually run.
    let (code, reply, stderr) = finding_cli(
        &estate.estate,
        &[
            "assert",
            "--finding",
            "finding-no-work-in-this-estate-holds",
            "--decision",
            "deferred",
            "--by",
            "a reviewer",
            "--reason",
            "forces the owner walk over every entry",
            "--requesting-work",
            &estate.parent.work_id,
        ],
    );
    assert_eq!(
        code,
        Some(2),
        "an unknown finding is still refused: {stderr} {reply}"
    );
    still_bare("finding-owner discovery");

    // The admin estate-wide listing: every entry, every time, no early
    // return at all.
    let (code, listed, stderr) = finding_cli(&estate.estate, &["list", "--admin"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        lists_finding(&listed, &finding),
        "the listing really did walk the estate: {listed}"
    );
    still_bare("the admin estate-wide finding list");

    // A mutating verb whose own owner walk succeeds — the ordinary case,
    // where the bare directory may or may not be reached before the
    // owner is found. It must be bare either way.
    let (code, _reply, stderr) = estate.assert_on(&finding, "an ordinary mutation");
    assert_eq!(code, Some(0), "{stderr}");
    still_bare("an ordinary finding assert");

    // A real restart: `open_deterministic_runs`, `reevaluate_waiting_
    // works` and `settle_ready_findings` all walk every entry on the way
    // up, unconditionally.
    estate.restart(&[]);
    still_bare("the startup sweeps");

    // The estate is otherwise exactly as healthy as it was: the bare
    // directory is a legal unused directory, not a partial observation.
    let admin = estate.admin_index();
    assert_eq!(admin["index"]["complete"], true, "{}", admin["index"]);
    assert!(indexes_finding(&admin, &finding), "{}", admin["rows"]);

    // And `remove_owned_containers`, on the way down. Last, because it
    // is the sweep that runs as the daemon exits.
    estate.stop();
    still_bare("wirkd stop's container sweep");
}

/// The other half of the same defect, and the one that loses real
/// records rather than inventing empty files: a Work whose canonical
/// journal is readable but not writable is a Work. `Journal::open`
/// demanded append permission on it and failed, so every estate-wide
/// sweep skipped it in silence.
#[test]
fn a_readable_but_unwritable_work_is_still_found_by_every_estate_sweep() {
    let mut estate = build_estate();
    let on_parent = estate.raise("the writable Work's record");
    let (code, _reply, stderr) = estate.assert_on(&on_parent, "indexed while healthy");
    assert_eq!(code, Some(0), "{stderr}");
    let on_child = estate.raise_on_child("the read-only Work's record");
    let (code, _reply, stderr) = estate.assert_as_child(&on_child, "indexed while healthy");
    assert_eq!(code, Some(0), "{stderr}");

    // The estate a restore, an archived tree or a `chmod -R a-w`
    // snapshot leaves: every byte readable, nothing writable.
    let child_journal = estate.journal_path(&estate.child.work_id);
    fs::set_permissions(&child_journal, fs::Permissions::from_mode(0o444)).unwrap();

    // The admin estate-wide listing sees both Works, not one.
    let (code, listed, stderr) = finding_cli(&estate.estate, &["list", "--admin"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        lists_finding(&listed, &on_parent),
        "the writable Work's record: {listed}"
    );
    assert!(
        lists_finding(&listed, &on_child),
        "a Work every reader can read is a Work this sweep read: {listed}"
    );

    // Finding-owner discovery finds it, so the mutating verbs that need
    // it can reach it at all — the read is what was broken, and lost the
    // Work with a `NotFound` that was simply untrue.
    //
    // This call then *succeeds*, and that is the daemon's real behaviour
    // rather than an oversight: `journal_for` caches one
    // `Arc<Mutex<Journal>>` per Work, this Work's handle was opened for
    // append before the `chmod`, and an open descriptor keeps the access
    // it was opened with. The writer path is unchanged and is asserted
    // below, across a restart, where there is no cached handle left.
    let (code, reply, stderr) = estate.assert_as_child(&on_child, "against a read-only journal");
    assert_ne!(
        reply["error"]["code"].as_str(),
        Some("NotFound"),
        "the owner was found, not lost: {reply} {stderr}"
    );
    assert_eq!(code, Some(0), "{stderr}");

    // From here on nothing is supposed to write to this journal at all,
    // so this is where the byte comparison starts: after the last write
    // the daemon was legitimately still able to make, and across every
    // sweep that follows.
    let before = fs::read(&child_journal).unwrap();

    // A restart walks it too — a fresh daemon, no cached handle — and
    // writes nothing to it.
    estate.restart(&[]);
    let (code, listed, stderr) = finding_cli(&estate.estate, &["list", "--admin"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(lists_finding(&listed, &on_child), "{listed}");

    // The writer is untouched: with no cached handle, `journal_for`'s
    // own `Journal::open` demands append permission on this journal and
    // is refused by the kernel. The reader stopped demanding it; the
    // writer never stopped, and does not report the Work as missing.
    let (code, reply, stderr) = estate.assert_as_child(&on_child, "with no cached handle left");
    assert_ne!(
        code,
        Some(0),
        "the write is refused, not silently performed: {reply} {stderr}"
    );
    assert_ne!(
        reply["error"]["code"].as_str(),
        Some("NotFound"),
        "and refused as a write failure, not as a missing Work: {reply}"
    );
    // The negative control is unchanged: a journal that genuinely cannot
    // be read is still not read, and still authorizes nothing. Readable
    // is the whole of what changed; unreadable is unchanged.
    fs::set_permissions(&child_journal, fs::Permissions::from_mode(0o000)).unwrap();
    let (code, listed, stderr) = finding_cli(&estate.estate, &["list", "--admin"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        !lists_finding(&listed, &on_child),
        "an unreadable journal is still unread: {listed}"
    );

    // `remove_owned_containers`, on the way down, over the read-only
    // journal, and then the bytes: every sweep since `before` was a
    // read, and the file is the file.
    fs::set_permissions(&child_journal, fs::Permissions::from_mode(0o444)).unwrap();
    estate.stop();
    assert_eq!(
        fs::read(&child_journal).unwrap(),
        before,
        "a sweep changed a canonical journal it was only reading"
    );
    fs::set_permissions(&child_journal, fs::Permissions::from_mode(0o600)).unwrap();
}

/// A `works/` entry whose journal is genuinely absent is never folded as
/// though it were a legitimately empty canonical history, and never
/// authorizes the mutation the sweep that found it decides on. This is
/// the estate's own layout rule — a directory with no journal is not a
/// Work, the rule `journal_for` already applies to every other read —
/// and the sweep is not the thing that decides otherwise by making one.
///
/// The completeness question is a different question and is answered
/// where it is asked: §9's `CanonicalScan` still reports `behind` here,
/// from the rows the index already holds, and `--rebuild` still refuses.
#[test]
fn an_absent_known_journal_is_never_read_as_an_empty_history() {
    let mut estate = build_estate();
    let parent_finding = estate.raise("the parent's record");
    let (code, _reply, stderr) = estate.assert_on(&parent_finding, "indexed while healthy");
    assert_eq!(code, Some(0), "{stderr}");
    let child_finding = estate.raise_on_child("the record whose journal goes missing");
    let (code, _reply, stderr) = estate.assert_as_child(&child_finding, "indexed while healthy");
    assert_eq!(code, Some(0), "{stderr}");

    let held = estate.hold_journal_aside(&estate.child.work_id);
    let held_bytes = fs::read(&held).unwrap();
    let gone = estate.journal_path(&estate.child.work_id);

    // Every whole-estate public operation, in one run.
    let (code, listed, stderr) = finding_cli(&estate.estate, &["list", "--admin"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        !gone.exists(),
        "the admin listing re-created the absent journal"
    );
    assert!(
        !lists_finding(&listed, &child_finding),
        "and did not invent an empty history to fold: {listed}"
    );

    let (code, _reply, stderr) = estate.assert_on(&parent_finding, "with a journal absent");
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        !gone.exists(),
        "a mutating verb re-created the absent journal"
    );

    estate.restart(&[]);
    assert!(
        !gone.exists(),
        "a startup sweep re-created the absent journal"
    );

    // What §9 decided is untouched: the *index* still knows the Work is
    // missing, the observation is still incomplete, and the rebuild
    // still refuses without replacing a byte.
    let admin = estate.admin_index();
    assert_eq!(
        admin["index"]["complete"], false,
        "an absent journal is still an incomplete observation: {}",
        admin["index"]
    );
    assert!(
        indexes_finding(&admin, &child_finding),
        "and the published row is still there: {}",
        admin["rows"]
    );
    let before = estate.index_bytes();
    let (code, _reply, stderr) = estate.rebuild();
    assert_eq!(code, Some(2), "the rebuild is still refused: {stderr}");
    assert_eq!(estate.index_bytes(), before, "byte-identical");

    // Recovery is unchanged: the exact bytes come back and one ordinary
    // mutation re-projects the estate.
    estate.restore_journal(&estate.child.work_id, &held);
    assert_eq!(
        fs::read(estate.journal_path(&estate.child.work_id)).unwrap(),
        held_bytes,
        "the held-aside journal is byte-identical: it was moved, never edited"
    );
    let (code, _reply, stderr) = estate.assert_on(&parent_finding, "after the journal came back");
    assert_eq!(code, Some(0), "{stderr}");
    let admin = estate.admin_index();
    assert_eq!(admin["index"]["complete"], true, "{}", admin["index"]);
    assert!(indexes_finding(&admin, &child_finding), "{}", admin["rows"]);
    assert_no_duplicate_rows(&admin);

    estate.stop();
}

/// §11.1 — the repair still repairs, and it no longer certifies what it
/// could not check.
///
/// A malformed final line is what a crash between a write and its
/// `fsync` leaves, and `--rebuild` exists to repair exactly it. The
/// defect ruling 0130 names is not that the rebuild ran: it is that it
/// ran while skipping its own preservation check, then reported
/// `synchronized`, `complete: true`, `pending_rows: 0` on a walk it had
/// not checked against anything — at exit 0, with an empty client
/// stderr, the only warning on the daemon's own log where the
/// administrator who typed the command never sees it.
///
/// So: the rebuild proceeds, the rows come back, and the reply says the
/// truth about what it could not establish. The bytes that could not be
/// parsed are kept, because they are the only remaining trace of
/// whatever they held, and while they are kept **nothing automatic**
/// reports this projection complete — not the next mutation, not the
/// next rebuild, not a restart. That last clause is the whole of ruling
/// 0130's objection to reporting `behind` for one call only.
#[test]
fn a_corrupt_index_over_intact_histories_is_repaired_without_certifying_the_unchecked() {
    let mut estate = build_estate();
    let parent_finding = estate.raise("a parent record, indexed while the estate is healthy");
    let (code, _reply, stderr) = estate.assert_on(&parent_finding, "the parent's assertion");
    assert_eq!(code, Some(0), "{stderr}");
    let child_finding = estate.raise_on_child("a child record, published and indexed");
    let (code, reply, stderr) = estate.assert_as_child(&child_finding, "the child's assertion");
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(reply["index"]["projection"], "synchronized", "{reply}");
    assert!(
        estate.preserved_copies().is_empty(),
        "a healthy estate holds none"
    );

    estate.corrupt_index_tail();
    let corrupt_bytes = estate.index_bytes();

    // An ordinary sweep first: the append's own read fails, so the
    // projection is honestly behind and nothing is replaced.
    let (code, reply, stderr) = estate.assert_on(&parent_finding, "swept over a corrupt index");
    assert_eq!(
        code,
        Some(0),
        "the mutation is journaled and durable: {stderr}"
    );
    assert_eq!(reply["index"]["projection"], "behind", "{reply}");
    assert_eq!(
        estate.index_bytes(),
        corrupt_bytes,
        "a sweep replaces nothing"
    );

    // The documented repair. It runs, and its reply is honest.
    let (code, reply, stderr) = estate.rebuild();
    assert_eq!(
        code,
        Some(0),
        "the one command that repairs this file still does: {stderr}"
    );
    assert!(
        indexes_finding(&reply, &child_finding) && indexes_finding(&reply, &parent_finding),
        "every published row is back in the index: {}",
        reply["rows"]
    );
    assert_no_duplicate_rows(&reply);
    assert_eq!(
        reply["index"]["projection"], "behind",
        "a rebuild that could not check itself against the standing index does not report \
         synchronized: {}",
        reply["index"]
    );
    assert_eq!(reply["index"]["complete"], false, "{}", reply["index"]);
    assert!(
        reply["index"]["pending_rows"].is_null(),
        "unknown, not zero: {}",
        reply["index"]
    );
    // The administrator who typed the command is told on their own
    // terminal, which is the surface the defect was invisible on.
    assert!(
        stderr.contains("subset") && stderr.contains("preserved"),
        "the operator's own stderr says so: {stderr:?}"
    );
    assert!(
        !stderr.to_lowercase().contains("delete") && !stderr.to_lowercase().contains("remove"),
        "and never tells them to destroy the evidence: {stderr:?}"
    );

    // The bytes that could not be parsed are kept, exactly.
    let preserved = estate.preserved_copies();
    assert_eq!(preserved.len(), 1, "{preserved:?}");
    assert_eq!(
        fs::read(estate.atlas_dir().join(&preserved[0])).unwrap(),
        corrupt_bytes,
        "the preserved copy is the original bytes, byte for byte"
    );
    let admin = estate.admin_index();
    assert_eq!(
        admin["index"]["preserved_index_copies"],
        serde_json::json!([preserved[0]]),
        "{}",
        admin["index"]
    );
    let detail = admin["index"]["detail"].as_str().unwrap();
    assert!(
        detail.contains(&preserved[0]) && detail.contains("--retire-preserved-index"),
        "administration is told what is held and what clears it: {detail:?}"
    );

    // Scoped callers learn the state and nothing else.
    let (scoped, scoped_stderr) = estate.scoped_index_as(&estate.parent.work_id);
    assert_eq!(scoped["index"]["complete"], false, "{scoped}");
    assert!(
        scoped["index"]["detail"].is_null()
            && scoped["index"]["pending_rows"].is_null()
            && scoped["index"]["preserved_index_copies"].is_null(),
        "a narrowed reader gets no count, no name and no path: {scoped}"
    );
    assert!(
        scoped["index"]["recovery"]
            .as_str()
            .unwrap()
            .contains("until an administrator"),
        "but it is told the honest recovery — a person, not a sweep: {scoped}"
    );
    let mut needles = estate.closed_secrets.clone();
    needles.push(estate.child.work_id.clone());
    needles.push(estate.estate.display().to_string());
    needles.push(preserved[0].clone());
    assert_discloses_nothing(
        "the scoped health block over a preserved unreadable index",
        &scoped["index"],
        &scoped_stderr,
        &needles,
    );

    // §11.1a — the anti-laundering property, which is the whole of
    // ruling 0130's objection. Every one of these has a readable index
    // now, and every one of them is comparing the shortened file with
    // itself, so none of them may report the projection complete.
    let (code, reply, stderr) = estate.assert_on(&parent_finding, "the next ordinary mutation");
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        reply["index"]["projection"], "behind",
        "the next mutation does not wash the unknown away: {}",
        reply["index"]
    );
    let (code, reply, stderr) = estate.rebuild();
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        reply["index"]["projection"], "behind",
        "and neither does another rebuild: {}",
        reply["index"]
    );
    assert_eq!(
        estate.preserved_copies().len(),
        1,
        "a rebuild over a now-readable index preserves nothing new"
    );
    estate.restart(&[]);
    let admin = estate.admin_index();
    assert_eq!(
        admin["index"]["complete"], false,
        "nor a daemon restart: {}",
        admin["index"]
    );

    // The same thing at a plain terminal, with no `--json` to read it
    // for them: the operator is told the projection is not complete and
    // what actually clears it.
    let (code, stdout, stderr) = atlas_text(&estate.estate, &["findings", "--admin"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(stdout.contains("row(s)"), "{stdout:?}");
    assert!(
        stderr.contains("behind") && stderr.contains("until an administrator"),
        "the plain-text surface says it too: {stderr:?}"
    );

    // §11.1b — and it is not a latch only a redesign can open. An
    // administrator says they have reviewed the bytes, and the bytes
    // are kept even then.
    let (code, reply, stderr) = estate.retire_preserved();
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        reply["retired_index_copies"][0]["preserved"], preserved[0],
        "{reply}"
    );
    assert!(estate.preserved_copies().is_empty(), "the marker is gone");
    let retired = estate.retired_copies();
    assert_eq!(retired.len(), 1, "{retired:?}");
    assert_eq!(
        fs::read(estate.atlas_dir().join(&retired[0])).unwrap(),
        corrupt_bytes,
        "retirement is a rename: not one byte of the evidence is destroyed"
    );
    // And it says so, in plain text, where an operator reads it.
    let (code, stdout, stderr) = atlas_text(
        &estate.estate,
        &["findings", "--admin", "--retire-preserved-index"],
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        stdout.contains("row(s)") && !stdout.contains("retired "),
        "a second retirement has nothing left to retire: {stdout:?}"
    );
    assert_eq!(
        reply["index"]["projection"], "synchronized",
        "and the estate may certify itself again: {}",
        reply["index"]
    );
    assert_eq!(reply["index"]["complete"], true, "{}", reply["index"]);
    assert_eq!(reply["index"]["pending_rows"], 0, "{}", reply["index"]);
    assert!(indexes_finding(&reply, &child_finding), "{}", reply["rows"]);
    assert!(
        indexes_finding(&reply, &parent_finding),
        "{}",
        reply["rows"]
    );
    assert_no_duplicate_rows(&reply);

    estate.stop();
}

/// §11.2 — **the executed erasure, refused.** The same corrupt index as
/// §11.1, with one known Work's canonical journal held aside.
///
/// This is `index-combined-verify`'s H2 verbatim: `--rebuild` exited 0
/// with an empty client stderr, replaced the whole index from a walk
/// short by one Work, deleted that Work's published row, and recorded
/// `synchronized / complete: true / pending_rows: 0` — and every later
/// observation agreed, because the short walk had become its own basis.
///
/// The fix is not a warning. It is that one malformed line is not
/// evidence that no valid row is in the file: the rows that still parse
/// are published evidence, they are checked exactly as a readable index
/// is checked, and a walk that cannot reproduce them refuses. Nothing is
/// replaced, no byte moves, and the canonical journal an operator may
/// still hold can be put back.
#[test]
fn a_corrupt_index_never_replaces_away_the_rows_of_a_work_whose_history_is_gone() {
    let mut estate = build_estate();
    let parent_finding = estate.raise("a parent record, indexed while the estate is healthy");
    let (code, _reply, stderr) = estate.assert_on(&parent_finding, "the parent's assertion");
    assert_eq!(code, Some(0), "{stderr}");
    let child_finding = estate.raise_on_child("a child record, published and indexed");
    let (code, reply, stderr) = estate.assert_as_child(&child_finding, "the child's assertion");
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(reply["index"]["projection"], "synchronized", "{reply}");
    assert!(indexes_finding(&estate.admin_index(), &child_finding));

    // The journal moves aside rather than dying, and the index is
    // corrupted the way a crash corrupts it.
    let held = estate.hold_journal_aside(&estate.child.work_id);
    let held_bytes = fs::read(&held).unwrap();
    estate.corrupt_index_tail();
    let before = estate.index_bytes();
    let child_row_lines = fs::read_to_string(estate.index_path())
        .unwrap()
        .lines()
        .filter(|line| line.contains(&child_finding))
        .count();
    assert!(
        child_row_lines >= 1,
        "the child's row is in the file to lose"
    );

    // 11.2a — the destructive path, which is the executed erasure.
    let (code, _reply, stderr) = estate.rebuild();
    assert_eq!(
        code,
        Some(2),
        "a rebuild whose walk lost a published Work is refused even when the standing index \
         cannot be parsed whole: {stderr}"
    );
    assert!(stderr.contains("IndexScanIncomplete"), "{stderr:?}");
    assert_eq!(
        estate.index_bytes(),
        before,
        "and the index file is byte-identical: nothing was replaced"
    );
    assert!(
        estate.preserved_copies().is_empty(),
        "a refusal preserves nothing, because it destroyed nothing"
    );
    assert_eq!(
        fs::read_to_string(estate.index_path())
            .unwrap()
            .lines()
            .filter(|line| line.contains(&child_finding))
            .count(),
        child_row_lines,
        "the child's published row bytes are still exactly where they were"
    );

    // The whole refusal reaches the administrator's own terminal: the
    // client renders `{code} {message}` on stderr. An ordinary
    // `atlas findings --admin` cannot be asked here at all, because the
    // file is still unreadable — which is the point.
    let refusal = stderr.clone();
    assert!(
        refusal.contains(&estate.child.work_id),
        "administration is told which Work to repair: {refusal:?}"
    );
    assert!(
        refusal.contains("could not be parsed whole"),
        "and that the check itself ran off a salvaged basis: {refusal:?}"
    );
    assert!(
        refusal.contains("findings.ndjson.unreadable-")
            && refusal.contains("restore the canonical journal"),
        "and is given a way out that keeps the evidence: {refusal:?}"
    );
    assert!(
        !refusal.to_lowercase().contains("delete ") && !refusal.to_lowercase().contains("remove "),
        "never one that destroys it: {refusal:?}"
    );

    // 11.2b — an ordinary mutation, and a restart, and the rebuild
    // again. None of them may erase it and none of them may certify it.
    let (code, reply, stderr) =
        estate.assert_on(&parent_finding, "swept while the history is gone");
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(reply["index"]["complete"], false, "{reply}");
    estate.restart(&[]);
    let (code, _reply, stderr) = estate.rebuild();
    assert_eq!(code, Some(2), "refused again across a restart: {stderr}");
    assert_eq!(estate.index_bytes(), before, "still byte-identical");

    // 11.2c — recovery, which is the whole reason the rows were kept.
    // The operator restores the exact canonical bytes; the index is
    // still corrupt, so this rebuild is the §11.1 path — it repairs,
    // preserves what it could not parse, and says so.
    estate.restore_journal(&estate.child.work_id, &held);
    assert_eq!(
        fs::read(estate.journal_path(&estate.child.work_id)).unwrap(),
        held_bytes,
        "the held-aside journal is byte-identical: it was moved, never edited"
    );
    let corrupt_bytes = estate.index_bytes();
    let (code, reply, stderr) = estate.rebuild();
    assert_eq!(
        code,
        Some(0),
        "the repair works once the history is back: {stderr}"
    );
    assert!(indexes_finding(&reply, &child_finding), "{}", reply["rows"]);
    assert!(
        indexes_finding(&reply, &parent_finding),
        "{}",
        reply["rows"]
    );
    assert_no_duplicate_rows(&reply);
    assert_eq!(reply["index"]["projection"], "behind", "{}", reply["index"]);
    let preserved = estate.preserved_copies();
    assert_eq!(preserved.len(), 1, "{preserved:?}");
    assert_eq!(
        fs::read(estate.atlas_dir().join(&preserved[0])).unwrap(),
        corrupt_bytes
    );

    // And the genuine, fully checkable reconciliation, once the
    // administrator has reviewed what could not be parsed.
    let (code, _reply, stderr) = estate.retire_preserved();
    assert_eq!(code, Some(0), "{stderr}");
    let (code, reply, stderr) = estate.assert_on(&parent_finding, "after everything came back");
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        reply["index"]["projection"], "synchronized",
        "the next genuine reconciliation over a whole estate is clean: {}",
        reply["index"]
    );
    let admin = estate.admin_index();
    assert_eq!(admin["index"]["complete"], true, "{}", admin["index"]);
    assert_eq!(admin["index"]["pending_rows"], 0);
    assert!(indexes_finding(&admin, &child_finding), "{}", admin["rows"]);
    assert!(
        indexes_finding(&admin, &parent_finding),
        "{}",
        admin["rows"]
    );
    assert_no_duplicate_rows(&admin);
    let (code, _reply, stderr) = estate.rebuild();
    assert_eq!(
        code,
        Some(0),
        "and the destructive path works again: {stderr}"
    );
    assert_eq!(estate.admin_index()["index"]["projection"], "synchronized");

    estate.stop();
}

/// §11.3 — a malformed line with **valid rows after it**, which is the
/// difference between a salvage and a shrug.
///
/// The all-or-nothing read refuses at the first bad line and never sees
/// what follows. If the recovery read did the same, a corruption that
/// lands *before* a Work's row would leave that row unchecked and the
/// replacement would delete it — the same erasure as §11.2, reachable by
/// moving the malformed line one place earlier in the file. So this
/// corrupts the **first** line and holds aside the history behind a row
/// that comes after it.
#[test]
fn a_malformed_line_never_hides_the_published_rows_that_follow_it() {
    let mut estate = build_estate();
    let parent_finding = estate.raise("a parent record, indexed first");
    let (code, _reply, stderr) = estate.assert_on(&parent_finding, "the parent's assertion");
    assert_eq!(code, Some(0), "{stderr}");
    let child_finding = estate.raise_on_child("a child record, indexed after the parent's");
    let (code, _reply, stderr) = estate.assert_as_child(&child_finding, "the child's assertion");
    assert_eq!(code, Some(0), "{stderr}");

    let lines: Vec<String> = fs::read_to_string(estate.index_path())
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    assert!(
        !lines[0].contains(&child_finding),
        "the fixture must put the child's row after the corrupted line: {lines:?}"
    );
    let child_line = lines
        .iter()
        .position(|line| line.contains(&child_finding))
        .expect("the child's row is in the index");
    assert!(child_line > 0, "and strictly after it");

    let held = estate.hold_journal_aside(&estate.child.work_id);
    estate.corrupt_index_line(1);
    let before = estate.index_bytes();

    let (code, _reply, stderr) = estate.rebuild();
    assert_eq!(
        code,
        Some(2),
        "a row after the malformed line is still published evidence, and a walk that cannot \
         reproduce it may not replace the file: {stderr}"
    );
    assert!(stderr.contains("IndexScanIncomplete"), "{stderr:?}");
    assert_eq!(estate.index_bytes(), before, "byte-identical");
    assert!(
        stderr.contains(&estate.child.work_id),
        "and the Work to repair is named: {stderr:?}"
    );

    estate.restore_journal(&estate.child.work_id, &held);
    let (code, reply, stderr) = estate.rebuild();
    assert_eq!(code, Some(0), "{stderr}");
    assert!(indexes_finding(&reply, &child_finding), "{}", reply["rows"]);
    let preserved = estate.preserved_copies();
    assert_eq!(preserved.len(), 1, "{preserved:?}");
    // Retired at a plain terminal, and the operator is told in words
    // what happened to the bytes.
    let (code, stdout, stderr) = atlas_text(
        &estate.estate,
        &["findings", "--admin", "--retire-preserved-index"],
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        stdout.contains(&format!("retired {}", preserved[0]))
            && stdout.contains("its bytes are kept, not removed"),
        "{stdout:?}"
    );
    assert_eq!(estate.retired_copies().len(), 1);
    assert_eq!(
        estate.admin_index()["index"]["projection"],
        "synchronized",
        "{}",
        estate.admin_index()["index"]
    );

    estate.stop();
}

/// §11.4 — the boundaries either side of the salvage.
///
/// **Absent and empty** are not "unreadable": an index holding no rows
/// contradicts nothing, preserves nothing, and rebuilds clean. That
/// distinction is the estate's only escape from a rebuild that could
/// otherwise only ever refuse, and it is kept.
///
/// **Wholly unparsable** is the far end: the salvage recovers no row at
/// all, so there is nothing to check the walk against — but the walk
/// itself is complete over an intact estate, so the repair proceeds, and
/// the bytes are kept because they are all that is left of whatever they
/// said.
///
/// **Unopenable** is neither: nothing at all can be established about
/// what the file holds, so nothing may replace it.
#[test]
fn absent_empty_unparsable_and_unopenable_indexes_are_four_different_facts() {
    let mut estate = build_estate();
    let parent_finding = estate.raise("a parent record");
    let (code, _reply, stderr) = estate.assert_on(&parent_finding, "the parent's assertion");
    assert_eq!(code, Some(0), "{stderr}");
    let child_finding = estate.raise_on_child("a child record");
    let (code, _reply, stderr) = estate.assert_as_child(&child_finding, "the child's assertion");
    assert_eq!(code, Some(0), "{stderr}");

    // Absent: a legal rebuild, and nothing is preserved.
    fs::remove_file(estate.index_path()).unwrap();
    let (code, reply, stderr) = estate.rebuild();
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        reply["index"]["projection"], "synchronized",
        "{}",
        reply["index"]
    );
    assert!(
        estate.preserved_copies().is_empty(),
        "an absent index holds no evidence"
    );
    assert!(indexes_finding(&reply, &child_finding), "{}", reply["rows"]);

    // Empty: the same. A file with no rows in it is a complete
    // observation of no rows.
    fs::write(estate.index_path(), "").unwrap();
    let (code, reply, stderr) = estate.rebuild();
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        reply["index"]["projection"], "synchronized",
        "{}",
        reply["index"]
    );
    assert!(estate.preserved_copies().is_empty());
    assert!(indexes_finding(&reply, &child_finding), "{}", reply["rows"]);

    // Unopenable: a real kernel denial on the file itself.
    let denied = estate.index_bytes();
    estate.set_index_readable(false);
    let (code, _reply, stderr) = estate.rebuild();
    assert_eq!(
        code,
        Some(2),
        "an index nobody can open is not an index known to be empty: {stderr}"
    );
    assert!(stderr.contains("IndexBasisUnreadable"), "{stderr:?}");
    estate.set_index_readable(true);
    assert_eq!(
        estate.index_bytes(),
        denied,
        "and it is byte-identical: nothing replaced what it could not read"
    );
    assert!(estate.preserved_copies().is_empty());

    // Wholly unparsable, over an estate whose journals are all intact:
    // the repair proceeds, keeps the bytes, and says what it could not
    // establish.
    estate.make_index_wholly_unparsable();
    let garbage = estate.index_bytes();
    let (code, reply, stderr) = estate.rebuild();
    assert_eq!(code, Some(0), "{stderr}");
    assert!(indexes_finding(&reply, &child_finding), "{}", reply["rows"]);
    assert!(
        indexes_finding(&reply, &parent_finding),
        "{}",
        reply["rows"]
    );
    assert_no_duplicate_rows(&reply);
    assert_eq!(
        reply["index"]["projection"], "behind",
        "no row could be checked, so completeness is unknown: {}",
        reply["index"]
    );
    let preserved = estate.preserved_copies();
    assert_eq!(preserved.len(), 1, "{preserved:?}");
    assert_eq!(
        fs::read(estate.atlas_dir().join(&preserved[0])).unwrap(),
        garbage
    );
    let (code, reply, stderr) = estate.retire_preserved();
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        reply["index"]["projection"], "synchronized",
        "{}",
        reply["index"]
    );
    assert_eq!(estate.retired_copies().len(), 1);

    estate.stop();
}

/// §11.5 — the two administrative acts stay two, and neither is a
/// scoped caller's to make.
#[test]
fn retiring_preserved_bytes_is_administrative_and_never_part_of_a_rebuild() {
    let mut estate = build_estate();
    let parent_finding = estate.raise("a parent record");
    let (code, _reply, stderr) = estate.assert_on(&parent_finding, "the parent's assertion");
    assert_eq!(code, Some(0), "{stderr}");

    let (code, _reply, stderr) = atlas(
        &estate.estate,
        &[
            "findings",
            "--requesting-work",
            &estate.parent.work_id,
            "--retire-preserved-index",
        ],
    );
    assert_eq!(code, Some(2), "a scoped requester cannot retire: {stderr}");
    assert!(stderr.contains("administrative call"), "{stderr:?}");

    let (code, _reply, stderr) = atlas(
        &estate.estate,
        &[
            "findings",
            "--admin",
            "--rebuild",
            "--retire-preserved-index",
        ],
    );
    assert_eq!(
        code,
        Some(2),
        "and a rebuild may not clear, in the same call, the unknown it raises: {stderr}"
    );
    assert!(
        stderr.contains("separate administrative acts"),
        "{stderr:?}"
    );

    estate.stop();
}

/// The retired name this product derives from a preserved copy's own —
/// deliberately predictable, which is exactly why a rename onto it must
/// not replace what is already there.
fn retired_name_for(preserved: &str) -> String {
    format!(
        "{}{}",
        wirk_atlas::RETIRED_INDEX_PREFIX,
        preserved
            .strip_prefix(wirk_atlas::PRESERVED_INDEX_PREFIX)
            .expect("a preserved copy's name")
    )
}

/// **D1** — a directory the daemon could not *list* is an unanswered
/// question, never a preserved copy.
///
/// `index-basis-recovery-verify/raw/46` executed this: with the estate's
/// atlas directory at `0o111` the daemon put the listing's own error
/// **string** into `preserved_index_copies` as though it were a file
/// name, then counted it — telling an administrator "1 preserved
/// copy(ies) … are held" and telling every scoped requester that bytes
/// "are preserved in this estate" and to go and retire them, from
/// nothing but a permission problem on a directory. There was no such
/// file, and the retirement they were told to run returned `EACCES`.
///
/// Both facts must stop the estate certifying itself. Only one of them
/// is a claim about bytes.
#[test]
fn an_unlistable_atlas_directory_is_an_unknown_and_never_a_preserved_copy() {
    let mut estate = build_estate();
    let parent_finding = estate.raise("a parent record, indexed while the estate is healthy");
    let (code, reply, stderr) = estate.assert_on(&parent_finding, "the parent's assertion");
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(reply["index"]["projection"], "synchronized", "{reply}");
    assert!(
        estate.preserved_copies().is_empty(),
        "a healthy estate holds none"
    );

    estate.set_atlas_listable(false);
    let (code, _reply, stderr) = estate.assert_on(
        &parent_finding,
        "a sweep whose preserved-copy question cannot be answered",
    );
    assert_eq!(
        code,
        Some(0),
        "the journal is canonical and the mutation still stands: {stderr}"
    );
    // The refusal an administrator actually meets in this state, kept as
    // the observed fact it is: the verb the old text told them to run
    // could not run either.
    let (code, _value, retire_stderr) = estate.retire_preserved();
    assert_eq!(
        code,
        Some(2),
        "retirement cannot list the directory either: {retire_stderr}"
    );
    assert!(
        !retire_stderr.contains(wirk_atlas::PRESERVED_INDEX_PREFIX),
        "and it names no copy, because it found none: {retire_stderr:?}"
    );

    // Restored, so the record can be read without the denial being in
    // the way. What is read is the record the sweep above left.
    estate.set_atlas_listable(true);

    let admin = estate.admin_index();
    assert_eq!(
        admin["index"]["complete"], false,
        "an unanswered question is not a complete projection: {}",
        admin["index"]
    );
    assert_eq!(
        admin["index"]["preserved_index_copies"],
        serde_json::json!([]),
        "a directory that could not be listed named no file: {}",
        admin["index"]
    );
    let unknown = admin["index"]["preserved_index_copies_unknown"]
        .as_str()
        .unwrap_or_else(|| panic!("the unknown is carried on its own: {}", admin["index"]));
    assert!(
        unknown.contains("Permission denied"),
        "administration gets the underlying cause: {unknown:?}"
    );
    let detail = admin["index"]["detail"].as_str().unwrap();
    assert!(
        detail.contains("could not be listed") && detail.contains("unknown"),
        "and the detail says the question could not be answered: {detail:?}"
    );
    assert!(
        !detail.contains("are held"),
        "never that a copy is held: {detail:?}"
    );

    // A scoped requester learns the state, and is told nothing that is
    // not true: no path, no count, and no claim that bytes are kept.
    let (scoped, scoped_stderr) = estate.scoped_index_as(&estate.parent.work_id);
    assert_eq!(scoped["index"]["complete"], false, "{scoped}");
    assert!(
        scoped["index"]["detail"].is_null()
            && scoped["index"]["pending_rows"].is_null()
            && scoped["index"]["preserved_index_copies"].is_null()
            && scoped["index"]["preserved_index_copies_unknown"].is_null(),
        "{scoped}"
    );
    let recovery = scoped["index"]["recovery"].as_str().unwrap();
    assert!(
        !recovery.contains("rather than dropped") && !recovery.contains("retires it"),
        "no invented claim that bytes are held, and no instruction to retire what is not \
         there: {recovery:?}"
    );
    assert!(
        recovery.contains("could not be established") && recovery.contains("administrator"),
        "but the honest one — the wait is on a person: {recovery:?}"
    );
    let mut needles = estate.closed_secrets.clone();
    needles.push(estate.child.work_id.clone());
    needles.push(estate.estate.display().to_string());
    assert_discloses_nothing(
        "the scoped health block over an unlistable atlas directory",
        &scoped["index"],
        &scoped_stderr,
        &needles,
    );

    // The same at a plain terminal, with no `--json` to read it for
    // them.
    let (code, _stdout, stderr) = atlas_text(&estate.estate, &["findings", "--admin"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        stderr.contains("could not be listed") && !stderr.contains("are held"),
        "{stderr:?}"
    );

    // And it is not sticky beyond its own fact: the next reconciliation
    // that can answer the question answers it.
    let (code, reply, stderr) = estate.assert_on(
        &parent_finding,
        "the next sweep, with the directory listable",
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        reply["index"]["projection"], "synchronized",
        "a question that can now be answered is answered: {}",
        reply["index"]
    );
    assert_eq!(reply["index"]["complete"], true, "{}", reply["index"]);
    assert!(
        estate.preserved_copies().is_empty(),
        "and no copy was ever held"
    );

    estate.stop();
}

/// **D2** — a retirement that stops part-way says what landed, what did
/// not, and leaves a health record that describes the estate as it now
/// is.
///
/// `index-basis-recovery-verify/raw/48` executed this: with two
/// preserved copies and the second's destination occupied, the reply was
/// the bare string `AtlasError I/O: Is a directory (os error 21)` — no
/// file name, no operation, no word about the rename that *had* already
/// happened — and the health record went on listing both original names,
/// one of which no longer existed. Every byte survived and the retry
/// finished, and both of those stay true here.
#[test]
fn a_partial_retirement_reports_what_landed_and_what_did_not() {
    let mut estate = build_estate();
    let parent_finding = estate.raise("a parent record");
    let (code, _reply, stderr) = estate.assert_on(&parent_finding, "the parent's assertion");
    assert_eq!(code, Some(0), "{stderr}");

    // Two preserved copies, from two corrupt-and-repair cycles — the
    // estate an operator who has repaired this file twice really has.
    let mut preserved_bytes = Vec::new();
    for cycle in 0..2 {
        estate.corrupt_index_tail();
        preserved_bytes.push(estate.index_bytes());
        let (code, _reply, stderr) = estate.rebuild();
        assert_eq!(code, Some(0), "cycle {cycle}: {stderr}");
    }
    let preserved = estate.preserved_copies();
    assert_eq!(preserved.len(), 2, "{preserved:?}");

    // The second copy's destination is occupied by a non-empty
    // directory: a real kernel refusal at the second rename, with the
    // first already done.
    let blocked = retired_name_for(&preserved[1]);
    let blocked_path = estate.atlas_dir().join(&blocked);
    fs::create_dir(&blocked_path).unwrap();
    fs::write(blocked_path.join("keep"), b"an operator's own directory").unwrap();

    let (code, _value, stderr) = estate.retire_preserved();
    assert_eq!(code, Some(2), "{stderr}");
    assert!(
        stderr.contains(&preserved[1]) && stderr.contains(&blocked),
        "the failed operation names both real paths: {stderr:?}"
    );
    assert!(
        stderr.contains("could not be retired as"),
        "and says what was being attempted: {stderr:?}"
    );
    assert!(
        stderr.contains(&preserved[0]) && stderr.contains(&retired_name_for(&preserved[0])),
        "and the rename that did land is reported, not swallowed: {stderr:?}"
    );
    assert!(
        stderr.contains("no byte of any preserved copy was removed"),
        "{stderr:?}"
    );

    // Every byte, at the real path the reply names.
    assert_eq!(
        fs::read(estate.atlas_dir().join(retired_name_for(&preserved[0]))).unwrap(),
        preserved_bytes[0],
        "the retired copy is the preserved bytes, byte for byte"
    );
    assert_eq!(
        fs::read(estate.atlas_dir().join(&preserved[1])).unwrap(),
        preserved_bytes[1],
        "and the one that could not move did not move"
    );
    assert_eq!(
        fs::read(blocked_path.join("keep")).unwrap(),
        b"an operator's own directory",
        "nor did what was in the way"
    );

    // The record describes the estate as it now is — one copy left, and
    // it is the one that is still there.
    let admin = estate.admin_index();
    assert_eq!(
        admin["index"]["preserved_index_copies"],
        serde_json::json!([preserved[1]]),
        "the record names no file that is gone: {}",
        admin["index"]
    );
    assert_eq!(admin["index"]["complete"], false, "{}", admin["index"]);

    // Unblocked, the retry finishes.
    fs::remove_dir_all(&blocked_path).unwrap();
    let (code, reply, stderr) = estate.retire_preserved();
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        reply["retired_index_copies"],
        serde_json::json!([{ "preserved": preserved[1], "retired": blocked }]),
        "{reply}"
    );
    assert!(estate.preserved_copies().is_empty(), "{reply}");
    assert_eq!(
        fs::read(estate.atlas_dir().join(&blocked)).unwrap(),
        preserved_bytes[1],
        "retirement is still a rename: not one byte destroyed"
    );
    assert_eq!(
        reply["index"]["projection"], "synchronized",
        "and the estate may certify itself again: {}",
        reply["index"]
    );

    estate.stop();
}

/// **The root's qualification of the review's limit** — the retired name
/// is derived from the preserved copy's own and is therefore entirely
/// predictable, and `fs::rename` is `rename(2)`, which silently replaces
/// an existing *regular file* at the destination.
///
/// That is reachable without an adversary: the product's own refusal
/// text documents `atlas/findings.ndjson.unreadable-<a name of your
/// choosing>` as the manual preservation, so an operator naming copies
/// by hand across repeated maintenance can arrive at a retired name that
/// is already taken. A verb whose contract is "Nothing is deleted" must
/// not be the thing that deletes them.
#[test]
fn an_occupied_retired_name_is_never_overwritten() {
    let mut estate = build_estate();
    let parent_finding = estate.raise("a parent record");
    let (code, _reply, stderr) = estate.assert_on(&parent_finding, "the parent's assertion");
    assert_eq!(code, Some(0), "{stderr}");
    estate.corrupt_index_tail();
    let corrupt_bytes = estate.index_bytes();
    let (code, _reply, stderr) = estate.rebuild();
    assert_eq!(code, Some(0), "{stderr}");
    let preserved = estate.preserved_copies();
    assert_eq!(preserved.len(), 1, "{preserved:?}");

    let occupied = estate.atlas_dir().join(retired_name_for(&preserved[0]));
    let earlier = b"bytes kept by hand from an earlier round of maintenance\n";
    fs::write(&occupied, earlier).unwrap();

    let (code, _value, stderr) = estate.retire_preserved();
    assert_eq!(
        code,
        Some(2),
        "an occupied retired name is a refusal, never a replacement: {stderr}"
    );
    assert_eq!(
        fs::read(&occupied).unwrap(),
        earlier,
        "not one byte of what was already there moved"
    );
    assert_eq!(
        fs::read(estate.atlas_dir().join(&preserved[0])).unwrap(),
        corrupt_bytes,
        "and the copy that could not move is still preserved"
    );
    assert!(
        stderr.contains(&preserved[0]) && stderr.contains(&retired_name_for(&preserved[0])),
        "both names are in the refusal: {stderr:?}"
    );

    let admin = estate.admin_index();
    assert_eq!(
        admin["index"]["preserved_index_copies"],
        serde_json::json!([preserved[0]]),
        "{}",
        admin["index"]
    );
    assert_eq!(admin["index"]["complete"], false, "{}", admin["index"]);

    // The positive control, on the same estate and the same call: with
    // the name free the retirement lands, and both byte sets are still
    // in the estate afterwards.
    let kept_elsewhere = estate
        .atlas_dir()
        .join("findings.ndjson.retired-kept-by-hand");
    fs::rename(&occupied, &kept_elsewhere).unwrap();
    let (code, reply, stderr) = estate.retire_preserved();
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        fs::read(&occupied).unwrap(),
        corrupt_bytes,
        "the preserved bytes are at the retired name now: {reply}"
    );
    assert_eq!(
        fs::read(&kept_elsewhere).unwrap(),
        earlier,
        "and the older bytes are still exactly where they were put"
    );
    assert_eq!(
        reply["index"]["projection"], "synchronized",
        "{}",
        reply["index"]
    );

    estate.stop();
}

// ---- 21. A backing file that is not there is not an empty index -------

/// Ruling 0137, executed: **an index file that is not on disk cannot
/// answer `complete: true` with no rows.**
///
/// The window this closes was reproduced on a real daemon with one real
/// asserted row: deleting `atlas/findings.ndjson` left the *recorded*
/// health saying `synchronized`, while the read beside it took
/// `read_rows`' old `!path.exists() -> Ok(vec![])` branch. The same
/// daemon, in the same second, then answered "the index is complete and
/// holds nothing" to `atlas findings` and "here is the Finding" to
/// `finding list` — and the reply's own `observed` sentence claimed the
/// two halves were one snapshot. Missing was indistinguishable from
/// legitimately empty, which is the exact inference the rebuild path
/// already refuses to make in the other direction ("an index that could
/// not be opened is not an index known to hold no rows").
///
/// What is asserted here is the whole shape of the repair, including the
/// things that must **not** change: the read still writes nothing and
/// re-scans no journal, the canonical journals are byte-identical
/// afterwards, a scoped requester learns the state and not one
/// administrative fact about it, unreadable and corrupt keep failing
/// honestly rather than being folded into this, and a real restart still
/// recovers the exact row from the journals.
///
/// The positive control that keeps this from becoming "absent means
/// broken" lives in
/// `a_legally_empty_estate_and_irrelevant_entries_are_a_complete_observation`:
/// an estate that never wrote an index has no file either, and is
/// complete and empty. The distinction this test turns on is not
/// absence — it is absence *against a reconciliation that read a file
/// which was there*.
#[test]
fn a_missing_index_file_is_never_answered_as_a_complete_empty_projection() {
    let mut estate = build_estate();
    let finding = estate.raise("a record the journals keep");
    let (code, _reply, stderr) = estate.assert_on(&finding, "the only assertion");
    assert_eq!(code, Some(0), "{stderr}");

    let healthy = estate.admin_index();
    assert_eq!(healthy["index"]["projection"], "synchronized");
    assert!(indexes_finding(&healthy, &finding), "{healthy}");
    let row_before = row_ids(&healthy);
    assert_eq!(row_before.len(), 1, "{row_before:?}");
    let journals_before = journal_state(&estate);

    // The file goes, and nothing else does. The journals, the atlas
    // directory and the daemon are all untouched.
    fs::remove_file(estate.index_path()).unwrap();

    // The third line is the one that settles it: the same daemon, in the
    // same window, still holds the Finding canonically.
    let (code, listed, stderr) = finding_cli(&estate.estate, &["list", "--admin"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        lists_finding(&listed, &finding),
        "the journals still hold the Finding: {listed}"
    );

    for round in 0..3 {
        let admin = estate.admin_index();
        assert_eq!(
            admin["index"]["complete"], false,
            "round {round}: a read from a file that is not there is not a complete projection: {}",
            admin["index"]
        );
        assert_eq!(admin["index"]["projection"], "behind", "{}", admin["index"]);
        assert!(
            admin["rows"].as_array().unwrap().is_empty(),
            "the read genuinely has no rows to show — it is the completeness claim beside them \
             that was false: {admin}"
        );
        // Unknown, never zero: what the file held is exactly what this
        // read cannot establish.
        assert!(
            admin["index"]["pending_rows"].is_null(),
            "{}",
            admin["index"]
        );
        let detail = admin["index"]["detail"].as_str().unwrap_or_default();
        assert!(
            detail.contains("was not there when these rows were read"),
            "the administrator is told which fact this is: {detail:?}"
        );
        assert!(
            admin["index"]["recovery"].is_string(),
            "and what clears it: {}",
            admin["index"]
        );

        let (scoped, scoped_stderr) = estate.scoped_index();
        assert_eq!(
            scoped["index"]["complete"], false,
            "round {round}: a scoped reader is told the same state: {}",
            scoped["index"]
        );
        assert_eq!(scoped["index"]["projection"], "behind");
        assert!(scoped["rows"].as_array().unwrap().is_empty(), "{scoped}");
        // The state and nothing else: no count, no detail, no path, no
        // file name (ruling 0135 R11, unchanged by this repair).
        assert!(
            scoped["index"]["pending_rows"].is_null(),
            "{}",
            scoped["index"]
        );
        assert!(
            scoped["index"].get("detail").is_none(),
            "{}",
            scoped["index"]
        );
        assert!(
            scoped["index"].get("preserved_index_copies").is_none(),
            "{}",
            scoped["index"]
        );
        assert_discloses_nothing(
            "a scoped read of an absent index",
            &scoped,
            &scoped_stderr,
            &[
                "findings.ndjson".to_string(),
                estate.estate.display().to_string(),
            ],
        );
    }

    // Six queries wrote nothing: the file the reads reported absent is
    // still absent, and no journal moved.
    assert!(
        !estate.index_path().exists(),
        "a query never writes the index it read"
    );
    assert_eq!(
        journals_before,
        journal_state(&estate),
        "and never re-scans or rewrites the canonical journals"
    );

    // The two states that were already honest stay exactly as honest,
    // and are **not** folded into the new one: an unreadable and a
    // corrupt index each still refuse the read outright.
    fs::write(estate.index_path(), "not json at all\n").unwrap();
    let (code, _value, stderr) = atlas(&estate.estate, &["findings", "--admin"]);
    assert_eq!(code, Some(2), "a corrupt index still refuses: {stderr}");
    assert!(stderr.contains("malformed"), "{stderr}");
    estate.set_index_readable(false);
    let (code, _value, stderr) = atlas(&estate.estate, &["findings", "--admin"]);
    assert_eq!(code, Some(2), "an unreadable index still refuses: {stderr}");
    assert!(
        stderr.contains("Permission denied"),
        "and for its own reason: {stderr}"
    );
    estate.set_index_readable(true);
    fs::remove_file(estate.index_path()).unwrap();

    // A real restart re-projects the exact row from the journals that
    // never stopped holding it — 0130's "recovers what the estate still
    // knows", unweakened by any of the above.
    estate.restart(&[]);
    let recovered = estate.admin_index();
    assert_eq!(
        recovered["index"]["projection"], "synchronized",
        "{}",
        recovered["index"]
    );
    assert_eq!(recovered["index"]["complete"], true);
    assert_eq!(
        row_ids(&recovered),
        row_before,
        "the same row, by its own content-addressed id: {recovered}"
    );
    assert!(indexes_finding(&recovered, &finding), "{recovered}");
    estate.stop();
}

/// What a read may say about an absent index file, asserted for both
/// surfaces at once: the row list is empty and it is the completeness
/// claim beside it that must not be made, the administrator is told
/// which fact this is and how it clears, and the scoped reader is told
/// the state and nothing else (ruling 0135 R11).
fn assert_an_absent_index_is_not_a_complete_projection(estate: &Estate, finding: &str, when: &str) {
    let admin = estate.admin_index();
    assert_eq!(
        admin["index"]["complete"], false,
        "{when}: a read from a file that is not there is not a complete projection: {}",
        admin["index"]
    );
    assert_eq!(
        admin["index"]["projection"], "behind",
        "{when}: {}",
        admin["index"]
    );
    assert!(
        admin["rows"].as_array().unwrap().is_empty(),
        "{when}: the read genuinely has no rows to show: {admin}"
    );
    assert!(
        admin["index"]["pending_rows"].is_null(),
        "{when}: unknown, never zero: {}",
        admin["index"]
    );
    let detail = admin["index"]["detail"].as_str().unwrap_or_default();
    assert!(
        detail.contains("was not there when these rows were read"),
        "{when}: the administrator is told which fact this is: {detail:?}"
    );
    assert!(
        admin["index"]["recovery"].is_string(),
        "{when}: and what clears it: {}",
        admin["index"]
    );

    let (scoped, scoped_stderr) = estate.scoped_index();
    assert_eq!(
        scoped["index"]["complete"], false,
        "{when}: a scoped reader is told the same state: {}",
        scoped["index"]
    );
    assert_eq!(scoped["index"]["projection"], "behind", "{when}: {scoped}");
    assert!(
        scoped["rows"].as_array().unwrap().is_empty(),
        "{when}: {scoped}"
    );
    assert!(
        scoped["index"]["pending_rows"].is_null(),
        "{when}: {}",
        scoped["index"]
    );
    assert!(
        scoped["index"].get("detail").is_none(),
        "{when}: {}",
        scoped["index"]
    );
    assert_discloses_nothing(
        "a scoped read of an index deleted inside a reconciliation's own window",
        &scoped,
        &scoped_stderr,
        &[
            "findings.ndjson".to_string(),
            estate.estate.display().to_string(),
        ],
    );

    // The line that settles what the completeness claim would have been
    // about: the same daemon, in the same window, still holds the
    // Finding canonically.
    let (code, listed, stderr) = finding_cli(&estate.estate, &["list", "--admin"]);
    assert_eq!(code, Some(0), "{when}: {stderr}");
    assert!(
        lists_finding(&listed, finding),
        "{when}: the journals still hold the Finding: {listed}"
    );
}

/// Ruling 0137 at the **recorded** half of the pair — the seam
/// `loop-c3-index-read-verify/VERDICT.md` §4 executed on a real daemon
/// and left standing by the read-side repair.
///
/// A read's own backing is decided by one `read_to_string` whose
/// `NotFound` *is* the absence, so no schedule can turn a disappearance
/// into a known empty *there*. The record's backing was decided
/// somewhere else entirely: a listing of `atlas/` taken inside
/// `record_index_projection`, which runs after `append_finding_rows` has
/// already published. Delete the file inside that window — the product
/// names it itself, `checkpoint("findings-index-published")`, "between
/// the publication and the record, with the lock held" — and the record
/// written a moment later says `Synchronized` with `index_backing:
/// Absent`. Every later read then finds no file, compares absent-now
/// against absent-then, reads the pair as "an estate that never wrote an
/// index", and answers `complete: true` with no rows to the admin and
/// the scoped surface alike, while the same daemon's journals still hold
/// the Finding. That is 0137's own prohibition, through the one door the
/// read cannot see, and it stands until the next reconciliation.
///
/// The repair is the fact the reconciliation already established and
/// threw away: its own append read the index file, or wrote it, and
/// knows which. A listing taken afterwards is a *later* observation of
/// the same file, and a later absence is not evidence that the estate
/// never wrote one. No new store, no timer, no journal re-scan, and
/// nothing here re-reads the estate.
///
/// Everything in this test is real: a real assertion in a real daemon,
/// parked on a real socket rendezvous by the product's own barrier and
/// released by a peer disappearing — no injected failure, and nothing
/// paced by time (ruling 0044 D134).
#[test]
fn an_index_deleted_after_its_own_sweep_published_it_is_never_a_known_empty() {
    let mut estate = build_estate();
    let kept = estate.raise("the record the journals keep across the whole window");
    let (code, _reply, stderr) = estate.assert_on(&kept, "the assertion that publishes the index");
    assert_eq!(code, Some(0), "{stderr}");
    let healthy = estate.admin_index();
    assert_eq!(healthy["index"]["projection"], "synchronized", "{healthy}");
    let rows_before = row_ids(&healthy);
    assert_eq!(rows_before.len(), 1, "{rows_before:?}");
    let journals_before = journal_state(&estate);

    let mut window = Window::new(estate.dir.path(), "published");
    estate.restart(&[(
        "WIRK_ATLAS_BARRIER",
        &window.env("findings-index-published"),
    )]);

    // A second real assertion: its sweep has a row to add, so its append
    // really writes the file, and it parks after publishing it and
    // before the record is formed.
    let second = estate.raise("the record whose own sweep is held at the published window");
    window.arm();
    let estate_root = estate.estate.clone();
    let parent = estate.parent.work_id.clone();
    let parked_finding = second.clone();
    let parked = std::thread::spawn(move || {
        finding_cli(
            &estate_root,
            &[
                "assert",
                "--finding",
                &parked_finding,
                "--decision",
                "deferred",
                "--by",
                "a reviewer",
                "--reason",
                "the sweep that publishes the index and is held before it records",
                "--requesting-work",
                &parent,
            ],
        )
    });
    window.wait_parked();

    // The publication really happened: the file the record is about to
    // describe is on disk, holding the row this sweep just appended.
    assert!(
        estate.index_path().exists(),
        "the sweep parks after its own publication"
    );
    assert_eq!(
        estate.index_rows_on_disk(),
        2,
        "and the row it appended is in the file"
    );

    // The external deletion, inside the window: nothing else in the
    // estate is touched, and the daemon is not told.
    fs::remove_file(estate.index_path()).unwrap();
    window.release();
    let (code, reply, stderr) = parked.join().unwrap();
    assert_eq!(code, Some(0), "the assertion is journaled: {stderr}");
    // Its own record is the one its own walk and its own append made,
    // and that is not what this repair changes: the sweep did publish a
    // complete index, and the deletion is not its to observe. What must
    // not happen is a later *read* of the missing file inheriting that
    // completeness.
    assert_eq!(
        reply["index"]["projection"], "synchronized",
        "the mutating reply still reports the reconciliation it really made: {reply}"
    );
    assert!(
        !estate.index_path().exists(),
        "and the file is gone before any read"
    );

    for round in 0..3 {
        assert_an_absent_index_is_not_a_complete_projection(
            &estate,
            &kept,
            &format!("round {round} after a deletion inside the publish-to-record window"),
        );
    }

    // Six reads wrote nothing and re-scanned nothing.
    assert!(
        !estate.index_path().exists(),
        "a query never writes the index it read"
    );
    assert_eq!(
        journals_before.len(),
        journal_state(&estate).len(),
        "and never creates a canonical journal"
    );

    // Not latched, and cleared by exactly what the surface says clears
    // it: the next real reconciliation re-projects the rows from the
    // journals that never stopped holding them.
    let (code, _reply, stderr) =
        estate.assert_on(&kept, "the mutation that repairs the projection");
    assert_eq!(code, Some(0), "{stderr}");
    let recovered = estate.admin_index();
    assert_eq!(
        recovered["index"]["projection"], "synchronized",
        "{}",
        recovered["index"]
    );
    assert_eq!(recovered["index"]["complete"], true, "{recovered}");
    for row in &rows_before {
        assert!(
            row_ids(&recovered).contains(row),
            "every earlier row is back by its own content-addressed id: {recovered}"
        );
    }
    assert!(indexes_finding(&recovered, &second), "{recovered}");
    estate.stop();
}

/// The same seam through a sweep that **wrote nothing at all**, and the
/// legitimate case it must not swallow.
///
/// A repair that only trusted an append which actually wrote would leave
/// the common case standing: after the first mutation, a sweep offers
/// rows the file already holds, `append_finding_rows` returns `Ok(0)`
/// "without a byte", and the file it read is exactly as real as one it
/// rewrote. `--retire-preserved-index` is that sweep on demand — it
/// re-observes the estate through the same reconciliation with nothing
/// to append — and it is used here for both directions:
///
/// * **Never written** (no index file, and this sweep did not make one):
///   the estate's empty projection of an estate with no findings really
///   is complete, and a read must go on saying so. Reading absence as
///   loss here would invent a lost row out of a healthy estate.
/// * **Read, then deleted inside the record's own window**: the file was
///   there when the sweep looked at it, so a later read that finds none
///   cannot call its emptiness known.
///
/// The two differ by one fact, and it is a fact the reconciliation
/// already has.
#[test]
fn a_sweep_that_wrote_nothing_still_records_whether_the_file_it_read_was_there() {
    let mut estate = build_estate();
    assert!(
        !estate.index_path().exists(),
        "an estate that has raised nothing has never written an index"
    );

    let mut window = Window::new(estate.dir.path(), "published");
    estate.restart(&[(
        "WIRK_ATLAS_BARRIER",
        &window.env("findings-index-published"),
    )]);

    // 1. The pre-publication control, through the same window: a real
    // no-op sweep parks at the published checkpoint having written
    // nothing, and the estate is still an honest known-empty afterwards.
    window.arm();
    let estate_root = estate.estate.clone();
    let never_written = std::thread::spawn(move || {
        atlas(
            &estate_root,
            &["findings", "--admin", "--retire-preserved-index"],
        )
    });
    window.wait_parked();
    assert!(
        !estate.index_path().exists(),
        "a sweep with nothing to append writes nothing at all"
    );
    window.release();
    let (code, _reply, stderr) = never_written.join().unwrap();
    assert_eq!(code, Some(0), "{stderr}");

    let empty = estate.admin_index();
    assert_eq!(
        empty["index"]["projection"], "synchronized",
        "an estate that never wrote an index is not behind: {}",
        empty["index"]
    );
    assert_eq!(empty["index"]["complete"], true, "{empty}");
    assert!(empty["rows"].as_array().unwrap().is_empty(), "{empty}");

    // 2. Now the estate really does write one.
    let finding = estate.raise("the record the index is published for");
    let (code, _reply, stderr) =
        estate.assert_on(&finding, "the assertion that publishes the index");
    assert_eq!(code, Some(0), "{stderr}");
    let healthy = estate.admin_index();
    assert_eq!(healthy["index"]["projection"], "synchronized", "{healthy}");
    assert_eq!(row_ids(&healthy).len(), 1, "{healthy}");
    let published = fs::read(estate.index_path()).unwrap();

    // 3. The same no-op sweep, parked in the same window, over a file
    // that is there: it offers rows the index already holds, so it
    // writes nothing — and the file it read is the one the record must
    // describe.
    window.arm();
    let estate_root = estate.estate.clone();
    let dedup = std::thread::spawn(move || {
        atlas(
            &estate_root,
            &["findings", "--admin", "--retire-preserved-index"],
        )
    });
    window.wait_parked();
    assert_eq!(
        fs::read(estate.index_path()).unwrap(),
        published,
        "this sweep appended nothing: the file is byte-for-byte the one it read"
    );
    fs::remove_file(estate.index_path()).unwrap();
    window.release();
    let (code, _reply, stderr) = dedup.join().unwrap();
    assert_eq!(code, Some(0), "{stderr}");

    assert_an_absent_index_is_not_a_complete_projection(
        &estate,
        &finding,
        "a deletion inside a no-op sweep's own publish-to-record window",
    );
    estate.stop();
}

/// The same seam of ruling 0137 on the **rebuild** path — the one door
/// the record-side repair deliberately left open.
///
/// `--rebuild` is a whole-file replacement: `rewrite_rows` renames a
/// freshly written file into place and `fsync`s the atlas directory, so
/// a rebuild that returns `Ok` has *published* an index file and knows
/// it. Every one of its six record sites nonetheless passed
/// `RecordedBacking::Unknown`, which hands the question to the listing
/// taken afterwards inside `record_index_projection`. Delete the file
/// between the rename and that listing — the product's own
/// `checkpoint("findings-renamed")`, inside the replacement — and the
/// record says `Synchronized` with `index_backing: Absent`, exactly the
/// pair that makes every later read of the missing file answer
/// `complete: true` with no rows while the journals still hold the
/// Finding.
///
/// The fact that closes it is the writer's own: this call renamed a file
/// into place. No listing is trusted to describe a moment it did not
/// observe, no store, no timer, and no journal re-scan.
///
/// Real throughout: a real rebuild in a real daemon, parked on the
/// product's own socket rendezvous and released by a peer disappearing
/// (ruling 0044 D134).
#[test]
fn an_index_deleted_after_a_rebuild_republished_it_is_never_a_known_empty() {
    let mut estate = build_estate();
    let kept = estate.raise("the record the journals keep across the whole window");
    let (code, _reply, stderr) = estate.assert_on(&kept, "the assertion that publishes the index");
    assert_eq!(code, Some(0), "{stderr}");
    let healthy = estate.admin_index();
    assert_eq!(healthy["index"]["projection"], "synchronized", "{healthy}");
    let rows_before = row_ids(&healthy);
    assert_eq!(rows_before.len(), 1, "{rows_before:?}");
    let journals_before = journal_state(&estate);

    let mut window = Window::new(estate.dir.path(), "renamed");
    estate.restart(&[("WIRK_ATLAS_BARRIER", &window.env("findings-renamed"))]);

    // The rebuild parks inside its own replacement, after the atomic
    // rename that publishes the file and before the record is formed.
    window.arm();
    let estate_root = estate.estate.clone();
    let rebuild =
        std::thread::spawn(move || atlas(&estate_root, &["findings", "--admin", "--rebuild"]));
    window.wait_parked();

    // The publication really happened: the file this rebuild's own
    // record is about to describe is on disk, holding the walked row.
    assert!(
        estate.index_path().exists(),
        "the rebuild parks after its own rename"
    );
    assert_eq!(
        estate.index_rows_on_disk(),
        1,
        "and the row it walked is in the file it just renamed into place"
    );

    // The external deletion, inside the window: nothing else is touched
    // and the daemon is not told.
    fs::remove_file(estate.index_path()).unwrap();
    window.release();
    let (code, reply, stderr) = rebuild.join().unwrap();
    assert_eq!(code, Some(0), "the rebuild itself succeeds: {stderr}");
    // The replacement really did publish a complete index — the file
    // above is the proof — and the deletion is not the rebuild's to
    // observe. But this reply's own row list is read *after* it, from a
    // file that is no longer there, so the reply is qualified by the
    // very rule this test is about rather than certifying rows it could
    // not read. Its record underneath is the one the later reads are
    // paired with.
    assert_eq!(
        reply["index"]["projection"], "behind",
        "the rebuild's own reply reads the file it no longer has: {reply}"
    );
    assert_eq!(reply["index"]["complete"], false, "{reply}");
    assert!(
        reply["rows"].as_array().unwrap().is_empty(),
        "and has no rows to show: {reply}"
    );
    assert!(
        !estate.index_path().exists(),
        "and the file is gone before any read"
    );

    for round in 0..2 {
        assert_an_absent_index_is_not_a_complete_projection(
            &estate,
            &kept,
            &format!("round {round} after a deletion inside a rebuild's rename-to-record window"),
        );
    }

    // The reads wrote nothing and re-scanned nothing.
    assert!(
        !estate.index_path().exists(),
        "a query never writes the index it read"
    );
    assert_eq!(
        journals_before.len(),
        journal_state(&estate).len(),
        "and never creates a canonical journal"
    );

    // Not latched: the next real reconciliation re-projects the rows
    // from the journals that never stopped holding them.
    let (code, _reply, stderr) = estate.rebuild();
    assert_eq!(code, Some(0), "{stderr}");
    let recovered = estate.admin_index();
    assert_eq!(
        recovered["index"]["projection"], "synchronized",
        "{}",
        recovered["index"]
    );
    assert_eq!(recovered["index"]["complete"], true, "{recovered}");
    assert_eq!(
        row_ids(&recovered),
        rows_before,
        "every earlier row is back by its own content-addressed id: {recovered}"
    );
    estate.stop();
}

/// The rebuild of an estate with **no findings at all**, and the
/// legitimate known-empty it must not swallow.
///
/// The two states differ by one fact and it is the writer's:
///
/// * **Never written.** No rebuild has run, there is no index file, and
///   an empty projection of an estate with no findings really is
///   complete. A read must go on saying so.
/// * **Written empty, then deleted inside the record's own window.**
///   `rebuild_finding_rows(vec![])` still renames a real, empty file
///   into place — the replacement is unconditional — so the estate did
///   have an index file, and a later read that finds none cannot call
///   its emptiness known. What that file held is what the *record*
///   cannot establish from an absence observed later.
#[test]
fn a_rebuild_that_wrote_an_empty_index_still_records_the_file_it_renamed() {
    let mut estate = build_estate();
    assert!(
        !estate.index_path().exists(),
        "an estate that has raised nothing has never written an index"
    );

    // 1. The never-written control, before anything writes: complete,
    // and it stays that way.
    let empty = estate.admin_index();
    assert_eq!(
        empty["index"]["projection"], "synchronized",
        "an estate that never wrote an index is not behind: {}",
        empty["index"]
    );
    assert_eq!(empty["index"]["complete"], true, "{empty}");
    assert!(empty["rows"].as_array().unwrap().is_empty(), "{empty}");

    let mut window = Window::new(estate.dir.path(), "renamed-empty");
    estate.restart(&[("WIRK_ATLAS_BARRIER", &window.env("findings-renamed"))]);
    let after_restart = estate.admin_index();
    assert_eq!(
        after_restart["index"]["complete"], true,
        "and a restart of it is still an honest known-empty: {}",
        after_restart["index"]
    );
    assert!(
        !estate.index_path().exists(),
        "with still no index file anywhere"
    );

    // 2. A real rebuild of that same estate: zero rows, and a real file
    // renamed into place all the same.
    window.arm();
    let estate_root = estate.estate.clone();
    let rebuild =
        std::thread::spawn(move || atlas(&estate_root, &["findings", "--admin", "--rebuild"]));
    window.wait_parked();
    assert!(
        estate.index_path().exists(),
        "an empty rebuild renames a real file into place"
    );
    assert_eq!(
        estate.index_rows_on_disk(),
        0,
        "holding no rows, because the estate has no findings"
    );

    fs::remove_file(estate.index_path()).unwrap();
    window.release();
    let (code, _reply, stderr) = rebuild.join().unwrap();
    assert_eq!(code, Some(0), "{stderr}");

    // 3. The read after it: there was a file, and this read has none, so
    // its emptiness is not the known-empty of section 1.
    let admin = estate.admin_index();
    assert_eq!(
        admin["index"]["complete"], false,
        "a rebuild published a file and it is not there now: {}",
        admin["index"]
    );
    assert_eq!(admin["index"]["projection"], "behind", "{}", admin["index"]);
    assert!(
        admin["index"]["pending_rows"].is_null(),
        "unknown, never zero: {}",
        admin["index"]
    );
    let detail = admin["index"]["detail"].as_str().unwrap_or_default();
    assert!(
        detail.contains("was not there when these rows were read"),
        "the administrator is told which fact this is: {detail:?}"
    );
    let (scoped, scoped_stderr) = estate.scoped_index();
    assert_eq!(
        scoped["index"]["complete"], false,
        "and the scoped reader is told the same state: {scoped}"
    );
    assert_discloses_nothing(
        "a scoped read after an empty rebuild's file was deleted",
        &scoped,
        &scoped_stderr,
        &[
            "findings.ndjson".to_string(),
            estate.estate.display().to_string(),
        ],
    );

    // 4. And it clears the way the surface says it clears.
    let (code, _reply, stderr) = estate.rebuild();
    assert_eq!(code, Some(0), "{stderr}");
    let recovered = estate.admin_index();
    assert_eq!(recovered["index"]["complete"], true, "{recovered}");
    assert_eq!(
        recovered["index"]["projection"], "synchronized",
        "{}",
        recovered["index"]
    );
    assert!(
        recovered["rows"].as_array().unwrap().is_empty(),
        "{recovered}"
    );
    estate.stop();
}

/// The rebuild arm that fails **after** its rename: rows visible, the
/// directory entry behind them unconfirmed.
///
/// `rewrite_rows` raises `DurabilityUncertain` only from the `fsync` of
/// the atlas directory, which is the last thing it does and strictly
/// after the atomic rename — so this call, too, published an index file
/// and knows it, and the listing that runs afterwards (denied here by
/// the very mode that makes the `fsync` fail) is in no position to say
/// otherwise. Nothing is injected into the product: a real kernel denial
/// of one syscall on a real directory.
#[test]
fn a_rebuild_whose_directory_sync_failed_still_recorded_the_file_it_renamed() {
    let mut estate = build_estate();
    let kept = estate.raise("the record the journals keep across the whole window");
    let (code, _reply, stderr) = estate.assert_on(&kept, "the assertion that publishes the index");
    assert_eq!(code, Some(0), "{stderr}");

    // Write and execute but not read: the temp file is written and
    // `fsync`ed, the rename lands, and only the directory `fsync` — and
    // the listing that follows it — are denied.
    estate.set_atlas_directory_syncable(false);
    let (code, _reply, stderr) = estate.rebuild();
    assert_ne!(
        code,
        Some(0),
        "an administrator whose rebuild could not confirm its directory entry is told: {stderr}"
    );
    estate.set_atlas_directory_syncable(true);
    assert_eq!(
        estate.index_rows_on_disk(),
        1,
        "the rename landed all the same, which is the whole point of this window"
    );

    // The file that rebuild renamed into place, gone afterwards.
    fs::remove_file(estate.index_path()).unwrap();
    let admin = estate.admin_index();
    assert_eq!(admin["index"]["complete"], false, "{}", admin["index"]);
    let detail = admin["index"]["detail"].as_str().unwrap_or_default();
    assert!(
        detail.contains("was not there when these rows were read"),
        "a rebuild that renamed a file and then failed its directory sync still established \
         that there was one: {detail:?}"
    );
    estate.stop();
}

/// Every canonical journal in the estate, by path and bytes — the thing
/// a read must leave exactly as it found it.
fn journal_state(estate: &Estate) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let works = estate.estate.join("works");
    let Ok(entries) = fs::read_dir(&works) else {
        return out;
    };
    for entry in entries {
        let path = entry.unwrap().path().join("journal.ndjson");
        if let Ok(bytes) = fs::read(&path) {
            out.push((path, bytes));
        }
    }
    out.sort();
    out
}
