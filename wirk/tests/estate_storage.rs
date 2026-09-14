//! P4.5 increment A (ruling 0256): what this estate owns, what still
//! needs it, and what an explicit cleanup may remove.
//!
//! Real daemon, real `git`, real `wirk work submit`, real `wirk atlas`,
//! real CLI — never a library call for anything the CLI exposes, the
//! discipline `job_authority.rs` and `work_clean.rs` already keep.
//!
//! **What these pin, and what they do not.** Each check below is a
//! contract: the arithmetic that must not double-charge a shared inode,
//! the retention rules that decide what is a candidate, the guards that
//! refuse, and the disclosure a scoped caller gets. What they are not is
//! the actual-use qualification: a real offline Semble edition, a real
//! materialized checkout, a real retained Claim and a real
//! `world expand` after a removal are executed through the same public
//! CLI in this work's REPORT.md. Ruling 0040 is why both exist rather
//! than either alone — a fixture pins a shape at most.

#[path = "support/nested_harness.rs"]
mod harness;
#[path = "support/route_fixture.rs"]
mod route_fixture;
#[path = "../src/wirkd/mod.rs"]
mod wirkd;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use harness::*;

/// Every CLI call here is an **operator's**, with this test process's
/// own injected execution triple removed — a test binary run inside a
/// Wirk pane inherits one, and under ruling 0117 it would decide the
/// default scope of the very verbs under test.
fn cli() -> Command {
    let mut command = wirk_cli();
    command
        .env_remove("WIRK_ESTATE_ROOT")
        .env_remove("WIRK_WORK_ID")
        .env_remove("WIRK_RUN_ID");
    command
}

fn run(args: &[&str]) -> (bool, String, String) {
    let output = cli().args(args).output().expect("wirk runs");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

fn json(args: &[&str]) -> serde_json::Value {
    let (_, stdout, stderr) = run(args);
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|err| panic!("{args:?} did not print JSON ({err}): {stdout:?} {stderr:?}"))
}

struct Fixture {
    _dir: tempfile::TempDir,
    estate: PathBuf,
    repo: PathBuf,
    socket: PathBuf,
}

impl Fixture {
    fn estate_arg(&self) -> &str {
        self.estate.to_str().expect("estate path is utf-8")
    }

    fn storage_admin(&self) -> serde_json::Value {
        json(&[
            "estate",
            "storage",
            "--estate",
            self.estate_arg(),
            "--admin",
            "--json",
        ])
    }

    fn class<'a>(&self, report: &'a serde_json::Value, class: &str) -> &'a serde_json::Value {
        report["classes"]
            .as_array()
            .expect("classes array")
            .iter()
            .find(|entry| entry["class"] == class)
            .unwrap_or_else(|| panic!("no {class} class in {report}"))
    }
}

fn fixture() -> (Fixture, KillOnDrop) {
    fixture_with(serde_json::Map::new())
}

/// The same estate, with extra `.wirk/resources.json` keys written
/// **before** the daemon starts — wirkd loads its policy once, at
/// startup, so a limit written afterwards would not be the one under
/// test.
fn fixture_with(extra: serde_json::Map<String, serde_json::Value>) -> (Fixture, KillOnDrop) {
    let dir = tempfile::tempdir().expect("temp dir");
    let estate = dir.path().join("estate");
    fs::create_dir_all(estate.join(".wirk")).expect("create estate");
    // Its own host pool: the default is this *user's* pool, shared with
    // every other wirk on the box including the rest of this suite, so a
    // fixture using it would bound, and be bounded by, unrelated work.
    let pool = dir.path().join("host-pool");
    let mut policy = extra;
    policy.insert(
        "host_pool_dir".to_string(),
        serde_json::Value::String(pool.to_string_lossy().into_owned()),
    );
    fs::write(
        estate.join(".wirk").join("resources.json"),
        serde_json::to_string(&serde_json::Value::Object(policy)).expect("serialize policy"),
    )
    .expect("write resources.json");
    route_fixture::install_route_fixture(&estate, "smoke");
    route_fixture::install_route_fixture(&estate, "outputs_read_reviewer");
    let (wirkd, pointer) = start_wirkd(&estate);

    let repo = dir.path().join("source-repo");
    init_repo(&repo);
    write_file(&repo, "alpha.rs", "fn alpha() {}\n");
    commit_all(&repo, "content");

    (
        Fixture {
            _dir: dir,
            estate,
            repo,
            socket: pointer.socket,
        },
        wirkd,
    )
}

fn commit_all(repo: &Path, message: &str) {
    for args in [
        vec!["add", "-A"],
        vec![
            "-c",
            "user.name=estate-storage-test",
            "-c",
            "user.email=estate-storage@example.test",
            "commit",
            "-q",
            "-m",
            message,
        ],
    ] {
        let status = Command::new("git")
            .args(&args)
            .current_dir(repo)
            .status()
            .expect("git runs");
        assert!(status.success(), "git {args:?}");
    }
}

/// An image directory with `bytes` of content, plus `pins` Run
/// directories whose own `wirk` is a **hard link** to it — the exact
/// arrangement `wirk_herdr::bind_runtime_image` produces.
fn install_image(estate: &Path, digest: &str, bytes: usize, pins: &[&str]) {
    let image_dir = estate
        .join(".wirk")
        .join("runtime")
        .join("images")
        .join(digest);
    fs::create_dir_all(&image_dir).expect("image dir");
    let image = image_dir.join("wirk");
    fs::write(&image, vec![b'i'; bytes]).expect("image bytes");
    for run in pins {
        let bin = estate.join(".wirk").join("runtime").join(run).join("bin");
        fs::create_dir_all(&bin).expect("pin dir");
        fs::hard_link(&image, bin.join("wirk")).expect("hard link the pin to the image");
    }
}

const IMAGE_BYTES: usize = 512 * 1024;

// ---------------------------------------------------------------------
// The arithmetic
// ---------------------------------------------------------------------

