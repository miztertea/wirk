//! P4.5 B (ruling 0237): bounded, owned resource jobs.
//!
//! These run **real processes** and take **real locks**. The containment
//! checks spawn a child that deliberately `setsid()`s a grandchild — the
//! stand-in for a backend runtime that daemonises — because that is the
//! exact case a process group cannot reach, and the case the whole
//! cgroup mechanism exists for.
//!
//! Where this host cannot supply a facility, the check asserts the
//! **documented fallback** rather than being skipped: a bound that
//! silently disappears on an unsupported platform is worse than no
//! bound, and "unsupported" must be a described, tested state.
//!
//! Nothing here stresses this machine. Limits are configured small so a
//! refusal can be provoked with one extra slot-holder, never by consuming
//! host memory.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};
use tempfile::TempDir;
use wirk_core::jobs::{
    self, BoundedChild, CancelToken, ChildEnd, JobCapabilities, JobClass, JobRequest,
    ResourcePolicy,
};

fn estate() -> TempDir {
    let dir = TempDir::new().unwrap();
    fs::create_dir_all(dir.path().join(".wirk")).unwrap();
    dir
}

/// A policy with everything relaxed except what the check is about, so a
/// refusal can only come from the bound under test.
///
/// `host_pool_dir` is given explicitly for a real reason, not to weaken
/// the check: the default per-user pool is genuinely shared by every wirk
/// process this uid runs, so two of these checks — or a build in another
/// terminal — would contend for the same slots. Pointing them at their
/// own directory keeps the *mechanism* exactly the same (a real `flock`
/// on a real shared file, taken by two independent estates) while making
/// the check about this code rather than about what else is running.
fn permissive() -> ResourcePolicy {
    ResourcePolicy {
        memory_pressure_avg10_max: f64::MAX,
        min_available_memory_bytes: 0,
        host_pool_dir: Some(host_pool()),
        ..ResourcePolicy::default()
    }
}

/// One private host pool per check, shared between the estates that check
/// uses.
///
/// `libtest` runs each `#[test]` on its own freshly spawned thread and
/// joins it before moving on, so a `thread_local` `TempDir` gives each
/// check its own real pool directory, stable across every call within
/// that one test (still one path per check), and — unlike the former
/// `std::env::temp_dir()` directory this created and never removed —
/// actually dropped when the test's own thread exits: ordinary RAII,
/// unwind included, the same fix ruling 0308 already applied to
/// `run_loop.rs`'s and `contracts.rs`'s fixture estates. A check that
/// shares one pool between several estates deliberately does so by
/// naming `host_pool_dir` explicitly, which overrides the one
/// `permissive()` mints here.
fn host_pool() -> PathBuf {
    thread_local! {
        static POOL: TempDir = TempDir::new().expect("host pool tempdir");
    }
    POOL.with(|dir| dir.path().to_path_buf())
}

// ---------------------------------------------------------------------
// Admission
// ---------------------------------------------------------------------

#[test]
fn a_second_expensive_job_is_refused_visibly_and_the_slot_returns_on_release() {
    let estate = estate();
    let policy = ResourcePolicy {
        max_expensive: 1,
        max_host_expensive: 4,
        ..permissive()
    };
    let first = jobs::admit(
        estate.path(),
        &policy,
        &JobRequest::new("atlas semantic build", "fixture"),
    )
    .expect("the first expensive job is admitted");

    let refusal = jobs::admit(
        estate.path(),
        &policy,
        &JobRequest::new("atlas acquire", "fixture"),
    )
    .expect_err("the second must be refused, not queued invisibly");
    assert_eq!(refusal.code, "ExpensiveJobBusy");
    assert!(
        refusal.message.contains("atlas acquire"),
        "the refusal names the verb that was refused: {refusal}"
    );
    assert!(
        refusal.message.contains("held for"),
        "the refusal names how long the holder has had it: {refusal}"
    );

    // Negative control: the bound is a bound, not a one-way door.
    drop(first);
    let again = jobs::admit(
        estate.path(),
        &policy,
        &JobRequest::new("atlas acquire", "fixture"),
    );
    assert!(
        again.is_ok(),
        "releasing the slot must admit the next job: {:?}",
        again.err()
    );
}

/// A released admission frees its slot even while a child process
/// spawned by an unrelated thread still holds a duplicate of the lock's
/// descriptor.
///
/// `flock(2)` belongs to the **open file description**, not to the
/// descriptor: a lock released only by closing the file stays held until
/// every inherited duplicate is closed too. Every wirk job child is
/// spawned through [`jobs::harden_execution_child`], which installs a
/// `pre_exec` hook and so forces the real `fork`+`exec` path — for the
/// whole window between the two, the child holds a copy of every
/// descriptor this process had open, including another thread's
/// still-held admission. Under real `cargo test` parallelism that window
/// is hit, and the slot a check had just dropped did not come back: the
/// refusal named the check's *own* already-released holder.
///
/// The fork here is that window, made deterministic rather than raced.
#[test]
fn a_released_slot_returns_even_while_a_forked_child_holds_the_descriptor() {
    let estate = estate();
    let policy = ResourcePolicy {
        max_expensive: 1,
        max_host_expensive: 4,
        ..permissive()
    };
    let first = jobs::admit(
        estate.path(),
        &policy,
        &JobRequest::new("atlas semantic build", "fixture"),
    )
    .expect("the first expensive job is admitted");

    // The window an unrelated concurrent job spawn opens. SAFETY: the
    // child only `pause`s and `_exit`s, both async-signal-safe, which is
    // all a forked child of a threaded process may do before `exec`.
    let child = unsafe { libc::fork() };
    assert!(
        child >= 0,
        "fork failed: {}",
        std::io::Error::last_os_error()
    );
    if child == 0 {
        unsafe {
            libc::pause();
            libc::_exit(0);
        }
    }

    drop(first);
    let again = jobs::admit(
        estate.path(),
        &policy,
        &JobRequest::new("atlas acquire", "fixture"),
    );

    // Reap before asserting, so a failure never leaks the child.
    unsafe {
        libc::kill(child, libc::SIGKILL);
        let mut status = 0;
        libc::waitpid(child, &mut status, 0);
    }

    assert!(
        again.is_ok(),
        "releasing the slot must admit the next job even while a forked \
         child still holds the descriptor: {:?}",
        again.err()
    );
}

#[test]
fn a_bounded_wait_admits_when_the_holder_leaves_and_refuses_when_it_does_not() {
    let estate = estate();
    let policy = ResourcePolicy {
        max_expensive: 1,
        max_host_expensive: 4,
        admission_wait_secs: 5,
        ..permissive()
    };
    let holder = jobs::admit(estate.path(), &policy, &JobRequest::new("build", "a")).unwrap();
    let root = estate.path().to_path_buf();
    // The waiting thread must share this check's pool, not mint its own.
    let shared = policy.clone();
    let waiter = std::thread::spawn(move || {
        let policy = shared;
        let started = Instant::now();
        (
            jobs::admit(&root, &policy, &JobRequest::new("build", "b")),
            started.elapsed(),
        )
    });
    std::thread::sleep(Duration::from_millis(300));
    drop(holder);
    let (outcome, waited) = waiter.join().unwrap();
    assert!(
        outcome.is_ok(),
        "a bounded wait must admit once the holder leaves: {:?}",
        outcome.err()
    );
    assert!(
        waited >= Duration::from_millis(250),
        "it really waited rather than racing in first: {waited:?}"
    );

    // And the same wait is bounded: a holder that stays produces a
    // refusal, not an indefinite block.
    let _stays = jobs::admit(estate.path(), &policy, &JobRequest::new("build", "c"));
    let short = ResourcePolicy {
        admission_wait_secs: 1,
        ..policy.clone()
    };
    let started = Instant::now();
    let refusal = jobs::admit(estate.path(), &short, &JobRequest::new("build", "d"))
        .expect_err("a holder that stays must produce a refusal");
    assert!(refusal.message.contains("Waited the configured 1s"));
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the wait must be bounded, took {:?}",
        started.elapsed()
    );
}

