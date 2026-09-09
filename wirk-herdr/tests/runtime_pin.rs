//! P3 execution-recovery **correction**, item 1: a Run's own `wirk`
//! runtime is stable for the life of the Run, and an unfulfilled pin
//! refuses the actor launch rather than silently handing the pane a
//! mutable or ambiguous `wirk`.
//!
//! The prior implementation removed and re-copied `current_exe` on
//! every call, so a reattach — or any second driver image attaching to
//! the same Run — replaced bytes the Run was already using; and a
//! failed copy only printed a line while the pane launched under the
//! weaker `exe.parent()` PATH prepend. Each test below is a red for one
//! of those two, driven against the real `ensure_pinned_wirk_bin` and,
//! for the refusal, against the real `HerdrExecutor::launch` path.
//!
//! Every "driver image" here is a real file with real bytes that is
//! really replaced or really deleted — never a stand-in for one.

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use tempfile::tempdir;
use wirk_core::{
    ActorKind, ActorWorld, ArtifactSpec, Boundary, ExecutionTriple, OutputContract, Run, RunId,
    RunState, WaypointId, WorkId, World, WorldHash,
};
use wirk_herdr::fake::FakeHerdrClient;
use wirk_herdr::{AgentStatus, HerdrExecutor, PaneInfo, ensure_pinned_wirk_bin};

/// A real executable file with the given bytes — this is what stands in
/// for "the driver binary that launched this Run", so that a test can
/// rebuild, rename, or delete it the way a real `cargo build` does.
fn driver_image(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).expect("write driver image");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod driver image");
    }
    path
}

fn pinned_bytes(dir: &Path) -> Vec<u8> {
    std::fs::read(dir.join("wirk")).expect("read this Run's own pinned wirk")
}

/// A reattach — a second `wirk run` for the same Run, from a *different*
/// driver image — must not change what this Run's `wirk` is. This is
/// the exact defect: `remove_file` + `copy` on every call.
#[test]
fn a_reattach_by_a_different_driver_image_leaves_this_runs_pinned_bytes_alone() {
    let estate = tempdir().expect("estate tempdir");
    let images = tempdir().expect("images tempdir");
    let first = driver_image(
        images.path(),
        "wirk-aaaaaaa-candidate",
        b"driver image ONE\n",
    );
    let second = driver_image(
        images.path(),
        "wirk-bbbbbbb-candidate",
        b"driver image TWO\n",
    );

    let dir = ensure_pinned_wirk_bin(&estate.path().to_string_lossy(), "run-1", &first)
        .expect("the first launch pins this Run's runtime");
    assert_eq!(pinned_bytes(&dir), b"driver image ONE\n");

    let again = ensure_pinned_wirk_bin(&estate.path().to_string_lossy(), "run-1", &second)
        .expect("a reattach from another driver image is admitted");
    assert_eq!(again, dir, "the same Run keeps the same runtime directory");
    assert_eq!(
        pinned_bytes(&dir),
        b"driver image ONE\n",
        "a second driver image attaching to an already-pinned Run must not replace its bytes"
    );

    // And a third, idempotent call with the original image is equally
    // a no-op — stability is a property of the pin, not of which image
    // happens to call.
    ensure_pinned_wirk_bin(&estate.path().to_string_lossy(), "run-1", &first)
        .expect("re-pinning with the original image is admitted");
    assert_eq!(pinned_bytes(&dir), b"driver image ONE\n");
}

/// The bytes are pinned, not named: rebuilding *and* deleting the file
/// they were installed from changes nothing for the Run.
#[test]
fn a_pinned_runtime_survives_rebuild_rename_and_deletion_of_its_source_binary() {
    let estate = tempdir().expect("estate tempdir");
    let images = tempdir().expect("images tempdir");
    let exe = driver_image(
        images.path(),
        "wirk-ccccccc-candidate",
        b"the original bytes\n",
    );

    let dir = ensure_pinned_wirk_bin(&estate.path().to_string_lossy(), "run-2", &exe)
        .expect("pin the Run's runtime");
    assert_eq!(pinned_bytes(&dir), b"the original bytes\n");

    // A rebuild in place, then a rename, then a deletion — the three
    // things that actually happen to a shared `debug/wirk`.
    std::fs::write(&exe, b"rebuilt, different bytes\n").expect("rebuild in place");
    assert_eq!(pinned_bytes(&dir), b"the original bytes\n");
    let renamed = images.path().join("wirk-ccccccc-renamed");
    std::fs::rename(&exe, &renamed).expect("rename the driver image");
    assert_eq!(pinned_bytes(&dir), b"the original bytes\n");
    std::fs::remove_file(&renamed).expect("delete the driver image");
    assert_eq!(
        pinned_bytes(&dir),
        b"the original bytes\n",
        "the Run's runtime must outlive the binary it was installed from"
    );

    // Still resolvable and still executable after all of that.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.join("wirk"))
            .expect("stat the pinned wirk")
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111, "the pinned wirk must stay executable");
    }
}