/// Watched red by charging every directory entry to the estate total
/// (dropping the `(st_dev, st_ino)` dedup): the estate then reported
/// four copies of one image, and this assertion failed with a figure
/// above four times the real bytes.
#[test]
fn a_shared_runtime_image_is_charged_once_to_the_estate_not_once_per_pin() {
    let (fixture, wirkd) = fixture();
    let submitted = submit(
        &fixture.estate,
        "smoke",
        &fixture.repo,
        &["repo:read"],
        None,
    )
    .expect("submit a Work so the estate has Runs to pin against");
    install_image(
        &fixture.estate,
        "aaaa1111",
        IMAGE_BYTES,
        &[&submitted.run_id, "run-extra-1", "run-extra-2"],
    );
    let holders = 4u64; // the image itself plus three Run pins

    let report = fixture.storage_admin();
    let images = fixture.class(&report, "runtime-images");
    let pins = fixture.class(&report, "run-pins");

    // Every holder sees the bytes: `allocated` is per directory entry,
    // and that is what makes the naive sum wrong.
    let naive = images["allocated_bytes"].as_u64().expect("image allocated")
        + pins["allocated_bytes"].as_u64().expect("pin allocated");
    assert!(
        naive >= holders * IMAGE_BYTES as u64,
        "the per-entry figures should see every one of the {holders} names: {naive}"
    );

    // The estate total charges the inode once.
    let total = report["estate_unique_allocated_bytes"]
        .as_u64()
        .expect("estate total");
    assert!(
        total < 2 * IMAGE_BYTES as u64,
        "one hard-linked image must be charged once to the estate, not once per holder: {total} \
         against a naive {naive}"
    );
    assert!(
        images["shared_entries"].as_u64().unwrap_or(0) > 0
            || pins["shared_entries"].as_u64().unwrap_or(0) > 0,
        "the second holder of the inode must be reported as sharing it: {report}"
    );
    stop_wirkd(&fixture.estate, wirkd);
}

/// Nothing in the report may be presented as a count of bytes a removal
/// would return. Watched red by asserting the phrase before it existed.
#[test]
fn no_measurement_is_offered_as_a_reclaimable_byte_count() {
    let (fixture, wirkd) = fixture();
    let report = fixture.storage_admin();
    let caveat = report["measurement"]["not_a_reclaim_estimate"]
        .as_str()
        .expect("the measurement caveat is always present");
    assert!(
        caveat.contains("no lower bound is claimed"),
        "the caveat must refuse the lower-bound inference outright: {caveat}"
    );
    for term in ["reflink", "compression"] {
        assert!(caveat.contains(term), "the caveat should name {term}");
    }
    stop_wirkd(&fixture.estate, wirkd);
}

/// The source repository's own bytes are never in the estate and are
/// never measured, which is what makes "cleanup cannot touch the user's
/// material" structural rather than a promise.
#[test]
fn a_registered_source_is_named_as_an_original_and_never_measured() {
    let (fixture, wirkd) = fixture();
    let acquired = json(&[
        "atlas",
        "acquire",
        "--estate",
        fixture.estate_arg(),
        "--source",
        "fx",
        "--repository",
        fixture.repo.to_str().unwrap(),
        "--revision",
        "HEAD",
        "--json",
    ]);
    assert_eq!(acquired["outcome"], "staged", "acquire staged: {acquired}");

    let report = fixture.storage_admin();
    let sources = report["sources"].as_array().expect("sources listed");
    let source = sources
        .iter()
        .find(|entry| entry["alias"] == "fx")
        .expect("the registered source is named");
    assert_eq!(source["owned"], false);
    assert_eq!(source["locator"], fixture.repo.to_str().unwrap());
    // And there is no class whose path is the repository.
    for class in report["classes"].as_array().expect("classes") {
        assert_ne!(
            class["path"].as_str().unwrap_or(""),
            fixture.repo.to_str().unwrap(),
            "no measured class may be the source repository itself"
        );
    }
    stop_wirkd(&fixture.estate, wirkd);
}

// ---------------------------------------------------------------------
// Retention
// ---------------------------------------------------------------------

/// Watched red by treating "removable" as "not published" alone: the
/// image a live Run pin hard-links to was then offered as a candidate.
#[test]
fn an_image_an_existing_pin_shares_an_inode_with_is_retained_and_refused_by_name() {
    let (fixture, wirkd) = fixture();
    let submitted = submit(
        &fixture.estate,
        "smoke",
        &fixture.repo,
        &["repo:read"],
        None,
    )
    .expect("submit");
    install_image(
        &fixture.estate,
        "pinned01",
        IMAGE_BYTES,
        &[&submitted.run_id],
    );
    install_image(&fixture.estate, "orphan01", IMAGE_BYTES, &[]);

    let report = fixture.storage_admin();
    let images = fixture.class(&report, "runtime-images");
    let items = images["items"]
        .as_array()
        .expect("itemized administratively");
    let pinned = items
        .iter()
        .find(|item| item["id"] == "pinned01")
        .expect("the pinned image is listed");
    let orphan = items
        .iter()
        .find(|item| item["id"] == "orphan01")
        .expect("the unreferenced image is listed");
    assert_eq!(pinned["removable"], false);
    assert!(
        pinned["retained_by"]
            .as_array()
            .expect("retained_by")
            .iter()
            .any(|holder| holder.as_str().unwrap_or("").contains("Run pin")),
        "the refusal must name the concrete consumer: {pinned}"
    );
    assert_eq!(orphan["removable"], true);

    // And naming it explicitly is refused by name, not silently skipped.
    let refusal = json(&[
        "estate",
        "clean",
        "--estate",
        fixture.estate_arg(),
        "--admin",
        "--class",
        "runtime-images",
        "--id",
        "pinned01",
        "--dry-run",
        "--json",
    ]);
    assert!(
        refusal["selected"].as_array().expect("selected").is_empty(),
        "a retained image must not be selected: {refusal}"
    );
    assert_eq!(refusal["refused"][0]["id"], "pinned01");
    assert_eq!(refusal["refused"][0]["reason"], "retained");
    assert!(
        fixture
            .estate
            .join(".wirk/runtime/images/pinned01/wirk")
            .exists()
    );
    stop_wirkd(&fixture.estate, wirkd);
}

/// A dry run proves every refusal and performs none of it — the stop
/// point `clean_work` already uses, kept here.
#[test]
fn a_dry_run_selects_the_unreferenced_image_and_removes_nothing() {
    let (fixture, wirkd) = fixture();
    install_image(&fixture.estate, "orphan02", IMAGE_BYTES, &[]);
    let image = fixture.estate.join(".wirk/runtime/images/orphan02/wirk");

    let dry = json(&[
        "estate",
        "clean",
        "--estate",
        fixture.estate_arg(),
        "--admin",
        "--class",
        "runtime-images",
        "--all-unreferenced",
        "--dry-run",
        "--json",
    ]);
    assert_eq!(dry["dry_run"], true);
    assert_eq!(dry["selected"][0]["id"], "orphan02");
    assert!(
        dry["estimated_unique_allocated_bytes"]
            .as_u64()
            .expect("estimate")
            >= IMAGE_BYTES as u64
    );
    assert!(
        dry["estimate_is_not_a_reclaim_promise"]
            .as_str()
            .expect("the estimate caveat travels with the estimate")
            .contains("not a promise")
    );
    assert!(image.exists(), "a dry run must not remove anything");

    let applied = json(&[
        "estate",
        "clean",
        "--estate",
        fixture.estate_arg(),
        "--admin",
        "--class",
        "runtime-images",
        "--all-unreferenced",
        "--json",
    ]);
    assert_eq!(applied["dry_run"], false);
    assert_eq!(applied["removed"][0]["id"], "orphan02");
    assert_eq!(applied["complete"], true);
    assert!(
        !image.exists(),
        "the real call removes what the dry run named"
    );
    // The observed figure is the filesystem's own, reported as
    // approximate — never presented as the estimate come true.
    assert!(
        applied["reclaimed_observed_is_approximate"]
            .as_str()
            .expect("observation caveat")
            .contains("approximate")
    );
    stop_wirkd(&fixture.estate, wirkd);
}