/// The host pool bounds this *user* across estates — and its refusal must
/// not disclose the other estate's private paths. A slot file shared
/// between estates is exactly where a leak would happen.
#[test]
fn the_host_pool_bounds_across_estates_without_disclosing_the_other_estate() {
    let one = estate();
    let two = estate();
    let secret_source = "confidential-client-alias";
    let policy = ResourcePolicy {
        max_expensive: 4,
        max_host_expensive: 1,
        ..permissive()
    };
    let _held = jobs::admit(
        one.path(),
        &policy,
        &JobRequest::new("atlas semantic build", secret_source),
    )
    .expect("first estate admitted");

    let refusal = jobs::admit(
        two.path(),
        &policy,
        &JobRequest::new("atlas semantic build", "other"),
    )
    .expect_err("a second estate must not exceed this user's host bound");
    assert_eq!(refusal.code, "HostExpensiveBusy");
    assert!(
        !refusal.message.contains(secret_source),
        "the host refusal leaked the other estate's source alias: {refusal}"
    );
    assert!(
        !refusal.message.contains(&one.path().display().to_string()),
        "the host refusal leaked the other estate's path: {refusal}"
    );
    assert!(
        refusal.message.contains("advisory") && refusal.message.contains("this uid"),
        "the host bound states its own scope honestly: {refusal}"
    );
}

/// One consistent capacity policy: a per-estate N above the host pool
/// must not become a way around it, and the clamp must be disclosed
/// rather than applied silently.
#[test]
fn a_larger_per_estate_limit_cannot_silently_bypass_the_host_pool() {
    let policy = ResourcePolicy {
        max_expensive: 8,
        max_host_expensive: 2,
        ..permissive()
    };
    assert_eq!(
        policy.effective_estate_slots(),
        2,
        "the estate is offered no more slots than the host pool can honour"
    );
    let note = policy.capacity_note().expect("the clamp is disclosed");
    assert!(note.contains('8') && note.contains('2'), "{note}");

    // And it really bounds: with host 1, the second job in *one* estate
    // is refused even though max_expensive says 8.
    let estate = estate();
    let policy = ResourcePolicy {
        max_expensive: 8,
        max_host_expensive: 1,
        ..permissive()
    };
    let _first = jobs::admit(estate.path(), &policy, &JobRequest::new("build", "a")).unwrap();
    let refusal = jobs::admit(estate.path(), &policy, &JobRequest::new("build", "b"))
        .expect_err("max_expensive 8 must not admit past a host bound of 1");
    assert_eq!(refusal.code, "ExpensiveJobBusy");
}

/// Materialization is a separate class on purpose: folding it into the
/// single expensive slot would serialize every concurrent Run in an
/// estate, which is ordinary supported behaviour.
#[test]
fn materialization_is_bounded_separately_from_expensive_work() {
    let estate = estate();
    let policy = ResourcePolicy {
        max_expensive: 1,
        max_host_expensive: 1,
        max_materialization: 2,
        ..permissive()
    };
    let _build = jobs::admit(
        estate.path(),
        &policy,
        &JobRequest::new("atlas semantic build", "a"),
    )
    .expect("the one expensive slot");

    // Two materializations still proceed beside it.
    let _first = jobs::admit(
        estate.path(),
        &policy,
        &JobRequest::new("worktree materialization", "work-a").in_class(JobClass::Materialization),
    )
    .expect("materialization does not take the expensive slot");
    let _second = jobs::admit(
        estate.path(),
        &policy,
        &JobRequest::new("worktree materialization", "work-b").in_class(JobClass::Materialization),
    )
    .expect("two concurrent Works materialize");

    // But it is still bounded, and refuses by its own name.
    let refusal = jobs::admit(
        estate.path(),
        &policy,
        &JobRequest::new("worktree materialization", "work-c").in_class(JobClass::Materialization),
    )
    .expect_err("the third exceeds max_materialization");
    assert_eq!(refusal.code, "MaterializationBusy");
}

// ---------------------------------------------------------------------
// Pressure and space: refusal, and recovery from it
// ---------------------------------------------------------------------

/// Provoked by configuring the floor above what this box has, never by
/// consuming the box's memory. The refusal must also say what it is: an
/// advisory sample, not a statement about global allocation safety.
#[test]
fn memory_pressure_refuses_advisedly_and_admits_again_when_it_clears() {
    let estate = estate();
    let observed = jobs::sample_pressure();
    let Some(available) = observed.available_bytes else {
        // Documented fallback: without the sample, admission proceeds
        // rather than claiming a safety it did not check.
        let policy = permissive();
        assert!(
            jobs::admit(estate.path(), &policy, &JobRequest::new("build", "a")).is_ok(),
            "with no pressure sample available, admission must not pretend to have checked"
        );
        return;
    };
    let refusing = ResourcePolicy {
        min_available_memory_bytes: available.saturating_add(1 << 40),
        ..permissive()
    };
    let refusal = jobs::admit(estate.path(), &refusing, &JobRequest::new("build", "a"))
        .expect_err("a floor above what the host has must refuse");
    assert_eq!(refusal.code, "MemoryPressure");
    assert!(
        refusal.message.contains("advisory"),
        "the refusal declares the sample advisory: {refusal}"
    );

    // Recovery: the same estate admits once the configured floor is
    // satisfiable again. The refusal left nothing behind that blocks it.
    let cleared = ResourcePolicy {
        min_available_memory_bytes: 0,
        ..permissive()
    };
    assert!(
        jobs::admit(estate.path(), &cleared, &JobRequest::new("build", "a")).is_ok(),
        "a pressure refusal must not leave a slot stuck"
    );
}

#[test]
fn insufficient_space_names_the_estimate_and_the_measurement_and_calls_it_an_estimate() {
    let estate = estate();
    let policy = permissive();
    let refusal = jobs::admit(
        estate.path(),
        &policy,
        &JobRequest::new("atlas semantic build", "a").with_space(estate.path(), u64::MAX / 2),
    )
    .expect_err("an estimate larger than the filesystem must refuse");
    assert_eq!(refusal.code, "InsufficientSpace");
    assert!(
        refusal.message.contains("estimate is an estimate"),
        "the refusal must not present the estimate as a measurement: {refusal}"
    );
}

// ---------------------------------------------------------------------
// Containment: real children, including one that escapes its group
// ---------------------------------------------------------------------

/// A child that prints its own pid, `setsid()`s a grandchild that prints
/// *its* pid, and then both sleep. The grandchild is the whole point: it
/// leaves the process group, so `kill(-pgid)` cannot reach it.
fn escaping_child(directory: &Path) -> PathBuf {
    let script = directory.join("escape.py");
    fs::write(
        &script,
        r#"#!/usr/bin/env python3
import os, sys, time
pid = os.fork()
if pid == 0:
    os.setsid()
    sys.stdout.write("grandchild %d\n" % os.getpid())
    sys.stdout.flush()
    time.sleep(600)
    os._exit(0)
sys.stdout.write("child %d\n" % os.getpid())
sys.stdout.flush()
time.sleep(600)
"#,
    )
    .unwrap();
    let mut permissions = fs::metadata(&script).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    fs::set_permissions(&script, permissions).unwrap();
    script
}