/// A Run whose own `bin/wirk` is removed is restored from the image its
/// recorded digest names — the *same* bytes, never whatever the current
/// driver happens to be.
#[test]
fn a_removed_pin_is_restored_from_its_own_image_not_from_the_current_driver() {
    let estate = tempdir().expect("estate tempdir");
    let images = tempdir().expect("images tempdir");
    let first = driver_image(images.path(), "wirk-ddddddd", b"pinned at reservation\n");
    let second = driver_image(
        images.path(),
        "wirk-eeeeeee",
        b"a later, different driver\n",
    );

    let dir = ensure_pinned_wirk_bin(&estate.path().to_string_lossy(), "run-3", &first)
        .expect("pin the Run's runtime");
    std::fs::remove_file(dir.join("wirk")).expect("remove this Run's own wirk");

    let restored = ensure_pinned_wirk_bin(&estate.path().to_string_lossy(), "run-3", &second)
        .expect("a Run whose file is gone but whose image remains is restored");
    assert_eq!(restored, dir);
    assert_eq!(
        pinned_bytes(&dir),
        b"pinned at reservation\n",
        "restoration must reinstate the pinned bytes, not the attaching driver's"
    );
}

/// When the Run's own file *and* the image behind it are both gone,
/// re-pinning would silently bind the Run to a different binary
/// mid-Run. That is refused, named, and never guessed at.
#[test]
fn a_pin_that_can_no_longer_be_honoured_is_refused_rather_than_re_pinned() {
    let estate = tempdir().expect("estate tempdir");
    let images = tempdir().expect("images tempdir");
    let first = driver_image(images.path(), "wirk-fffffff", b"pinned at reservation\n");
    let second = driver_image(
        images.path(),
        "wirk-ggggggg",
        b"a later, different driver\n",
    );

    let dir = ensure_pinned_wirk_bin(&estate.path().to_string_lossy(), "run-4", &first)
        .expect("pin the Run's runtime");
    std::fs::remove_file(dir.join("wirk")).expect("remove this Run's own wirk");
    std::fs::remove_dir_all(estate.path().join(".wirk").join("runtime").join("images"))
        .expect("remove the shared image store too");

    let err = ensure_pinned_wirk_bin(&estate.path().to_string_lossy(), "run-4", &second)
        .expect_err("an unrestorable pin must be refused, not silently re-pinned");
    let message = err.to_string();
    assert!(
        message.contains("refusing to re-pin"),
        "the refusal must say what it refuses: {message}"
    );
    assert!(
        !dir.join("wirk").exists(),
        "nothing may be installed for a Run whose pin cannot be honoured"
    );
}

/// Bounded residue: two Runs launched from one driver image own one
/// installed image between them, not two copies of it.
#[test]
fn two_runs_from_one_driver_image_share_a_single_installed_image() {
    let estate = tempdir().expect("estate tempdir");
    let images = tempdir().expect("images tempdir");
    let exe = driver_image(images.path(), "wirk-hhhhhhh", b"one driver, two runs\n");

    let one =
        ensure_pinned_wirk_bin(&estate.path().to_string_lossy(), "run-5", &exe).expect("pin run-5");
    let two =
        ensure_pinned_wirk_bin(&estate.path().to_string_lossy(), "run-6", &exe).expect("pin run-6");
    assert_ne!(one, two, "each Run gets its own directory");
    assert_eq!(pinned_bytes(&one), pinned_bytes(&two));

    let store = estate.path().join(".wirk").join("runtime").join("images");
    let installed: Vec<_> = std::fs::read_dir(&store)
        .expect("read the image store")
        .map(|entry| entry.expect("dir entry").file_name())
        .collect();
    assert_eq!(
        installed.len(),
        1,
        "one distinct driver image installs exactly once: {installed:?}"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let image = std::fs::metadata(store.join(&installed[0]).join("wirk")).expect("stat image");
        let run5 = std::fs::metadata(one.join("wirk")).expect("stat run-5's wirk");
        let run6 = std::fs::metadata(two.join("wirk")).expect("stat run-6's wirk");
        assert_eq!(
            (image.dev(), image.ino()),
            (run5.dev(), run5.ino()),
            "a Run's wirk is a link to the shared image, not a second copy of its bytes"
        );
        assert_eq!((run5.dev(), run5.ino()), (run6.dev(), run6.ino()));
        assert_eq!(
            image.nlink(),
            3,
            "the image plus one link per Run — no per-Run duplication of the bytes"
        );
    }
}

// ---- through the real launch path -----------------------------------------

fn run() -> Run {
    Run {
        id: RunId("run-pin".to_string()),
        waypoint: WaypointId("route-1/wp-1".to_string()),
        attempt: 1,
        world_hash: WorldHash("deadbeef".to_string()),
        state: RunState::Open,
        kind: ActorKind::opencode(),
        selection: Default::default(),
        launched: false,
        launch_requested: false,
        launch_argv: Vec::new(),
        launch_attempt: None,
        expansions: Vec::new(),
    }
}

