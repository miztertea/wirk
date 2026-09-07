use std::fs;
use std::process::Command;

use tempfile::TempDir;
use wirk_atlas::{AtlasError, AtlasStore, ExtractorPolicy, GenerationId};

fn git(dir: &std::path::Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn repo() -> TempDir {
    let repo = TempDir::new().unwrap();
    git(repo.path(), &["init", "-q"]);
    git(repo.path(), &["config", "user.email", "atlas@example.test"]);
    git(repo.path(), &["config", "user.name", "Atlas"]);
    fs::write(repo.path().join("note.md"), "# one\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "one"]);
    repo
}

#[test]
fn child_register_crash() {
    let Some(root) = std::env::var_os("W1_CHILD_CRASH_ROOT") else {
        return;
    };
    let repo = std::env::var_os("W1_CHILD_CRASH_REPO").unwrap();
    let mut atlas = AtlasStore::open(root, "estate").unwrap();
    atlas.register_git("source", repo, "HEAD").unwrap();
}

#[test]
fn interrupted_catalog_file_reopens_to_a_complete_catalog() {
    let repo = repo();
    let estate = TempDir::new().unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("child_register_crash")
        .arg("--nocapture")
        .env("W1_CHILD_CRASH_ROOT", estate.path())
        .env("W1_CHILD_CRASH_REPO", repo.path())
        .env("WIRK_ATLAS_FAILPOINT", "catalog-file-synced")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(86), "{output:?}");
    AtlasStore::open(estate.path(), "estate").expect("crash temp file must be cleaned");
}

#[test]
fn failed_publication_never_becomes_current_in_memory() {
    let repo = repo();
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate").unwrap();
    let membership = atlas.register_git("source", repo.path(), "HEAD").unwrap();
    let old = atlas
        .acquire(&membership, "HEAD", ExtractorPolicy::default())
        .unwrap()
        .staged()
        .unwrap();
    atlas.publish(&membership, &old.id).unwrap();
    git(repo.path(), &["commit", "--allow-empty", "-qm", "two"]);
    let new = atlas
        .acquire(&membership, "HEAD", ExtractorPolicy::default())
        .unwrap()
        .staged()
        .unwrap();
    let atlas_dir = estate.path().join("atlas");
    let original = fs::metadata(&atlas_dir).unwrap().permissions();
    let mut readonly = original.clone();
    readonly.set_readonly(true);
    fs::set_permissions(&atlas_dir, readonly).unwrap();
    let result = atlas.publish(&membership, &new.id);
    fs::set_permissions(&atlas_dir, original).unwrap();
    assert!(
        result.is_err(),
        "fixture must make the real catalog write fail"
    );
    assert_eq!(atlas.current(&membership).unwrap().unwrap().id, old.id);
}

#[test]
fn child_post_rename_sync_error_reconciles_visible_catalog() {
    let Some(root) = std::env::var_os("W1_POST_RENAME_ROOT") else {
        return;
    };
    let repo = std::env::var_os("W1_POST_RENAME_REPO").unwrap();
    let next = GenerationId(std::env::var("W1_POST_RENAME_GENERATION").unwrap());
    let mut atlas = AtlasStore::open(root, "estate").unwrap();
    let member = atlas.register_git("source", repo, "HEAD").unwrap();
    let result = atlas.publish(&member, &next);
    assert!(matches!(result, Err(AtlasError::DurabilityUncertain(_))));
    assert_eq!(atlas.current(&member).unwrap().unwrap().id, next);
}

#[test]
fn post_rename_directory_sync_failure_is_explicit_and_recoverable() {
    let repo = repo();
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate").unwrap();
    let member = atlas.register_git("source", repo.path(), "HEAD").unwrap();
    let old = atlas
        .acquire(&member, "HEAD", ExtractorPolicy::default())
        .unwrap()
        .staged()
        .unwrap();
    atlas.publish(&member, &old.id).unwrap();
    git(repo.path(), &["commit", "--allow-empty", "-qm", "two"]);
    let new = atlas
        .acquire(&member, "HEAD", ExtractorPolicy::default())
        .unwrap()
        .staged()
        .unwrap();
    drop(atlas);

    let shim_dir = TempDir::new().unwrap();
    let shim = shim_dir.path().join("fail_nth_dir_fsync.so");
    let source = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/fail_nth_dir_fsync.c");
    let compiled = Command::new("/usr/bin/cc")
        .args(["-shared", "-fPIC"])
        .arg(&source)
        .arg("-o")
        .arg(&shim)
        .arg("-ldl")
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let output = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("child_post_rename_sync_error_reconciles_visible_catalog")
        .arg("--nocapture")
        .env("LD_PRELOAD", &shim)
        .env("W1_FAIL_DIRECTORY_FSYNC_CALL", "2")
        .env("W1_POST_RENAME_ROOT", estate.path())
        .env("W1_POST_RENAME_REPO", repo.path())
        .env("W1_POST_RENAME_GENERATION", &new.id.0)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    println!(
        "old_generation={} new_generation={} child={}",
        old.id.0,
        new.id.0,
        String::from_utf8_lossy(&output.stdout)
    );
    let reopened = AtlasStore::open(estate.path(), "estate").unwrap();
    assert_eq!(reopened.current(&member).unwrap().unwrap().id, new.id);
}
