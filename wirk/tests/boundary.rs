//! Crate-boundary test (D7): pins the allowed internal edge set
//! {herdr->core, atlas->core, wirk->core, wirk->herdr, wirk->atlas} and
//! wirk-core's dependency deny-list per 0022 D71 (no Herdr, socket, or
//! RPC shaped dependency, not zero dependencies — narrowed from D7) by
//! reading the workspace manifests at compile time and checking
//! substrings. See wirk/tests/README or the ruling that added this file
//! for why `include_str!` + `&str::contains` (stdlib, R3) rather than a
//! TOML parser: these manifests are ours, hand-written, and small enough
//! that presence/absence of a dependency name needs no real parsing.

const WIRK_CORE_MANIFEST: &str = include_str!("../../wirk-core/Cargo.toml");
const WIRK_HERDR_MANIFEST: &str = include_str!("../../wirk-herdr/Cargo.toml");
const WIRK_ATLAS_MANIFEST: &str = include_str!("../../wirk-atlas/Cargo.toml");
const WIRK_BIN_MANIFEST: &str = include_str!("../Cargo.toml");

/// Names whose presence at the start of a `[dependencies]` line marks a
/// Herdr-, socket-, or RPC-shaped dependency wirk-core may not carry
/// (0022 D71).
const DENIED_DEPENDENCY_PREFIXES: &[&str] = &[
    "herdr",
    "tokio",
    "hyper",
    "tonic",
    "jsonrpc",
    "jsonrpsee",
    "reqwest",
    "tungstenite",
    "ws",
    "zmq",
    "nng",
    "capnp",
    "prost",
    "grpc",
];

/// Line-based scan of the `[dependencies]` table only: from the line
/// that equals `[dependencies]` (trimmed) up to, but not including, the
/// next line whose trimmed form starts with `[` (the next table
/// header). A character-level `find('[')` was tried first and found the
/// wrong `[`: `serde`'s `features = ["derive"]` contains one, which
/// truncated the section right after the first dependency line and left
/// every later line (e.g. `thiserror`, or anything a builder appends)
/// unscanned (w1/VERIFY.md Finding 1). Scanning line by line and
/// matching only a line whose *trimmed* form starts with `[` avoids
/// that: `["derive"]` is never itself a line.
///
/// Within the table, a dependency line is any non-empty, non-comment
/// line with a name before `=`; the name is checked against
/// `DENIED_DEPENDENCY_PREFIXES`. Returns the denied names found, in
/// scan order.
fn denied_dependencies(manifest: &str) -> Vec<String> {
    let mut in_deps = false;
    let mut found = Vec::new();
    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed == "[dependencies]" {
            in_deps = true;
            continue;
        }
        if !in_deps {
            continue;
        }
        if trimmed.starts_with('[') {
            break; // next table header: [dependencies] is over
        }
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((name, _rest)) = trimmed.split_once('=') else {
            continue; // no name=value shape on this line
        };
        let name = name.trim();
        for denied in DENIED_DEPENDENCY_PREFIXES {
            if name.starts_with(denied) {
                found.push(name.to_string());
            }
        }
    }
    found
}

/// 0022 D71: wirk-core may not depend on anything Herdr-, socket-, or
/// RPC-shaped. Narrower than D7's original zero-dependency reading —
/// `serde` and `thiserror` are allowed (W1).
#[test]
fn wirk_core_dependencies_are_not_herdr_shaped() {
    let denied = denied_dependencies(WIRK_CORE_MANIFEST);
    assert!(
        denied.is_empty(),
        "wirk-core/Cargo.toml has denied Herdr/socket/RPC-shaped dependencies: {denied:?} (0022 D71)"
    );
}

#[cfg(test)]
mod denied_dependencies_tests {
    use super::denied_dependencies;

    /// `features = ["derive"]` on the serde line must not be mistaken
    /// for a `[section]` header — a character-level `find('[')` did
    /// exactly that (w1/VERIFY.md Finding 1) and silently stopped
    /// scanning before `tokio`.
    #[test]
    fn scans_past_a_features_array_bracket() {
        let manifest = r#"
[package]
name = "wirk-core"

[dependencies]
serde = { version = "1.0.229", features = ["derive"] }
tokio = "1"
"#;
        assert_eq!(denied_dependencies(manifest), vec!["tokio".to_string()]);
    }

    /// Same shape, with the denied name being `herdr` itself rather
    /// than `tokio` — the name the deny-list exists for.
    #[test]
    fn finds_herdr_after_a_features_array_bracket() {
        let manifest = r#"
[dependencies]
serde = { version = "1.0.229", features = ["derive"] }
herdr = "0"
"#;
        assert_eq!(denied_dependencies(manifest), vec!["herdr".to_string()]);
    }

    /// Only allowed dependencies: nothing reported.
    #[test]
    fn no_denied_names_yields_empty() {
        let manifest = r#"
[dependencies]
serde = { version = "1.0.229", features = ["derive"] }
thiserror = "2.0.20"
"#;
        assert!(denied_dependencies(manifest).is_empty());
    }

    /// The scan is `[dependencies]`-only (per this file's existing
    /// scope, W1): a denied name in a later table such as
    /// `[dev-dependencies]` is not reported.
    #[test]
    fn denied_name_in_a_later_table_is_not_reported() {
        let manifest = r#"
[dependencies]
serde = { version = "1.0.229", features = ["derive"] }

[dev-dependencies]
tokio = "1"
"#;
        assert!(denied_dependencies(manifest).is_empty());
    }
}

/// Allowed edge herdr->core; forbidden edge herdr->atlas.
#[test]
fn wirk_herdr_depends_only_on_wirk_core() {
    assert!(
        WIRK_HERDR_MANIFEST.contains("wirk-core"),
        "wirk-herdr/Cargo.toml must depend on wirk-core"
    );
    assert!(
        !WIRK_HERDR_MANIFEST.contains("wirk-atlas"),
        "wirk-herdr/Cargo.toml must not depend on wirk-atlas"
    );
}

/// Allowed edge atlas->core; forbidden edge atlas->herdr.
#[test]
fn wirk_atlas_depends_only_on_wirk_core() {
    assert!(
        WIRK_ATLAS_MANIFEST.contains("wirk-core"),
        "wirk-atlas/Cargo.toml must depend on wirk-core"
    );
    assert!(
        !WIRK_ATLAS_MANIFEST.contains("wirk-herdr"),
        "wirk-atlas/Cargo.toml must not depend on wirk-herdr"
    );
}

/// Allowed edges wirk->core, wirk->herdr, wirk->atlas: the bin is the
/// only crate that depends on all three of the others.
#[test]
fn wirk_bin_depends_on_all_three_internal_crates() {
    for dep in ["wirk-core", "wirk-herdr", "wirk-atlas"] {
        assert!(
            WIRK_BIN_MANIFEST.contains(dep),
            "wirk/Cargo.toml must depend on {dep}"
        );
    }
}
