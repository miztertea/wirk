//! P4.1 (ruling 0202): wirk's **shared worker contract** — the short,
//! product-shipped operating guidance every actor gets, separate from
//! the assignment it is given.
//!
//! Three facts, three homes, no new framework:
//!
//! * **Content** is `ActorWorld.contract` (`wirk_core::WorkerContractRef`
//!   — version and digest, never the bytes), hashed into `WorldHash::of`,
//!   so the contract a stage operates under is fixed at reservation and
//!   is the same across every attempt that reservation backs.
//! * **Bytes** are `<estate_root>/.wirk/contracts/<digest>.md`, written
//!   durably at reservation before anything references them, with the
//!   temp-file/fsync/rename/directory-fsync discipline
//!   `ProjectionFile::write_new` already uses (R2, not a second
//!   durability protocol). Content-addressed, so the file is immutable
//!   by construction and legitimately shared by every Work reserved
//!   against the same product build.
//! * **Delivery** is `RunLaunched.contract` (`ContractDelivery`) — how
//!   the launch actually got the contract in front of the actor, decided
//!   at launch time and journaled, never hashed.
//!
//! The contract text itself is `include_str!`d (R2, the pattern
//! `claim_hook::WIRK_CLAIM_PLUGIN_JS_TEMPLATE` already uses for a file
//! that must ship with the binary) and **static**: no per-Run
//! substitution. Everything per-Run — the pinned binary path, the
//! required artifact names, the claim guidance — already lives in
//! `run_loop::compose_first_prompt` and stays there, because it is
//! assignment mechanism rather than shared operating doctrine. Being
//! static is what makes the rendered bytes a pure function of the
//! product build, so one digest identifies the source version and the
//! delivered content at once.
//!
//! **Delivery is additive in every mode, and replaces nothing.** claude
//! gets `--append-system-prompt-file`, never `--system-prompt` — and
//! not even that when the launch already carries an append control of
//! its own; opencode gets one more entry in the `instructions` array of
//! the overlay wirk already writes, delivered through opencode's own
//! `instructions` array through whichever of opencode's two
//! per-launch config slots the launch left free, so neither an
//! `OPENCODE_CONFIG` file nor an inherited inline layer is
//! displaced; codex gets
//! `-c developer_instructions=…` **only** when its own dry render *of
//! this launch's own arguments* shows that adding it displaces nothing
//! (§`codex`), and a truthful prompt fallback otherwise. Nothing is ever
//! written into the worktree, into `~/`, or into any file the user or
//! the repository owns.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use wirk_core::{ContractDelivery, ContractDeliveryMode, WorkerContractRef};

/// The contract's shape identifier. Bumped when the text changes in a
/// way a reader should notice; the digest changes on every byte.
pub const WORKER_CONTRACT_VERSION: &str = "wirk.worker-contract/v1";

/// The shipped contract text, embedded at compile time.
pub const WORKER_CONTRACT: &str = include_str!("worker-contract.md");

/// Lowercase hex SHA-256, the same encoding `WorldHash` uses.
fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// This build's own contract digest.
pub fn digest() -> String {
    sha256_hex(WORKER_CONTRACT.as_bytes())
}

/// `<estate_root>/.wirk/contracts/` — estate-owned, beside the `.wirk`
/// directories `claim_hook` and `wirkd::client::locate` already use.
/// Never the worktree (0050's boundary check never sees it) and never
/// `~/`.
pub fn contracts_dir(estate_root: &Path) -> PathBuf {
    estate_root.join(".wirk").join("contracts")
}

/// Where one contract's bytes live. Content-addressed: the name *is*
/// the digest, so the file can never disagree with the reference that
/// names it without the mismatch being detectable.
pub fn contract_path(estate_root: &Path, digest: &str) -> PathBuf {
    contracts_dir(estate_root).join(format!("{digest}.md"))
}

