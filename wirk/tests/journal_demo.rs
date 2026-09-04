//! Integration test for `wirk journal demo` (item 2, ruling 0028 D93's
//! tried step is the verifier's own run of this binary; this test is
//! the decisive check the BRIEF names: "runs the binary... on a
//! tempdir twice and asserts the second run prints state completed and
//! events 6", plus the `--pause-after` kill/replay case). Runs the
//! real `CARGO_BIN_EXE_wirk` binary via `std::process::Command` — no
//! library call, no sleep used as a wait: the pause case polls a file
//! (the journal itself) rather than guessing a duration (build-brief.md
//! §8 amendment 2, issue 359).

use std::fs;
use std::io::Write as _;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn wirk_bin() -> &'static str {
    env!("CARGO_BIN_EXE_wirk")
}

/// Two plain invocations on an empty tempdir: the first appends the
/// six-event lifecycle, the second replays and folds it. Asserts the
/// second run's stdout names `state completed` and `events 6` — the
/// literal words the BRIEF's outcome names.
#[test]
fn replay_after_full_run_reports_completed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let dir_path = dir.path();

    let first = Command::new(wirk_bin())
        .args(["journal", "demo"])
        .arg(dir_path)
        .output()
        .expect("first invocation runs");
    assert!(
        first.status.success(),
        "first invocation failed: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first_stdout = String::from_utf8_lossy(&first.stdout);
    // One line per appended event (BRIEF outcome).
    assert_eq!(
        first_stdout.lines().count(),
        6,
        "expected 6 appended-event lines, got:\n{first_stdout}"
    );

    let second = Command::new(wirk_bin())
        .args(["journal", "demo"])
        .arg(dir_path)
        .output()
        .expect("second invocation runs");
    assert!(
        second.status.success(),
        "second invocation failed: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    let second_stdout = String::from_utf8_lossy(&second.stdout);
    assert!(
        second_stdout.contains("state completed"),
        "expected 'state completed' in:\n{second_stdout}"
    );
    assert!(
        second_stdout.contains("events 6"),
        "expected 'events 6' in:\n{second_stdout}"
    );
}

/// `--pause-after 3` in the background, killed with `SIGKILL` once the
/// journal file has exactly 3 lines (a bounded poll on the file the
/// process itself is writing, not a fixed sleep), then a plain replay
/// invocation asserts exactly 3 events landed and the Work is not
/// `Completed` — a partial lifecycle, not the full one.
#[test]
fn pause_after_then_kill_leaves_a_partial_journal() {
    let dir = tempfile::tempdir().expect("tempdir");
    let dir_path = dir.path();
    let journal_path = dir_path.join("journal.ndjson");

    let mut child = Command::new(wirk_bin())
        .args(["journal", "demo"])
        .arg(dir_path)
        .args(["--pause-after", "3"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn paused invocation");

    // Bounded poll: wait until the journal file has exactly 3 lines,
    // never a fixed-duration sleep standing in for that condition.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let line_count = fs::read_to_string(&journal_path)
            .map(|contents| contents.lines().count())
            .unwrap_or(0);
        if line_count == 3 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "journal never reached 3 lines within the bound (last count {line_count})"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    child.kill().expect("SIGKILL the paused process");
    child.wait().expect("reap the killed process");

    // No `continue` signal file is written: the process never reaches
    // "appends the rest" — that is the point of the kill.
    assert!(!dir_path.join("continue").exists());

    let replay = Command::new(wirk_bin())
        .args(["journal", "demo"])
        .arg(dir_path)
        .output()
        .expect("replay invocation runs");
    assert!(
        replay.status.success(),
        "replay after kill failed: {}",
        String::from_utf8_lossy(&replay.stderr)
    );
    let replay_stdout = String::from_utf8_lossy(&replay.stdout);
    assert!(
        replay_stdout.contains("events 3"),
        "expected 'events 3' in:\n{replay_stdout}"
    );
    assert!(
        !replay_stdout.contains("state completed"),
        "expected a non-completed state in:\n{replay_stdout}"
    );
}

/// Bad args print usage to stderr and exit 1, the claim stub's shape
/// (BRIEF outcome).
#[test]
fn bad_args_print_usage_and_exit_one() {
    let output = Command::new(wirk_bin())
        .args(["journal", "nonsense"])
        .output()
        .expect("bad-args invocation runs");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("usage: wirk journal demo"),
        "expected a usage line, got: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A malformed journal (a hand-mangled line, same shape `wirk-core/
/// tests/journal.rs`'s corruption test builds) prints the
/// `JournalError` to stderr and exits 2, never a silent success.
#[test]
fn malformed_journal_exits_two() {
    let dir = tempfile::tempdir().expect("tempdir");
    let dir_path = dir.path();
    fs::create_dir_all(dir_path).expect("dir exists");
    let mut file = fs::File::create(dir_path.join("journal.ndjson")).expect("create journal file");
    writeln!(file, "not json").expect("write malformed line");
    drop(file);

    let output = Command::new(wirk_bin())
        .args(["journal", "demo"])
        .arg(dir_path)
        .output()
        .expect("invocation on a malformed journal runs");
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("malformed line"),
        "expected the JournalError's own message, got: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
