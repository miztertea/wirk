//! Live integration test for `wirk work list` and the human `wirk work
//! status` rendering, as an ordinary person actually reads them.
//! Drives the real built binary end to end — a real `wirk wirkd`, real
//! ad hoc deterministic submissions, a real `run-deterministic
//! --executor child` completion, a real `wirk work clean` and a real
//! `wirk artifact read` — no fakes anywhere in this file.
//! Self-contained (no shared `#[path]` fixture module), following this
//! repo's own document-format-tests precedent for test files that do
//! not need the heavier live-Herdr fixtures.

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use wirk::wirkd;
use wirkd::WirkdPointer;

fn wirk_bin() -> &'static str {
    env!("CARGO_BIN_EXE_wirk")
}

/// Every invocation in this file runs the way an operator's shell runs
/// it: with no injected execution triple at all. That is the whole
/// point of the retrieval check below — a person reading a listing has
/// no `WIRK_RUN_ID`, and a command that only works because one was
/// already exported has not been shown to work for them.
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

fn wait_for_wirkd(estate: &Path) -> WirkdPointer {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(pointer) = wirkd::client::locate(estate) {
            return pointer;
        }
        assert!(
            Instant::now() < deadline,
            "wirkd pointer file never appeared (readable) under {}",
            estate.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Same ad hoc deterministic submission shape as
/// `deterministic_run.rs::submit_deterministic`.
fn submit_deterministic(estate: &Path, command: &[&str]) -> (String, String) {
    let output = wirk_cli()
        .args(["work", "submit", "--estate"])
        .arg(estate)
        .args([
            "--repo",
            "demo:write",
            "--base",
            "deadbeef",
            "--kind",
            "deterministic",
            "--command",
        ])
        .args(command)
        .output()
        .expect("work submit runs");
    assert!(
        output.status.success(),
        "work submit failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let words: Vec<&str> = stdout.split_whitespace().collect();
    let mut work_id = String::new();
    let mut run_id = String::new();
    for pair in words.chunks(2) {
        if let [key, value] = pair {
            match *key {
                "work_id" => work_id = (*value).to_string(),
                "run_id" => run_id = (*value).to_string(),
                _ => {}
            }
        }
    }
    assert!(
        !work_id.is_empty() && !run_id.is_empty(),
        "unexpected work submit stdout: {stdout:?}"
    );
    (work_id, run_id)
}

/// `(exit code, stdout, stderr)` of one `wirk` invocation.
fn run_cli(estate: &Path, verb: &[&str], extra: &[&str]) -> (Option<i32>, String, String) {
    let mut command = wirk_cli();
    command.args(verb).arg("--estate").arg(estate).args(extra);
    let output = command.output().expect("wirk runs");
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// An estate with no Work in it says so, on the surface a person reads.
///
/// **Meaningful red before this change**: the human walk printed
/// *nothing at all* and exited 0. That is what the Herdr plugin's
/// `wirkd status` workspace action runs, so choosing it on a freshly
/// configured estate opened a blank pane — indistinguishable from a
/// broken action, a hung daemon, or the estate setting pointing
/// somewhere else entirely. Both facts the empty answer needs are
/// asserted here, because either alone leaves the ambiguity: which
/// estate was walked, and in whose scope.
///
/// The machine surface is deliberately pinned unchanged in the same
/// test: `--json` already answered completely with `[]`, and an
/// empty-state line printed into it would be a parse error, not a
/// courtesy.
#[test]
fn an_estate_with_no_work_says_so_rather_than_printing_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();

    let mut wirkd_child = KillOnDrop(
        wirk_cli()
            .args(["wirkd", "start", "--estate"])
            .arg(&estate)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn wirkd"),
    );
    wait_for_wirkd(&estate);

    for verb in [["wirkd", "status"], ["work", "list"]] {
        let (code, out, err) = run_cli(&estate, &verb, &["--admin"]);
        assert_eq!(code, Some(0), "{verb:?} failed: {err}");
        assert!(
            out.contains(estate.to_str().unwrap()),
            "{verb:?} must name the estate it actually walked:\n{out}"
        );
        assert!(
            out.contains("scope administrative"),
            "{verb:?} must say which scope answered:\n{out}"
        );
        assert!(
            out.contains("no Work is recorded under this estate"),
            "{verb:?} must say the estate is empty, not print nothing:\n{out}"
        );
    }

    // The machine answer is untouched: a complete, parseable empty list.
    let (code, out, err) = run_cli(&estate, &["wirkd", "status"], &["--admin", "--json"]);
    assert_eq!(code, Some(0), "json walk failed: {err}");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(out.trim()).expect("the walk prints JSON"),
        serde_json::json!([]),
        "--json stays exactly the empty array it already was:\n{out}"
    );

    let _ = run_cli(&estate, &["wirkd", "stop"], &[]);
    let _ = wirkd_child.0.wait();
}

/// The whole increment against one real estate: the listing verb a
/// person looks for, what a listing row is worth reading for, what a
/// single-Work read still shows, and — the decisive one — whether a
/// person holding nothing but that output can actually retrieve the
/// validated result it points at.
#[test]
fn a_person_can_list_work_and_retrieve_a_validated_result_from_what_is_printed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();

    let mut wirkd_child = KillOnDrop(
        wirk_cli()
            .args(["wirkd", "start", "--estate"])
            .arg(&estate)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn wirkd"),
    );
    wait_for_wirkd(&estate);

    let (work_a, run_a) = submit_deterministic(&estate, &["sh", "-c", "echo hello > report.md"]);
    let (work_b, _run_b) = submit_deterministic(&estate, &["true"]);

    // Run work_a to a real, Claimed completion with real evidence, and
    // clean it, so its detailed status has real content the listing
    // must still hold back — not merely content that happens to be
    // empty for an unrun Work.
    let (code, _out, err) = run_cli(
        &estate,
        &["run-deterministic"],
        &["--work", &work_a, "--executor", "child"],
    );
    assert_eq!(code, Some(0), "run-deterministic failed: {err}");
    let (code, _out, err) = run_cli(&estate, &["work", "clean"], &["--work", &work_a]);
    assert_eq!(code, Some(0), "wirk work clean failed: {err}");

    // -- the listing verb ---------------------------------------------
    // The question "what Work is there" has a verb of its own now; it
    // is the same estate walk `wirk wirkd status` (no --work) has always
    // been, reached by the name the question has.
    let (code, listing, err) = run_cli(&estate, &["work", "list"], &["--admin"]);
    assert_eq!(code, Some(0), "wirk work list failed: {err}");
    let (_, walk, _) = run_cli(&estate, &["wirkd", "status"], &["--admin"]);
    assert_eq!(
        listing.lines().count(),
        walk.lines().count(),
        "wirk work list is the same estate walk, not a second rendering:\n{listing}\n---\n{walk}"
    );

    let work_id_lines: Vec<&str> = listing
        .lines()
        .filter(|line| line.starts_with("work_id "))
        .collect();
    assert_eq!(
        work_id_lines.len(),
        2,
        "exactly one row per Work in the listing:\n{listing}"
    );
    assert!(
        listing.contains(&format!("work_id {work_a} "))
            && listing.contains(&format!("work_id {work_b} ")),
        "both submitted Works must appear in the listing:\n{listing}"
    );

    // A row is worth reading: stage and state on the first line, the
    // live attempt, its Run and that Run's own state, and whether this
    // Work has a validated result to fetch, on the second. work_a ran
    // to a Claimed completion and produced one available artifact;
    // work_b has never run and has none.
    let row_a = listing
        .lines()
        .skip_while(|line| !line.starts_with(&format!("work_id {work_a} ")))
        .nth(1)
        .unwrap_or_else(|| panic!("no summary line under work_a:\n{listing}"));
    assert!(
        row_a.contains(&format!("run {run_a}")) && row_a.contains("attempt 1"),
        "the row must name the live attempt and its Run: {row_a}"
    );
    assert!(
        row_a.contains("outputs 1/1 available"),
        "the row must say this Work has a validated result to fetch: {row_a}"
    );
    let row_b = listing
        .lines()
        .skip_while(|line| !line.starts_with(&format!("work_id {work_b} ")))
        .nth(1)
        .unwrap_or_else(|| panic!("no summary line under work_b:\n{listing}"));
    assert!(
        row_b.contains("outputs none"),
        "a Work with no validated artifacts must say so, not borrow another's: {row_b}"
    );

    // The detail dump a single-Work read carries — none of it belongs
    // in the listing, even for the Work that has real evidence and a
    // real cleanup entry to show.
    for absent in ["  run run-", "  evidence ", "  clean at", "    read with:"] {
        assert!(
            !listing.contains(absent),
            "the listing must stay concise and not dump {absent:?}:\n{listing}"
        );
    }

    // -- the listing's scope is still resolved, not assumed ------------
    // A requester that is not on the target's lineage is refused for
    // that target and answered for its own: a scoped read of an estate
    // it does not hold never quietly becomes the operator's walk.
    let (code, scoped, err) = run_cli(&estate, &["work", "list"], &["--requesting-work", &work_b]);
    assert_ne!(
        code,
        Some(0),
        "a scoped walk that could not answer for every Work must not exit 0: {scoped} {err}"
    );
    assert!(
        scoped.contains(&format!("work_id {work_b} ")) && !scoped.contains(&work_a),
        "a scoped requester is answered about its own Work and refused the foreign one:\n{scoped}"
    );
    assert!(
        err.contains(&work_a),
        "the refusal for the foreign Work must be reported, not swallowed: {err}"
    );

    // Naming one Work is a different question, and this verb says so
    // rather than handing back the whole estate instead.
    let (code, out, err) = run_cli(&estate, &["work", "list"], &["--admin", "--work", &work_a]);
    assert_eq!(
        code,
        Some(1),
        "wirk work list --work must be refused: {out}"
    );
    assert!(err.contains("wirk work status"), "{err}");

    // -- the single read still carries the detail ---------------------
    let (code, detail, err) = run_cli(
        &estate,
        &["work", "status"],
        &["--admin", "--work", &work_a],
    );
    assert_eq!(code, Some(0), "wirk work status failed: {err}");
    assert!(
        detail.starts_with(&format!("work_id {work_a} state ")),
        "a single-Work read's own first line is unchanged:\n{detail}"
    );
    assert!(
        detail.contains("  run ") && detail.contains("  evidence ") && detail.contains("report.md"),
        "a single-Work read still shows Run detail and this Work's real evidence:\n{detail}"
    );
    // The same summary line the listing carries, on the detailed read
    // too: the attempt and the Run's own state appear nowhere else on
    // this surface, and the per-Run lines below report only checkout
    // and pin presence.
    let detail_summary = detail
        .lines()
        .nth(1)
        .unwrap_or_else(|| panic!("no summary line in:\n{detail}"));
    assert!(
        detail_summary.contains("attempt 1")
            && detail_summary.contains(&format!("run {run_a}"))
            && detail_summary.contains("outputs 1/1 available"),
        "the detailed read carries the same stage/attempt/result summary: {detail_summary}"
    );

    // -- the cleanup time is a real recorded time ---------------------
    // `at` is a number on the wire; reading it as a string reported the
    // recorded cleanup time as unknown on every real entry.
    let clean_line = detail
        .lines()
        .find(|line| line.trim_start().starts_with("clean at"))
        .unwrap_or_else(|| panic!("no clean-at line in:\n{detail}"));
    assert!(
        !clean_line.contains("clean at ?"),
        "the recorded cleanup time must not render as unknown: {clean_line}"
    );
    assert!(
        ["s ago", "m ago", "h ago", "d ago"]
            .iter()
            .any(|unit| clean_line.contains(unit)),
        "the cleanup time must render as an elapsed duration: {clean_line}"
    );

    // -- the decisive one: follow the printed guidance -----------------
    // The retrieval line is copied out of the status output verbatim
    // and handed to a shell that has no execution triple in its
    // environment at all — which is the only situation a person reading
    // a listing is ever in. Ruling 0339: the administrative surface
    // names the Work directly (`--estate`/`--work`/`--admin`) rather
    // than naming a Run at all, so the printed line carries no triple —
    // a bare verb, or one naming a possibly-since-superseded producing
    // Run, would each fail here for reasons this line must not repeat.
    let read_with = detail
        .lines()
        .find_map(|line| line.trim_start().strip_prefix("read with: "))
        .unwrap_or_else(|| panic!("no retrieval guidance in:\n{detail}"));
    assert!(
        read_with.contains("wirk artifact read --estate ")
            && read_with.contains("--admin")
            && read_with.contains("--claim "),
        "the guidance must name the administrative retrieval verb: {read_with}"
    );
    assert!(
        !read_with.contains("WIRK_RUN_ID"),
        "the administrative guidance must name no Run at all: {read_with}"
    );
    let printed = read_with.replace(
        "wirk artifact read",
        &format!("{} artifact read", wirk_bin()),
    );
    let followed = Command::new("sh")
        .arg("-c")
        .arg(&printed)
        .env_remove("WIRK_ESTATE_ROOT")
        .env_remove("WIRK_WORK_ID")
        .env_remove("WIRK_RUN_ID")
        .output()
        .expect("the printed retrieval command runs");
    assert!(
        followed.status.success(),
        "the printed retrieval command must work from the shell it is printed to: {} / {}",
        String::from_utf8_lossy(&followed.stderr),
        printed
    );
    assert_eq!(
        String::from_utf8_lossy(&followed.stdout),
        "hello\n",
        "following the printed guidance must return this Work's own claimed bytes"
    );

    let (code, _out, err) = run_cli(&estate, &["wirkd", "stop"], &[]);
    assert_eq!(code, Some(0), "wirkd stop failed: {err}");
    let exit_status = wirkd_child.0.wait().expect("reap wirkd child");
    assert!(
        exit_status.success(),
        "wirkd did not exit clean: {exit_status:?}"
    );
    let _ = fs::remove_dir_all(&estate);
}
