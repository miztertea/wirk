//! P4.5 B correction (ruling 0251, F1/F2/F4): who may see and stop a
//! running expensive Atlas job, and what a caller is told about the
//! shared capacity it is actually running under.
//!
//! Real daemon, real `git`, real `wirk work submit`, real CLI — never a
//! library call for anything the CLI exposes, the discipline
//! `nested_work.rs` and `wirkd_process.rs` already keep.
//!
//! **What holds a job open here, and why it is not a substitute for the
//! product.** A registered job exists for as long as its backend child
//! runs, so these checks need a child that stays. The backend is a
//! blocking script: every line of product under test — admission, the
//! job registry, requester binding, the cancel token, the child kill and
//! the cleanup — is the real thing, and only the embedding model is
//! absent, which none of these assertions is about. The corresponding
//! real offline Semble build/query, cancellation while the atlas is held
//! and successful subsequent work are executed through the same public
//! CLI in this work's REPORT.md; ruling 0040's "a fake pins a shape at
//! most" is why both exist rather than either alone.
//!
//! The decisive shape throughout is **one running job, several askers**:
//! an estate runs one expensive job at a time (one atlas mutex, and
//! `max_expensive` defaults to 1), so "two Works' jobs at once" is not a
//! state this product reaches. What is asked instead is the question
//! that matters — with someone else's job running, what can a Work that
//! is not its requester see and do?

#[path = "support/nested_harness.rs"]
mod harness;
#[path = "support/route_fixture.rs"]
mod route_fixture;
#[path = "../src/wirkd/mod.rs"]
mod wirkd;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use harness::*;

/// Every CLI call here is an **operator's**, with this test process's
/// own injected execution triple removed.
///
/// A test binary run inside a Wirk pane inherits `WIRK_ESTATE_ROOT`/
/// `WIRK_WORK_ID`/`WIRK_RUN_ID`, and under ruling 0117 those now decide
/// the default scope of the very verbs under test. Leaving them in
/// would make these results depend on who ran the suite.
fn cli() -> Command {
    let mut command = Command::new(wirk_bin());
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

/// A backend that blocks forever instead of speaking the embedding
/// protocol: the job is admitted, registered and killable, which is
/// every part of it these checks are about.
fn blocking_backend(dir: &Path) -> PathBuf {
    let path = dir.join("blocking-backend.sh");
    fs::write(&path, "#!/bin/sh\nexec sleep 600\n").expect("write backend");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod backend");
    }
    path
}

struct Fixture {
    _dir: tempfile::TempDir,
    estate: PathBuf,
    repo: PathBuf,
    backend: PathBuf,
    model: PathBuf,
    generation: String,
}

/// A real estate with a real registered source and one staged
/// generation — everything an `atlas semantic build` needs before it
/// can start a job.
fn fixture(dir: tempfile::TempDir) -> (Fixture, KillOnDrop) {
    let estate = dir.path().join("estate");
    fs::create_dir_all(estate.join(".wirk")).expect("create estate");
    // Its own shared pool. The default is this *user's* pool, shared
    // with every other wirk on the box — including the other tests in
    // this run — so a fixture that used it would be bounded by, and
    // would bound, work it has nothing to do with. `bounded_jobs.rs`
    // isolates the same way and for the same reason.
    let pool = dir.path().join("host-pool");
    fs::write(
        estate.join(".wirk").join("resources.json"),
        format!(
            "{{\"host_pool_dir\": {:?}, \"max_host_expensive\": 2}}\n",
            pool.to_str().expect("pool path")
        ),
    )
    .expect("write resources.json");
    route_fixture::install_route_fixture(&estate, "smoke");
    route_fixture::install_route_fixture(&estate, "wa_container_child_role");
    route_fixture::install_route_fixture(&estate, "wa_simple_leaf");
    let (wirkd, _pointer) = start_wirkd(&estate);

    let repo = dir.path().join("source-repo");
    init_repo(&repo);
    // Real indexable content: a generation with nothing the extractor
    // admits has no resource to embed, and the build refuses before it
    // ever starts a job.
    write_file(&repo, "alpha.rs", "fn alpha() {}\n");
    write_file(&repo, "README.md", "# fixture\n\nalpha\n");
    commit_all(&repo, "content");

    let model = dir.path().join("model");
    fs::create_dir_all(&model).expect("create model dir");

    let acquired = json(&[
        "atlas",
        "acquire",
        "--estate",
        estate.to_str().unwrap(),
        "--source",
        "fx",
        "--repository",
        repo.to_str().unwrap(),
        "--revision",
        "HEAD",
        "--json",
    ]);
    assert_eq!(acquired["outcome"], "staged", "acquire staged: {acquired}");
    let generation = acquired["generation"]["generation"]
        .as_str()
        .expect("generation id")
        .to_string();

    (
        Fixture {
            backend: blocking_backend(dir.path()),
            _dir: dir,
            estate,
            repo,
            model,
            generation,
        },
        wirkd,
    )
}