/// Poll until no process is running `script`, or the bound expires. A
/// kill is delivered synchronously but reaping is not, so "did it die?"
/// is a bounded observation, never a single instantaneous read.
fn survivors_within(script: &Path, within: Duration) -> Vec<i32> {
    let deadline = Instant::now() + within;
    loop {
        let running = pids_running(script);
        if running.is_empty() || Instant::now() >= deadline {
            return running;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The decisive containment check. A real child, a real `setsid()`
/// grandchild, a real deadline.
///
/// Where `cgroup.kill` is available this asserts the strong property:
/// both die. Where it is not, it asserts the **documented fallback** —
/// the direct child dies by process group, and the escaped grandchild is
/// not claimed to. That asymmetry is the honest capability boundary, and
/// it is why `wirkd ping` reports it.
#[test]
fn a_deadline_kills_the_child_and_its_escaped_grandchild_where_the_host_supports_it() {
    let scratch = TempDir::new().unwrap();
    let estate = estate();
    let script = escaping_child(scratch.path());
    let capabilities = jobs::capabilities();
    let policy = ResourcePolicy {
        job_deadline_secs: 1,
        ..permissive()
    };

    let mut command = Command::new("python3");
    command
        .arg(&script)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let child = BoundedChild {
        capabilities,
        policy: &policy,
        cancel: CancelToken::new(),
        job_id: "deadline-check".into(),
        estate_root: Some(estate.path().to_path_buf()),
        staging: None,
        verb: "test".into(),
        scope: "test-scope".into(),
        requester: None,
        registry: None,
    };
    let started = Instant::now();
    let end = child.run(command, |_stdin| Ok(()));
    let elapsed = started.elapsed();

    let ChildEnd::Cancelled { reason, .. } = &end else {
        panic!("a child that sleeps 600s must hit its 1s deadline, got {end:?}");
    };
    assert!(reason.contains("deadline"), "{reason}");
    assert!(
        elapsed < Duration::from_secs(30),
        "the deadline must actually bound the wait, took {elapsed:?}"
    );

    // What is actually still running, read from /proc: a real
    // observation of real processes, not an inference from the exit path.
    let survivors = survivors_within(&script, Duration::from_secs(10));

    if capabilities.kill_available {
        assert!(
            survivors.is_empty(),
            "cgroup.kill is available on this host, so nothing of this job may survive its \
             deadline; these did: {survivors:?}"
        );
    } else {
        // The documented fallback, asserted rather than assumed.
        eprintln!(
            "FALLBACK: cgroup.kill unavailable ({}); only the direct child is covered, and an \
             escaped grandchild may survive. Survivors: {survivors:?}",
            capabilities
                .kill_unavailable_reason
                .as_deref()
                .unwrap_or("unknown")
        );
        for pid in survivors {
            // This check owns these processes; it does not leave them.
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
    }
}

/// Every process currently running this exact script path, read from
/// `/proc`. A real observation of real processes.
fn pids_running(script: &Path) -> Vec<i32> {
    let needle = script.display().to_string();
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return found;
    };
    for entry in entries.filter_map(|entry| entry.ok()) {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<i32>() else {
            continue;
        };
        let Ok(cmdline) = fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        if String::from_utf8_lossy(&cmdline).contains(&needle) {
            found.push(pid);
        }
    }
    found
}

#[test]
fn a_cancel_ends_a_running_child_promptly_and_is_not_reported_as_a_backend_failure() {
    let scratch = TempDir::new().unwrap();
    let estate = estate();
    let script = escaping_child(scratch.path());
    let policy = ResourcePolicy {
        job_deadline_secs: 600,
        ..permissive()
    };
    let cancel = CancelToken::new();

    let mut command = Command::new("python3");
    command
        .arg(&script)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let flag = cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(400));
        flag.cancel();
    });

    let child = BoundedChild {
        capabilities: jobs::capabilities(),
        policy: &policy,
        cancel,
        job_id: "cancel-check".into(),
        estate_root: Some(estate.path().to_path_buf()),
        staging: None,
        verb: "test".into(),
        scope: "test-scope".into(),
        requester: None,
        registry: None,
    };
    let started = Instant::now();
    let end = child.run(command, |_stdin| Ok(()));
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "cancel must not wait for the 600s deadline"
    );
    match end {
        ChildEnd::Cancelled { reason, .. } => assert!(
            reason.contains("cancelled"),
            "a cancelled job names cancellation: {reason}"
        ),
        other => panic!("a cancelled job must report cancellation, not failure: {other:?}"),
    }
    for pid in pids_running(&script) {
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
}

// ---------------------------------------------------------------------
// Containment under the *fallback* capability, on every host
// ---------------------------------------------------------------------
//
// The two checks above assert the strong property where `cgroup.kill`
// is available and the documented fallback where it is not, so on a host
// that has delegation they never exercise the fallback at all. That is
// how CI 34806268796 stalled on a path a developer box always skipped:
// there, the escaped grandchild survived the group kill, kept the
// inherited stdout/stderr write ends open, and the parent waited for an
// EOF that could only arrive when that grandchild's own 600s sleep ended.
//
// These checks declare the fallback capability explicitly instead of
// asking the host for it. Nothing else is simulated: a real child, a
// real `setsid()` grandchild, real pipes, a real process-group kill.
// Declaring the capability is what makes "where this host cannot contain
// it" reachable on a host that can — and the bound under test is
// precisely the one that must hold when containment is weak.

/// The capability set a host without delegated `cgroup.kill` reports —
/// a GitHub runner, a container without a delegated subtree. Stated, not
/// probed, so the fallback path is checkable wherever this runs.
fn without_strong_containment() -> JobCapabilities {
    JobCapabilities {
        own_cgroup: None,
        kill_available: false,
        kill_unavailable_reason: Some(
            "declared unavailable by this check, which is about the fallback path".to_string(),
        ),
        ..JobCapabilities::default()
    }
}

/// Children of this process that have exited and not been reaped.
///
/// A job whose direct child is killed but never `wait`ed for leaves a
/// zombie, and "the bound returned promptly" is not the whole property:
/// returning while leaking the one process this code unambiguously owns
/// would be a different defect wearing the same green.
fn own_zombies() -> Vec<i32> {
    let me = std::process::id() as i32;
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return found;
    };
    for entry in entries.filter_map(|entry| entry.ok()) {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<i32>() else {
            continue;
        };
        let Ok(stat) = fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        // `comm` can contain spaces and parentheses, so the fields are
        // read from after the last ')': state is the first, ppid the
        // second.
        let Some(rest) = stat.rsplit_once(')').map(|(_, rest)| rest) else {
            continue;
        };
        let mut fields = rest.split_whitespace();
        let state = fields.next().unwrap_or_default();
        let parent: i32 = fields.next().unwrap_or_default().parse().unwrap_or(0);
        if parent == me && state == "Z" {
            found.push(pid);
        }
    }
    found
}

/// Kill whatever this check's own script still has running. Owned by
/// path, not by name: another `python3` on this host is not ours.
fn kill_own(script: &Path) {
    for pid in pids_running(script) {
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
}

/// A cancel must return on a host that cannot kill an escaped
/// descendant.
///
/// The grandchild holds the job's stdout and stderr write ends and is
/// unreachable by `kill(-pgid)`. Waiting for those pipes to reach EOF is
/// therefore waiting for a process this host has already admitted it
/// cannot stop — the exact wait that stalled CI 34806268796, where the
/// direct child was a zombie and the escaped grandchild held the same
/// two pipe inodes for its full 600s sleep.
#[test]
fn a_cancel_returns_when_an_escaped_descendant_holds_the_job_pipes_open() {
    let scratch = TempDir::new().unwrap();
    let estate = estate();
    let script = escaping_child(scratch.path());
    let capabilities = without_strong_containment();
    let policy = ResourcePolicy {
        job_deadline_secs: 600,
        ..permissive()
    };
    let cancel = CancelToken::new();

    let mut command = Command::new("python3");
    command
        .arg(&script)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let flag = cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(400));
        flag.cancel();
    });

    let child = BoundedChild {
        capabilities: &capabilities,
        policy: &policy,
        cancel,
        job_id: "fallback-cancel".into(),
        estate_root: Some(estate.path().to_path_buf()),
        staging: None,
        verb: "test".into(),
        scope: "test-scope".into(),
        requester: None,
        registry: None,
    };
    let started = Instant::now();
    let end = child.run(command, |_stdin| Ok(()));
    let elapsed = started.elapsed();
    kill_own(&script);

    match end {
        ChildEnd::Cancelled { reason, .. } => {
            assert!(reason.contains("cancelled"), "{reason}")
        }
        other => panic!("a cancelled job must report cancellation: {other:?}"),
    }
    assert!(
        elapsed < Duration::from_secs(10),
        "the cancel must not wait on a descendant this host cannot kill, took {elapsed:?}"
    );
    assert!(
        own_zombies().is_empty(),
        "the direct child is this process's own and must be reaped, not left: {:?}",
        own_zombies()
    );
}