/// A generation a **non-terminal** Work's delivered World names is a
/// required reference. Watched red by deriving retention from the
/// catalog alone: the staged generation was then offered as removable
/// while an open Work still resolved against it.
#[test]
fn a_published_generation_is_retained_and_a_superseded_one_is_not() {
    let (fixture, wirkd) = fixture();
    let first = json(&[
        "atlas",
        "acquire",
        "--estate",
        fixture.estate_arg(),
        "--source",
        "fx",
        "--repository",
        fixture.repo.to_str().unwrap(),
        "--revision",
        "HEAD",
        "--json",
    ]);
    let first_id = first["generation"]["generation"]
        .as_str()
        .expect("generation id")
        .to_string();
    let published = json(&[
        "atlas",
        "publish",
        "--estate",
        fixture.estate_arg(),
        "--source",
        "fx",
        "--generation",
        &first_id,
        "--json",
    ]);
    assert_eq!(
        published["generation"], first_id,
        "publish names the generation it published: {published}"
    );

    // A second acquisition at a new revision: staged, never published.
    write_file(&fixture.repo, "beta.rs", "fn beta() {}\n");
    commit_all(&fixture.repo, "more content");
    let second = json(&[
        "atlas",
        "refresh",
        "--estate",
        fixture.estate_arg(),
        "--source",
        "fx",
        "--revision",
        "HEAD",
        "--json",
    ]);
    let second_id = second["generation"]["generation"]
        .as_str()
        .expect("second generation id")
        .to_string();
    assert_ne!(first_id, second_id);

    let report = fixture.storage_admin();
    let generations = fixture.class(&report, "atlas-generations");
    let items = generations["items"].as_array().expect("items");
    let published_item = items
        .iter()
        .find(|item| item["id"] == first_id.as_str())
        .expect("the published generation is listed");
    let staged_item = items
        .iter()
        .find(|item| item["id"] == second_id.as_str())
        .expect("the staged generation is listed");
    assert_eq!(published_item["removable"], false);
    assert!(
        published_item["retained_by"]
            .as_array()
            .expect("retained_by")
            .iter()
            .any(|holder| holder.as_str().unwrap_or("").contains("published")),
        "the published generation's holder must be named: {published_item}"
    );
    assert_eq!(
        staged_item["removable"], true,
        "a staged, unpublished generation nothing references is an optional replay asset"
    );
    stop_wirkd(&fixture.estate, wirkd);
}

/// A worker contract a **non-terminal** Work reserves is required; the
/// build's own digest is always required. Watched red by retaining only
/// what a journal mentions: a finished Work's digest was then kept
/// forever, which is exactly the rule REFINED.md rejects.
#[test]
fn a_contract_an_open_work_reserves_is_retained_and_a_stale_digest_is_not() {
    let (fixture, wirkd) = fixture();
    submit(
        &fixture.estate,
        "smoke",
        &fixture.repo,
        &["repo:read"],
        None,
    )
    .expect("submit");

    // A contract nothing reserves: same directory, same shape, no Work.
    let contracts = fixture.estate.join(".wirk").join("contracts");
    fs::create_dir_all(&contracts).expect("contracts dir");
    fs::write(contracts.join("deadbeef.md"), "an older rendering\n").expect("stale contract");

    let report = fixture.storage_admin();
    let class = fixture.class(&report, "contracts");
    let items = class["items"].as_array().expect("items");
    let stale = items
        .iter()
        .find(|item| item["id"] == "deadbeef")
        .expect("the stale contract is listed");
    assert_eq!(stale["removable"], true);
    let live = items
        .iter()
        .find(|item| item["removable"] == false)
        .expect("the open Work's own contract is retained");
    assert!(
        !live["retained_by"]
            .as_array()
            .expect("retained_by")
            .is_empty(),
        "a retained contract must name who reserves it: {live}"
    );
    stop_wirkd(&fixture.estate, wirkd);
}

/// A retention set with a hole in it cannot establish that anything is
/// unreferenced. Watched red by letting an unreadable journal contribute
/// nothing: the cleanup then removed an asset whose only consumer it had
/// failed to read.
#[test]
fn an_unreadable_journal_refuses_the_cleanup_instead_of_removing() {
    let (fixture, wirkd) = fixture();
    install_image(&fixture.estate, "orphan03", IMAGE_BYTES, &[]);
    let image = fixture.estate.join(".wirk/runtime/images/orphan03/wirk");

    // A Work directory whose journal cannot be replayed: what it still
    // references is unknown, not nothing.
    let broken = fixture.estate.join("works").join("work-unreadable");
    fs::create_dir_all(&broken).expect("broken work dir");
    fs::write(broken.join("journal.ndjson"), "{ not json at all\n").expect("broken journal");

    let (ok, stdout, stderr) = run(&[
        "estate",
        "clean",
        "--estate",
        fixture.estate_arg(),
        "--admin",
        "--class",
        "runtime-images",
        "--all-unreferenced",
        "--json",
    ]);
    assert!(!ok, "the cleanup must refuse: {stdout} {stderr}");
    assert!(
        stderr.contains("RetentionIncomplete"),
        "and say why, in its own code: {stderr}"
    );
    assert!(
        image.exists(),
        "nothing may be removed while the retention set is incomplete"
    );

    // The read-only inventory still answers, and says the same thing.
    let report = fixture.storage_admin();
    assert_eq!(report["retention"]["complete"], false);
    assert!(
        !report["retention"]["unreadable"]
            .as_array()
            .expect("unreadable list")
            .is_empty()
    );
    stop_wirkd(&fixture.estate, wirkd);
}

// ---------------------------------------------------------------------
// Guards, authority and disclosure
// ---------------------------------------------------------------------

/// No default target, and never two. The discipline `atlas cancel`
/// applies, for the same reason: an operation that removes things must
/// not be reached by leaving an argument off.
#[test]
fn estate_clean_refuses_no_target_and_refuses_two_targets() {
    let (fixture, wirkd) = fixture();
    let base = [
        "estate",
        "clean",
        "--estate",
        fixture.estate_arg(),
        "--admin",
        "--class",
        "runtime-images",
    ];

    let (ok, _, stderr) = run(&base);
    assert!(!ok);
    assert!(stderr.contains("NoTarget"), "{stderr}");

    let mut both = base.to_vec();
    both.extend_from_slice(&["--id", "anything", "--all-unreferenced"]);
    let (ok, _, stderr) = run(&both);
    assert!(!ok);
    assert!(stderr.contains("AmbiguousTarget"), "{stderr}");
    stop_wirkd(&fixture.estate, wirkd);
}

