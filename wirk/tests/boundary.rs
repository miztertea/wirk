//! Crate-boundary test (D7): pins the allowed internal edge set
//! {herdr->core, atlas->core, wirk->core, wirk->herdr, wirk->atlas} and
//! wirk-core's deny-list (no dependencies at all) by reading the
//! workspace manifests at compile time and checking substrings. See
//! wirk/tests/README or the ruling that added this file for why
//! `include_str!` + `&str::contains` (stdlib, R3) rather than a TOML
//! parser: these manifests are ours, hand-written, and small enough
//! that presence/absence of a dependency name needs no real parsing.

const WIRK_CORE_MANIFEST: &str = include_str!("../../wirk-core/Cargo.toml");
const WIRK_HERDR_MANIFEST: &str = include_str!("../../wirk-herdr/Cargo.toml");
const WIRK_ATLAS_MANIFEST: &str = include_str!("../../wirk-atlas/Cargo.toml");
const WIRK_BIN_MANIFEST: &str = include_str!("../Cargo.toml");

/// D7: wirk-core may depend on nothing at all, internal or external.
/// Stricter than a deny-list of specific (Herdr/socket/RPC-shaped)
/// names: any dependency addition trips it.
#[test]
fn wirk_core_has_no_dependencies() {
    assert!(
        !WIRK_CORE_MANIFEST.contains("[dependencies]"),
        "wirk-core/Cargo.toml must have no [dependencies] table (D7: wirk-core depends on nothing)"
    );
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