/// The same property for a deadline, which is the other way a job ends
/// without the child agreeing to it.
#[test]
fn a_deadline_returns_when_an_escaped_descendant_holds_the_job_pipes_open() {
    let scratch = TempDir::new().unwrap();
    let estate = estate();
    let script = escaping_child(scratch.path());
    let capabilities = without_strong_containment();
    let policy = ResourcePolicy {
        job_deadline_secs: 1,
        ..permissive()
    };

    let mut command = Command::new("python3");
    command
        .arg(&script)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let child = BoundedChild {
        capabilities: &capabilities,
        policy: &policy,
        cancel: CancelToken::new(),
        job_id: "fallback-deadline".into(),
        estate_root: Some(estate.path().to_path_buf()),
        staging: None,
        verb: "test".into(),
        scope: "test-scope".into(),
        requester: None,
        registry: None,
    };
    let started = Instant::now();
    let end = child.run(command, |_stdin| Ok(()));
    let elapsed = started.elapsed();
    kill_own(&script);

    let ChildEnd::Cancelled { reason, .. } = &end else {
        panic!("a child that sleeps 600s must hit its 1s deadline, got {end:?}");
    };
    assert!(reason.contains("deadline"), "{reason}");
    assert!(
        elapsed < Duration::from_secs(10),
        "the deadline must bound the wait even where the grandchild survives, took {elapsed:?}"
    );
    assert!(
        own_zombies().is_empty(),
        "the direct child is this process's own and must be reaped, not left: {:?}",
        own_zombies()
    );
}

/// Input production is the other descriptor an escaped descendant holds.
///
/// The grandchild inherits stdin's *read* end and never reads it, so
/// once the pipe buffer fills, a write to it can never complete and the
/// reader never goes away to make it fail. A request larger than a pipe
/// buffer is the ordinary case here — both real callers write a whole
/// serialised batch — so the writer must be released when the job ends
/// rather than left blocked in `write(2)` for the life of the process.
///
/// `run` joins its writer before returning, which is what makes this
/// checkable: if the thread were still blocked, this check would not
/// return at all.
#[test]
fn a_blocked_input_write_is_released_when_the_job_ends() {
    let scratch = TempDir::new().unwrap();
    let estate = estate();
    let script = escaping_child(scratch.path());
    let capabilities = without_strong_containment();
    let policy = ResourcePolicy {
        job_deadline_secs: 1,
        ..permissive()
    };

    let mut command = Command::new("python3");
    command
        .arg(&script)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let child = BoundedChild {
        capabilities: &capabilities,
        policy: &policy,
        cancel: CancelToken::new(),
        job_id: "fallback-input".into(),
        estate_root: Some(estate.path().to_path_buf()),
        staging: None,
        verb: "test".into(),
        scope: "test-scope".into(),
        requester: None,
        registry: None,
    };
    // Far larger than any pipe buffer this host configures, and nothing
    // on the other end is reading it.
    let request = vec![b'x'; 4 * 1024 * 1024];
    let started = Instant::now();
    let end = child.run(command, move |stdin| {
        stdin.write_all(&request)?;
        stdin.flush()
    });
    let elapsed = started.elapsed();
    kill_own(&script);

    let ChildEnd::Cancelled { reason, .. } = &end else {
        panic!("the job ended by its deadline, whatever became of its input: {end:?}");
    };
    assert!(reason.contains("deadline"), "{reason}");
    assert!(
        elapsed < Duration::from_secs(10),
        "a writer blocked on a pipe nothing will drain must be released, took {elapsed:?}"
    );
    assert!(
        own_zombies().is_empty(),
        "the direct child is this process's own and must be reaped, not left: {:?}",
        own_zombies()
    );
}

/// The ordinary path the bound must not cost anything: a child that
/// reads its whole request and answers with far more than a pipe buffer
/// on **both** descriptors still has every byte delivered.
///
/// Draining two pipes by hand is easy to get subtly wrong — one
/// descriptor drained while the other is only polled, a short read taken
/// for EOF — and each of those mistakes is invisible until the output is
/// big enough to need more than one read.
#[test]
fn a_successful_child_still_delivers_all_of_a_large_stdout_and_stderr() {
    let scratch = TempDir::new().unwrap();
    let estate = estate();
    let script = scratch.path().join("loud.py");
    fs::write(
        &script,
        r#"#!/usr/bin/env python3
import sys
request = sys.stdin.buffer.read()
sys.stdout.buffer.write(b"o" * 3000000)
sys.stdout.buffer.flush()
sys.stderr.buffer.write(b"e" * 2000000)
sys.stderr.buffer.flush()
sys.stdout.buffer.write(b"|%d" % len(request))
sys.stdout.buffer.flush()
"#,
    )
    .unwrap();
    let policy = ResourcePolicy {
        job_deadline_secs: 120,
        ..permissive()
    };

    let mut command = Command::new("python3");
    command
        .arg(&script)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let child = BoundedChild {
        capabilities: jobs::capabilities(),
        policy: &policy,
        cancel: CancelToken::new(),
        job_id: "large-io".into(),
        estate_root: Some(estate.path().to_path_buf()),
        staging: None,
        verb: "test".into(),
        scope: "test-scope".into(),
        requester: None,
        registry: None,
    };
    let request = vec![b'x'; 5 * 1024 * 1024];
    let sent = request.len();
    let end = child.run(command, move |stdin| {
        stdin.write_all(&request)?;
        stdin.flush()
    });

    let ChildEnd::Finished(output) = end else {
        panic!("an ordinary child must finish, not be bounded: {end:?}");
    };
    assert!(output.status.success(), "{:?}", output.status);
    assert_eq!(
        output.stdout.len(),
        3_000_000 + format!("|{sent}").len(),
        "every stdout byte is delivered, not just the first read"
    );
    assert_eq!(
        output.stderr.len(),
        2_000_000,
        "stderr is drained alongside stdout, not starved by it"
    );
    assert!(
        output.stdout.ends_with(format!("|{sent}").as_bytes()),
        "the child read the whole request: {}",
        String::from_utf8_lossy(&output.stdout[output.stdout.len() - 32..])
    );
}