/// These assets belong to the estate, not to any Work, so a Work-scoped
/// caller has no authority to select them — and is told where its own
/// residue is reached instead.
#[test]
fn estate_clean_refuses_a_work_scoped_caller_and_names_the_verb_that_does_apply() {
    let (fixture, wirkd) = fixture();
    let submitted = submit(
        &fixture.estate,
        "smoke",
        &fixture.repo,
        &["repo:read"],
        None,
    )
    .expect("submit");
    let (ok, _, stderr) = run(&[
        "estate",
        "clean",
        "--estate",
        fixture.estate_arg(),
        "--requesting-work",
        &submitted.work_id,
        "--class",
        "runtime-images",
        "--all-unreferenced",
    ]);
    assert!(!ok);
    assert!(stderr.contains("AdministrativeOnly"), "{stderr}");
    assert!(
        stderr.contains("wirk work clean"),
        "the refusal must name the verb that does apply to a Work's own residue: {stderr}"
    );
    stop_wirkd(&fixture.estate, wirkd);
}

/// A Work's own output scratch is never removed blind by the estate-wide
/// verb: it is selected per Work, by the verb that owns the guards.
#[test]
fn outputs_staging_is_not_selectable_through_the_estate_wide_verb() {
    let (fixture, wirkd) = fixture();
    let (ok, _, stderr) = run(&[
        "estate",
        "clean",
        "--estate",
        fixture.estate_arg(),
        "--admin",
        "--class",
        "outputs-staging",
        "--all-unreferenced",
    ]);
    assert!(!ok);
    assert!(stderr.contains("WorkScopedClass"), "{stderr}");
    assert!(stderr.contains("--outputs-staging"), "{stderr}");
    stop_wirkd(&fixture.estate, wirkd);
}

/// A class that is not selectable at all — a journal, a retained Claim —
/// is refused with the list of the ones that are.
#[test]
fn a_class_that_is_never_removable_is_refused_with_the_selectable_list() {
    let (fixture, wirkd) = fixture();
    for class in ["journals", "outputs-claims", "projections", "not-a-class"] {
        let (ok, _, stderr) = run(&[
            "estate",
            "clean",
            "--estate",
            fixture.estate_arg(),
            "--admin",
            "--class",
            class,
            "--all-unreferenced",
        ]);
        assert!(!ok, "{class} must not be selectable");
        assert!(stderr.contains("UnknownClass"), "{class}: {stderr}");
        assert!(
            stderr.contains("runtime-images"),
            "{class}: the refusal should say what is selectable: {stderr}"
        );
    }
    stop_wirkd(&fixture.estate, wirkd);
}

/// Ruling 0095's existence disclosure, kept. A Work-scoped read gets the
/// class totals and the caveats; it does not get a list of every source
/// alias, generation id and Work in the estate. Watched red by
/// itemizing unconditionally.
#[test]
fn a_work_scoped_storage_read_withholds_item_identities() {
    let (fixture, wirkd) = fixture();
    let submitted = submit(
        &fixture.estate,
        "smoke",
        &fixture.repo,
        &["repo:read"],
        None,
    )
    .expect("submit");
    let acquired = json(&[
        "atlas",
        "acquire",
        "--estate",
        fixture.estate_arg(),
        "--source",
        "fx",
        "--repository",
        fixture.repo.to_str().unwrap(),
        "--revision",
        "HEAD",
        "--json",
    ]);
    assert_eq!(acquired["outcome"], "staged");

    let scoped = json(&[
        "estate",
        "storage",
        "--estate",
        fixture.estate_arg(),
        "--requesting-work",
        &submitted.work_id,
        "--json",
    ]);
    assert_eq!(scoped["scope"], "requester");
    assert!(
        scoped["sources"].is_null(),
        "a scoped read must not name the estate's registered sources: {scoped}"
    );
    for class in scoped["classes"].as_array().expect("classes") {
        assert!(
            class.get("items").is_none(),
            "a scoped read must not itemize {}: {class}",
            class["class"]
        );
        // The totals and the caveats still answer — this is a
        // withholding of identities, not of the whole verb.
        assert!(class.get("unique_allocated_bytes").is_some());
    }
    assert!(scoped["measurement"]["not_a_reclaim_estimate"].is_string());

    let admin = fixture.storage_admin();
    assert_eq!(admin["scope"], "administrative");
    assert!(admin["sources"].is_array());
    assert!(
        fixture.class(&admin, "atlas-generations")["items"].is_array(),
        "the administrative read does itemize"
    );
    stop_wirkd(&fixture.estate, wirkd);
}

/// A soft limit discloses and refuses nothing. Watched red by comparing
/// against the limit in `admit`: ordinary work then started refusing on
/// a storage budget, with no reclamation behind it to make room.
#[test]
fn a_storage_soft_limit_is_disclosed_and_refuses_nothing() {
    let mut policy = serde_json::Map::new();
    policy.insert(
        "storage_soft_limits".to_string(),
        serde_json::json!({ "runtime-images": 1024 }),
    );
    let (fixture, wirkd) = fixture_with(policy);
    install_image(&fixture.estate, "big00001", IMAGE_BYTES, &[]);

    let report = fixture.storage_admin();
    let images = fixture.class(&report, "runtime-images");
    assert_eq!(images["soft_limit_bytes"], 1024);
    assert_eq!(images["over_soft_limit"], true);
    assert!(
        report["over_soft_limit"]
            .as_array()
            .expect("over list")
            .iter()
            .any(|class| class == "runtime-images")
    );
    assert!(
        report["soft_limits_are_disclosure_only"]
            .as_str()
            .expect("the disclosure-only statement")
            .contains("refuses nothing")
    );

    // And ordinary expensive work is still admitted: a budget is not an
    // outage, because nothing here reclaims anything on its own.
    let acquired = json(&[
        "atlas",
        "acquire",
        "--estate",
        fixture.estate_arg(),
        "--source",
        "fx",
        "--repository",
        fixture.repo.to_str().unwrap(),
        "--revision",
        "HEAD",
        "--json",
    ]);
    assert_eq!(
        acquired["outcome"], "staged",
        "being over a soft storage limit must not refuse work: {acquired}"
    );
    stop_wirkd(&fixture.estate, wirkd);
}