fn commit_all(repo: &Path, message: &str) {
    let status = Command::new("git")
        .args(["add", "-A"])
        .current_dir(repo)
        .status()
        .expect("git add runs");
    assert!(status.success());
    let status = Command::new("git")
        .args([
            "-c",
            "user.name=job-authority-test",
            "-c",
            "user.email=job-authority@example.test",
            "commit",
            "-q",
            "-m",
            message,
        ])
        .current_dir(repo)
        .status()
        .expect("git commit runs");
    assert!(status.success());
}

impl Fixture {
    fn estate_arg(&self) -> &str {
        self.estate.to_str().unwrap()
    }

    /// Start a real `atlas semantic build` as `work` (or
    /// administratively when `None`) and return once its job is
    /// registered.
    fn start_job(&self, work: Option<&str>) -> (Child, String) {
        let mut command = cli();
        command
            .args(["atlas", "semantic", "build", "--estate"])
            .arg(&self.estate)
            .args(["--source", "fx", "--generation", &self.generation])
            .arg("--backend")
            .arg(&self.backend)
            .arg("--model")
            .arg(&self.model)
            .args(["--json"]);
        match work {
            Some(work) => command.args(["--requesting-work", work]),
            None => command.arg("--admin"),
        };
        let log = self.estate.parent().unwrap().join("build.log");
        let sink = fs::File::create(&log).expect("create build log");
        let child = command
            .stdout(sink.try_clone().expect("clone build log"))
            .stderr(sink)
            .spawn()
            .expect("spawn semantic build");
        let job = self.wait_for_job(&log);
        (child, job)
    }