/// A real child that exits without reading its stdin at all: the writer
/// thread's next `write(2)` past the kernel's pipe buffer gets `EPIPE`
/// once the child's own copy of the read end has closed.
///
/// Ruling 0308: `run` used to join this thread with `let _ =
/// writer.join()`, discarding exactly this `Err` — so a request that was
/// only ever partly delivered still came back as `ChildEnd::Finished`
/// with the child's real (successful) exit status, indistinguishable
/// from a child that read the whole thing. Both real callers
/// (`wirk-atlas/src/semantic.rs`'s and `retrieval.rs`'s backend
/// protocols) trusted that `Finished` at face value. This is the same
/// defect those two callers' now-removed `let write: std::io::Result<()>
/// = Ok(());` placeholders papered over: a `write` outcome that was
/// never actually threaded through from the writer thread at all.
#[test]
fn an_input_write_that_fails_is_reported_and_not_papered_over_as_success() {
    let scratch = TempDir::new().unwrap();
    let estate = estate();
    let script = scratch.path().join("deaf.py");
    fs::write(&script, "#!/usr/bin/env python3\nimport sys\nsys.exit(0)\n").unwrap();
    let policy = ResourcePolicy {
        job_deadline_secs: 30,
        ..permissive()
    };

    let mut command = Command::new("python3");
    command
        .arg(&script)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let child = BoundedChild {
        capabilities: jobs::capabilities(),
        policy: &policy,
        cancel: CancelToken::new(),
        job_id: "discarded-writer".into(),
        estate_root: Some(estate.path().to_path_buf()),
        staging: None,
        verb: "test".into(),
        scope: "test-scope".into(),
        requester: None,
        registry: None,
    };
    // Far larger than any pipe buffer, and a child that never reads a
    // byte of it: the write past the buffer's capacity has to see EPIPE,
    // not merely block (which `a_blocked_input_write_is_released_when_
    // the_job_ends` above already covers with a different, hung, child).
    let request = vec![b'x'; 4 * 1024 * 1024];
    let end = child.run(command, move |stdin| {
        stdin.write_all(&request)?;
        stdin.flush()
    });

    let ChildEnd::Failed(detail) = &end else {
        panic!(
            "a child whose request was never fully written must not be reported as a success, \
             whatever its own exit status was: {end:?}"
        );
    };
    assert!(
        detail.contains("its input could not be written"),
        "the failure must name what actually happened, not a generic one: {detail}"
    );
}

// ---------------------------------------------------------------------
// Recovery: only what this estate recorded
// ---------------------------------------------------------------------

/// Recovery must reach this estate's own recorded jobs and **nothing
/// else**. A prefix scan under a shared cgroup parent would sweep another
/// live estate's jobs; this check makes that failure mode visible by
/// putting a second estate's job right beside the first.
#[test]
fn recovery_reaches_only_the_jobs_this_estate_recorded() {
    let capabilities = jobs::capabilities();
    let mine = estate();
    let theirs = estate();

    let Some(own_cgroup) = capabilities.own_cgroup.clone() else {
        // Fallback: with no cgroup, recovery still clears records and
        // staging, and still must not touch the other estate's.
        let staging = mine.path().join("staging-a");
        fs::create_dir_all(&staging).unwrap();
        jobs::record_owned_job(
            mine.path(),
            &jobs::OwnedJobRecord {
                job_id: "j1".into(),
                cgroup: None,
                pid: None,
                verb: "build".into(),
                started_unix_millis: jobs::now_unix_millis(),
                staging: Some(staging.clone()),
            },
        )
        .unwrap();
        let outcome = jobs::recover_owned_jobs(mine.path());
        assert_eq!(outcome.records_seen, 1);
        assert!(!staging.exists());
        return;
    };

    // A real cgroup each, made the way a job makes one.
    let my_job = own_cgroup.join("wirk-job-recovery-mine");
    let their_job = own_cgroup.join("wirk-job-recovery-theirs");
    fs::create_dir(&my_job).unwrap();
    fs::create_dir(&their_job).unwrap();

    for (root, job) in [(mine.path(), &my_job), (theirs.path(), &their_job)] {
        jobs::record_owned_job(
            root,
            &jobs::OwnedJobRecord {
                job_id: job.file_name().unwrap().to_string_lossy().into_owned(),
                cgroup: Some(job.clone()),
                pid: None,
                verb: "build".into(),
                started_unix_millis: jobs::now_unix_millis(),
                staging: None,
            },
        )
        .unwrap();
    }

    let outcome = jobs::recover_owned_jobs(mine.path());
    assert_eq!(outcome.records_seen, 1, "only this estate's record is seen");
    assert!(
        !my_job.exists(),
        "this estate's recorded job cgroup is removed"
    );
    assert!(
        their_job.exists(),
        "another estate's job cgroup, sharing the same parent and the same wirk-job- prefix, \
         must be left completely alone — ownership is the record, never the name"
    );
    assert!(
        jobs::recover_owned_jobs(theirs.path()).records_seen == 1,
        "and the other estate can still recover its own"
    );
    let _ = fs::remove_dir(&their_job);
}

/// Recovery is bounded *recovery*, and the code must not be able to
/// describe itself as prevention: the capability summary says so.
#[test]
fn the_capability_summary_states_what_is_not_guaranteed() {
    let capabilities = jobs::capabilities();
    let summary = capabilities.summary();
    assert!(
        summary.contains("pressure") && summary.contains("advisory"),
        "the pressure sample must be labelled advisory: {summary}"
    );
    if !capabilities.memory_cap_available {
        assert!(
            capabilities.memory_cap_unavailable_reason.is_some(),
            "an unavailable memory cap must carry its reason, never silence"
        );
    }
    if !capabilities.kill_available {
        assert!(
            summary.contains("setsid"),
            "a degraded containment must say what it cannot reach: {summary}"
        );
    }
}

// ---------------------------------------------------------------------
// Policy loading
// ---------------------------------------------------------------------

/// Watched red against the **real service**: a `resources.json` written
/// the way anyone writes JSON — `{ "job_deadline_secs": 1 }`, with a
/// space after the colon — was silently ignored, and `wirkd ping`
/// reported the 3600s default while the operator believed a 1s deadline
/// was in force. The cause was a hand-rolled string scanner; the fix is
/// the crate's own JSON parser.
///
/// A configured bound that is silently not applied is worse than no
/// bound, so this pins ordinary formatting, not just the compact form.
#[test]
fn a_configured_policy_is_actually_applied_whatever_the_whitespace() {
    let estate = estate();
    fs::write(
        ResourcePolicy::config_path(estate.path()),
        r#"{
            "job_deadline_secs": 1,
            "max_expensive":   3,
            "memory_pressure_avg10_max": 12.5,
            "job_memory_max_bytes": 67108864
        }"#,
    )
    .unwrap();
    let (policy, note) = ResourcePolicy::load(estate.path());
    assert!(note.is_none(), "a valid file must not complain: {note:?}");
    assert_eq!(policy.job_deadline_secs, 1);
    assert_eq!(policy.max_expensive, 3);
    assert_eq!(policy.memory_pressure_avg10_max, 12.5);
    assert_eq!(policy.job_memory_max_bytes, Some(67_108_864));
    // Untouched fields keep their defaults.
    assert_eq!(
        policy.max_materialization,
        ResourcePolicy::default().max_materialization
    );
}

/// A file that cannot be read must say so loudly. Running on defaults
/// while an operator believes their file is in force is the same failure
/// as above, one step further along.
#[test]
fn a_malformed_policy_is_reported_rather_than_silently_ignored() {
    let estate = estate();
    fs::write(
        ResourcePolicy::config_path(estate.path()),
        "{ \"max_expensive\": ",
    )
    .unwrap();
    let (policy, note) = ResourcePolicy::load(estate.path());
    let note = note.expect("a malformed policy file must be reported");
    assert!(
        note.contains("NOT on the values in that file"),
        "the report must say the file is not in force: {note}"
    );
    assert_eq!(
        policy.max_expensive,
        ResourcePolicy::default().max_expensive
    );

    // And a key nobody recognises is a mistake worth reporting too — a
    // misspelled bound is a bound that is not in force.
    fs::write(
        ResourcePolicy::config_path(estate.path()),
        r#"{ "max_expensiv": 4 }"#,
    )
    .unwrap();
    let (_, note) = ResourcePolicy::load(estate.path());
    assert!(
        note.is_some(),
        "a misspelled key must not be silently dropped"
    );
}

/// An absent file is ordinary, and must be silent.
#[test]
fn an_absent_policy_file_is_not_an_error() {
    let estate = estate();
    let (policy, note) = ResourcePolicy::load(estate.path());
    assert!(note.is_none());
    assert_eq!(policy.max_expensive, 1, "the default changes no behaviour");
}