fn actor_world(run: &Run, estate_root: &Path) -> World {
    World::Actor(ActorWorld {
        repository: "wirk".to_string(),
        worktree_path: estate_root.join("worktrees").join("work-1"),
        branch: "p3/execution-recovery".to_string(),
        base_sha: "abc123".to_string(),
        source_basis: wirk_core::SourceBasis::Git {
            base: "abc123".to_string(),
        },
        triple: ExecutionTriple {
            estate_root: estate_root.to_string_lossy().into_owned(),
            work_id: WorkId("work-1".to_string()),
            run_id: run.id.clone(),
        },
        intent: "write report.md".to_string(),
        output_contract: OutputContract(vec![ArtifactSpec {
            name: "report.md".to_string(),
            required: true,
        }]),
        boundary: Boundary(vec!["src/**".to_string()]),
        review_targets: Vec::new(),
        evidence: None,
    })
}

fn pane_info(pane_id: &str) -> PaneInfo {
    PaneInfo {
        pane_id: pane_id.to_string(),
        terminal_id: format!("term-{pane_id}"),
        workspace_id: "w1".to_string(),
        tab_id: "tab1".to_string(),
        focused: false,
        agent_status: AgentStatus::Idle,
        revision: 1,
        agent: None,
        agent_session: None,
        cwd: None,
        display_agent: None,
        foreground_cwd: None,
        label: None,
        scroll: None,
        state_labels: None,
        terminal_title: None,
        terminal_title_stripped: None,
        title: None,
        tokens: None,
    }
}

/// The whole point of item 1's correction: an unfulfilled pin refuses
/// the launch *before* a pane exists. The prior version printed a line
/// and launched the actor anyway, under the very `PATH` ambiguity the
/// item exists to close.
#[test]
fn an_unfulfillable_pin_refuses_the_actor_launch_before_any_pane_is_created() {
    let dir = tempdir().expect("tempdir");
    // An estate root that cannot be created: its parent is a file.
    let blocker = dir.path().join("not-a-directory");
    std::fs::write(&blocker, b"a file where a directory would have to be\n").expect("write file");
    let estate_root = blocker.join("estate");

    let run = run();
    let world = actor_world(&run, &estate_root);
    let (_tx, rx) = mpsc::channel();
    let client = std::sync::Arc::new(
        FakeHerdrClient::default()
            .with_split_pane_response(pane_info(&run.id.0))
            .with_subscribe_channel(rx),
    );
    let executor = HerdrExecutor::new(client.clone());

    let err = executor
        .launch_actor(&run, &world)
        .expect_err("an unpinnable Run must not launch an actor");
    let message = err.to_string();
    assert!(
        message.contains("refusing to launch this actor"),
        "the refusal must name itself as one: {message}"
    );
    assert!(
        client.split_pane_calls.lock().unwrap().is_empty(),
        "no pane may be created for a Run whose runtime could not be pinned"
    );
    assert!(
        client.start_agent_calls.lock().unwrap().is_empty(),
        "and certainly no agent started"
    );
}

/// The same stability property, expressed only through the public
/// launch path so it can be (and was) run against the *true old
/// source*: two launches for one Run must leave the Run's own `wirk`
/// the same file — same inode, same bytes, same mtime. The prior
/// implementation removed and re-copied it on every call, so the second
/// launch replaced the file the first launch's actor may already be
/// executing.
#[test]
fn a_second_launch_for_one_run_leaves_its_pinned_wirk_the_very_same_file() {
    let estate = tempdir().expect("estate tempdir");
    let run = run();
    let world = actor_world(&run, estate.path());

    let launch_once = || {
        let (_tx, rx) = mpsc::channel();
        let client = std::sync::Arc::new(
            FakeHerdrClient::default()
                .with_split_pane_response(pane_info(&run.id.0))
                .with_subscribe_channel(rx),
        );
        HerdrExecutor::new(client)
            .launch_actor(&run, &world)
            .expect("launch");
    };

    launch_once();
    let pinned = estate
        .path()
        .join(".wirk")
        .join("runtime")
        .join(&run.id.0)
        .join("bin")
        .join("wirk");
    let before = std::fs::metadata(&pinned).expect("stat the pinned wirk after the first launch");
    let before_bytes = std::fs::read(&pinned).expect("read the pinned wirk");

    launch_once();

    let after = std::fs::metadata(&pinned).expect("stat the pinned wirk after the second launch");
    assert_eq!(
        std::fs::read(&pinned).expect("read the pinned wirk again"),
        before_bytes,
        "a reattach must not change this Run's runtime bytes"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            (before.dev(), before.ino()),
            (after.dev(), after.ino()),
            "a reattach must not replace this Run's runtime file: the actor may be executing it"
        );
    }
    assert_eq!(
        before.modified().expect("mtime"),
        after.modified().expect("mtime"),
        "an already-pinned Run's runtime is not rewritten at all"
    );
}