/// Writes this build's contract under `estate_root` if it is not
/// already there, and returns the reference a reservation records.
///
/// Called by wirkd when it reserves an Actor World, **before** the World
/// that references it is built — the same "durable first, then
/// referenced" ordering the delivered projection already has.
///
/// Deliberately not `ProjectionFile::write_new`'s `create_new`: a
/// projection is one observation's own file and a name that exists is a
/// minting bug, while a contract is named by its content and two Works
/// reserved against the same build *should* share one file. So: if the
/// path already holds bytes that hash to this digest, it is already
/// correct and is left alone; otherwise it is written atomically. The
/// durability discipline itself (temp file, fsync, rename, directory
/// fsync) is `write_new`'s, reused rather than reinvented.
pub fn reserve(estate_root: &Path) -> std::io::Result<WorkerContractRef> {
    let digest = digest();
    let path = contract_path(estate_root, &digest);
    if std::fs::read(&path).is_ok_and(|bytes| sha256_hex(&bytes) == digest) {
        return Ok(WorkerContractRef {
            version: WORKER_CONTRACT_VERSION.to_string(),
            digest,
        });
    }

    let dir = contracts_dir(estate_root);
    std::fs::create_dir_all(&dir)?;
    let temp = dir.join(format!(".tmp-{digest}-{}", std::process::id()));
    {
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(WORKER_CONTRACT.as_bytes())?;
        file.sync_all()?;
    }
    std::fs::rename(&temp, &path)?;
    std::fs::File::open(&dir).and_then(|directory| directory.sync_all())?;
    Ok(WorkerContractRef {
        version: WORKER_CONTRACT_VERSION.to_string(),
        digest,
    })
}

/// Why a reserved contract could not be honoured at launch.
///
/// This is a refusal, not a degradation. wirkd is a long-lived daemon
/// and the driver is a separately pinned binary, so the file the World
/// names can legitimately be gone, truncated or rewritten by the time a
/// launch reads it — and an actor operating under bytes nobody reserved
/// is exactly the failure the digest exists to prevent.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContractError {
    #[error(
        "the worker contract this Run was reserved with is not readable at {path}: {reason} — \
         refusing to launch an actor without the contract it was reserved under"
    )]
    Unreadable { path: String, reason: String },
    #[error(
        "the worker contract at {path} hashes to {found}, not the {expected} this Run's World \
         reserved — refusing to launch an actor under bytes nobody reserved"
    )]
    DigestMismatch {
        path: String,
        expected: String,
        found: String,
    },
}

/// Reads the bytes a reserved World names and proves they are those
/// bytes. Nothing downstream of this may assume it was called.
///
/// The check is against the *reserved* digest, not against this
/// binary's own embedded text: the World is what is authoritative about
/// which contract a stage operates under, and a driver built after the
/// reservation must still honour the reservation rather than substitute
/// its own newer text.
pub fn read_verified(
    estate_root: &Path,
    reference: &WorkerContractRef,
) -> Result<(PathBuf, String), ContractError> {
    let path = contract_path(estate_root, &reference.digest);
    let bytes = std::fs::read(&path).map_err(|error| ContractError::Unreadable {
        path: path.display().to_string(),
        reason: error.to_string(),
    })?;
    let found = sha256_hex(&bytes);
    if found != reference.digest {
        return Err(ContractError::DigestMismatch {
            path: path.display().to_string(),
            expected: reference.digest.clone(),
            found,
        });
    }
    let text = String::from_utf8(bytes).map_err(|error| ContractError::Unreadable {
        path: path.display().to_string(),
        reason: error.to_string(),
    })?;
    Ok((path, text))
}

/// Builds the journaled delivery record for one mode.
pub fn delivery(
    reference: &WorkerContractRef,
    mode: ContractDeliveryMode,
    fallback_reason: Option<String>,
) -> ContractDelivery {
    debug_assert_eq!(
        mode == ContractDeliveryMode::Prompt,
        fallback_reason.is_some(),
        "a prompt fallback always discloses its reason, and a native delivery never has one"
    );
    ContractDelivery {
        version: reference.version.clone(),
        digest: reference.digest.clone(),
        mode,
        fallback_reason,
    }
}