/// A limit written for a class that does not exist is *reported*, not
/// silently attached to nothing — the discipline B's own
/// `resources.json` defect established.
#[test]
fn a_soft_limit_for_a_class_that_does_not_exist_is_reported() {
    let (policy, note) = wirk_core::jobs::ResourcePolicy::load(Path::new("/nonexistent-estate"));
    assert!(policy.storage_soft_limits.is_empty());
    assert!(note.is_none(), "an absent file is the ordinary case");

    let dir = tempfile::tempdir().expect("temp dir");
    fs::create_dir_all(dir.path().join(".wirk")).expect("wirk dir");
    fs::write(
        dir.path().join(".wirk").join("resources.json"),
        r#"{"storage_soft_limits": {"runtime-images": 10, "nonsense": 20}}"#,
    )
    .expect("write policy");
    let (policy, note) = wirk_core::jobs::ResourcePolicy::load(dir.path());
    assert_eq!(policy.storage_soft_limits.get("runtime-images"), Some(&10));
    assert!(
        !policy.storage_soft_limits.contains_key("nonsense"),
        "an unknown class must not be applied"
    );
    let note = note.expect("the unknown class is complained about");
    assert!(note.contains("nonsense"), "{note}");
    assert!(
        note.contains("runtime-images"),
        "the complaint should list the real classes: {note}"
    );
}

/// The shared per-uid query index cache is named so a reader can find
/// it, reported as host-scoped, and never charged to this estate.
#[test]
fn the_shared_query_index_cache_is_named_but_not_charged_to_this_estate() {
    let (fixture, wirkd) = fixture();
    let report = fixture.storage_admin();
    let shared = report["host_shared"]
        .as_array()
        .expect("host_shared")
        .iter()
        .find(|entry| entry["name"] == "query-index-cache")
        .expect("the cache is named");
    assert_eq!(shared["charged_to_this_estate"], false);
    assert!(
        shared["note"]
            .as_str()
            .expect("note")
            .contains("self-bounded"),
        "the note should say why this estate does not collect it: {shared}"
    );
    // And it is not a class an estate cleanup can select.
    assert!(
        !report["classes"]
            .as_array()
            .expect("classes")
            .iter()
            .any(|class| class["class"] == "query-index-cache")
    );
    stop_wirkd(&fixture.estate, wirkd);
}

// ---------------------------------------------------------------------
// A live owner, and the legitimate cleanup after it
// ---------------------------------------------------------------------

/// A backend that blocks instead of speaking the embedding protocol: the
/// job is admitted, registered, holds the atlas and is killable, which
/// is every part of it these checks are about. The real offline Semble
/// build, and the same sequence against it, are in REPORT.md.
fn blocking_backend(dir: &Path) -> PathBuf {
    let path = dir.join("blocking-backend.sh");
    fs::write(&path, "#!/bin/sh\nexec sleep 600\n").expect("write backend");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod backend");
    path
}

/// The decisive concurrency contract: while a job this estate started is
/// running, a cleanup refuses and removes nothing; once the job is
/// stopped through the public verb, the same cleanup succeeds.
///
/// Watched red by deriving and removing without consulting the job
/// registry: the cleanup then ran happily beside a live build.
#[test]
fn a_live_job_refuses_the_cleanup_and_a_later_one_succeeds() {
    let (fixture, wirkd) = fixture();
    install_image(&fixture.estate, "orphan05", IMAGE_BYTES, &[]);
    let image = fixture.estate.join(".wirk/runtime/images/orphan05/wirk");

    let acquired = json(&[
        "atlas",
        "acquire",
        "--estate",
        fixture.estate_arg(),
        "--source",
        "fx",
        "--repository",
        fixture.repo.to_str().unwrap(),
        "--revision",
        "HEAD",
        "--json",
    ]);
    let generation = acquired["generation"]["generation"]
        .as_str()
        .expect("generation id")
        .to_string();

    let scratch = fixture.estate.parent().expect("estate has a parent");
    let backend = blocking_backend(scratch);
    let model = scratch.join("model");
    fs::create_dir_all(&model).expect("model dir");
    let log = scratch.join("build.log");
    let sink = fs::File::create(&log).expect("build log");
    let mut build = cli()
        .args(["atlas", "semantic", "build", "--estate"])
        .arg(&fixture.estate)
        .args(["--source", "fx", "--generation", &generation])
        .arg("--backend")
        .arg(&backend)
        .arg("--model")
        .arg(&model)
        .args(["--admin", "--json"])
        .stdout(sink.try_clone().expect("clone log"))
        .stderr(sink)
        .spawn()
        .expect("spawn semantic build");

    // Wait until the daemon actually reports a running job — never a
    // sleep standing in for the state under test.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let listed = json(&[
            "atlas",
            "cancel",
            "--estate",
            fixture.estate_arg(),
            "--admin",
            "--list",
            "--json",
        ]);
        if listed["running"]
            .as_array()
            .is_some_and(|jobs| !jobs.is_empty())
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the build never registered a job; log: {}",
            fs::read_to_string(&log).unwrap_or_default()
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    // The read-only inventory still answers while the atlas is held —
    // "why is my disk full" is asked exactly then.
    let held = fixture.storage_admin();
    assert!(fixture.class(&held, "runtime-images")["allocated_bytes"].is_u64());
    assert_eq!(
        held["retention"]["complete"], false,
        "with the atlas held, what nothing retains is not established: {held}"
    );

    // The cleanup refuses, in B's own vocabulary, and touches nothing.
    let (ok, stdout, stderr) = run(&[
        "estate",
        "clean",
        "--estate",
        fixture.estate_arg(),
        "--admin",
        "--class",
        "runtime-images",
        "--all-unreferenced",
        "--json",
    ]);
    assert!(!ok, "a live job must refuse the cleanup: {stdout} {stderr}");
    assert!(stderr.contains("ExpensiveJobBusy"), "{stderr}");
    assert!(
        stderr.contains("nothing was selected and nothing was removed"),
        "the refusal must say what it did not do: {stderr}"
    );
    assert!(image.exists(), "nothing may be removed while a job is live");

    // Stopped through the public verb, not by killing a pid.
    let cancelled = json(&[
        "atlas",
        "cancel",
        "--estate",
        fixture.estate_arg(),
        "--admin",
        "--all",
        "--reason",
        "estate_storage contract check",
        "--wait",
        "30",
        "--json",
    ]);
    assert!(
        !cancelled["acknowledged"]
            .as_array()
            .expect("acknowledged")
            .is_empty(),
        "the cancel should name the job it signalled: {cancelled}"
    );
    let _ = build.wait();

    // And now the same cleanup is legitimate.
    let applied = json(&[
        "estate",
        "clean",
        "--estate",
        fixture.estate_arg(),
        "--admin",
        "--class",
        "runtime-images",
        "--all-unreferenced",
        "--json",
    ]);
    assert_eq!(applied["complete"], true, "{applied}");
    assert_eq!(applied["removed"][0]["id"], "orphan05");
    assert!(!image.exists());
    stop_wirkd(&fixture.estate, wirkd);
}

