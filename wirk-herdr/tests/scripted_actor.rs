//! The scripted actor's own contract, pinned against real Herdr with no
//! wirkd/`RunLoop` in the path (P2.5 W1, ruling 0049 D148,
//! `orient/scripted-actor.md` §4 item 5). Proves the thing the whole
//! item's design leans on: Herdr's `agent.start{kind:"opencode"}`
//! accepts this program as started on a pane whose `PATH` holds it
//! (Herdr has no generic/arbitrary-program agent kind — the lever is
//! naming the program to match an already-recognized label and putting
//! its directory ahead of any real `opencode` on the pane's `PATH`),
//! and that its `pane.report_agent` calls flow to
//! `pane.agent_status_changed` in the exact sequence its script
//! predicts. If Herdr refuses to treat it as started, this test is
//! where that refusal shows up first.

#[path = "support/live_herdr.rs"]
mod live_herdr;
#[path = "support/scripted_actor.rs"]
mod scripted_actor;

use std::fs;

use tempfile::tempdir;

use wirk_herdr::{
    AgentStatus as WireAgentStatus, CreateWorkspace, EventSubscription, HerdrClient, HerdrEvent,
    PromptAgent, SplitDirection, SplitPane, StartAgent,
};

/// Reads the next event off `events`, asserting it decoded cleanly and
/// is a `PaneAgentStatusChanged` for `pane_id` carrying `expected` —
/// naming what was actually seen on a mismatch (this test's own
/// termination bound is the subscription's blocking read itself: a
/// pane that never reaches the expected state hangs the test, exactly
/// as the module doc says a wedged actor should — nothing here retries
/// or times out on top of it).
fn expect_status(
    events: &mut dyn Iterator<Item = Result<HerdrEvent, wirk_herdr::HerdrError>>,
    pane_id: &str,
    expected: WireAgentStatus,
) {
    let event = events
        .next()
        .expect("subscription ended before the expected status arrived")
        .expect("pushed line was not a well-formed HerdrEvent");
    match event {
        HerdrEvent::PaneAgentStatusChanged {
            pane_id: got_pane,
            agent_status,
            ..
        } => {
            assert_eq!(got_pane, pane_id, "status change for the wrong pane");
            assert_eq!(
                agent_status, expected,
                "expected {expected:?}, got {agent_status:?}"
            );
        }
        other => panic!("expected PaneAgentStatusChanged({expected:?}), got {other:?}"),
    }
}

#[test]
fn scripted_actor_reports_idle_working_blocked_over_report_agent() {
    let scripted = scripted_actor::ScriptedActor::install(&[
        "edit:report.md:hello from the scripted actor",
        "block",
    ]);
    let path = format!(
        "{}:{}",
        scripted.bin_dir().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let script_path = scripted.script_path();
    let script_path = script_path.to_str().expect("script path is utf-8");

    let Some(session) = live_herdr::LiveHerdrSession::start_with_env(
        "scripted_actor_reports_idle_working_blocked_over_report_agent",
        &[
            ("PATH", &path),
            ("WIRK_SCRIPTED_ACTOR_SCRIPT", script_path),
            // `pane_shell` reads `SHELL` from the launching process's
            // own env (`refs/herdr/src/pane.rs::pane_shell_from`) when
            // no shell is configured; an interactive `bash` sources
            // `~/.bashrc` regardless of login status, and this box's
            // own `.bashrc` re-prepends a real `opencode`'s directory
            // ahead of anything set here (measured live, `w1/BUILD.md`)
            // — `/bin/sh` reads no such file for a non-login shell, so
            // the session's own `PATH` prepend actually wins.
            ("SHELL", "/bin/sh"),
        ],
    ) else {
        return;
    };
    let client = session.client();

    let cwd = tempdir().expect("pane cwd tempdir");
    let ws = client
        .create_workspace(CreateWorkspace {
            cwd: cwd.path().to_path_buf(),
            env: Default::default(),
            label: Some("wirk-scripted-actor-contract".to_string()),
        })
        .expect("workspace.create");
    let pane = client
        .split_pane(SplitPane {
            workspace_id: Some(ws.workspace_id.clone()),
            target_pane_id: None,
            direction: SplitDirection::Down,
            cwd: cwd.path().to_path_buf(),
            env: Default::default(),
        })
        .expect("pane.split");

    // Subscribe before `agent.start` (D51's ordering, matching
    // `HerdrExecutor::launch_actor`) so the boot `idle` report is never
    // missed.
    let mut events = client
        .subscribe(vec![EventSubscription::PaneAgentStatusChanged {
            pane_id: pane.pane_id.clone(),
        }])
        .expect("events.subscribe");

    let agent_name = "wirk-scripted-actor-contract";
    client
        .start_agent(StartAgent {
            pane_id: pane.pane_id.clone(),
            kind: "opencode".to_string(),
            name: agent_name.to_string(),
            args: vec![],
            timeout_ms: None,
        })
        .expect(
            "agent.start{kind:\"opencode\"} on a pane whose PATH holds the scripted actor must \
             succeed — a refusal here is the finding this test exists to surface, not a \
             workaround",
        );

    // Boot: the program reports idle before reading any prompt.
    expect_status(&mut events, &pane.pane_id, WireAgentStatus::Idle);

    // One turn: `edit:report.md:...` — working, then idle.
    client
        .prompt_agent(PromptAgent {
            target: agent_name.to_string(),
            text: "go".to_string(),
        })
        .expect("agent.prompt");
    expect_status(&mut events, &pane.pane_id, WireAgentStatus::Working);
    expect_status(&mut events, &pane.pane_id, WireAgentStatus::Idle);

    let written = fs::read_to_string(cwd.path().join("report.md"))
        .expect("the edit step's file exists in cwd");
    assert_eq!(written, "hello from the scripted actor");

    // The next turn: `block` — reports blocked directly, with nothing
    // in this program itself able to release it (row 15 is the
    // human/test's job); this test never sends a release, so it never
    // asserts past this point.
    client
        .prompt_agent(PromptAgent {
            target: agent_name.to_string(),
            text: "continue".to_string(),
        })
        .expect("agent.prompt");
    expect_status(&mut events, &pane.pane_id, WireAgentStatus::Blocked);
}
