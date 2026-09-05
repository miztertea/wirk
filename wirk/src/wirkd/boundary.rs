//! The Waypoint boundary's glob matcher (P2.4 W1, `orient/build-brief.md`
//! §8 amendment 1): the predecessor's own way of matching a flat list of
//! globs against a path (`refs/sergeant-rs/src/runtime/atlas/deny.rs`,
//! `GlobSetBuilder`/`GlobBuilder`/`GlobSet::is_match` — cited by the
//! `AcquisitionFilter::new`/`verdict` shape there), applied to
//! `wirk_core::Boundary` instead of a secrets deny-list. Lives in `wirk`,
//! not `wirk-core` (§8: "the check and the I/O" are wirkd's; `Boundary`
//! itself stays a dependency-free data type, `wirk-core/src/lib.rs`
//! unchanged beyond nothing).
//!
//! Unlike the predecessor's `AcquisitionFilter`, nothing here is
//! case-insensitive or defaults a bare pattern to `**/<pattern>` — a
//! Route's boundary globs are authored explicitly relative to the
//! worktree root (`routes.md`), not a secrets floor an operator widens
//! ad hoc, so a pattern means exactly what it says.

use globset::{Glob, GlobSetBuilder};

use wirk_core::Boundary;

/// Whether `path` (worktree-relative, `/`-separated) is inside `boundary`
/// — `true` when any of `boundary`'s globs matches, `false` for an empty
/// boundary (authoring nothing yet is not "matches everything") or a
/// boundary whose every glob fails to compile (a malformed glob is
/// treated as absent, never as "matches everything": the safer of the
/// two readings for an enforcement gate, same direction as the
/// predecessor's own case-insensitivity choice — "only ever widens" run
/// in reverse, `deny.rs`'s module doc "Case is not a way through the
/// floor").
pub fn allows(boundary: &Boundary, path: &str) -> bool {
    let mut builder = GlobSetBuilder::new();
    for pattern in &boundary.0 {
        if let Ok(glob) = Glob::new(pattern) {
            builder.add(glob);
        }
    }
    match builder.build() {
        Ok(set) => set.is_match(path),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundary_allows_matches_star_star_prefix_and_suffix_globs() {
        let star_star = Boundary(vec!["**".to_string()]);
        assert!(allows(&star_star, "anything/at/all.rs"));
        assert!(allows(&star_star, "top.txt"));

        let prefix = Boundary(vec!["src/**".to_string()]);
        assert!(allows(&prefix, "src/a/b.rs"));
        assert!(allows(&prefix, "src/main.rs"));
        assert!(!allows(&prefix, "docs/x.md"));

        // `*` crosses `/` under globset's default (non-`literal_separator`)
        // compilation, the same default the predecessor's own
        // `GlobBuilder::new(pattern).case_insensitive(true).build()` uses
        // (`deny.rs`, no `.literal_separator(true)` there either) — a bare
        // `*.md` matches by extension at any depth, not only at the
        // worktree root.
        let suffix = Boundary(vec!["*.md".to_string()]);
        assert!(allows(&suffix, "readme.md"));
        assert!(allows(&suffix, "docs/readme.md"));
        assert!(!allows(&suffix, "readme.txt"));
    }

    #[test]
    fn boundary_allows_is_false_for_an_empty_boundary() {
        let empty = Boundary(Vec::new());
        assert!(!allows(&empty, "anything.rs"));
    }
}
