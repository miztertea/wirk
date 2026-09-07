//! P2.7 Wave 3 (`build-brief.md` §6 item 1): the claude settings
//! file's `Stop` hook *command* actually runs the driver's own binary
//! by absolute path with the env it is given, the way Claude Code
//! itself would run it — `sh -c <command>` (`type: "command"`'s own
//! documented shape, `reorient.md` §C), fed the Stop hook's stdin JSON
//! exactly as Claude Code sends it (`reorient.md` §C's cited payload
//! shape), with a fake driver binary (`support/fake_wirk.sh`, reused
//! verbatim from Wave 2, R2) recording argv and env. No live Claude
//! Code process runs here — this is the ceiling of "deterministic"
//! available for the hook command itself (a shell invocation, not a
//! real Claude Code process); the wave's tried step is a real claude
//! actor's own Stop firing it.
//!
//! Rule 4 (`native-progress-contract-use/HANDOFF.md` §1.4): the fake
//! driver is deliberately named `wirk-renamed-probe`, not `wirk`, and
//! lives under a directory whose name carries a space, a `$`, and a
//! `'` — the shell metacharacters `shell_quote` exists for — and is
//! never put on `PATH`. The command must still resolve and run it.

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use wirk_herdr::claim_hook;

const FAKE_WIRK_SH: &str = include_str!("support/fake_wirk.sh");

/// The Stop hook's stdin payload, exactly as Claude Code sends it
/// (`reorient.md` §C, Context7 "Inspect Stop Hook Input Payload in
/// JSON"). The hook command here reads none of it (it only calls the
/// driver's own binary with `claim`, which reads the pane's env, not
/// stdin) — fed anyway so this test exercises the real invocation
/// shape, not a stripped-down stand-in.
const STOP_HOOK_STDIN: &str = r#"{
  "session_id": "abc123",
  "transcript_path": "~/.claude/projects/example/abc123.jsonl",
  "cwd": "/work/worktree",
  "permission_mode": "default",
  "hook_event_name": "Stop",
  "stop_hook_active": false,
  "last_assistant_message": "done",
  "background_tasks": [],
  "session_crons": []
}"#;

#[cfg(unix)]
fn set_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)
        .expect("stat fake wirk")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).expect("chmod fake wirk");
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) {}

/// A fake driver's absolute path under a directory whose name is
/// itself a shell-quoting stress case: a space, a `$`, and a `'`.
/// Never placed on `PATH`.
fn fake_driver(dir: &Path) -> std::path::PathBuf {
    let bin_dir = dir.join("a dir with $pecial 'chars'");
    std::fs::create_dir_all(&bin_dir).expect("bin dir");
    let fake_wirk = bin_dir.join("wirk-renamed-probe");
    std::fs::write(&fake_wirk, FAKE_WIRK_SH).expect("write fake wirk");
    set_executable(&fake_wirk);
    fake_wirk
}

/// Red before Rule 4 (`native-progress-contract-use/HANDOFF.md` §1.4):
/// the old bare `wirk claim` command could not resolve this renamed,
/// off-`PATH` fake driver at all — `sh -c` would exit `127`. Green: the
/// shell-quoted absolute path resolves and runs it regardless of its
/// name, `PATH`, or the space/`$`/`'` in its directory.
#[test]
fn the_stop_hooks_command_runs_the_drivers_own_binary_with_the_panes_env() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake_wirk = fake_driver(dir.path());

    // The settings file's own JSON, as `claim_hook::write_claude_claim_hook`
    // writes it — pull the command string out of it the way Claude
    // Code's own hook runner would, rather than hardcoding it a second
    // time in this test.
    let settings = claim_hook::claude_settings_json(&fake_wirk);
    let command = settings["hooks"]["Stop"][0]["hooks"][0]["command"]
        .as_str()
        .expect("Stop hook command is a string")
        .to_string();
    assert_ne!(command, "wirk claim");
    assert!(command.ends_with(" claim"), "{command:?}");

    let record = dir.path().join("record.txt");

    let mut child = Command::new("sh")
        .arg("-c")
        .arg(&command)
        // Deliberately not adding the fake driver's directory to
        // `PATH` at all — resolution must come from the absolute path
        // in `command` itself, matching a pane whose driver binary is
        // preserved or renamed and whose directory (D151) names no
        // file called `wirk`.
        .env("WIRK_FAKE_RECORD", &record)
        .env("WIRK_ESTATE_ROOT", "/estate/sentinel")
        .env("WIRK_WORK_ID", "work-sentinel")
        .env("WIRK_RUN_ID", "run-sentinel")
        .stdin(Stdio::piped())
        .spawn()
        .expect("spawn sh -c <command>");
    child
        .stdin
        .as_mut()
        .expect("child stdin")
        .write_all(STOP_HOOK_STDIN.as_bytes())
        .expect("write stop hook stdin");
    let status = child.wait().expect("wait for sh");
    assert!(status.success(), "sh -c {command:?} exited non-zero");

    let recorded = std::fs::read_to_string(&record).expect("fake wirk must have run and recorded");
    assert!(
        recorded.starts_with("ARGV:claim\n"),
        "the hook command must run the driver's own binary with exactly `claim`, got: {recorded}"
    );
    assert!(recorded.contains("WIRK_ESTATE_ROOT=/estate/sentinel\n"));
    assert!(recorded.contains("WIRK_WORK_ID=work-sentinel\n"));
    assert!(recorded.contains("WIRK_RUN_ID=run-sentinel\n"));
}

/// The settings file `write_claude_claim_hook` writes on disk names the
/// same driver-binary command, end to end through the real writer (not
/// just `claude_settings_json`'s in-memory value above) — and, run the
/// same way through `sh -c`, it still resolves and runs the renamed,
/// off-`PATH` fake driver.
#[test]
fn the_written_settings_file_names_the_same_absolute_driver_command() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake_wirk = fake_driver(dir.path());
    let estate = dir.path().join("estate");

    let path = claim_hook::write_claude_claim_hook(&estate.to_string_lossy(), "run-1", &fake_wirk)
        .expect("write claude settings");
    let contents = std::fs::read_to_string(&path).expect("read written settings");
    let settings: serde_json::Value =
        serde_json::from_str(&contents).expect("written settings is valid JSON");
    let command = settings["hooks"]["Stop"][0]["hooks"][0]["command"]
        .as_str()
        .expect("command is a string")
        .to_string();
    let in_memory = claim_hook::claude_settings_json(&fake_wirk);
    let in_memory_command = in_memory["hooks"]["Stop"][0]["hooks"][0]["command"]
        .as_str()
        .expect("command is a string");
    assert_eq!(command, in_memory_command);

    let record = dir.path().join("record.txt");
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(&command)
        .env("WIRK_FAKE_RECORD", &record)
        .stdin(Stdio::piped())
        .spawn()
        .expect("spawn sh -c <command>");
    child
        .stdin
        .as_mut()
        .expect("child stdin")
        .write_all(STOP_HOOK_STDIN.as_bytes())
        .expect("write stop hook stdin");
    let status = child.wait().expect("wait for sh");
    assert!(status.success(), "sh -c {command:?} exited non-zero");
    let recorded = std::fs::read_to_string(&record).expect("fake wirk must have run and recorded");
    assert!(recorded.starts_with("ARGV:claim\n"), "got: {recorded}");
}