// ---------------------------------------------------------------------
// A Work's own output staging
// ---------------------------------------------------------------------

/// `wirk output dir` for one Run, as the actor itself asks for it.
fn output_dir(estate: &Path, work_id: &str, run_id: &str) -> PathBuf {
    let out = wirk_cli()
        .arg("output")
        .arg("dir")
        .env("WIRK_ESTATE_ROOT", estate)
        .env("WIRK_WORK_ID", work_id)
        .env("WIRK_RUN_ID", run_id)
        .output()
        .expect("wirk output dir runs");
    assert!(
        out.status.success(),
        "wirk output dir: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    PathBuf::from(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn work_clean(estate: &Path, work_id: &str, extra: &[&str]) -> (bool, serde_json::Value, String) {
    let estate_str = estate.to_string_lossy().into_owned();
    let mut args = vec!["work", "clean", "--estate", &estate_str, "--work", work_id];
    args.extend_from_slice(extra);
    args.push("--json");
    let output = cli().args(&args).output().expect("wirk work clean runs");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    (
        output.status.success(),
        serde_json::from_str(stdout.trim()).unwrap_or(serde_json::Value::Null),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// One Read-bound Actor Work on the reviewer Route, materialized — the
/// ordinary shape a managed-output Claim actually has (ruling 0145).
fn read_bound_reviewer(fixture: &Fixture) -> (String, String) {
    let submitted = submit_kind(
        &fixture.estate,
        "outputs_read_reviewer",
        &fixture.repo,
        &["demo:read"],
        None,
        Some("actor"),
    )
    .expect("submit the read-bound reviewer");
    materialize_actor(
        &fixture.socket,
        &fixture.estate,
        &submitted.work_id,
        &submitted.run_id,
    );
    (submitted.work_id, submitted.run_id)
}

/// A Work's own scratch is removable only once that Work is finished,
/// and a validated Claim's artifacts — an independent write-once copy
/// under `claims/` — survive it.
///
/// Watched red twice: removing staging without the terminal check took
/// an open Work's working files, and removing it without excluding
/// `claims/` took the Claim's own evidence with it.
#[test]
fn outputs_staging_is_removable_only_when_the_work_is_terminal_and_never_the_claim() {
    let (fixture, wirkd) = fixture();
    let (work_id, run_id) = read_bound_reviewer(&fixture);

    // The actor writes its declared output where the daemon addresses
    // it, through the real public verb.
    let staging = output_dir(&fixture.estate, &work_id, &run_id);
    fs::write(staging.join("report.md"), "# the real report\n").expect("write the output");
    fs::write(staging.join("scratch.tmp"), "working notes\n").expect("write scratch");
    assert!(staging.is_dir());

    // Open Work: refused outright, nothing touched.
    let (ok, _, stderr) = work_clean(
        &fixture.estate,
        &work_id,
        &["--outputs-staging", "--dry-run"],
    );
    assert!(!ok, "an open Work's scratch must not be removable");
    assert!(stderr.contains("NotTerminal"), "{stderr}");
    assert!(staging.join("report.md").exists());

    // A real Claim against the declared managed output.
    let (code, out) = claim(
        &fixture.estate,
        &work_id,
        &run_id,
        &["--output", "report.md"],
    );
    assert_eq!(code, Some(0), "claim: {out}");
    let claims = fixture
        .estate
        .join("works")
        .join(&work_id)
        .join("outputs")
        .join("claims");
    assert!(claims.is_dir(), "the Claim snapshotted its own copy");

    // Terminal: the dry run names the Run and still removes nothing.
    let (ok, dry, stderr) = work_clean(
        &fixture.estate,
        &work_id,
        &["--outputs-staging", "--dry-run"],
    );
    assert!(ok, "{stderr}");
    assert_eq!(dry["outputs_staging_requested"], true);
    assert_eq!(dry["outputs_staging_removed"][0], run_id);
    assert!(staging.exists(), "a dry run removes nothing");

    // And the real call removes the scratch and nothing else.
    let (ok, applied, stderr) = work_clean(&fixture.estate, &work_id, &["--outputs-staging"]);
    assert!(ok, "{stderr}");
    assert_eq!(applied["complete"], true);
    assert_eq!(applied["outputs_staging_removed"][0], run_id);
    assert!(!staging.exists(), "the scratch is gone");
    assert!(
        claims.is_dir(),
        "the validated Claim's own artifacts are a separate copy and are never touched"
    );
    let snapshot: Vec<PathBuf> = fs::read_dir(&claims)
        .expect("claims dir")
        .flatten()
        .map(|entry| entry.path())
        .collect();
    assert_eq!(snapshot.len(), 1, "one Claim directory: {snapshot:?}");
    assert_eq!(
        fs::read_to_string(snapshot[0].join("report.md")).expect("the retained artifact reads"),
        "# the real report\n",
        "the retained bytes are the bytes the Claim validated"
    );
    stop_wirkd(&fixture.estate, wirkd);
}

/// Without the flag, staging is left exactly where it is — an operator
/// tidying a checkout did not thereby ask to drop the actor's files.
#[test]
fn work_clean_leaves_output_staging_alone_unless_it_is_asked_for() {
    let (fixture, wirkd) = fixture();
    let (work_id, run_id) = read_bound_reviewer(&fixture);
    let staging = output_dir(&fixture.estate, &work_id, &run_id);
    fs::write(staging.join("report.md"), "# report\n").expect("write output");
    let (code, out) = claim(
        &fixture.estate,
        &work_id,
        &run_id,
        &["--output", "report.md"],
    );
    assert_eq!(code, Some(0), "claim: {out}");

    let (ok, applied, stderr) = work_clean(&fixture.estate, &work_id, &[]);
    assert!(ok, "{stderr}");
    assert_eq!(applied["outputs_staging_requested"], false);
    assert!(
        applied["outputs_staging_removed"]
            .as_array()
            .expect("array")
            .is_empty()
    );
    assert!(staging.exists(), "staging is left alone by default");
    stop_wirkd(&fixture.estate, wirkd);
}

// ---------------------------------------------------------------------
// Scoped diagnostics (ruling 0260)
// ---------------------------------------------------------------------

/// Make `path` unreadable to this uid, returning `true` when that
/// actually took effect. Refuses to pretend under a uid that bypasses
/// the mode bits, because a test that cannot make the read fail is not
/// evidence about what happens when it does.
fn deny_read(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o000)).expect("chmod 000");
    fs::read_dir(path).is_err()
}

fn restore_read(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o755));
}

