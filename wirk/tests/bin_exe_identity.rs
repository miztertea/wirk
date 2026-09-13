//! Source qualification for anything that runs the CLI (ruling 0241).
//!
//! The estate shares one `CARGO_TARGET_DIR` across lanes, and
//! `<target>/debug/wirk` is a **hardlink cargo re-points to whichever of
//! `cargo build` / `cargo test` ran last**. Those two produce different
//! bytes for the same source — a test invocation unifies dev-dependency
//! features into the binary, giving it different `-C metadata` — so the
//! path alone is not an identity.
//!
//! Integration tests are safe, because `CARGO_BIN_EXE_wirk` is built by
//! the same invocation that runs them. What is *not* safe is quoting
//! `debug/wirk`'s digest for a run that happened at some other time. This
//! check prints the exact executable a real-service test in this
//! invocation would spawn, so a report can name it instead of guessing.

use std::path::Path;

#[test]
fn the_cli_real_service_tests_spawn_is_present_and_named() {
    let path = env!("CARGO_BIN_EXE_wirk");
    let binary = Path::new(path);
    assert!(
        binary.is_file(),
        "CARGO_BIN_EXE_wirk does not resolve to a file: {path}"
    );
    let bytes = std::fs::read(binary).expect("the CLI under test must be readable");
    assert!(!bytes.is_empty(), "the CLI under test is empty: {path}");

    use sha2::Digest;
    let digest = sha2::Sha256::digest(&bytes);
    let digest = digest.iter().fold(String::new(), |mut acc, byte| {
        use std::fmt::Write;
        let _ = write!(acc, "{byte:02x}");
        acc
    });
    // Printed, not asserted against a constant: pinning a digest in
    // source would be a lie the moment anything legitimately changes.
    // The point is that a run can *name* what it used.
    println!("wirk-cli-under-test path={path}");
    println!("wirk-cli-under-test sha256={digest}");
    println!("wirk-cli-under-test bytes={}", bytes.len());
}