    /// The administrative listing is how the test learns the job id —
    /// deliberately, because a scoped caller must never be able to.
    fn wait_for_job(&self, log: &Path) -> String {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let listed = self.admin_list();
            if let Some(job) = listed["running"].as_array().and_then(|jobs| jobs.first()) {
                return job["job_id"].as_str().expect("job id").to_string();
            }
            assert!(
                Instant::now() < deadline,
                "no expensive job registered within 30s: {listed}; build said: {:?}",
                fs::read_to_string(log).unwrap_or_default()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn admin_list(&self) -> serde_json::Value {
        json(&[
            "atlas",
            "cancel",
            "--estate",
            self.estate_arg(),
            "--admin",
            "--list",
            "--json",
        ])
    }

    fn list_as(&self, work: &str) -> serde_json::Value {
        json(&[
            "atlas",
            "cancel",
            "--estate",
            self.estate_arg(),
            "--requesting-work",
            work,
            "--list",
            "--json",
        ])
    }

    fn cancel_as(&self, work: &str, target: &[&str]) -> serde_json::Value {
        let mut args = vec![
            "atlas",
            "cancel",
            "--estate",
            self.estate_arg(),
            "--requesting-work",
            work,
        ];
        args.extend_from_slice(target);
        args.push("--json");
        json(&args)
    }

    /// Stop everything still running, administratively, and reap.
    fn drain(&self, mut child: Child) {
        let _ = run(&[
            "atlas",
            "cancel",
            "--estate",
            self.estate_arg(),
            "--admin",
            "--all",
            "--wait",
            "20",
            "--json",
        ]);
        let _ = child.wait();
    }

    fn submit_work(&self) -> Submitted {
        let repo = self.repo.parent().unwrap().join(format!(
            "work-repo-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        init_repo(&repo);
        submit(&self.estate, "smoke", &repo, &["fx:read"], None).expect("submit work")
    }
}

/// **The defect, exactly as it was found.** A Work bound to nothing but
/// its own narrow contract could list every running job in the estate
/// *with its source alias*, and stop all of them with `--all`. Before
/// this correction `atlas cancel` had no `--work` at all and
/// `handle_atlas_cancel` never resolved a scope: the gate was the socket
/// mode, which is uid, not Work.
///
/// The unrelated Work here is bound to the **same source alias** as the
/// job's owner. Sharing a source is not shared authority over each
/// other's running work, and this is the case a source-based rule would
/// have got wrong.
#[test]
fn an_unrelated_work_can_neither_see_nor_stop_another_works_job() {
    let (fx, wirkd) = fixture(tempfile::tempdir().expect("tempdir"));
    let owner = fx.submit_work();
    let stranger = fx.submit_work();
    let (build, job) = fx.start_job(Some(&owner.work_id));

    let listed = fx.list_as(&stranger.work_id);
    assert_eq!(listed["scope"], "requester");
    assert_eq!(
        listed["running"].as_array().expect("running array").len(),
        0,
        "a stranger enumerated another Work's job (and its source alias): {listed}"
    );

    // Naming the job exactly is answered as a miss — the same answer an
    // id that was never issued gets, so the verb is no existence oracle.
    // A foreign job and a job id that was never issued must be
    // indistinguishable. The reply echoes the caller's own target back —
    // which the caller already knows — so what must match is everything
    // else, and the echo must be the caller's own words rather than
    // anything read out of the registry.
    let named = fx.cancel_as(&stranger.work_id, &["--job", &job]);
    let invented = fx.cancel_as(&stranger.work_id, &["--job", "01NOTAREALJOBIDATALL00000"]);
    for (reply, asked) in [
        (&named, job.as_str()),
        (&invented, "01NOTAREALJOBIDATALL00000"),
    ] {
        assert_eq!(reply["outcome"], "no_match", "{reply}");
        assert_eq!(reply["acknowledged"].as_array().expect("array").len(), 0);
        assert_eq!(
            reply["detail"].as_str().expect("detail"),
            format!(
                "nothing running in this estate matches job {asked}; it may have already ended"
            ),
            "a miss said something the caller had not already said: {reply}"
        );
    }
    assert_eq!(
        named["outcome"], invented["outcome"],
        "a foreign job and a nonexistent one must be indistinguishable"
    );

    // The alias this stranger *is* bound to, and "everything".
    for target in [vec!["--source", "fx"], vec!["--all"]] {
        let refused = fx.cancel_as(&stranger.work_id, &target);
        assert_eq!(refused["outcome"], "no_match", "{target:?}: {refused}");
        assert_eq!(
            refused["acknowledged"].as_array().expect("array").len(),
            0,
            "{target:?} acknowledged something: {refused}"
        );
    }

    // Foreign work is genuinely untouched, not merely unreported.
    let admin = fx.admin_list();
    let running = admin["running"].as_array().expect("running array");
    assert_eq!(running.len(), 1, "{admin}");
    assert_eq!(running[0]["job_id"], job.as_str());
    assert_eq!(
        running[0]["cancel_signalled"], false,
        "a denied request still signalled the job: {admin}"
    );

    fx.drain(build);
    stop_wirkd(&fx.estate, wirkd);
}

/// The other half: scoping must leave the legitimate caller able to do
/// its own work. A correction that made every actor useless would pass
/// the negative above and fail the product.
#[test]
fn a_work_sees_and_stops_its_own_job_and_signalling_is_not_stopping() {
    let (fx, wirkd) = fixture(tempfile::tempdir().expect("tempdir"));
    let owner = fx.submit_work();
    let (build, job) = fx.start_job(Some(&owner.work_id));

    let listed = fx.list_as(&owner.work_id);
    let running = listed["running"].as_array().expect("running array");
    assert_eq!(
        running.len(),
        1,
        "the owner cannot see its own job: {listed}"
    );
    assert_eq!(running[0]["job_id"], job.as_str());

    // Acknowledgement and completion stay two separate answers.
    let signalled = fx.cancel_as(&owner.work_id, &["--job", &job]);
    assert_eq!(signalled["outcome"], "signalled", "{signalled}");
    assert!(
        signalled["completed"].is_null(),
        "completion was claimed without being observed: {signalled}"
    );
    let after = fx.list_as(&owner.work_id);
    assert_eq!(after["running"][0]["cancel_signalled"], true, "{after}");

    let completed = fx.cancel_as(&owner.work_id, &["--all", "--wait", "20"]);
    assert_eq!(completed["outcome"], "completed", "{completed}");
    assert_eq!(
        completed["still_running"].as_array().expect("array").len(),
        0
    );
    assert_eq!(
        fx.admin_list()["running"].as_array().expect("array").len(),
        0,
        "the registry did not empty"
    );

    fx.drain(build);
    stop_wirkd(&fx.estate, wirkd);
}

/// A container Work submitted its child and granted it what it has.
/// Being unable to stop the work it caused would make the scope useless
/// for the case it most obviously needs — and this authority is read
/// from the child's own journaled parent binding, never from anything
/// the caller says about itself.
#[test]
fn a_parent_work_may_control_the_job_of_a_work_it_submitted() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (fx, wirkd) = fixture(dir);
    let parent_repo = fx.repo.parent().unwrap().join("parent-repo");
    init_repo(&parent_repo);
    let parent = submit(
        &fx.estate,
        "wa_container_child_role",
        &parent_repo,
        &["demo:write", "fx:read"],
        None,
    )
    .expect("submit parent");
    write_file(
        &fx.estate.join("worktrees").join(&parent.work_id),
        "a.md",
        "a\n",
    );
    claim_ok(&fx.estate, &parent.work_id, &parent.run_id, "a.md=a.md");

    let child_repo = fx.repo.parent().unwrap().join("child-repo");
    init_repo(&child_repo);
    let child = submit(
        &fx.estate,
        "wa_simple_leaf",
        &child_repo,
        &["fx:read"],
        Some(ParentRef {
            work: &parent.work_id,
            waypoint: "outer",
            run: &parent.run_id,
            role: "helper",
            attempt: None,
        }),
    )
    .expect("submit child");

    let (build, job) = fx.start_job(Some(&child.work_id));
    let listed = fx.list_as(&parent.work_id);
    let running = listed["running"].as_array().expect("running array");
    assert_eq!(
        running.len(),
        1,
        "a parent cannot see the job of the child it submitted: {listed}"
    );
    assert_eq!(running[0]["job_id"], job.as_str());

    let cancelled = fx.cancel_as(&parent.work_id, &["--job", &job, "--wait", "20"]);
    assert_eq!(cancelled["outcome"], "completed", "{cancelled}");

    // And not the other way around: a child has no authority over its
    // parent, so the relationship is directed, not mutual.
    let (build2, job2) = fx.start_job(Some(&parent.work_id));
    let upward = fx.cancel_as(&child.work_id, &["--job", &job2]);
    assert_eq!(
        upward["outcome"], "no_match",
        "a child reached upward into its parent's job: {upward}"
    );
    fx.drain(build2);
    fx.drain(build);
    stop_wirkd(&fx.estate, wirkd);
}

/// An unknown, incomplete or mismatched identity must refuse. The one
/// thing it must never do is resolve to the wider, administrative
/// answer, which is how the original defect was reachable by simply
/// omitting a flag.
#[test]
fn an_unknown_or_mismatched_requester_never_falls_back_to_administration() {
    let (fx, wirkd) = fixture(tempfile::tempdir().expect("tempdir"));
    let owner = fx.submit_work();
    let (build, _job) = fx.start_job(Some(&owner.work_id));

    let (ok, stdout, stderr) = run(&[
        "atlas",
        "cancel",
        "--estate",
        fx.estate_arg(),
        "--requesting-work",
        "work-that-was-never-submitted",
        "--all",
        "--json",
    ]);
    assert!(!ok, "an unknown Work was served: {stdout}");
    assert!(stderr.contains("NotFound"), "{stderr}");
    assert!(
        !stdout.contains("acknowledged"),
        "an unknown Work reached the registry: {stdout}"
    );

    // A half-injected environment names no identity, and reading it as
    // "no context" would buy exactly the wider answer.
    let partial = cli()
        .env("WIRK_ESTATE_ROOT", fx.estate_arg())
        .args(["atlas", "cancel", "--estate", fx.estate_arg(), "--list"])
        .output()
        .expect("wirk runs");
    assert!(!partial.status.success(), "a partial context was served");
    let partial_err = String::from_utf8_lossy(&partial.stderr);
    assert!(
        partial_err.contains("incomplete"),
        "expected an incomplete-context refusal, got: {partial_err}"
    );

    // Inside a complete context, asking as somebody else is refused —
    // the scoped read here is asked as this actor's own Work.
    let mismatched = cli()
        .env("WIRK_ESTATE_ROOT", fx.estate_arg())
        .env("WIRK_WORK_ID", &owner.work_id)
        .env("WIRK_RUN_ID", &owner.run_id)
        .args([
            "atlas",
            "cancel",
            "--estate",
            fx.estate_arg(),
            "--requesting-work",
            "work-somebody-else",
            "--list",
        ])
        .output()
        .expect("wirk runs");
    assert!(!mismatched.status.success());
    let mismatched_err = String::from_utf8_lossy(&mismatched.stderr);
    assert!(
        mismatched_err.contains("other than this actor's own"),
        "{mismatched_err}"
    );

    fx.drain(build);
    stop_wirkd(&fx.estate, wirkd);
}

/// An actor that types the obvious command gets its **own** scope, and
/// the operator's administrative surface is reached only by asking for
/// it. This is the omission path the whole correction is about, and it
/// covers `wirkd ping` too: `ping`'s resource answer names every running
/// job's source alias, so leaving it unscoped would have left the fix
/// cosmetic.
#[test]
fn an_actor_context_scopes_cancel_and_ping_while_admin_stays_explicit() {
    let (fx, wirkd) = fixture(tempfile::tempdir().expect("tempdir"));
    let owner = fx.submit_work();
    let stranger = fx.submit_work();
    let (build, job) = fx.start_job(Some(&owner.work_id));

    let as_actor = |work: &Submitted, args: &[&str]| -> (bool, String, String) {
        let mut command = cli();
        command
            .env("WIRK_ESTATE_ROOT", fx.estate_arg())
            .env("WIRK_WORK_ID", &work.work_id)
            .env("WIRK_RUN_ID", &work.run_id)
            .args(args);
        let output = command.output().expect("wirk runs");
        (
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).to_string(),
            String::from_utf8_lossy(&output.stderr).to_string(),
        )
    };

    // The stranger's obvious command, with no flag naming any scope.
    let (ok, stdout, _) = as_actor(
        &stranger,
        &[
            "atlas",
            "cancel",
            "--estate",
            fx.estate_arg(),
            "--list",
            "--json",
        ],
    );
    assert!(ok, "{stdout}");
    let listed: serde_json::Value = serde_json::from_str(stdout.trim()).expect("json");
    assert_eq!(listed["scope"], "requester", "{listed}");
    assert_eq!(listed["running"].as_array().expect("array").len(), 0);

    // `ping`'s job listing follows the same scope.
    let (ok, stdout, _) = as_actor(&stranger, &["wirkd", "ping", "--estate", fx.estate_arg()]);
    assert!(ok, "{stdout}");
    assert!(
        stdout.contains("running jobs (requester): 0"),
        "ping leaked another Work's job to a stranger: {stdout}"
    );
    assert!(
        !stdout.contains(&job),
        "ping named a foreign job id: {stdout}"
    );
    let (ok, stdout, _) = as_actor(&owner, &["wirkd", "ping", "--estate", fx.estate_arg()]);
    assert!(ok);
    assert!(
        stdout.contains("running jobs (requester): 1") && stdout.contains(&job),
        "the owner cannot see its own job through ping: {stdout}"
    );

    // Administration is preserved, and reached by naming it.
    let (ok, stdout, stderr) = as_actor(
        &stranger,
        &["wirkd", "ping", "--estate", fx.estate_arg(), "--admin"],
    );
    assert!(ok);
    assert!(
        stdout.contains("running jobs (administrative): 1") && stdout.contains(&job),
        "the administrative read lost its estate-wide answer: {stdout}"
    );
    assert!(
        stderr.contains("--admin named"),
        "an actor's administrative override was not said out loud: {stderr}"
    );

    // `ping` is a health check: a caller whose context resolves no scope
    // still learns whether the daemon is alive and what it can enforce.
    // The one Work-shaped field is withheld with its reason, never
    // widened to the administrative listing and never silently empty.
    let unresolvable = cli()
        .env("WIRK_ESTATE_ROOT", "/nonexistent/other/estate")
        .env("WIRK_WORK_ID", &stranger.work_id)
        .env("WIRK_RUN_ID", &stranger.run_id)
        .args(["wirkd", "ping", "--estate", fx.estate_arg()])
        .output()
        .expect("wirk runs");
    assert!(
        unresolvable.status.success(),
        "a health check was refused over a Work scope: {}",
        String::from_utf8_lossy(&unresolvable.stderr)
    );
    let health = String::from_utf8_lossy(&unresolvable.stdout);
    assert!(health.contains("protocol_version"), "{health}");
    assert!(
        health.contains("running jobs: withheld"),
        "the job listing was not withheld: {health}"
    );
    assert!(
        !health.contains(&job),
        "an unresolvable scope still saw a job: {health}"
    );

    fx.drain(build);
    stop_wirkd(&fx.estate, wirkd);
}

/// An administratively started job belongs to the operator. Read access
/// to a source is not permission to interrupt, and an unattributed job
/// must not become everyone's to stop.
#[test]
fn an_administrative_job_is_invisible_and_untouchable_to_every_bound_caller() {
    let (fx, wirkd) = fixture(tempfile::tempdir().expect("tempdir"));
    let work = fx.submit_work();
    let (build, job) = fx.start_job(None);

    let listed = fx.list_as(&work.work_id);
    assert_eq!(
        listed["running"].as_array().expect("array").len(),
        0,
        "{listed}"
    );
    let attempted = fx.cancel_as(&work.work_id, &["--all"]);
    assert_eq!(attempted["outcome"], "no_match", "{attempted}");

    let admin = fx.admin_list();
    assert_eq!(admin["running"][0]["job_id"], job.as_str());
    assert_eq!(admin["running"][0]["cancel_signalled"], false, "{admin}");

    fx.drain(build);
    stop_wirkd(&fx.estate, wirkd);
}

/// The requester check must not move cancellation behind the mutex it
/// exists to reach past. A build holds the atlas for its whole run; a
/// cheap read waits and then answers `AtlasBusy`, while the scoped
/// cancellation answers immediately — from the registry beside that
/// mutex, and from journals, neither of which the build holds.
#[test]
fn a_requester_check_keeps_cancellation_reachable_while_the_atlas_is_held() {
    let (fx, wirkd) = fixture(tempfile::tempdir().expect("tempdir"));
    let owner = fx.submit_work();
    let (build, _job) = fx.start_job(Some(&owner.work_id));

    // Proof the mutex is genuinely held at this instant.
    // `atlas status` takes its scope as `--work`; omitted (and with no
    // injected triple) it is the administrative read.
    let (ok, _, stderr) = run(&["atlas", "status", "--estate", fx.estate_arg(), "--json"]);
    assert!(!ok, "a cheap read succeeded, so the atlas was not held");
    assert!(stderr.contains("AtlasBusy"), "{stderr}");

    let started = Instant::now();
    let listed = fx.list_as(&owner.work_id);
    let elapsed = started.elapsed();
    assert_eq!(listed["running"].as_array().expect("array").len(), 1);
    // The cheap read above already waited out `cheap_wait_millis`
    // (250ms by default) before refusing. A scoped cancellation that
    // had been moved behind the atlas could not beat that.
    assert!(
        elapsed < Duration::from_millis(250),
        "a scoped cancellation queued behind the build: {elapsed:?}"
    );

    fx.drain(build);
    stop_wirkd(&fx.estate, wirkd);
}