/// Ruling 0260 / finding F1. The identities ruling 0095 withholds from a
/// Work-scoped ordinary row are the same identities its *diagnostics*
/// must withhold. Two separate free-form channels carried them:
///
/// - `retention.unreadable`, which named the full path of a foreign
///   Work whose journal could not be replayed; and
/// - each class's `measurement_limit`, which named the full path of
///   whatever under a foreign Work could not be walked.
///
/// Watched red on both: before the correction this asserted against a
/// scoped JSON that contained the foreign Work's id verbatim.
///
/// What is *not* under test here is withholding the failure itself.
/// `retention.complete` must stay `false`, the reason must stay
/// readable, and the administrative read must still name every path —
/// an operator repairing this needs the path, and has the authority for
/// it.
#[test]
fn a_work_scoped_storage_read_withholds_identities_from_its_diagnostics_too() {
    let (fixture, wirkd) = fixture();
    let caller = submit(
        &fixture.estate,
        "smoke",
        &fixture.repo,
        &["repo:read"],
        None,
    )
    .expect("submit the caller's own Work");
    let foreign = submit(
        &fixture.estate,
        "smoke",
        &fixture.repo,
        &["repo:read"],
        None,
    )
    .expect("submit a second, foreign Work");

    // Channel 1: a foreign Work whose journal cannot be replayed. Its
    // own bytes, corrupted on disk — not a mutated event, and not a
    // fake: this is exactly the state `derive_retention` reports.
    let foreign_work_dir = fixture.estate.join("works").join(&foreign.work_id);
    fs::write(
        foreign_work_dir.join("journal.ndjson"),
        b"{ this is not an event }\n",
    )
    .expect("corrupt the foreign journal");

    // Channel 2: a directory under the *caller's own* Work is not the
    // test — it must be one the scoped caller has no business seeing.
    // A third Work, replayable, with an unwalkable projections
    // directory, drives the measurement channel.
    let third = submit(
        &fixture.estate,
        "smoke",
        &fixture.repo,
        &["repo:read"],
        None,
    )
    .expect("submit a third Work");
    let unwalkable = fixture
        .estate
        .join("works")
        .join(&third.work_id)
        .join("projections");
    fs::create_dir_all(&unwalkable).expect("projections dir");
    let measurement_channel_live = deny_read(&unwalkable);

    let scoped = json(&[
        "estate",
        "storage",
        "--estate",
        fixture.estate_arg(),
        "--requesting-work",
        &caller.work_id,
        "--json",
    ]);
    let (_, scoped_text, _) = run(&[
        "estate",
        "storage",
        "--estate",
        fixture.estate_arg(),
        "--requesting-work",
        &caller.work_id,
    ]);
    let admin = fixture.storage_admin();
    let (_, admin_text, _) = run(&[
        "estate",
        "storage",
        "--estate",
        fixture.estate_arg(),
        "--admin",
    ]);
    restore_read(&unwalkable);

    eprintln!(
        "SCOPED retention: {}",
        serde_json::to_string_pretty(&scoped["retention"]).expect("pretty")
    );
    eprintln!("SCOPED text:\n{scoped_text}");

    // The incompleteness itself is never hidden: that is the fact a
    // cleanup refuses on, and hiding it would be the worse defect.
    assert_eq!(scoped["scope"], "requester");
    assert_eq!(
        scoped["retention"]["complete"], false,
        "the hole must stay visible to a scoped caller: {scoped}"
    );
    let scoped_unreadable = scoped["retention"]["unreadable"]
        .as_array()
        .expect("scoped retention.unreadable is an array");
    assert!(
        !scoped_unreadable.is_empty(),
        "a scoped caller is still told there is a hole: {scoped}"
    );

    // No foreign identity, in any channel of the scoped response.
    let scoped_json = serde_json::to_string(&scoped).expect("serialize");
    for (label, id) in [
        ("foreign work", foreign.work_id.as_str()),
        ("foreign run", foreign.run_id.as_str()),
        ("third work", third.work_id.as_str()),
    ] {
        assert!(
            !scoped_json.contains(id),
            "the scoped JSON disclosed the {label} id {id}: {scoped_json}"
        );
        assert!(
            !scoped_text.contains(id),
            "the scoped text disclosed the {label} id {id}: {scoped_text}"
        );
    }

    // Truthful and useful: the reason survives the withholding, and the
    // caller is told where the detail lives.
    let reasons: String = scoped_unreadable
        .iter()
        .map(|line| line.as_str().unwrap_or_default())
        .collect::<Vec<_>>()
        .join(" | ");
    assert!(
        reasons.contains("journal") && reasons.contains("could not be replayed"),
        "the scoped reason must stay actionable: {reasons}"
    );
    assert!(
        scoped["retention"]["identities_withheld"].is_string(),
        "a scoped caller is told the identities were withheld, not that there were none: {scoped}"
    );

    if measurement_channel_live {
        let projections = fixture.class(&scoped, "projections");
        let limit = projections["measurement_limit"]
            .as_str()
            .expect("the scoped measurement limit is still reported");
        assert!(
            limit.contains("could not be read"),
            "the measurement gap stays disclosed: {limit}"
        );
        // Already covered by the loop above, restated here so a failure
        // points at the measurement channel specifically.
        assert!(
            !limit.contains(&third.work_id),
            "the scoped measurement limit disclosed a foreign Work id: {limit}"
        );
    }

    // The administrative read is unchanged: full paths, for the
    // operator who has to go and fix them.
    assert_eq!(admin["scope"], "administrative");
    assert_eq!(admin["retention"]["complete"], false);
    let admin_json = serde_json::to_string(&admin).expect("serialize");
    assert!(
        admin_json.contains(&foreign.work_id),
        "the administrative read must still name the unreplayable Work: {admin_json}"
    );
    assert!(
        admin_json.contains(foreign_work_dir.to_str().expect("utf-8")),
        "the administrative read must still name the exact path: {admin_json}"
    );
    assert!(
        admin_text.contains(&foreign.work_id),
        "the administrative text must still name the unreplayable Work: {admin_text}"
    );
    assert!(
        admin["retention"]["identities_withheld"].is_null(),
        "nothing is withheld from an administrative read: {admin}"
    );
    if measurement_channel_live {
        assert!(
            admin_json.contains(&third.work_id),
            "the administrative measurement limit must still name the path: {admin_json}"
        );
    }

    stop_wirkd(&fixture.estate, wirkd);
}

// ---- local document sources ----------------------------------------------

