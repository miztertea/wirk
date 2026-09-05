//! P2.7 Wave 3 (`build-brief.md` §6 item 1): the claude settings
//! file's `Stop` hook *command* actually runs `wirk claim` with the
//! env it is given, the way Claude Code itself would run it — `sh -c
//! <command>` (`type: "command"`'s own documented shape,
//! `reorient.md` §C), fed the Stop hook's stdin JSON exactly as
//! Claude Code sends it (`reorient.md` §C's cited payload shape), with
//! a fake `wirk` on `PATH` (`support/fake_wirk.sh`, reused verbatim
//! from Wave 2, R2) recording argv and env. No live Claude Code process
//! runs here — this is the ceiling of "deterministic" available for
//! the hook command itself (a shell invocation, not a real Claude Code
//! process); the wave's tried step is a real claude actor's own Stop
//! firing it.

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use wirk_herdr::claim_hook;

const FAKE_WIRK_SH: &str = include_str!("support/fake_wirk.sh");

/// The Stop hook's stdin payload, exactly as Claude Code sends it
/// (`reorient.md` §C, Context7 "Inspect Stop Hook Input Payload in
/// JSON"). The hook command here reads none of it (it only calls bare
/// `wirk claim`, which reads the pane's env, not stdin) — fed anyway so
/// this test exercises the real invocation shape, not a stripped-down
/// stand-in.
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

/// Red on `main` (`BUILD.md`'s pasted output): `claim_hook` does not
/// exist as a module name yet (P2.7 W2 shipped `opencode_hook`) and
/// `claude_settings_json`/`write_claude_claim_hook` do not exist at
/// all — this file fails to compile.
#[test]
fn the_stop_hooks_command_runs_wirk_claim_with_the_panes_env() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bin_dir = dir.path().join("bin");
    std::fs::create_dir_all(&bin_dir).expect("bin dir");
    let fake_wirk = bin_dir.join("wirk");
    std::fs::write(&fake_wirk, FAKE_WIRK_SH).expect("write fake wirk");
    set_executable(&fake_wirk);

    // The settings file's own JSON, as `claim_hook::write_claude_claim_hook`
    // writes it — pull the command string out of it the way Claude
    // Code's own hook runner would, rather than hardcoding `"wirk claim"`
    // a second time in this test.
    let settings = claim_hook::claude_settings_json();
    let command = settings["hooks"]["Stop"][0]["hooks"][0]["command"]
        .as_str()
        .expect("Stop hook command is a string")
        .to_string();
    assert_eq!(command, "wirk claim");

    let record = dir.path().join("record.txt");
    let path = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let mut child = Command::new("sh")
        .arg("-c")
        .arg(&command)
        .env("PATH", path)
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
        "the hook command must run exactly `wirk claim`, got: {recorded}"
    );
    assert!(recorded.contains("WIRK_ESTATE_ROOT=/estate/sentinel\n"));
    assert!(recorded.contains("WIRK_WORK_ID=work-sentinel\n"));
    assert!(recorded.contains("WIRK_RUN_ID=run-sentinel\n"));
}

/// The settings file `write_claude_claim_hook` writes on disk names the
/// same bare command, end to end through the real writer (not just
/// `claude_settings_json`'s in-memory value above).
#[test]
fn the_written_settings_file_names_the_same_bare_command() {
    let estate = tempfile::tempdir().expect("estate tempdir");
    let path = claim_hook::write_claude_claim_hook(&estate.path().to_string_lossy(), "run-1")
        .expect("write claude settings");
    let contents = std::fs::read_to_string(&path).expect("read written settings");
    let settings: serde_json::Value =
        serde_json::from_str(&contents).expect("written settings is valid JSON");
    assert_eq!(
        settings["hooks"]["Stop"][0]["hooks"][0]["command"],
        "wirk claim"
    );
}