// ---------------------------------------------------------------------
// N1: one coherent policy for a shared pool whose participants disagree
// ---------------------------------------------------------------------

/// The defect, exactly as it was observed: same pool directory, one
/// estate configured `max_host_expensive: 1` and one configured `3`, two
/// concurrent expensive jobs — and **both** were admitted, because the
/// pool was sized from whichever caller was asking. The wider caller
/// simply created `slot-1`, a slot the narrower caller had never agreed
/// existed.
#[test]
fn a_wider_participant_cannot_create_slots_the_shared_pool_never_agreed_to() {
    let pool = host_pool();
    let narrow_estate = estate();
    let wide_estate = estate();
    let narrow = ResourcePolicy {
        max_expensive: 4,
        max_host_expensive: 1,
        host_pool_dir: Some(pool.clone()),
        ..permissive()
    };
    let wide = ResourcePolicy {
        max_expensive: 4,
        max_host_expensive: 3,
        host_pool_dir: Some(pool.clone()),
        ..permissive()
    };

    // The narrow participant arrives first and settles the pool at 1.
    let _held = jobs::admit(
        narrow_estate.path(),
        &narrow,
        &JobRequest::new("atlas semantic build", "a"),
    )
    .expect("the first participant is admitted and agrees the capacity");

    let refusal = jobs::admit(
        wide_estate.path(),
        &wide,
        &JobRequest::new("atlas semantic build", "b"),
    )
    .expect_err("a participant configured 3 must not widen a pool agreed at 1");
    assert_eq!(refusal.code, "HostExpensiveBusy");
    assert!(
        refusal.message.contains("agreed capacity is 1"),
        "the refusal names the pool's agreement, not the caller's number: {refusal}"
    );

    // The mechanism, not just the outcome: no slot beyond the agreement
    // was ever created on disk. `slot-1` existing at all is the defect.
    assert!(
        !pool.join("slot-1").exists(),
        "a slot outside the pool's agreed capacity was created in {}",
        pool.display()
    );
}

/// A participant's own maximum is not a shared agreement, and the
/// disagreement is disclosed rather than silently resolved.
#[test]
fn a_disagreeing_participant_is_bound_by_the_pool_and_told_so() {
    let pool = host_pool();
    let first = estate();
    let second = estate();
    let narrow = ResourcePolicy {
        max_host_expensive: 1,
        host_pool_dir: Some(pool.clone()),
        ..permissive()
    };
    let wide = ResourcePolicy {
        max_host_expensive: 5,
        host_pool_dir: Some(pool.clone()),
        ..permissive()
    };
    let held = jobs::admit(first.path(), &narrow, &JobRequest::new("build", "a")).unwrap();
    drop(held);

    let admitted = jobs::admit(second.path(), &wide, &JobRequest::new("build", "b"))
        .expect("the pool is free, so the job runs — under the pool's number");
    let disclosure = admitted.notes.join(" | ");
    assert!(
        disclosure.contains("max_host_expensive is 5")
            && disclosure.contains("agreed capacity is 1"),
        "the disagreement must be disclosed, not silently resolved: {disclosure}"
    );
    assert!(
        disclosure.contains("not a shared agreement"),
        "the disclosure says why the caller's own number does not govern: {disclosure}"
    );
}

/// A deliberate change is possible, and it is refused while it would
/// strand a live lease outside the new bound.
#[test]
fn shrinking_a_shared_pool_is_refused_while_an_incompatible_lease_is_live() {
    let pool = host_pool();
    let wide_estate = estate();
    let shrinking_estate = estate();
    let wide = ResourcePolicy {
        max_expensive: 3,
        max_host_expensive: 3,
        host_pool_dir: Some(pool.clone()),
        ..permissive()
    };
    // Two live leases: slot-0 and slot-1.
    let _one = jobs::admit(wide_estate.path(), &wide, &JobRequest::new("build", "a")).unwrap();
    let _two = jobs::admit(wide_estate.path(), &wide, &JobRequest::new("build", "b")).unwrap();

    let shrinking = ResourcePolicy {
        max_host_expensive: 1,
        host_pool_capacity_authority: true,
        host_pool_dir: Some(pool.clone()),
        ..permissive()
    };
    let refusal = jobs::admit(
        shrinking_estate.path(),
        &shrinking,
        &JobRequest::new("build", "c"),
    )
    .expect_err("shrinking past a live lease must be refused, not applied silently");
    assert_eq!(refusal.code, "PoolCapacityInUse");
    assert!(
        refusal.message.contains("slot-1"),
        "the refusal names the lease that blocks the change: {refusal}"
    );

    // The existing agreement is intact: nothing was half-applied.
    let standing = jobs::pool_standing(&pool, 3, false).expect("the pool is still usable");
    assert_eq!(standing.agreed_capacity, 3);
}

/// The same deliberate change, once the pool is idle, is applied and
/// disclosed.
#[test]
fn a_declared_authority_changes_a_shared_pool_when_no_lease_is_stranded() {
    let pool = host_pool();
    let owner = estate();
    let starter = ResourcePolicy {
        max_host_expensive: 3,
        host_pool_dir: Some(pool.clone()),
        ..permissive()
    };
    drop(jobs::admit(owner.path(), &starter, &JobRequest::new("build", "a")).unwrap());

    let authoritative = ResourcePolicy {
        max_host_expensive: 1,
        host_pool_capacity_authority: true,
        host_pool_dir: Some(pool.clone()),
        ..permissive()
    };
    let admitted = jobs::admit(owner.path(), &authoritative, &JobRequest::new("build", "b"))
        .expect("an idle pool accepts a deliberate change");
    assert!(
        admitted.notes.join(" | ").contains("changed from 3 to 1"),
        "a deliberate capacity change is announced: {:?}",
        admitted.notes
    );
    assert_eq!(
        jobs::pool_standing(&pool, 9, false)
            .unwrap()
            .agreed_capacity,
        1,
        "the changed agreement is what a later participant reads"
    );
}

/// Concurrent initialization settles on exactly one agreement.
#[test]
fn concurrent_first_participants_settle_on_one_agreement() {
    let pool = host_pool();
    let handles: Vec<_> = (0..6)
        .map(|index| {
            let pool = pool.clone();
            std::thread::spawn(move || {
                jobs::pool_standing(&pool, 1 + index, false).map(|s| s.agreed_capacity)
            })
        })
        .collect();
    let agreed: Vec<u32> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap().expect("the pool settles"))
        .collect();
    let first = agreed[0];
    assert!(
        agreed.iter().all(|value| *value == first),
        "concurrent initializers must agree on one capacity, got {agreed:?}"
    );
}

// ---------------------------------------------------------------------
// N3: the memory bound that actually applies to this process
// ---------------------------------------------------------------------

/// A controlled metadata fixture: a cgroup-shaped directory tree with
/// the interface files this code reads. No real controller is created,
/// nothing is written outside the temp directory, and no memory is
/// consumed — the shape is what is under test.
struct FixtureLevel {
    relative: &'static str,
    high: Option<u64>,
    max: Option<u64>,
    current: u64,
    some_avg10: Option<f64>,
}

fn level(
    relative: &'static str,
    high: Option<u64>,
    max: Option<u64>,
    current: u64,
    some_avg10: Option<f64>,
) -> FixtureLevel {
    FixtureLevel {
        relative,
        high,
        max,
        current,
        some_avg10,
    }
}