/// The disclosure sentence that precedes a prompt-delivered contract.
///
/// 0202: a fallback is truthful or it is not a fallback. The actor is
/// told *that* this is ordinary prompt text rather than harness-native
/// instructions, and why, so it can weigh the contract against its own
/// configuration knowing where it came from.
pub fn prompt_disclosure(kind: &str, reason: &str) -> String {
    format!(
        "The shared Wirk worker contract below is delivered as ordinary prompt text rather \
         than as {kind}'s own native instructions, because {reason}. It is additive: it does \
         not replace this repository's instruction files, your own configuration, or the \
         assignment that follows."
    )
}

// ---- codex: compose or fall back, decided by the harness itself -------
//
// 0202 names the hazard precisely: `-c developer_instructions=<text>`
// **overrides** the key, so a user or profile that already sets
// `developer_instructions` has that value replaced, not extended.
// Measured on this box, 2026-09-12, codex-cli 0.154.0: with
// `developer_instructions = "EXISTING USER RULE"` configured, the
// baseline `codex debug prompt-input` renders that text as the first
// developer item, and the same render with `-c
// developer_instructions="WIRK CONTRACT"` renders *only* the wirk text
// there — the user's rule is gone.
//
// So wirk does not guess, and does not parse anyone's config. It asks
// the harness's own dry renderer what the override would actually do,
// and uses the native mechanism only when the answer is "everything
// that was there is still there, plus the contract". Otherwise the
// contract is delivered as disclosed prompt text and the user's
// `developer_instructions` is left completely untouched.

/// The seam the codex decision is taken through: something that can
/// render codex's model-visible input list for a given working
/// directory **under a given argument list**.
///
/// `args` is the launch's own argument vector, not a private set of
/// extra overrides: a decision taken from any other configuration is a
/// decision about a launch that will never happen (review F1). The
/// baseline render is asked for with the launch's own arguments, and the
/// composed render with those same arguments plus wirk's `-c` element in
/// the position the launch will actually put it — last, where codex's
/// own last-wins resolution can be seen.
///
/// A trait rather than a direct `Command` call so the launch path can be
/// tested deterministically without an installed codex — and so the one
/// real implementation is the only place that knows the command line.
pub trait CodexProbe: Send + Sync {
    /// The rendered developer/user item texts, in order. `Err` when the
    /// render could not be obtained at all — including when the launch's
    /// own arguments are ones this renderer does not accept, which is a
    /// truthful "cannot be shown to compose", never a reason to render
    /// some other configuration instead (review F2).
    fn render(&self, cwd: &Path, args: &[String]) -> Result<Vec<String>, String>;
}

/// The real probe: `codex debug prompt-input`, which renders exactly the
/// model-visible input list this launch would produce. R4 — the
/// harness's own dry-render facility, not a reimplementation of its
/// configuration layering.
#[derive(Debug, Default, Clone, Copy)]
pub struct CodexCliProbe;

impl CodexProbe for CodexCliProbe {
    fn render(&self, cwd: &Path, args: &[String]) -> Result<Vec<String>, String> {
        let mut command = std::process::Command::new("codex");
        command.arg("debug").arg("prompt-input");
        for token in args {
            command.arg(token);
        }
        // A fixed, inert probe prompt: the user item it produces is
        // identical in both renders and so cancels out of the
        // comparison.
        command.arg("wirk contract composition probe");
        command.current_dir(cwd);
        let output = command
            .output()
            .map_err(|error| format!("`codex debug prompt-input` could not be run: {error}"))?;
        if !output.status.success() {
            // codex's own first line of complaint, carried verbatim into
            // the disclosure: an argument this renderer does not accept
            // (`--profile`, `--model`, measured on codex-cli 0.154.0)
            // says so here, and the actor is told which argument stopped
            // the composition from being provable — rather than wirk
            // keeping a list of argument names of its own.
            let complaint = String::from_utf8_lossy(&output.stderr)
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .unwrap_or_default()
                .to_string();
            return Err(if complaint.is_empty() {
                format!("`codex debug prompt-input` exited {}", output.status)
            } else {
                format!(
                    "`codex debug prompt-input` exited {}: {complaint}",
                    output.status
                )
            });
        }
        let parsed: serde_json::Value = serde_json::from_slice(&output.stdout)
            .map_err(|error| format!("`codex debug prompt-input` output is not JSON: {error}"))?;
        let items = parsed
            .as_array()
            .ok_or_else(|| "`codex debug prompt-input` output is not a list".to_string())?;
        let mut texts = Vec::new();
        for item in items {
            let Some(content) = item.get("content").and_then(|c| c.as_array()) else {
                continue;
            };
            for part in content {
                if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                    texts.push(text.to_string());
                }
            }
        }
        Ok(texts)
    }
}