/// A document collection admitted as its own source is owned, inventoried
/// and reclaimable exactly like a Git one — and reclaiming it never
/// reaches the originals.
///
/// The estate's own bytes for a document source are the same two things
/// they are for any source: a generation manifest and its resource list.
/// The documents themselves stay where their owner put them. This walks
/// the whole lifecycle through the public verbs: admit, publish, see the
/// generation held by its own publication, unregister, see it become
/// reclaimable, reclaim it, and confirm every original file is still
/// there, byte for byte.
#[test]
fn a_document_source_is_owned_inventoried_and_reclaimable_without_touching_originals() {
    let (fixture, wirkd) = fixture();

    let docs = fixture.estate.parent().expect("parent").join("docs");
    fs::create_dir_all(&docs).expect("docs dir");
    fs::write(docs.join("guide.md"), "# Guide\n\nA sentence.\n").expect("write guide");
    fs::write(docs.join("notes.md"), "# Notes\n\nAnother sentence.\n").expect("write notes");
    let before: Vec<(PathBuf, String)> = ["guide.md", "notes.md"]
        .iter()
        .map(|name| {
            let path = docs.join(name);
            let body = fs::read_to_string(&path).expect("read original");
            (path, body)
        })
        .collect();

    let acquired = json(&[
        "atlas",
        "acquire",
        "--estate",
        fixture.estate_arg(),
        "--source",
        "docs",
        "--repository",
        docs.to_str().unwrap(),
        "--kind",
        "document-tree",
        "--json",
    ]);
    let generation = acquired["generation"]["generation"]
        .as_str()
        .expect("generation id")
        .to_string();
    let published = json(&[
        "atlas",
        "publish",
        "--estate",
        fixture.estate_arg(),
        "--source",
        "docs",
        "--generation",
        &generation,
        "--json",
    ]);
    assert!(
        published["publication_revision"].is_u64(),
        "publish must report the catalog revision it advanced to: {published}"
    );

    // Held by its own publication, so not removable yet.
    let report = fixture.storage_admin();
    let item = fixture.class(&report, "atlas-generations")["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|item| item["id"] == generation.as_str())
        .unwrap_or_else(|| panic!("the document generation is inventoried: {report}"))
        .clone();
    assert_eq!(
        item["removable"], false,
        "a published generation is retained by its own publication: {item}"
    );

    // The source's locator is *named* in the inventory and never
    // measured: the originals are not this estate's to account for.
    let named = report["sources"]
        .as_array()
        .expect("sources")
        .iter()
        .any(|entry| entry["locator"] == docs.to_str().unwrap());
    assert!(
        named,
        "the document source's locator is disclosed: {report}"
    );

    // Unregister, then reclaim through the one cleanup owner.
    let removed = json(&[
        "atlas",
        "remove",
        "--estate",
        fixture.estate_arg(),
        "--source",
        "docs",
        "--json",
    ]);
    assert_eq!(removed["outcome"], "removed", "{removed}");

    let report = fixture.storage_admin();
    let item = fixture.class(&report, "atlas-generations")["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|item| item["id"] == generation.as_str())
        .unwrap_or_else(|| panic!("still inventoried before reclaim: {report}"))
        .clone();
    assert_eq!(
        item["removable"], true,
        "once unregistered, nothing retains it: {item}"
    );

    let cleaned = json(&[
        "estate",
        "clean",
        "--estate",
        fixture.estate_arg(),
        "--admin",
        "--class",
        "atlas-generations",
        "--all-unreferenced",
        "--json",
    ]);
    assert!(
        cleaned["removed"]
            .as_array()
            .expect("removed")
            .iter()
            .any(|entry| entry["id"] == generation.as_str()),
        "the unreferenced document generation is reclaimed: {cleaned}"
    );

    // The decisive half: the user's own documents are exactly as they
    // were. Nothing in this lifecycle ever had the right to touch them.
    for (path, body) in before {
        assert_eq!(
            fs::read_to_string(&path).expect("original still readable"),
            body,
            "reclaiming estate-owned bytes must never reach {}",
            path.display()
        );
    }

    stop_wirkd(&fixture.estate, wirkd);
}

/// D5's operator recourse, through the real configuration surface.
///
/// The default document bounds refuse a collection this wide. An estate
/// that raises `document_max_entries` in its own `.wirk/resources.json`
/// admits exactly the same collection. Nothing about the capture changes
/// except whether it was allowed to happen — which is the difference
/// between a bound and a wall.
#[test]
fn a_configured_document_bound_admits_a_collection_the_default_refuses() {
    fn collection(root: &Path) -> PathBuf {
        let docs = root.join("wide-docs");
        fs::create_dir_all(&docs).expect("docs dir");
        for n in 0..24 {
            fs::write(docs.join(format!("d{n}.md")), "# x\n").expect("write");
        }
        docs
    }

    // Tight: the estate's own policy refuses it, by name, with the
    // setting an operator would change.
    let mut tight = serde_json::Map::new();
    tight.insert("document_max_entries".to_string(), serde_json::json!(4));
    let (fixture, wirkd) = fixture_with(tight);
    let docs = collection(fixture.estate.parent().expect("parent"));
    let (ok, stdout, stderr) = run(&[
        "atlas",
        "acquire",
        "--estate",
        fixture.estate_arg(),
        "--source",
        "wide",
        "--repository",
        docs.to_str().unwrap(),
        "--kind",
        "document-tree",
        "--json",
    ]);
    let said = format!("{stdout}{stderr}");
    assert!(!ok, "the tight bound must refuse this collection: {said}");
    assert!(
        said.contains("document_max_entries"),
        "the refusal must name the setting an operator can raise: {said}"
    );
    stop_wirkd(&fixture.estate, wirkd);

    // Raised: the same collection, admitted.
    let mut raised = serde_json::Map::new();
    raised.insert("document_max_entries".to_string(), serde_json::json!(5_000));
    let (fixture, wirkd) = fixture_with(raised);
    let docs = collection(fixture.estate.parent().expect("parent"));
    let acquired = json(&[
        "atlas",
        "acquire",
        "--estate",
        fixture.estate_arg(),
        "--source",
        "wide",
        "--repository",
        docs.to_str().unwrap(),
        "--kind",
        "document-tree",
        "--json",
    ]);
    assert_eq!(
        acquired["generation"]["coverage"]["total"], 24,
        "a raised bound admits the whole collection: {acquired}"
    );
    stop_wirkd(&fixture.estate, wirkd);
}

/// The `wirk` CLI with the *test runner's own* actor triple removed from
/// the child's environment.
///
/// `resolve_scope` reads `WIRK_ESTATE_ROOT`/`WIRK_WORK_ID`/`WIRK_RUN_ID`
/// to decide whether a call is an actor's own or an operator's, and a
/// test process inherits whatever its runner had. This suite is run from
/// inside a real actor pane often enough that an inherited triple makes
/// a fixture's administrative call against its own temp estate refuse as
/// a cross-estate read — so the fixture has to say which it is rather
/// than depend on who started it.
///
/// Sites that mean to act *as* an actor set the three back explicitly on
/// the returned command; a later `env` overrides this removal.
fn wirk_cli() -> Command {
    let mut command = Command::new(wirk_bin());
    command
        .env_remove("WIRK_ESTATE_ROOT")
        .env_remove("WIRK_WORK_ID")
        .env_remove("WIRK_RUN_ID");
    command
}