fn cgroup_fixture(levels: &[FixtureLevel]) -> TempDir {
    let root = TempDir::new().unwrap();
    for FixtureLevel {
        relative,
        high,
        max,
        current,
        some_avg10,
    } in levels
    {
        let dir = if relative.is_empty() {
            root.path().to_path_buf()
        } else {
            root.path().join(relative)
        };
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("memory.current"), format!("{current}\n")).unwrap();
        fs::write(
            dir.join("memory.high"),
            high.map_or("max\n".to_string(), |bytes| format!("{bytes}\n")),
        )
        .unwrap();
        fs::write(
            dir.join("memory.max"),
            max.map_or("max\n".to_string(), |bytes| format!("{bytes}\n")),
        )
        .unwrap();
        if let Some(avg10) = some_avg10 {
            fs::write(
                dir.join("memory.pressure"),
                format!(
                    "some avg10={avg10:.2} avg60=0.00 avg300=0.00 total=0\n\
                     full avg10=0.00 avg60=0.00 avg300=0.00 total=0\n"
                ),
            )
            .unwrap();
        }
    }
    root
}

/// Ruling 0246's own numbers, in fixture form.
///
/// Host `MemAvailable` said roughly 9.5 GiB free while the slice this
/// process actually ran in sat at its 12 GiB `memory.high` under a
/// 16 GiB `memory.max`. What this pins is **scope and kind**: the
/// applicable hard ceiling is 3–4× tighter than the host estimate the
/// earlier version read, the soft threshold is tracked separately and
/// refuses nothing by itself, and pressure comes from the scope the job
/// will actually run in. It does **not** claim these defaults would have
/// refused at that instant — the recorded figures do not establish
/// that, and the numbers here are a fixture, not a product default.
#[test]
fn the_tightest_applicable_bound_is_what_a_memory_floor_is_compared_against() {
    const GIB: u64 = 1024 * 1024 * 1024;
    let fixture = cgroup_fixture(&[
        // The process's own leaf declares no limit of its own...
        level("wirkp3.slice/leaf", None, None, 2 * GIB, Some(41.0)),
        // ...but the slice above it is at its soft threshold.
        level(
            "wirkp3.slice",
            Some(12 * GIB),
            Some(16 * GIB),
            12 * GIB,
            Some(39.0),
        ),
        level("", None, None, 13 * GIB, Some(1.0)),
    ]);
    let scope = jobs::CgroupScope {
        mount: fixture.path().to_path_buf(),
        own: fixture.path().join("wirkp3.slice/leaf"),
    };
    let observed = jobs::observe_memory_in(&scope);

    assert_eq!(
        observed.soft_headroom_bytes,
        Some(0),
        "the slice is at its memory.high, so nothing is available before throttling"
    );
    assert_eq!(
        observed.hard_headroom_bytes,
        Some(4 * GIB),
        "the hard ceiling is a different, larger number and must stay distinct"
    );
    // The floor is compared against the tightest *hard* bound that
    // applies — 4 GiB here — not against the host's whole-machine
    // estimate, which is what the earlier version read.
    assert_eq!(
        observed.effective_available_bytes(),
        Some(4 * GIB),
        "the floor is compared against the applicable hard ceiling, not the host's estimate"
    );
    let origin = observed.effective_available_origin().unwrap();
    assert!(
        origin.contains("hard ceiling") && origin.contains("wirkp3.slice"),
        "the operator is told which bound applies and what kind it is: {origin}"
    );
    // And the soft threshold is reported as what it is: a throttling
    // signal, never a refusal on its own. A busy cgroup sits at its
    // memory.high normally; refusing there would refuse everything.
    assert!(observed.is_throttled());
    assert!(
        !observed
            .effective_available_origin()
            .unwrap()
            .contains("soft"),
        "soft headroom must not be what a floor refuses against"
    );
    assert_eq!(
        observed.scoped_some_avg10,
        Some(41.0),
        "pressure comes from the nearest scope that publishes it, not from the root"
    );
}

/// Every applicable ancestor, not the nearest one that happens to carry
/// a limit: a grandparent's ceiling constrains a child whose parent
/// declares none, and the binding constraint is the minimum over the
/// whole chain.
#[test]
fn a_constraint_on_a_distant_ancestor_still_binds() {
    const MIB: u64 = 1024 * 1024;
    let fixture = cgroup_fixture(&[
        level("outer/middle/leaf", None, None, 10 * MIB, None),
        level("outer/middle", None, Some(4096 * MIB), 40 * MIB, None),
        level("outer", None, Some(100 * MIB), 90 * MIB, None),
        level("", None, None, 200 * MIB, None),
    ]);
    let scope = jobs::CgroupScope {
        mount: fixture.path().to_path_buf(),
        own: fixture.path().join("outer/middle/leaf"),
    };
    let observed = jobs::observe_memory_in(&scope);
    assert_eq!(
        observed.hard_headroom_bytes,
        Some(10 * MIB),
        "the outermost ancestor is the binding one here and must not be skipped"
    );
    assert_eq!(observed.hard_headroom_from.as_deref(), Some("/outer"));
}

/// `max` means no limit and must never be read as a number.
#[test]
fn an_unlimited_level_contributes_no_headroom_and_says_so() {
    let fixture = cgroup_fixture(&[
        level("solo", None, None, 5000, None),
        level("", None, None, 9000, None),
    ]);
    let scope = jobs::CgroupScope {
        mount: fixture.path().to_path_buf(),
        own: fixture.path().join("solo"),
    };
    let observed = jobs::observe_memory_in(&scope);
    assert_eq!(observed.soft_headroom_bytes, None);
    assert_eq!(observed.hard_headroom_bytes, None);
    let unavailable = observed.unavailable.join(" | ");
    assert!(
        unavailable.contains("no cgroup level this process runs in declares a memory.high")
            && unavailable.contains("declares a memory.max"),
        "an environment without these facilities is told so, not silently treated as bounded: \
         {unavailable}"
    );
}

/// The supported/unavailable boundary on whatever host actually runs
/// this: a real, read-only observation that must either produce a scope
/// or explain why it could not. It asserts no number about this machine.
#[test]
fn a_real_observation_either_resolves_a_scope_or_discloses_why_not() {
    let observed = jobs::observe_memory();
    match jobs::cgroup_scope() {
        Ok(scope) => {
            assert!(
                scope.mount.is_dir(),
                "a resolved cgroup2 mount is a real directory"
            );
            assert!(
                !observed.levels.is_empty() || !observed.unavailable.is_empty(),
                "a resolved scope yields levels or an explanation, never silence"
            );
        }
        Err(reason) => {
            assert!(
                observed
                    .unavailable
                    .iter()
                    .any(|note| note.contains("cgroup memory accounting")),
                "an unsupported environment discloses that, rather than claiming a check it did \
                 not make; reason was {reason}"
            );
        }
    }
}

// ---------------------------------------------------------------------
// N2: cancellation is per job, addressable, and not sticky
// ---------------------------------------------------------------------

/// The registry is what an operator's cancellation reaches. A job is
/// visible while it runs, gone when it ends, and cancelling one leaves
/// the estate able to do legitimate work immediately afterwards — the
/// single sticky store-wide token could not do that.
#[test]
fn cancelling_one_job_does_not_poison_the_next_one() {
    let estate = estate();
    let policy = ResourcePolicy {
        job_deadline_secs: 30,
        ..permissive()
    };
    let registry = jobs::JobRegistry::new();
    let script = estate.path().join("sleep.sh");
    fs::write(&script, "#!/bin/sh\nsleep 20\n").unwrap();
    fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

    let run_one = |job_id: &str, scope: &str| {
        let mut command = Command::new("/bin/sh");
        command
            .arg(&script)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let child = BoundedChild {
            capabilities: jobs::capabilities(),
            policy: &policy,
            cancel: CancelToken::new(),
            job_id: job_id.to_string(),
            estate_root: Some(estate.path().to_path_buf()),
            staging: None,
            verb: "atlas semantic build".into(),
            scope: scope.to_string(),
            requester: None,
            registry: Some(registry.clone()),
        };
        child.run(command, |_stdin| Ok(()))
    };

    // First job: cancel it by *name* from another thread while it runs.
    let watcher = {
        let registry = registry.clone();
        std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                let acknowledged = registry.cancel(
                    &jobs::JobSelector::Scope("source-a".into()),
                    "was cancelled by an operator (test)",
                );
                if !acknowledged.is_empty() {
                    return acknowledged;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Vec::new()
        })
    };
    let started = Instant::now();
    let first = run_one("job-one", "source-a");
    let acknowledged = watcher.join().unwrap();

    assert_eq!(acknowledged.len(), 1, "the named job was reached");
    assert_eq!(acknowledged[0].job_id, "job-one");
    assert_eq!(acknowledged[0].scope, "source-a");
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "the cancellation actually ended the child rather than waiting out its sleep"
    );
    match &first {
        ChildEnd::Cancelled { reason, .. } => assert!(
            reason.contains("cancelled by an operator"),
            "the end names the deliberate act, not a backend failure: {reason}"
        ),
        other => panic!("expected a cancelled end, got {other:?}"),
    }
    assert!(
        !registry.is_active("job-one"),
        "a finished job leaves the registry, so 'still running' stays a truthful answer"
    );

    // The estate is immediately usable again. Under the old sticky
    // store-wide token this second job died on arrival.
    fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
    let second = run_one("job-two", "source-a");
    assert!(
        matches!(second, ChildEnd::Finished(_)),
        "legitimate work after a cancellation must run: got {second:?}"
    );
}