/// What a codex launch should do with the contract, decided by asking
/// codex itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodexComposition {
    /// Safe: the override adds the contract and displaces nothing.
    Additive,
    /// Not safe, or not knowable: deliver by prompt and say why.
    Fallback(String),
}

/// The `-c` override element a codex launch would submit for `text`.
/// One element, `key=value`, value a TOML basic string — which is what
/// makes a multi-line contract survive Herdr's argv path at all (§
/// `toml_basic_string`).
pub fn codex_override(text: &str) -> String {
    format!("developer_instructions={}", toml_basic_string(text))
}

/// Decides whether codex can take the contract natively here.
///
/// Accepts only the one unambiguous answer: the overridden render is the
/// baseline render with **exactly one** item added, and that item is the
/// contract. Every baseline item must still be present, in order and
/// byte-identical — which is precisely the check that catches an
/// already-configured `developer_instructions` being replaced.
///
/// Any other answer — a probe that will not run, output that cannot be
/// compared, an item that disappeared or changed — is a fallback, never
/// a guess and never a silent overwrite.
pub fn codex_composition(
    cwd: &Path,
    text: &str,
    probe: &dyn CodexProbe,
    launch_args: &[String],
) -> CodexComposition {
    let baseline = match probe.render(cwd, launch_args) {
        Ok(items) => items,
        Err(reason) => {
            return CodexComposition::Fallback(format!(
                "codex could not render this launch's own configuration ({reason}), so wirk \
                 cannot show that setting `developer_instructions` here would displace nothing"
            ));
        }
    };
    let mut composed_args = launch_args.to_vec();
    composed_args.push("-c".to_string());
    composed_args.push(codex_override(text));
    let overridden = match probe.render(cwd, &composed_args) {
        Ok(items) => items,
        Err(reason) => {
            return CodexComposition::Fallback(format!(
                "codex could not render this launch's configuration with the contract added \
                 ({reason})"
            ));
        }
    };

    // Exactly the baseline, plus the contract, and nothing else moved.
    let added: Vec<&String> = overridden
        .iter()
        .filter(|item| !baseline.contains(item))
        .collect();
    let lost: Vec<&String> = baseline
        .iter()
        .filter(|item| !overridden.contains(item))
        .collect();
    if !lost.is_empty() {
        return CodexComposition::Fallback(
            "this launch's codex configuration already supplies `developer_instructions`, and \
             setting it again would replace that value rather than add to it"
                .to_string(),
        );
    }
    if overridden.len() != baseline.len() + 1 || added.len() != 1 || added[0].trim() != text.trim()
    {
        return CodexComposition::Fallback(
            "codex's rendered input for this configuration is not the baseline plus the \
             contract, so wirk cannot prove the override composes additively here"
                .to_string(),
        );
    }
    CodexComposition::Additive
}

