//! Ruling 0205, against a **real Herdr server**: the probe wirk now uses
//! really does read the configuration the actor's pane will actually
//! have, including a value carried only in the server's own environment
//! — the value the driver's `std::env::var` could never see.
//!
//! `#[ignore]`, like `opencode_live_layering.rs` and
//! `codex_live_composition.rs`, so the ordinary suite stays offline. Run
//! against a **throwaway** session, never the estate's own coordinator:
//!
//! ```sh
//! OPENCODE_CONFIG_CONTENT='{"instructions":["/tmp/server.md"],"small_model":"probe/server"}' \
//!   nohup herdr --session wirk-pane-env-live server &
//! WIRK_LIVE_HERDR_SOCKET=~/.config/herdr/sessions/wirk-pane-env-live/herdr.sock \
//!   cargo test -p wirk-herdr --test opencode_pane_env_live -- --ignored
//! herdr session stop wirk-pane-env-live
//! ```
//!
//! Nothing is started in the panes but wirk's own probe script: no
//! agent, no model, no inference. The workspace this test creates is
//! closed again on the way out.

use std::collections::BTreeMap;

use tempfile::tempdir;
use wirk_herdr::claim_hook::{
    OPENCODE_CONFIG_CONTENT_ENV, OpencodeDelivery, OpencodeOverlay, opencode_delivery,
    write_opencode_env_probe,
};
use wirk_herdr::socket::SocketClient;
use wirk_herdr::{CloseWorkspace, CreateWorkspace, HerdrClient, SplitDirection, SplitPane};

#[test]
#[ignore = "needs a throwaway Herdr server carrying OPENCODE_CONFIG_CONTENT; see this file's docs"]
fn the_probe_reads_a_layer_carried_only_by_the_herdr_server() {
    let socket = std::env::var("WIRK_LIVE_HERDR_SOCKET")
        .expect("WIRK_LIVE_HERDR_SOCKET names the throwaway session's socket");
    let client = SocketClient::connect(std::path::PathBuf::from(socket)).expect("herdr is running");

    let estate = tempdir().expect("estate tempdir");
    let probe = write_opencode_env_probe(&estate.path().to_string_lossy(), "run-live")
        .expect("probe script written");
    probe.clear();

    let workspace = client
        .create_workspace(CreateWorkspace {
            cwd: estate.path().to_path_buf(),
            env: BTreeMap::new(),
            label: Some("wirk-pane-env-live".to_string()),
        })
        .expect("workspace created");

    // Exactly what `actor_pane` does: a pane with **no environment of
    // its own**, so what it reports is what this placement inherits.
    let pane = client
        .split_pane(SplitPane {
            workspace_id: Some(workspace.workspace_id.clone()),
            target_pane_id: None,
            direction: SplitDirection::Down,
            cwd: estate.path().to_path_buf(),
            env: BTreeMap::new(),
        })
        .expect("probe pane created");

    let command = probe.command();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while std::time::Instant::now() < deadline && !probe.done.exists() {
        client
            .send_input(&pane.pane_id, &command)
            .expect("probe command sent");
        let attempt = std::time::Instant::now() + std::time::Duration::from_millis(750);
        while std::time::Instant::now() < attempt && !probe.done.exists() {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
    let answered = probe.done.exists();
    let answer = if answered { probe.read().ok() } else { None };
    let _ = client.close_pane(&pane.pane_id);
    let _ = client.close_workspace(CloseWorkspace {
        workspace_id: workspace.workspace_id.clone(),
    });

    assert!(answered, "the probe pane answered within the deadline");
    let (content, config) = answer.expect("the probe's two values are readable");
    assert!(
        content.contains("\"instructions\""),
        "the probe must see the layer the *server* carries — the one this driver's own \
         environment does not have: {content:?}"
    );

    // And that answer is what decides the slot: the inline one is
    // taken, so wirk's overlay goes in the free file slot rather than
    // replacing what the server carried.
    let overlay = OpencodeOverlay {
        config_path: estate.path().join("wirk-opencode-config.json"),
        config_content: r#"{"plugin":["/estate/wirk-claim.js"]}"#.to_string(),
    };
    match opencode_delivery(&overlay, Some(content.as_str()), Some(config.as_str())) {
        OpencodeDelivery::ConfigFile(path) => assert_eq!(path, overlay.config_path),
        other => panic!(
            "a server-carried {OPENCODE_CONFIG_CONTENT_ENV} must push wirk to the free file \
             slot, got {other:?}"
        ),
    }
}