/// A cancellation reaches only what its selector names, and a miss says
/// nothing about what else is running.
#[test]
fn a_selector_reaches_only_its_target() {
    let registry = jobs::JobRegistry::new();
    for (id, scope) in [("a1", "alpha"), ("a2", "alpha"), ("b1", "beta")] {
        registry.register(jobs::ActiveJob {
            job_id: id.into(),
            verb: "atlas semantic build".into(),
            scope: scope.into(),
            requester: None,
            started_unix_millis: jobs::now_unix_millis(),
            cancel: CancelToken::new(),
        });
    }
    let acknowledged = registry.cancel(&jobs::JobSelector::Scope("alpha".into()), "by test");
    let mut reached: Vec<&str> = acknowledged.iter().map(|a| a.job_id.as_str()).collect();
    reached.sort_unstable();
    assert_eq!(reached, vec!["a1", "a2"]);

    let untouched = registry
        .list()
        .into_iter()
        .find(|job| job.job_id == "b1")
        .expect("the unrelated job is still registered");
    assert!(
        !untouched.cancel.is_cancelled(),
        "an unrelated job must not be signalled by another source's cancellation"
    );

    assert!(
        registry
            .cancel(&jobs::JobSelector::Job("not-a-job".into()), "by test")
            .is_empty(),
        "a miss reaches nothing"
    );
}

// ---------------------------------------------------------------------
// Ruling 0251 F1/F2/F3: what the caller is told, and what reading costs
// ---------------------------------------------------------------------

/// **The defect.** `Refusal` carried no notes, and `admit_expensive`
/// printed `Admission.notes` only on the `Ok` path. So the one fact the
/// operator needed — the pool's agreed number against this estate's own,
/// and the deliberate way to change it — was dropped at precisely the
/// moment the job was refused for that capacity.
#[test]
fn a_refusal_carries_the_disclosure_the_success_path_already_had() {
    let pool = host_pool();
    let narrow = estate();
    let wide = estate();
    let narrow_policy = ResourcePolicy {
        max_expensive: 1,
        max_host_expensive: 1,
        host_pool_dir: Some(pool.clone()),
        ..permissive()
    };
    let wide_policy = ResourcePolicy {
        max_expensive: 3,
        max_host_expensive: 3,
        host_pool_dir: Some(pool.clone()),
        ..permissive()
    };
    // The narrow participant settles the pool at 1 and holds the slot.
    let held = jobs::admit(
        narrow.path(),
        &narrow_policy,
        &jobs::JobRequest::new("atlas semantic build", "alpha"),
    )
    .expect("the first participant is admitted");

    let refusal = jobs::admit(
        wide.path(),
        &wide_policy,
        &jobs::JobRequest::new("atlas semantic build", "beta"),
    )
    .expect_err("the pool's one slot is taken");
    assert_eq!(refusal.code, "HostExpensiveBusy");
    let notes = refusal.notes.join("\n");
    assert!(
        notes.contains("the pool's agreed capacity is 1")
            || notes.contains("shared pool's agreed capacity is 1"),
        "a refusal did not name the agreement it was refused against: {notes:?}"
    );
    assert!(
        notes.contains("host_pool_capacity_authority"),
        "a refusal did not say how to change the number deliberately: {notes:?}"
    );
    drop(held);
}

/// Reading the shared policy must not *be* a participation in it.
/// `pool_standing` writes a first agreement as a side effect of
/// admitting a job, which is right there and would be indefensible in an
/// inspection: an operator asking "what is the shared bound?" must not
/// thereby become the estate that set it.
#[test]
fn inspecting_an_uninitialized_pool_reports_it_honestly_and_creates_nothing() {
    let pool = host_pool();
    fs::remove_dir_all(&pool).expect("start from no pool at all");

    let view = jobs::inspect_pool(&pool, 3, false);
    assert_eq!(view.agreement_status, "uninitialized");
    assert!(
        view.effective_usable_slots.is_none(),
        "an uninitialized pool reported an effective capacity nobody has agreed to"
    );
    assert_eq!(view.configured_max_host_expensive, 3);
    let notes = view.notes.join("\n");
    assert!(
        notes.contains("first participant") && notes.contains("does not create it"),
        "the uninitialized reading did not disclose startup order: {notes:?}"
    );
    assert!(
        !pool.exists(),
        "reading the pool created {}",
        pool.display()
    );

    // Once a participant has actually admitted a job, the same read
    // reports the agreement and this caller's own restraint against it.
    let first = estate();
    let policy = ResourcePolicy {
        max_expensive: 1,
        max_host_expensive: 1,
        host_pool_dir: Some(pool.clone()),
        ..permissive()
    };
    let held = jobs::admit(
        first.path(),
        &policy,
        &jobs::JobRequest::new("atlas acquire", "alpha"),
    )
    .expect("admitted");
    drop(held);

    let view = jobs::inspect_pool(&pool, 3, false);
    assert_eq!(view.agreement_status, "initialized");
    assert_eq!(view.agreement.as_ref().expect("agreement").capacity, 1);
    assert_eq!(
        view.effective_usable_slots,
        Some(1),
        "a caller configured 3 in a pool agreed at 1 is offered 1"
    );
    assert!(
        view.notes.join("\n").contains("not a shared agreement"),
        "{:?}",
        view.notes
    );
}

/// The other direction, and the reason the three numbers are reported
/// separately: a participant configured *below* the agreement restrains
/// only itself, and the disclosure says so rather than describing that
/// preference as a host-wide bound.
///
/// Its own pool (`host_pool` is one directory per test thread), because
/// an agreement is written once and this check needs a wide one.
#[test]
fn a_participant_below_the_agreement_restrains_only_itself_and_is_told_so() {
    let pool = host_pool();
    let estate = estate();
    let policy = ResourcePolicy {
        max_expensive: 1,
        max_host_expensive: 4,
        host_pool_dir: Some(pool.clone()),
        ..permissive()
    };
    let held = jobs::admit(
        estate.path(),
        &policy,
        &jobs::JobRequest::new("atlas acquire", "alpha"),
    )
    .expect("the first participant settles the pool at 4");
    drop(held);

    let restrained = jobs::inspect_pool(&pool, 2, false);
    assert_eq!(
        restrained.agreement.as_ref().expect("agreement").capacity,
        4
    );
    assert_eq!(restrained.effective_usable_slots, Some(2));
    let notes = restrained.notes.join("\n");
    assert!(
        notes.contains("cannot impose that smaller bound"),
        "a lower caller's preference was not distinguished from a host bound: {notes:?}"
    );
}