/// Renders `value` as a TOML basic string — `"…"` with the escapes TOML
/// requires.
///
/// This is the only reason a multi-line contract can reach codex at all.
/// Herdr does not spawn a process: it POSIX-quotes each argv element and
/// **types the line into the pane's interactive shell**, refusing
/// outright (`invalid_agent_argument`, present in the installed herdr
/// 0.9.0 binary) any element containing a control character. A newline
/// is a control character, so the contract must cross as escapes or not
/// at all.
///
/// Hand-written rather than borrowed from `serde_json::to_string`
/// (R3/R6, the same reasoning `claim_hook::shell_quote` gives for not
/// adopting a crate for one function): JSON and TOML disagree on exactly
/// one character that matters here — TOML requires `U+007F` to be
/// escaped and JSON does not — and "nearly the same escaping" is not a
/// property to rely on for the one element whose rejection fails a
/// launch.
pub fn toml_basic_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            // Every other control character, and DEL, which TOML
            // requires escaped and JSON would have passed through raw.
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\u{:04X}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shipped contract must itself cross Herdr's argv path: the
    /// escaped codex element carries no control character, whatever the
    /// text contains.
    #[test]
    fn the_shipped_contract_crosses_the_argv_path_escaped() {
        let element = codex_override(WORKER_CONTRACT);
        assert!(
            !element.chars().any(char::is_control),
            "Herdr refuses an argv element with a control character"
        );
        assert!(
            WORKER_CONTRACT.contains('\n'),
            "this test is only meaningful because the contract is multi-line"
        );
    }

    #[test]
    fn toml_escaping_covers_quotes_backslashes_newlines_and_del() {
        assert_eq!(toml_basic_string("a\"b"), "\"a\\\"b\"");
        assert_eq!(toml_basic_string("a\\b"), "\"a\\\\b\"");
        assert_eq!(toml_basic_string("a\nb"), "\"a\\nb\"");
        assert_eq!(toml_basic_string("a\tb"), "\"a\\tb\"");
        assert_eq!(toml_basic_string("a\u{7f}b"), "\"a\\u007Fb\"");
        // Non-ASCII is legal raw in a TOML basic string and stays raw.
        assert_eq!(toml_basic_string("é✓"), "\"é✓\"");
    }

    struct FixedProbe {
        baseline: Vec<String>,
        overridden: Vec<String>,
        seen: std::sync::Mutex<Vec<Vec<String>>>,
    }

    impl FixedProbe {
        fn new(baseline: Vec<String>, overridden: Vec<String>) -> Self {
            FixedProbe {
                baseline,
                overridden,
                seen: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    impl CodexProbe for FixedProbe {
        fn render(&self, _cwd: &Path, args: &[String]) -> Result<Vec<String>, String> {
            let mut seen = self.seen.lock().unwrap();
            seen.push(args.to_vec());
            // `codex_composition` asks for the baseline first and the
            // composed render second, so call order — not argument
            // sniffing — is what tells the two apart here.
            Ok(if seen.len() == 1 {
                self.baseline.clone()
            } else {
                self.overridden.clone()
            })
        }
    }

    fn items(texts: &[&str]) -> Vec<String> {
        texts.iter().map(|t| (*t).to_string()).collect()
    }

    #[test]
    fn an_empty_developer_instructions_composes_additively() {
        let probe = FixedProbe::new(
            items(&["skills", "permissions", "AGENTS.md"]),
            items(&["CONTRACT", "skills", "permissions", "AGENTS.md"]),
        );
        assert_eq!(
            codex_composition(Path::new("/tmp"), "CONTRACT", &probe, &[]),
            CodexComposition::Additive
        );
    }

    /// The case ruling 0202 names: a nonempty existing value. The
    /// override replaces it, the dry render shows the replacement, and
    /// wirk must refuse the native mechanism rather than silently
    /// discard what the user configured.
    #[test]
    fn an_existing_developer_instructions_value_forces_the_disclosed_fallback() {
        let probe = FixedProbe::new(
            items(&["EXISTING USER RULE", "skills", "AGENTS.md"]),
            items(&["CONTRACT", "skills", "AGENTS.md"]),
        );
        let CodexComposition::Fallback(reason) =
            codex_composition(Path::new("/tmp"), "CONTRACT", &probe, &[])
        else {
            panic!("replacing an existing developer_instructions must not be treated as additive");
        };
        assert!(
            reason.contains("developer_instructions"),
            "the disclosure must name what it protected: {reason}"
        );
    }

    #[test]
    fn a_probe_that_cannot_run_falls_back_rather_than_guessing() {
        struct Broken;
        impl CodexProbe for Broken {
            fn render(&self, _cwd: &Path, _overrides: &[String]) -> Result<Vec<String>, String> {
                Err("no such binary".to_string())
            }
        }
        assert!(matches!(
            codex_composition(Path::new("/tmp"), "CONTRACT", &Broken, &[]),
            CodexComposition::Fallback(_)
        ));
    }

    /// A render that gains the contract *and* something else, or loses
    /// ordering, is not a proven additive composition either.
    #[test]
    fn an_unexplained_extra_item_is_not_a_proven_composition() {
        let probe = FixedProbe::new(
            items(&["skills"]),
            items(&["CONTRACT", "skills", "surprise"]),
        );
        assert!(matches!(
            codex_composition(Path::new("/tmp"), "CONTRACT", &probe, &[]),
            CodexComposition::Fallback(_)
        ));
    }

    /// Review F1. The decision must be taken from **this launch's own**
    /// configuration: the baseline render is asked for with the launch's
    /// arguments, and the composed render with those arguments plus
    /// wirk's element last, where codex's own last-wins resolution is
    /// visible.
    #[test]
    fn the_decision_is_rendered_under_the_launch_s_own_arguments() {
        let launch = vec![
            "-c".to_string(),
            "developer_instructions=\"ROUTE RULE\"".to_string(),
        ];
        let probe = FixedProbe::new(
            items(&["ROUTE RULE", "skills"]),
            // codex resolves the last `-c` for a key: the Route's value
            // is gone from the composed render.
            items(&["CONTRACT", "skills"]),
        );
        let CodexComposition::Fallback(reason) =
            codex_composition(Path::new("/tmp"), "CONTRACT", &probe, &launch)
        else {
            panic!("replacing the launch's own developer_instructions is not additive");
        };
        assert!(reason.contains("developer_instructions"), "{reason}");
        let seen = probe.seen.lock().unwrap();
        assert_eq!(
            seen[0], launch,
            "the baseline is the launch's own configuration"
        );
        assert_eq!(
            seen[1][..2],
            launch[..2],
            "the composed render keeps the launch's arguments ahead of wirk's element"
        );
        assert_eq!(seen[1].len(), 4, "…and appends exactly `-c <override>`");
    }

    /// Review F2. An argument the native renderer will not accept is a
    /// truthful "cannot be shown to compose" — not a reason to render a
    /// different configuration and call the answer additive, and not a
    /// list of argument names wirk maintains.
    #[test]
    fn an_argument_the_renderer_rejects_falls_back_and_carries_its_complaint() {
        struct Picky;
        impl CodexProbe for Picky {
            fn render(&self, _cwd: &Path, args: &[String]) -> Result<Vec<String>, String> {
                if args.iter().any(|arg| arg == "--profile") {
                    return Err(
                        "exit status: 2: error: unexpected argument '--profile' found".to_string(),
                    );
                }
                Ok(vec!["skills".to_string()])
            }
        }
        let launch = vec!["--profile".to_string(), "review".to_string()];
        let CodexComposition::Fallback(reason) =
            codex_composition(Path::new("/tmp"), "CONTRACT", &Picky, &launch)
        else {
            panic!("an unrenderable launch configuration cannot be proven additive");
        };
        assert!(
            reason.contains("--profile"),
            "the disclosure names it: {reason}"
        );
    }

    #[test]
    fn reserving_twice_is_idempotent_and_content_addressed() {
        let estate = tempfile::tempdir().expect("tempdir");
        let first = reserve(estate.path()).expect("reserve");
        let second = reserve(estate.path()).expect("reserve again");
        assert_eq!(first, second);
        assert_eq!(first.digest, digest());
        let path = contract_path(estate.path(), &first.digest);
        assert_eq!(
            std::fs::read_to_string(&path).expect("bytes"),
            WORKER_CONTRACT
        );
        assert_eq!(
            read_verified(estate.path(), &first).expect("verifies").1,
            WORKER_CONTRACT
        );
    }

    #[test]
    fn verification_refuses_corrupt_and_missing_bytes() {
        let estate = tempfile::tempdir().expect("tempdir");
        let reference = reserve(estate.path()).expect("reserve");
        std::fs::write(
            contract_path(estate.path(), &reference.digest),
            "tampered\n",
        )
        .expect("corrupt");
        assert!(matches!(
            read_verified(estate.path(), &reference),
            Err(ContractError::DigestMismatch { .. })
        ));

        let absent = WorkerContractRef {
            version: WORKER_CONTRACT_VERSION.to_string(),
            digest: "0".repeat(64),
        };
        assert!(matches!(
            read_verified(estate.path(), &absent),
            Err(ContractError::Unreadable { .. })
        ));
    }
}
