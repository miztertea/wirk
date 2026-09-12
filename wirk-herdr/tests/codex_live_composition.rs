//! The codex composition decision, taken against the **installed**
//! `codex debug prompt-input` rather than a stand-in (review F1/F2).
//!
//! `#[ignore]` by default: these need codex on `PATH`, so the ordinary
//! suite stays deterministic and offline. Run them explicitly:
//!
//! ```text
//! cargo test -p wirk-herdr --test codex_live_composition -- --ignored --nocapture
//! ```
//!
//! No model call is made and no session is created: `codex debug
//! prompt-input` renders the model-visible input list and exits. Nothing
//! here writes to the owner's `~/.codex`, and `CODEX_HOME` is not set,
//! read or repurposed — every configuration under test is expressed as
//! the launch's own arguments, which is exactly what the corrected
//! decision now renders under.

use std::path::Path;

use tempfile::tempdir;
use wirk_herdr::worker_contract::{CodexCliProbe, CodexComposition, codex_composition};

const CONTRACT: &str = "# Wirk shared worker contract, probe\n\nA \"quoted\" line.\n";

/// A launch whose arguments the renderer can carry, with no
/// `developer_instructions` of its own: the native path must still
/// engage. This is the clean-composition case the increment exists for,
/// and the correction must not have cost it.
#[test]
#[ignore = "requires the installed codex CLI"]
fn a_clean_launch_still_composes_natively() {
    let cwd = tempdir().expect("tempdir");
    let args = vec![
        "-c".to_string(),
        "model_reasoning_effort=\"high\"".to_string(),
    ];
    assert_eq!(
        codex_composition(cwd.path(), CONTRACT, &CodexCliProbe, &args),
        CodexComposition::Additive,
        "codex's own render says the override adds the contract and displaces nothing"
    );
}

/// Review F1 against the real binary: the launch itself supplies
/// `developer_instructions`, codex resolves the last one, and the
/// decision — now rendered under those same arguments — must refuse the
/// native mechanism.
#[test]
#[ignore = "requires the installed codex CLI"]
fn a_developer_instructions_argument_in_the_launch_forces_the_fallback() {
    let cwd = tempdir().expect("tempdir");
    let args = vec![
        "-c".to_string(),
        "developer_instructions=\"EXISTING ROUTE RULE\"".to_string(),
    ];
    let CodexComposition::Fallback(reason) =
        codex_composition(cwd.path(), CONTRACT, &CodexCliProbe, &args)
    else {
        panic!("codex resolves the last -c for a key: this is a replacement, not an addition");
    };
    assert!(reason.contains("developer_instructions"), "{reason}");
}

/// Review F2 against the real binary: `--profile` is an interactive-only
/// argument the renderer refuses outright, so the composition cannot be
/// shown and the disclosure carries codex's own words.
#[test]
#[ignore = "requires the installed codex CLI"]
fn an_interactive_only_argument_is_disclosed_rather_than_rendered_away() {
    let cwd = tempdir().expect("tempdir");
    // Both are arguments only the interactive CLI accepts. `--model` is
    // the consequential one: wirk emits it for any Route that requests a
    // model, so such a codex launch now takes the disclosed fallback.
    for argument in ["--profile", "--model"] {
        let args = vec![argument.to_string(), "review".to_string()];
        let CodexComposition::Fallback(reason) =
            codex_composition(cwd.path(), CONTRACT, &CodexCliProbe, &args)
        else {
            panic!("a configuration the renderer will not accept cannot be proven additive");
        };
        assert!(
            reason.contains(argument),
            "the disclosure must carry codex's own complaint: {reason}"
        );
    }
}

/// The probe's own inputs are this launch's arguments and nothing else —
/// the property F1 turned on. Rendered twice against the real binary to
/// show the answer is stable.
#[test]
#[ignore = "requires the installed codex CLI"]
fn the_live_render_is_deterministic_for_one_configuration() {
    use wirk_herdr::worker_contract::CodexProbe;
    let cwd = tempdir().expect("tempdir");
    let args: Vec<String> = Vec::new();
    let first = CodexCliProbe.render(cwd.path(), &args).expect("render");
    let second = CodexCliProbe.render(cwd.path(), &args).expect("render");
    assert_eq!(first, second, "one configuration, one answer");
    assert!(
        !first.is_empty(),
        "codex renders at least its own developer items"
    );
    let _ = Path::new(".");
}
