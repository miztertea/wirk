//! Bounded, owned resource jobs (P4.5 increment B, ruling 0237).
//!
//! Wirk starts expensive children of its own — an embedding backend, a
//! query backend — and materializes worktrees. Before this module those
//! ran with no owner, no bound and no end: `AtlasStore::open` swept
//! `.tmp-*` directories it merely *assumed* were abandoned, the backend
//! spawns had no process group, no `PDEATHSIG`, no deadline and no kill,
//! and the only concurrency bound was an in-process mutex that a second
//! process did not share and that cheap reads blocked behind invisibly.
//!
//! What this module supplies, and what it deliberately does not:
//!
//! * **Ownership** ([`OwnerLock`]) — `flock(LOCK_EX|LOCK_NB)` held for
//!   the owner's life. Advisory, per open-file-description, released by
//!   the kernel when the holder dies (measured, `refine/CHECKS.json`
//!   `P5`), and unreliable on some network filesystems. Stated, not
//!   hidden.
//! * **Containment** ([`JobCgroup`]) — a per-job cgroup v2 sub-cgroup and
//!   `cgroup.kill`, which is the only facility measured to reach a
//!   descendant that called `setsid()`. `process_group(0)` +
//!   `PR_SET_PDEATHSIG` stay, because they are what covers the *direct*
//!   child when no cgroup is available.
//! * **Admission** ([`admit`]) — one consistent capacity policy over a
//!   per-estate slot pool and a per-user host slot pool, both file locks,
//!   so the bound holds across processes and not merely within one
//!   daemon.
//!
//! **Not** supplied, on purpose: any promise that a descendant dies
//! immediately when Wirk itself is `SIGKILL`ed. Nothing of Wirk's runs
//! after that. What is offered is *bounded recovery* — the next owner,
//! under [`OwnerLock`], kills and removes the jobs this estate recorded —
//! and the interval between the two is the restart interval, which is not
//! bounded. Any text that implies otherwise is wrong.
//!
//! Nothing here writes outside the estate except the per-user host slot
//! directory under `$XDG_RUNTIME_DIR`, which is this uid's own runtime
//! directory. No host setting, no parent controller, no harness setting
//! and no other owner's cgroup is ever written.

use std::collections::BTreeMap;
use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::fd::{AsRawFd, IntoRawFd, RawFd};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------------
// Process hardening
// ---------------------------------------------------------------------

/// `orient/child.md` §3, R3+R5: own process group
/// (`CommandExt::process_group(0)`, not the parent's own group — sharing
/// it would make a group-kill of one job take the parent with it) plus
/// `libc::prctl(PR_SET_PDEATHSIG, SIGKILL)` armed in `pre_exec`, matching
/// sergeant-rs `child.rs:150-181`'s mechanism verbatim, not a `nix`
/// reimplementation of the same call.
///
/// Moved here from `wirk/src/executors/child.rs` unchanged (R2): the
/// `wirk` binary crate is unreachable from `wirk-atlas`, which needs the
/// identical mechanism for both backend protocols. Every caller in the
/// workspace now shares one implementation.
///
/// **What it does not do**, measured rather than assumed
/// (`refine/CHECKS.json` `P1`/`P3`): a grandchild that called `setsid()`
/// leaves this group and survives both the parent's death and
/// `kill(-pgid)`. [`JobCgroup`] is what covers that case.
pub fn harden_execution_child(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
    // SAFETY: this closure runs on the single forked child thread,
    // strictly after `fork` and strictly before `exec` — no other thread
    // exists yet in this process image, and no lock any other thread held
    // survives the fork to deadlock this one. Only async-signal-safe libc
    // calls are made (`prctl`, `getppid`, `_exit`), no allocation, no
    // `std` I/O — matching the SAFETY discipline sergeant-rs
    // `child.rs:150-159` documents for the same call.
    unsafe {
        command.pre_exec(|| {
            let parent_before = libc::getppid();
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            // Closes the fork/prctl race (`orient/child.md` §3): if the
            // parent died between `fork` and arming `PDEATHSIG`, the
            // signal was never armed against a still-live parent and
            // never will be delivered for this death — better to `_exit`
            // now than exec into a permanently orphaned child.
            if libc::getppid() != parent_before {
                libc::_exit(1);
            }
            Ok(())
        });
    }
}

/// Kill-then-reap on every exit path, not only a deadline. `pgid` is
/// always the child's own pid where [`harden_execution_child`] was used
/// (`process_group(0)`), so `-pgid` signals the whole group it leads. A
/// grandchild is not this process's child to `waitpid` on, only to
/// signal. Moved from `wirk/src/executors/child.rs` unchanged (R2).
pub fn kill_process_group(pgid: i32) {
    // SAFETY: a plain `kill(2)` call; `ESRCH` (already gone) is the
    // expected, ignored outcome on an already-exited group.
    unsafe {
        libc::kill(-pgid, libc::SIGKILL);
    }
}

// ---------------------------------------------------------------------
// Ownership: flock held for a lifetime
// ---------------------------------------------------------------------

/// An exclusive `flock` held for as long as this value lives, with the
/// holder's identity recorded in the file *under* the lock so a refusal
/// can name who has it.
///
/// Released by dropping it, and — the property that removes any need for
/// a stale-lock reaper — by the kernel when the holding process dies,
/// however it dies (measured, `refine/CHECKS.json` `P5`). The recorded
/// pid is therefore a *hint for the refusal message*, never the lock: a
/// pid-file check would be the "PID-only lock fiction" this design
/// refuses.
#[derive(Debug)]
pub struct OwnerLock {
    file: File,
    path: PathBuf,
}

/// Who a lock refusal says is holding it. Read from the file's recorded
/// hint, which may be absent or stale — the lock itself is the truth.
#[derive(Debug, Clone, Default)]
pub struct HolderHint {
    pub pid: Option<i32>,
    pub since_unix_millis: Option<i64>,
    pub detail: Option<String>,
}

impl HolderHint {
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if let Some(pid) = self.pid {
            parts.push(format!("pid {pid}"));
        }
        if let Some(since) = self.since_unix_millis {
            let elapsed = now_unix_millis().saturating_sub(since).max(0);
            parts.push(format!("held for {:.1}s", elapsed as f64 / 1000.0));
        }
        if let Some(detail) = &self.detail {
            parts.push(detail.clone());
        }
        if parts.is_empty() {
            "an unidentified holder (no recorded hint)".to_string()
        } else {
            parts.join(", ")
        }
    }
}

impl OwnerLock {
    /// Take the lock without waiting. `Ok(None)` means someone else holds
    /// it, with the hint they recorded.
    pub fn try_acquire(path: &Path, detail: &str) -> std::io::Result<Result<Self, HolderHint>> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        // SAFETY: `flock(2)` on a descriptor this process owns.
        let locked = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if locked != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EWOULDBLOCK) {
                return Ok(Err(read_hint(path)));
            }
            return Err(error);
        }
        let mut owner = Self {
            file,
            path: path.to_path_buf(),
        };
        owner.record(detail)?;
        Ok(Ok(owner))
    }

    /// Take the lock, waiting at most `timeout` and polling for it. A
    /// bounded wait, never an indefinite block: the refusal that follows
    /// a timeout is the visible outcome this whole increment is for.
    pub fn acquire_within(
        path: &Path,
        detail: &str,
        timeout: Duration,
    ) -> std::io::Result<Result<Self, HolderHint>> {
        let deadline = Instant::now() + timeout;
        loop {
            match Self::try_acquire(path, detail)? {
                Ok(owner) => return Ok(Ok(owner)),
                Err(hint) => {
                    if Instant::now() >= deadline {
                        return Ok(Err(hint));
                    }
                    std::thread::sleep(
                        POLL_INTERVAL.min(
                            deadline
                                .saturating_duration_since(Instant::now())
                                .max(Duration::from_millis(1)),
                        ),
                    );
                }
            }
        }
    }

    fn record(&mut self, detail: &str) -> std::io::Result<()> {
        let body = serde_json::to_string(&serde_json::json!({
            "pid": std::process::id(),
            "since_unix_millis": now_unix_millis(),
            "detail": detail,
        }))
        .unwrap_or_default();
        self.file.set_len(0)?;
        std::io::Seek::seek(&mut self.file, std::io::SeekFrom::Start(0))?;
        self.file.write_all(body.as_bytes())?;
        self.file.flush()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Release the lock explicitly, rather than relying on closing the file
/// to do it.
///
/// `flock(2)` belongs to the **open file description**, so closing our
/// own descriptor releases the lock only once *every* descriptor
/// referring to that description is closed. Any child forked by another
/// thread while this lock is held inherits a duplicate of it, and every
/// wirk job child is spawned through [`harden_execution_child`], whose
/// `pre_exec` hook forces the real `fork`+`exec` path — so the duplicate
/// exists for the whole window until `exec`, and longer for anything
/// that does not `exec` promptly. Until then a dropped [`Admission`]
/// kept its slot: the next admission was refused, naming the holder that
/// had already let go (measured — `a_released_slot_returns_even_while_a_
/// forked_child_holds_the_descriptor`).
///
/// `LOCK_UN` removes the lock from the open file description itself, so
/// it takes effect for every inherited duplicate at once. This is the
/// release path; the kernel's release on process death remains the
/// backstop that makes a stale-lock reaper unnecessary.
impl Drop for OwnerLock {
    fn drop(&mut self) {
        // SAFETY: `flock(2)` on a descriptor this process owns, which is
        // open for as long as `self.file` is.
        unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

/// The recorded hint, read with the crate's own JSON dependency rather
/// than a hand-rolled scanner.
///
/// An earlier version of this file parsed these files by string search.
/// It silently failed on `"key": 1` — a space after the colon — which
/// meant a configured deadline was quietly ignored and the default
/// reported instead. A config that is silently not applied is exactly the
/// invisible behaviour this increment exists to remove, so the parsing is
/// `serde_json`'s (R2: `wirk-core` already depends on it), and a
/// malformed file is *reported*, never shrugged off.
#[derive(Debug, Default, serde::Deserialize)]
struct RecordedHint {
    pid: Option<i32>,
    since_unix_millis: Option<i64>,
    detail: Option<String>,
}

fn read_hint(path: &Path) -> HolderHint {
    let Ok(body) = fs::read_to_string(path) else {
        return HolderHint::default();
    };
    let recorded: RecordedHint = serde_json::from_str(&body).unwrap_or_default();
    HolderHint {
        pid: recorded.pid,
        since_unix_millis: recorded.since_unix_millis,
        detail: recorded.detail,
    }
}

pub fn now_unix_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

const POLL_INTERVAL: Duration = Duration::from_millis(50);

// ---------------------------------------------------------------------
// Containment: a per-job cgroup v2 sub-cgroup
// ---------------------------------------------------------------------

/// What this host actually supports, with a reason whenever it does not.
///
/// Every field is the result of *probing*, not of assuming a platform.
/// `wirk wirkd status` prints this verbatim, so an operator sees
/// `memory_cap: unavailable (<reason>)` rather than a silent claim that a
/// cap is in force.
#[derive(Debug, Clone, Default)]
pub struct JobCapabilities {
    /// The cgroup this process is in, if it could be resolved and is
    /// writable by us. Jobs are created *inside* it, which is what makes
    /// them verifiably ours.
    pub own_cgroup: Option<PathBuf>,
    /// `cgroup.kill` — the only measured facility that reaches a
    /// `setsid()` descendant.
    pub kill_available: bool,
    pub kill_unavailable_reason: Option<String>,
    /// `memory.max` on a job cgroup, which exists only when the parent
    /// has `+memory` in `cgroup.subtree_control`. We never enable it:
    /// writing a parent controller is a host/parent-controller change,
    /// and an attempt on a populated cgroup fails `EBUSY` anyway.
    pub memory_cap_available: bool,
    pub memory_cap_unavailable_reason: Option<String>,
    /// `/proc/pressure/memory`, advisory.
    pub pressure_available: bool,
    pub pressure_unavailable_reason: Option<String>,
}

impl JobCapabilities {
    /// The one-line summary the human surfaces print. Deliberately says
    /// "bounded recovery" and never "prevention".
    pub fn summary(&self) -> String {
        let containment = if self.kill_available {
            "job-cgroup kill: available".to_string()
        } else {
            format!(
                "job-cgroup kill: unavailable ({}); containment degrades to process group + \
                 PDEATHSIG, which does not reach a descendant that called setsid()",
                self.kill_unavailable_reason.as_deref().unwrap_or("unknown")
            )
        };
        let memory = if self.memory_cap_available {
            "per-job memory cap: available".to_string()
        } else {
            format!(
                "per-job memory cap: unavailable ({})",
                self.memory_cap_unavailable_reason
                    .as_deref()
                    .unwrap_or("unknown")
            )
        };
        let pressure = if self.pressure_available {
            "memory pressure sample: available (advisory only, not an allocation guarantee)"
                .to_string()
        } else {
            format!(
                "memory pressure sample: unavailable ({})",
                self.pressure_unavailable_reason
                    .as_deref()
                    .unwrap_or("unknown")
            )
        };
        format!("{containment}; {memory}; {pressure}")
    }
}

/// What this host gives us, probed **once per process** and cached: the
/// answer is a property of the host and our placement in it, and probing
/// it on every store open would both cost a `mkdir`/`rmdir` per open and
/// let concurrent opens race in our own cgroup.
pub fn capabilities() -> &'static JobCapabilities {
    static CAPABILITIES: std::sync::OnceLock<JobCapabilities> = std::sync::OnceLock::new();
    CAPABILITIES.get_or_init(detect_capabilities)
}

/// Probe what this host gives us. Cheap: a few reads and one
/// `mkdir`/`rmdir` of a throwaway directory inside our *own* cgroup.
pub fn detect_capabilities() -> JobCapabilities {
    let mut capabilities = JobCapabilities::default();

    match std::fs::read_to_string("/proc/pressure/memory") {
        Ok(_) => capabilities.pressure_available = true,
        Err(error) => {
            capabilities.pressure_unavailable_reason =
                Some(format!("/proc/pressure/memory: {error}"));
        }
    }

    let own = match own_cgroup_dir() {
        Ok(path) => path,
        Err(reason) => {
            capabilities.kill_unavailable_reason = Some(reason.clone());
            capabilities.memory_cap_unavailable_reason = Some(reason);
            return capabilities;
        }
    };

    // Probe by actually creating and removing a sub-cgroup: whether we
    // may is a property of delegation, not of the path's spelling.
    // Unique per call, not per pid: two threads probing at once would
    // otherwise share one directory and race each other's create/remove.
    let probe = own.join(format!(
        "wirk-probe-{}-{}",
        std::process::id(),
        ulid::Ulid::generate()
    ));
    match fs::create_dir(&probe) {
        Ok(()) => {
            capabilities.own_cgroup = Some(own.clone());
            if probe.join("cgroup.kill").exists() {
                capabilities.kill_available = true;
            } else {
                capabilities.kill_unavailable_reason = Some(
                    "cgroup.kill is absent on a created sub-cgroup (needs Linux >= 5.14)"
                        .to_string(),
                );
            }
            if probe.join("memory.max").exists() {
                capabilities.memory_cap_available = true;
            } else {
                let controllers =
                    fs::read_to_string(own.join("cgroup.subtree_control")).unwrap_or_default();
                capabilities.memory_cap_unavailable_reason = Some(format!(
                    "memory.max is absent on a job cgroup: the parent {} does not carry +memory \
                     in cgroup.subtree_control (it has {:?}). Wirk does not enable a parent \
                     controller — that is a host/parent-controller change, and on a cgroup that \
                     already holds processes the kernel refuses it EBUSY anyway",
                    own.display(),
                    controllers.trim()
                ));
            }
            let _ = fs::remove_dir(&probe);
        }
        Err(error) => {
            let reason = format!(
                "cannot create a sub-cgroup under {}: {error}",
                own.display()
            );
            capabilities.kill_unavailable_reason = Some(reason.clone());
            capabilities.memory_cap_unavailable_reason = Some(reason);
        }
    }
    capabilities
}

/// Resolve this process's own cgroup v2 directory.
///
/// Delegates to [`cgroup_scope`], which discovers the `cgroup2` mount
/// from `/proc/self/mountinfo` instead of assuming `/sys/fs/cgroup`.
/// The assumption held on the box this was written on and is not a
/// property of Linux: a container, a user namespace or a non-default
/// mount point each move it.
fn own_cgroup_dir() -> Result<PathBuf, String> {
    cgroup_scope().map(|scope| scope.own)
}

/// One job's own cgroup. Created inside *our* cgroup, which is what makes
/// "this is ours" checkable without matching a name prefix across the
/// host — another owner's `wirk-job-*` is not inside our directory, and
/// we never look outside it.
#[derive(Debug)]
pub struct JobCgroup {
    dir: PathBuf,
    procs_fd: Option<RawFd>,
}

impl JobCgroup {
    pub fn create(capabilities: &JobCapabilities, job_id: &str) -> Option<Self> {
        let own = capabilities.own_cgroup.as_ref()?;
        if !capabilities.kill_available {
            return None;
        }
        let dir = own.join(format!("wirk-job-{job_id}"));
        fs::create_dir(&dir).ok()?;
        // Opened *before* the fork so `pre_exec` only has to `write` an
        // already-open descriptor — async-signal-safe, no allocation, no
        // path resolution on the child thread.
        let procs = OpenOptions::new()
            .write(true)
            .open(dir.join("cgroup.procs"))
            .ok()
            .map(|file| file.into_raw_fd());
        Some(Self {
            dir,
            procs_fd: procs,
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Best-effort per-job memory cap. Returns the reason when it cannot
    /// be applied, so the caller can say so instead of implying a cap.
    pub fn set_memory_max(&self, bytes: u64) -> Result<(), String> {
        let path = self.dir.join("memory.max");
        if !path.exists() {
            return Err("memory.max is not present on this job cgroup".to_string());
        }
        fs::write(&path, bytes.to_string())
            .map_err(|error| format!("writing {}: {error}", path.display()))?;
        // Without this a capped job merely swaps instead of being bounded.
        let swap = self.dir.join("memory.swap.max");
        if swap.exists() {
            let _ = fs::write(&swap, "0");
        }
        Ok(())
    }

    /// Arms the child: in `pre_exec`, write our own pid into the
    /// already-open `cgroup.procs` descriptor. Every process the child
    /// then forks is born inside this cgroup, `setsid()` or not — that is
    /// what a process group cannot do.
    pub fn arm(&self, command: &mut Command) {
        let Some(fd) = self.procs_fd else {
            return;
        };
        use std::os::unix::process::CommandExt;
        // SAFETY: runs on the single forked child thread, after `fork`
        // and before `exec`. `getpid`, `write` and the integer formatting
        // below are async-signal-safe and allocate nothing: the buffer is
        // a fixed stack array. A failure here is deliberately *not* fatal
        // — the child still has its own process group and PDEATHSIG, and
        // the caller has already been told containment may be degraded.
        unsafe {
            command.pre_exec(move || {
                let pid = libc::getpid();
                let mut buffer = [0u8; 24];
                let mut length = 0usize;
                let mut digits = [0u8; 20];
                let mut count = 0usize;
                let mut value = pid as u64;
                if value == 0 {
                    digits[0] = b'0';
                    count = 1;
                }
                while value > 0 {
                    digits[count] = b'0' + (value % 10) as u8;
                    value /= 10;
                    count += 1;
                }
                while count > 0 {
                    count -= 1;
                    buffer[length] = digits[count];
                    length += 1;
                }
                buffer[length] = b'\n';
                length += 1;
                libc::write(fd, buffer.as_ptr() as *const libc::c_void, length);
                Ok(())
            });
        }
    }

    /// Kill every process in this cgroup, `setsid()` descendants
    /// included. Measured at 0.02s on this host (`refine/CHECKS.json`
    /// `P2`).
    pub fn kill(&self) -> std::io::Result<()> {
        fs::write(self.dir.join("cgroup.kill"), "1")
    }

    pub fn procs(&self) -> Vec<i32> {
        fs::read_to_string(self.dir.join("cgroup.procs"))
            .map(|body| {
                body.lines()
                    .filter_map(|line| line.trim().parse().ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Kill, then remove. `rmdir` on a cgroup only succeeds once it is
    /// empty, so this polls briefly rather than pretending the kill is
    /// synchronous with respect to reaping.
    pub fn shutdown(&self) {
        let _ = self.kill();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if fs::remove_dir(&self.dir).is_ok() {
                return;
            }
            if !self.dir.exists() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for JobCgroup {
    fn drop(&mut self) {
        if let Some(fd) = self.procs_fd.take() {
            // SAFETY: a descriptor this value owns, closed exactly once.
            unsafe {
                libc::close(fd);
            }
        }
        self.shutdown();
    }
}

// ---------------------------------------------------------------------
// Owned job records: recovery that does not guess at ownership
// ---------------------------------------------------------------------

/// Where this estate records the jobs it started, so a later owner can
/// clean up exactly those.
///
/// This exists because **placement is not stable**: a restarted wirkd can
/// land in a different cgroup than the one its previous incarnation's
/// jobs were created under, so "scan my own cgroup for `wirk-job-*`" would
/// miss them. The tempting fix — scan the *parent* for the `wirk-job-`
/// prefix — is exactly what must not happen: another live estate's jobs,
/// or another user's, sit under that same parent and are not ours to
/// kill. A name prefix is not ownership.
///
/// So ownership is recorded where only this estate can have written it:
/// `<estate>/.wirk/jobs/<job>.json`, which names the absolute cgroup path
/// this estate created. Recovery reads its own records and touches
/// nothing it did not record. Another estate's records live under its own
/// root and are never read.
pub fn jobs_dir(estate_root: &Path) -> PathBuf {
    estate_root.join(".wirk").join("jobs")
}

/// A job this estate started, as recorded on disk.
#[derive(Debug, Clone)]
pub struct OwnedJobRecord {
    pub job_id: String,
    pub cgroup: Option<PathBuf>,
    pub pid: Option<i32>,
    pub verb: String,
    pub started_unix_millis: i64,
    pub staging: Option<PathBuf>,
}

fn record_path(estate_root: &Path, job_id: &str) -> PathBuf {
    jobs_dir(estate_root).join(format!("{job_id}.json"))
}

/// The on-disk shape of an owned job record.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct StoredJob {
    job_id: String,
    cgroup: Option<String>,
    pid: Option<i32>,
    verb: String,
    started_unix_millis: i64,
    staging: Option<String>,
}

pub fn record_owned_job(estate_root: &Path, record: &OwnedJobRecord) -> std::io::Result<()> {
    let directory = jobs_dir(estate_root);
    fs::create_dir_all(&directory)?;
    let stored = StoredJob {
        job_id: record.job_id.clone(),
        cgroup: record
            .cgroup
            .as_ref()
            .map(|path| path.display().to_string()),
        pid: record.pid,
        verb: record.verb.clone(),
        started_unix_millis: record.started_unix_millis,
        staging: record
            .staging
            .as_ref()
            .map(|path| path.display().to_string()),
    };
    let body = serde_json::to_vec(&stored)?;
    let path = record_path(estate_root, &record.job_id);
    // Renamed into place so a recovery pass never reads a half-written
    // record and acts on a partial cgroup path.
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, body)?;
    fs::rename(&temporary, &path)
}

pub fn clear_owned_job(estate_root: &Path, job_id: &str) {
    let _ = fs::remove_file(record_path(estate_root, job_id));
}

fn read_owned_job(path: &Path) -> Option<OwnedJobRecord> {
    let body = fs::read_to_string(path).ok()?;
    let stored: StoredJob = serde_json::from_str(&body).ok()?;
    Some(OwnedJobRecord {
        job_id: stored.job_id,
        cgroup: stored.cgroup.map(PathBuf::from),
        pid: stored.pid,
        verb: stored.verb,
        started_unix_millis: stored.started_unix_millis,
        staging: stored.staging.map(PathBuf::from),
    })
}

/// What a recovery pass actually did, so the caller can report it rather
/// than assert that everything is clean.
#[derive(Debug, Clone, Default)]
pub struct RecoveryOutcome {
    pub records_seen: usize,
    pub cgroups_killed: Vec<String>,
    pub staging_removed: Vec<String>,
    pub skipped: Vec<String>,
}

/// Kill and remove every job **this estate recorded** and then clear the
/// records. Called by the store's owner once it holds the ownership lock,
/// so exactly one process ever does this and it can only reach jobs this
/// estate wrote down.
///
/// This is **recovery, not prevention**. Between the moment Wirk was
/// killed and the moment the next owner runs this, a descendant that
/// escaped its process group keeps running. That interval is the restart
/// interval and it is not bounded. Nothing in this function shortens it.
pub fn recover_owned_jobs(estate_root: &Path) -> RecoveryOutcome {
    let mut outcome = RecoveryOutcome::default();
    let directory = jobs_dir(estate_root);
    let Ok(entries) = fs::read_dir(&directory) else {
        return outcome;
    };
    for entry in entries.filter_map(|entry| entry.ok()) {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let Some(record) = read_owned_job(&path) else {
            outcome.skipped.push(format!(
                "{}: unreadable job record, left in place rather than acted on",
                path.display()
            ));
            continue;
        };
        outcome.records_seen += 1;
        if let Some(cgroup) = &record.cgroup {
            // Only a directory this estate recorded, and only one that
            // still looks like a cgroup we made. Never a prefix scan.
            if cgroup.is_dir() && cgroup.join("cgroup.kill").exists() {
                let killed = fs::write(cgroup.join("cgroup.kill"), "1").is_ok();
                let deadline = Instant::now() + Duration::from_secs(5);
                while Instant::now() < deadline && fs::remove_dir(cgroup).is_err() {
                    if !cgroup.exists() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                if killed {
                    outcome.cgroups_killed.push(cgroup.display().to_string());
                }
            } else if cgroup.exists() {
                outcome.skipped.push(format!(
                    "{}: recorded path is not a cgroup we can act on; left alone",
                    cgroup.display()
                ));
            }
        }
        if let Some(staging) = &record.staging
            && staging.exists()
            && fs::remove_dir_all(staging).is_ok()
        {
            outcome.staging_removed.push(staging.display().to_string());
        }
        let _ = fs::remove_file(&path);
    }
    outcome
}

// ---------------------------------------------------------------------
// Policy
// ---------------------------------------------------------------------

/// Resource policy for one estate. Defaults are *construction* choices,
/// chosen to change no existing behaviour on the day they land, and are
/// overridable per estate in `<estate>/.wirk/resources.json`. No
/// machine-specific path or value appears here.
#[derive(Debug, Clone)]
pub struct ResourcePolicy {
    /// Expensive jobs this estate may run at once. Default 1 — the value
    /// already in force via wirkd's single atlas mutex, so the default
    /// changes no behaviour.
    pub max_expensive: u32,
    /// Expensive jobs this *user* may run at once across every estate on
    /// this host. Default 2.
    pub max_host_expensive: u32,
    /// Worktree materializations this estate may run at once.
    ///
    /// A **separate class on purpose**, and the reason is a real one:
    /// materializing a worktree is disk-and-IO work, while an embedding
    /// build is memory-and-compute work competing for one model runtime.
    /// Folding them into one pool at `max_expensive = 1` would serialize
    /// every concurrent Run in an estate behind a single slot — running
    /// two Works at once is ordinary, supported behaviour, and a resource
    /// bound must not quietly remove it.
    ///
    /// This class does **not** take a host expensive slot, because it is
    /// not competing for the resource that pool exists to protect. It is
    /// still bounded, still configurable, still refuses visibly, and
    /// still checks space before it starts.
    pub max_materialization: u32,
    /// How long an admission request may wait before it is refused.
    /// Default 0: refuse immediately and visibly, rather than wait
    /// invisibly, which is the defect being repaired.
    pub admission_wait_secs: u64,
    /// How long `AtlasStore::open` waits for a previous owner to go away
    /// before refusing.
    ///
    /// Non-zero for a measured reason, not for comfort. `flock` is held
    /// on the *open file description*, and `fork` duplicates it: a child
    /// another thread spawns while the owner fd is open keeps the lock
    /// alive until that child reaches `exec` and `O_CLOEXEC` closes its
    /// copy. Measured on this host: an exec'd child with a CLOEXEC fd
    /// released the lock, while a forked child that had not yet exec'd
    /// still held it. wirkd is thread-per-connection and spawns backend
    /// children, so that window is real. A short bounded wait absorbs it;
    /// a genuinely live owner still refuses, visibly, naming the holder.
    pub store_ownership_wait_millis: u64,
    /// How long a cheap read may wait for the atlas lock before
    /// answering `AtlasBusy`. Small and non-zero: a cheap read should
    /// tolerate another cheap read, never queue behind a build.
    pub cheap_wait_millis: u64,
    /// Wall-clock bound on one expensive child. Default 1 hour.
    pub job_deadline_secs: u64,
    /// `some avg10` on `/proc/pressure/memory` above which admission is
    /// refused. Advisory (see [`PressureSample`]).
    pub memory_pressure_avg10_max: f64,
    /// `MemAvailable` floor below which admission is refused. Advisory.
    pub min_available_memory_bytes: u64,
    /// Where the per-user host slot pool lives.
    ///
    /// `None` means the default: `$XDG_RUNTIME_DIR/wirk/expensive`, this
    /// uid's own runtime directory. No path is hardcoded in the product;
    /// the default is derived at runtime. An operator whose runtime
    /// directory is not shared the way this assumes — separate login
    /// sessions, a container with its own `/run/user` — can point every
    /// estate that should share a bound at one directory, which is the
    /// only way the host bound can mean anything in that arrangement.
    pub host_pool_dir: Option<PathBuf>,
    /// Per-job `memory.max`, applied only where the cgroup actually
    /// supports it. `None` means no cap is requested — and where a cap is
    /// requested but unsupported, the caller is told, never silently
    /// ignored.
    pub job_memory_max_bytes: Option<u64>,
    /// Per-class **soft** storage limits, in bytes, keyed by a name in
    /// [`crate::storage::CLASSES`]. Default empty.
    ///
    /// Soft is the whole contract and it is deliberate (P4.5 A, ruling
    /// 0256). Being over one of these is *disclosed* — on
    /// `wirk estate storage` and, where a class total is already in
    /// hand, on an admission's own notes — and refuses nothing. A hard
    /// storage refusal would turn an operator's budget into an outage
    /// on a Work that was already admitted, and there is no automatic
    /// reclamation behind it to make room: cleanup here is always
    /// user-selected (ruling 0124, "age is not evidence of orphanhood"
    /// applied to size).
    ///
    /// A key that is not a known class is reported rather than ignored,
    /// because a limit silently attached to nothing is worse than no
    /// limit at all.
    pub storage_soft_limits: BTreeMap<String, u64>,
    /// Whether this estate's `max_host_expensive` may *set* the shared
    /// host pool's agreed capacity, rather than merely be bound by it.
    ///
    /// Default `false`, and the default carries the whole point of
    /// [`PoolAgreement`]: a participant's own maximum is not a shared
    /// agreement. An operator who owns the pool says so explicitly, once,
    /// and only then does a differing local number change the shared
    /// bound — and even then not while it would strand a live lease.
    pub host_pool_capacity_authority: bool,
}

impl Default for ResourcePolicy {
    fn default() -> Self {
        Self {
            max_expensive: 1,
            max_host_expensive: 2,
            max_materialization: 4,
            admission_wait_secs: 0,
            store_ownership_wait_millis: 2_000,
            cheap_wait_millis: 250,
            job_deadline_secs: 3600,
            memory_pressure_avg10_max: 60.0,
            min_available_memory_bytes: 512 * 1024 * 1024,
            job_memory_max_bytes: None,
            host_pool_dir: None,
            host_pool_capacity_authority: false,
            storage_soft_limits: BTreeMap::new(),
        }
    }
}

/// The on-disk overlay. Every field optional: a resources.json names only
/// what it wants to change.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfiguredPolicy {
    max_expensive: Option<u32>,
    max_host_expensive: Option<u32>,
    max_materialization: Option<u32>,
    admission_wait_secs: Option<u64>,
    store_ownership_wait_millis: Option<u64>,
    cheap_wait_millis: Option<u64>,
    job_deadline_secs: Option<u64>,
    memory_pressure_avg10_max: Option<f64>,
    min_available_memory_bytes: Option<u64>,
    job_memory_max_bytes: Option<u64>,
    host_pool_dir: Option<String>,
    host_pool_capacity_authority: Option<bool>,
    storage_soft_limits: Option<BTreeMap<String, u64>>,
}

impl ResourcePolicy {
    pub fn config_path(estate_root: &Path) -> PathBuf {
        estate_root.join(".wirk").join("resources.json")
    }

    /// Read `<estate>/.wirk/resources.json` over the defaults.
    ///
    /// An absent file is the ordinary case, not an error. A **malformed**
    /// file is reported, and a value that cannot be applied is reported:
    /// silently falling back to a default the operator did not choose is
    /// the invisible behaviour this increment exists to remove. Every
    /// field is optional and overlays the default.
    pub fn load(estate_root: &Path) -> (Self, Option<String>) {
        let path = Self::config_path(estate_root);
        let Ok(body) = fs::read_to_string(&path) else {
            return (Self::default(), None);
        };
        let configured: ConfiguredPolicy = match serde_json::from_str(&body) {
            Ok(configured) => configured,
            Err(error) => {
                return (
                    Self::default(),
                    Some(format!(
                        "{}: is not readable as resource policy ({error}); this estate is \
                         running on built-in defaults, NOT on the values in that file",
                        path.display()
                    )),
                );
            }
        };
        let mut policy = Self::default();
        let mut complaints = Vec::new();
        macro_rules! overlay {
            ($field:ident) => {
                if let Some(value) = configured.$field {
                    policy.$field = value;
                }
            };
        }
        overlay!(max_expensive);
        overlay!(max_host_expensive);
        overlay!(max_materialization);
        overlay!(admission_wait_secs);
        overlay!(store_ownership_wait_millis);
        overlay!(cheap_wait_millis);
        overlay!(job_deadline_secs);
        overlay!(memory_pressure_avg10_max);
        overlay!(min_available_memory_bytes);
        overlay!(host_pool_capacity_authority);
        if let Some(bytes) = configured.job_memory_max_bytes {
            policy.job_memory_max_bytes = (bytes > 0).then_some(bytes);
        }
        if let Some(directory) = configured.host_pool_dir {
            policy.host_pool_dir = Some(PathBuf::from(directory));
        }
        if let Some(limits) = configured.storage_soft_limits {
            // Every key is checked against the one class vocabulary, so
            // a limit written for a class that does not exist is named
            // in the same complaint channel a zero concurrency value is
            // — never applied to nothing in silence.
            let (known, unknown): (BTreeMap<String, u64>, BTreeMap<String, u64>) = limits
                .into_iter()
                .partition(|(class, _)| crate::storage::is_class(class));
            if !unknown.is_empty() {
                complaints.push(format!(
                    "storage_soft_limits names {} that is not a storage class and was not \
                     applied; the classes are {}",
                    unknown.keys().cloned().collect::<Vec<_>>().join(", "),
                    crate::storage::CLASSES.join(", ")
                ));
            }
            policy.storage_soft_limits = known;
        }
        for (label, value) in [
            ("max_expensive", &mut policy.max_expensive),
            ("max_host_expensive", &mut policy.max_host_expensive),
            ("max_materialization", &mut policy.max_materialization),
        ] {
            if *value == 0 {
                complaints.push(format!("{label} 0 would admit nothing; using 1"));
                *value = 1;
            }
        }
        let note = (!complaints.is_empty())
            .then(|| format!("{}: {}", path.display(), complaints.join("; ")));
        (policy, note)
    }

    /// This estate's own local clamp: `min(max_expensive,
    /// max_host_expensive)`, disclosed rather than applied quietly.
    ///
    /// **This is not the shared agreement.** The earlier version of this
    /// comment claimed "a second estate cannot bypass the host bound
    /// either", and that was only true when every estate was configured
    /// identically — the host pool was sized from whichever caller was
    /// asking, so a wider caller simply created extra slots. The shared
    /// bound is [`PoolAgreement`], read from the pool itself; [`admit`]
    /// clamps this number against it. What is left here is the purely
    /// local part: an estate does not offer itself more slots than its
    /// own configuration asks for.
    pub fn effective_estate_slots(&self) -> u32 {
        self.max_expensive.min(self.max_host_expensive)
    }

    pub fn capacity_note(&self) -> Option<String> {
        (self.max_expensive > self.max_host_expensive).then(|| {
            format!(
                "max_expensive {} exceeds max_host_expensive {}; this estate is offered {} \
                 expensive slot(s) so the per-user host bound is not bypassed",
                self.max_expensive,
                self.max_host_expensive,
                self.effective_estate_slots()
            )
        })
    }
}

// ---------------------------------------------------------------------
// Memory: what actually constrains this process, labelled by kind
// ---------------------------------------------------------------------

/// Where the cgroup v2 filesystem is actually mounted, and what this
/// process's own path inside it is.
///
/// Discovered, never assumed. The earlier version of this module joined
/// `/sys/fs/cgroup` to the `0::` line by hand, which is this development
/// box's arrangement rather than a property of Linux: a container, a
/// user namespace or a non-default `cgroup2` mount point all break it.
/// `/proc/self/mountinfo` is the kernel's own answer to "where is it",
/// and the `0::` path is relative to *this process's cgroup namespace
/// root*, which is exactly what the mount shows us.
#[derive(Debug, Clone)]
pub struct CgroupScope {
    /// The `cgroup2` mount point as this process sees it.
    pub mount: PathBuf,
    /// This process's own cgroup directory beneath that mount.
    pub own: PathBuf,
}

/// Resolve the `cgroup2` mount point from `/proc/self/mountinfo`.
///
/// The mountinfo line's *root* field matters: in a cgroup namespace the
/// mount may expose a subtree, and the `0::` path is already expressed
/// relative to that same namespace root, so joining the two is correct
/// while joining `0::` to a hardcoded `/sys/fs/cgroup` is not.
fn cgroup2_mount() -> Result<PathBuf, String> {
    let body = fs::read_to_string("/proc/self/mountinfo")
        .map_err(|error| format!("/proc/self/mountinfo: {error}"))?;
    for line in body.lines() {
        // `... - <fstype> <source> <superopts>`; the separator is a
        // lone "-" field, and the mount point is field 5 before it.
        let Some((before, after)) = line.split_once(" - ") else {
            continue;
        };
        let fstype = after.split_whitespace().next().unwrap_or_default();
        if fstype != "cgroup2" {
            continue;
        }
        if let Some(point) = before.split_whitespace().nth(4) {
            // mountinfo octal-escapes space, tab, newline and backslash.
            return Ok(PathBuf::from(unescape_mountinfo(point)));
        }
    }
    Err(
        "no cgroup2 mount in /proc/self/mountinfo: this system is cgroup v1 or has no unified \
         hierarchy"
            .to_string(),
    )
}

fn unescape_mountinfo(field: &str) -> String {
    let mut out = String::with_capacity(field.len());
    let mut chars = field.chars();
    while let Some(character) = chars.next() {
        if character != '\\' {
            out.push(character);
            continue;
        }
        let digits: String = chars.clone().take(3).collect();
        match u8::from_str_radix(&digits, 8) {
            Ok(byte) if digits.len() == 3 => {
                out.push(byte as char);
                for _ in 0..3 {
                    chars.next();
                }
            }
            _ => out.push('\\'),
        }
    }
    out
}

/// This process's cgroup v2 scope: the real mount and the real path.
pub fn cgroup_scope() -> Result<CgroupScope, String> {
    if !cfg!(target_os = "linux") {
        return Err("not Linux: cgroup v2 is Linux-only".to_string());
    }
    let mount = cgroup2_mount()?;
    let body = fs::read_to_string("/proc/self/cgroup")
        .map_err(|error| format!("/proc/self/cgroup: {error}"))?;
    let relative = body
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .ok_or_else(|| "no unified (0::) entry in /proc/self/cgroup: cgroup v1 only".to_string())?;
    let own = mount.join(relative.trim().trim_start_matches('/'));
    if !own.is_dir() {
        return Err(format!(
            "{} is not a readable directory: this process's cgroup is not visible at its own \
             mount, which happens in a namespace that does not export it",
            own.display()
        ));
    }
    Ok(CgroupScope { mount, own })
}

/// One cgroup level that actually constrains this process, with the
/// **kind** of each number kept distinct.
///
/// `memory.high` and `memory.max` are not the same promise and must not
/// be added together or reported as one "limit":
///
/// * `memory.max` is a **hard limit**. Exceeding it means reclaim, then
///   the OOM killer. Headroom against it is the closest thing to a
///   ceiling that exists, and it is still not an allocation guarantee.
/// * `memory.high` is a **soft threshold**. Exceeding it means the
///   kernel throttles the cgroup and reclaims aggressively; nothing is
///   killed. Headroom against it predicts *stalling*, which is what the
///   incident under ruling 0246 actually looked like.
/// * `memory.current` and `memory.pressure` are **advisory samples** of
///   an instant and of the recent past respectively.
#[derive(Debug, Clone)]
pub struct CgroupLevel {
    /// Path of the level, relative to the cgroup2 mount — never the
    /// absolute host path, which would leak the layout of a hierarchy
    /// the reader may not be in.
    pub relative: String,
    pub current_bytes: Option<u64>,
    /// Soft throttling threshold, `None` when `max` (unset).
    pub high_bytes: Option<u64>,
    /// Hard ceiling, `None` when `max` (unset).
    pub max_bytes: Option<u64>,
    pub some_avg10: Option<f64>,
    pub full_avg10: Option<f64>,
}

impl CgroupLevel {
    /// Bytes before this level starts being throttled. Soft.
    pub fn soft_headroom(&self) -> Option<u64> {
        match (self.high_bytes, self.current_bytes) {
            (Some(high), Some(current)) => Some(high.saturating_sub(current)),
            _ => None,
        }
    }

    /// Bytes before this level hits its hard ceiling.
    pub fn hard_headroom(&self) -> Option<u64> {
        match (self.max_bytes, self.current_bytes) {
            (Some(max), Some(current)) => Some(max.saturating_sub(current)),
            _ => None,
        }
    }
}

/// Everything observed about memory at one instant, from every scope
/// that applies, with each number's kind and origin preserved.
///
/// **None of this is an allocation guarantee.** Host `MemAvailable` is
/// the kernel's own estimate; `current` and the pressure averages are
/// samples. What changed in this correction is not the confidence, it is
/// the *scope*: reading only the host's `MemAvailable` answers a
/// question about the machine when the question that matters is what
/// this process is allowed to use. Under ruling 0246 those differed by
/// more than three times, in the direction that admits work that then
/// stalls.
#[derive(Debug, Clone, Default)]
pub struct MemoryObservation {
    /// Host `MemAvailable`, the whole-machine estimate.
    pub host_available_bytes: Option<u64>,
    /// Root `/proc/pressure/memory` `some avg10`.
    pub host_some_avg10: Option<f64>,
    /// Every cgroup level from this process's own cgroup up to the
    /// mount root that could be read, nearest first.
    pub levels: Vec<CgroupLevel>,
    /// Smallest soft headroom over all levels that declare a
    /// `memory.high`, with the level that produced it.
    pub soft_headroom_bytes: Option<u64>,
    pub soft_headroom_from: Option<String>,
    /// Smallest hard headroom over all levels that declare a
    /// `memory.max`.
    pub hard_headroom_bytes: Option<u64>,
    pub hard_headroom_from: Option<String>,
    /// The `some avg10` of the nearest level that publishes one,
    /// preferred over the host figure because it describes the scope the
    /// job will actually run in.
    pub scoped_some_avg10: Option<f64>,
    pub scoped_some_avg10_from: Option<String>,
    /// Why a capability is missing, when it is. Never fatal: an
    /// unsupported environment gets host-only observation and is told
    /// so, rather than a silent claim of a check that did not happen.
    pub unavailable: Vec<String>,
}

impl MemoryObservation {
    /// The number a memory floor is compared against: the smallest of
    /// the host estimate and every applicable **hard** cgroup ceiling
    /// that could be read. `None` when nothing at all could be observed.
    ///
    /// **Soft headroom is deliberately not in this minimum**, and the
    /// distinction is the whole reason the two are tracked separately.
    /// `memory.high` is a throttling threshold the kernel actively holds
    /// `memory.current` near by reclaiming — a healthy, busy cgroup sits
    /// at its `high` more or less permanently, and much of what it is
    /// counting is reclaimable page cache. Treating exhausted soft
    /// headroom as "no memory available" would refuse all work on any
    /// host that sets a `memory.high` at all, which is a worse failure
    /// than the one being repaired. Soft headroom is disclosed, and
    /// sustained throttling shows up where it actually belongs: in the
    /// scoped pressure figure.
    pub fn effective_available_bytes(&self) -> Option<u64> {
        [self.host_available_bytes, self.hard_headroom_bytes]
            .into_iter()
            .flatten()
            .min()
    }

    /// Which observation produced [`Self::effective_available_bytes`],
    /// phrased for a refusal message so an operator knows which bound to
    /// act on and what kind of bound it is.
    pub fn effective_available_origin(&self) -> Option<String> {
        let effective = self.effective_available_bytes()?;
        if self.hard_headroom_bytes == Some(effective) {
            return Some(format!(
                "the hard ceiling (memory.max) of {}, a cgroup this process runs in — exceeding \
                 it means reclaim and then the OOM killer",
                self.hard_headroom_from.clone().unwrap_or_default()
            ));
        }
        Some("the host's MemAvailable estimate".to_string())
    }

    /// Whether this process is currently inside a cgroup that is at or
    /// past its soft throttling threshold. Advisory disclosure only: it
    /// refuses nothing on its own.
    pub fn is_throttled(&self) -> bool {
        self.soft_headroom_bytes == Some(0)
    }

    /// The pressure figure admission should use: the nearest applicable
    /// scope, falling back to the host.
    pub fn admission_some_avg10(&self) -> Option<f64> {
        self.scoped_some_avg10.or(self.host_some_avg10)
    }

    pub fn admission_some_avg10_origin(&self) -> String {
        match &self.scoped_some_avg10_from {
            Some(level) => format!("cgroup {level} memory.pressure"),
            None => "host /proc/pressure/memory".to_string(),
        }
    }
}

fn read_u64_file(path: &Path) -> Option<u64> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// `memory.high`/`memory.max` hold either a byte count or the literal
/// `max`, which means *no limit* and must not be read as a number.
fn read_limit_file(path: &Path) -> Option<u64> {
    let body = fs::read_to_string(path).ok()?;
    let body = body.trim();
    if body == "max" {
        None
    } else {
        body.parse().ok()
    }
}

fn read_pressure_file(path: &Path) -> (Option<f64>, Option<f64>) {
    let Ok(body) = fs::read_to_string(path) else {
        return (None, None);
    };
    let field = |prefix: &str| {
        body.lines()
            .find(|line| line.starts_with(prefix))
            .and_then(|line| {
                line.split_whitespace()
                    .find_map(|part| part.strip_prefix("avg10="))
            })
            .and_then(|value| value.parse::<f64>().ok())
    };
    (field("some "), field("full "))
}

/// Walk from `scope.own` up to `scope.mount`, reading every level that
/// applies to this process.
///
/// **Every** ancestor, not the nearest one that happens to carry a
/// limit. The verifier's suggested "nearest limiting ancestor" is a
/// reasonable guess about this host's layout and nothing more: limits
/// nest, a grandparent's `memory.max` constrains a child whose parent
/// declares none, and the binding constraint is the *minimum* headroom
/// over the whole chain. Reading one level would be right here and
/// wrong on a two-level slice.
///
/// Unreadable levels are skipped rather than fatal — a namespace can
/// expose a directory whose interface files are not readable, and half
/// an answer with the gap disclosed beats no answer or a false one.
pub fn observe_cgroup_levels(scope: &CgroupScope) -> Vec<CgroupLevel> {
    let mut levels = Vec::new();
    let mut cursor = scope.own.clone();
    loop {
        let relative = cursor
            .strip_prefix(&scope.mount)
            .map(|path| {
                let text = path.to_string_lossy().to_string();
                if text.is_empty() {
                    "/".to_string()
                } else {
                    format!("/{text}")
                }
            })
            .unwrap_or_else(|_| "/".to_string());
        let (some_avg10, full_avg10) = read_pressure_file(&cursor.join("memory.pressure"));
        let level = CgroupLevel {
            relative,
            current_bytes: read_u64_file(&cursor.join("memory.current")),
            high_bytes: read_limit_file(&cursor.join("memory.high")),
            max_bytes: read_limit_file(&cursor.join("memory.max")),
            some_avg10,
            full_avg10,
        };
        let empty = level.current_bytes.is_none()
            && level.high_bytes.is_none()
            && level.max_bytes.is_none()
            && level.some_avg10.is_none();
        if !empty {
            levels.push(level);
        }
        if cursor == scope.mount {
            break;
        }
        match cursor.parent() {
            Some(parent) if parent.starts_with(&scope.mount) || parent == scope.mount => {
                cursor = parent.to_path_buf();
            }
            _ => break,
        }
    }
    levels
}

/// Observe memory from every scope that applies to this process.
pub fn observe_memory() -> MemoryObservation {
    let mut observation = MemoryObservation::default();

    match fs::read_to_string("/proc/meminfo") {
        Ok(body) => {
            observation.host_available_bytes = body
                .lines()
                .find(|line| line.starts_with("MemAvailable:"))
                .and_then(|line| line.split_whitespace().nth(1))
                .and_then(|value| value.parse::<u64>().ok())
                .map(|kb| kb * 1024);
        }
        Err(error) => observation
            .unavailable
            .push(format!("host MemAvailable: /proc/meminfo: {error}")),
    }
    let (host_some, _) = read_pressure_file(Path::new("/proc/pressure/memory"));
    if host_some.is_none() {
        observation.unavailable.push(
            "host /proc/pressure/memory: not readable (kernel built without PSI, or not \
                   exported here)"
                .to_string(),
        );
    }
    observation.host_some_avg10 = host_some;

    match cgroup_scope() {
        Ok(scope) => {
            observation.levels = observe_cgroup_levels(&scope);
            if observation.levels.is_empty() {
                observation.unavailable.push(format!(
                    "cgroup memory accounting: no level under {} exposes memory interface files \
                     (the memory controller is not enabled for this process's hierarchy)",
                    scope.mount.display()
                ));
            }
        }
        Err(reason) => observation
            .unavailable
            .push(format!("cgroup memory accounting: {reason}")),
    }

    summarize_levels(&mut observation);
    observation
}

fn summarize_levels(observation: &mut MemoryObservation) {
    for level in &observation.levels {
        if let Some(soft) = level.soft_headroom()
            && observation
                .soft_headroom_bytes
                .is_none_or(|held| soft < held)
        {
            observation.soft_headroom_bytes = Some(soft);
            observation.soft_headroom_from = Some(level.relative.clone());
        }
        if let Some(hard) = level.hard_headroom()
            && observation
                .hard_headroom_bytes
                .is_none_or(|held| hard < held)
        {
            observation.hard_headroom_bytes = Some(hard);
            observation.hard_headroom_from = Some(level.relative.clone());
        }
        if observation.scoped_some_avg10.is_none()
            && let Some(avg10) = level.some_avg10
        {
            observation.scoped_some_avg10 = Some(avg10);
            observation.scoped_some_avg10_from = Some(level.relative.clone());
        }
    }
    if observation.soft_headroom_bytes.is_none() {
        observation.unavailable.push(
            "soft memory headroom: no cgroup level this process runs in declares a memory.high, \
             so no throttling threshold applies"
                .to_string(),
        );
    }
    if observation.hard_headroom_bytes.is_none() {
        observation.unavailable.push(
            "hard memory headroom: no cgroup level this process runs in declares a memory.max, \
             so no cgroup ceiling applies"
                .to_string(),
        );
    }
}

/// Observe memory against an **injected** cgroup scope, for tests that
/// need a controlled hierarchy shape without touching a real one.
///
/// This exists so the walking and the arithmetic can be pinned against
/// fixtures — a two-level slice, a `max` with no `high`, an unreadable
/// level — on any machine, rather than only where a particular
/// hierarchy happens to be configured. It reads; it never writes a
/// controller, here or anywhere.
pub fn observe_memory_in(scope: &CgroupScope) -> MemoryObservation {
    let mut observation = MemoryObservation {
        levels: observe_cgroup_levels(scope),
        ..MemoryObservation::default()
    };
    summarize_levels(&mut observation);
    observation
}

/// A memory observation at one instant, in the shape the earlier
/// increment published.
///
/// Retained because `wirkd ping` and the store both read it, and because
/// the host figures remain genuinely useful — they are simply not the
/// whole answer. [`MemoryObservation`] is what admission now uses.
#[derive(Debug, Clone, Default)]
pub struct PressureSample {
    pub some_avg10: Option<f64>,
    pub available_bytes: Option<u64>,
    pub unavailable_reason: Option<String>,
}

pub fn sample_pressure() -> PressureSample {
    let observed = observe_memory();
    PressureSample {
        some_avg10: observed.admission_some_avg10(),
        available_bytes: observed.effective_available_bytes(),
        unavailable_reason: (!observed.unavailable.is_empty())
            .then(|| observed.unavailable.join("; ")),
    }
}

/// Bytes actually available to an unprivileged writer on the filesystem
/// holding `path`, via `statvfs(3)`.
pub fn available_space_bytes(path: &Path) -> Result<u64, String> {
    let raw = CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|_| format!("{}: path contains a NUL byte", path.display()))?;
    // SAFETY: `statvfs(3)` against a NUL-terminated path we own, writing
    // into a zeroed struct we own.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    let result = unsafe { libc::statvfs(raw.as_ptr(), &mut stat) };
    if result != 0 {
        return Err(format!(
            "statvfs {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        ));
    }
    Ok(stat.f_bavail as u64 * stat.f_frsize as u64)
}

// ---------------------------------------------------------------------
// Admission
// ---------------------------------------------------------------------

/// Why a job was not admitted. Each variant names the verb, the holder
/// and the elapsed time where it can, so a refusal is actionable instead
/// of merely negative.
#[derive(Debug, Clone)]
pub struct Refusal {
    pub code: &'static str,
    pub message: String,
    /// P4.5 B correction (ruling 0251, F2): what the caller should have
    /// been told about this admission *anyway* — a clamped shared
    /// capacity, the pool's agreed number against this estate's own, the
    /// deliberate way to change it.
    ///
    /// These were collected before the refusal and then thrown away,
    /// because only the `Ok` path printed them. The actionable half of
    /// the disclosure therefore vanished exactly when the operator most
    /// needed it. A refusal carries them now, and the daemon puts them
    /// on the wire.
    pub notes: Vec<String>,
}

impl Refusal {
    /// Carry the admission notes gathered before this refusal, keeping
    /// them ahead of any the refusing step added of its own.
    pub fn with_notes_before(mut self, earlier: &[String]) -> Self {
        let mut all = earlier.to_vec();
        all.append(&mut self.notes);
        self.notes = all;
        self
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

/// Which bound a job is subject to.
///
/// Two classes, not one, because they contend for different things — see
/// [`ResourcePolicy::max_materialization`]. There is no third: a class
/// per verb would be a capacity policy nobody could reason about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobClass {
    /// Model-runtime work: acquire, refresh, semantic build. Takes an
    /// estate slot *and* a host slot, so this user cannot exceed the host
    /// bound by spreading jobs across estates.
    Expensive,
    /// Worktree materialization. Estate-bounded only.
    Materialization,
}

/// What the caller wants to run. `detail` is recorded in the **estate**
/// slot file only.
#[derive(Debug, Clone)]
pub struct JobRequest {
    pub class: JobClass,
    pub verb: String,
    pub detail: String,
    /// The filesystem the job will write to, if it writes: checked
    /// against `estimate_bytes` before admission.
    pub space_path: Option<PathBuf>,
    pub estimate_bytes: Option<u64>,
}

impl JobRequest {
    pub fn new(verb: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            class: JobClass::Expensive,
            verb: verb.into(),
            detail: detail.into(),
            space_path: None,
            estimate_bytes: None,
        }
    }

    pub fn in_class(mut self, class: JobClass) -> Self {
        self.class = class;
        self
    }

    pub fn with_space(mut self, path: impl Into<PathBuf>, estimate_bytes: u64) -> Self {
        self.space_path = Some(path.into());
        self.estimate_bytes = Some(estimate_bytes);
        self
    }
}

/// An admitted job. Both slots are held for exactly as long as this value
/// lives; dropping it releases them, and so does the holder dying.
#[derive(Debug)]
pub struct Admission {
    _estate_slot: OwnerLock,
    _host_slot: Option<OwnerLock>,
    pub job_id: String,
    pub waited: Duration,
    /// Things the operator should know about this admission that are not
    /// refusals: a clamped capacity, an unavailable pressure sample, a
    /// requested memory cap that this host cannot apply.
    pub notes: Vec<String>,
}

fn host_slot_dir(policy: &ResourcePolicy) -> PathBuf {
    if let Some(explicit) = &policy.host_pool_dir {
        return explicit.clone();
    }
    default_host_slot_dir()
}

fn default_host_slot_dir() -> PathBuf {
    // This uid's own runtime directory. Not a host setting, not shared
    // with another user, and removed by the system on logout.
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(value) => PathBuf::from(value).join("wirk").join("expensive"),
        None => std::env::temp_dir()
            .join(format!("wirk-{}", unsafe { libc::getuid() }))
            .join("expensive"),
    }
}

fn estate_slot_dir(estate_root: &Path) -> PathBuf {
    estate_root.join(".wirk").join("expensive")
}

/// Try each slot in a pool once; the first free one wins. Returns the
/// hint of the *longest-held* contender when the pool is full, which is
/// the most useful thing to name in a refusal.
fn take_slot(
    directory: &Path,
    slots: u32,
    detail: &str,
) -> std::io::Result<Result<OwnerLock, HolderHint>> {
    fs::create_dir_all(directory)?;
    let mut oldest: Option<HolderHint> = None;
    for index in 0..slots {
        let path = directory.join(format!("slot-{index}"));
        match OwnerLock::try_acquire(&path, detail)? {
            Ok(owner) => return Ok(Ok(owner)),
            Err(hint) => {
                let older = match (&oldest, &hint.since_unix_millis) {
                    (None, _) => true,
                    (Some(current), Some(candidate)) => current
                        .since_unix_millis
                        .is_none_or(|held| *candidate < held),
                    (Some(_), None) => false,
                };
                if older {
                    oldest = Some(hint);
                }
            }
        }
    }
    Ok(Err(oldest.unwrap_or_default()))
}

// ---------------------------------------------------------------------
// Shared pool capacity: a property of the pool, not of the caller
// ---------------------------------------------------------------------

/// The capacity a shared slot pool was agreed at, recorded in the pool
/// itself.
///
/// **The defect this repairs.** Admission used to size the host pool
/// from `policy.max_host_expensive` — the *caller's* number. Slot files
/// are created on demand, so an estate configured `3` simply created
/// `slot-1` and `slot-2` in a pool an estate configured `1` believed was
/// full at one, and both ran. Effective host capacity was the maximum
/// over every participant's configuration, which is the opposite of a
/// shared bound: the least restrained participant set it, silently.
///
/// A shared bound has to be a property of the shared thing. This record
/// is written once, under the pool's own lock, and every later
/// participant reads it and is bound by it. A caller's own maximum is
/// still honoured where it is *more* restrictive — restraining yourself
/// is always safe — but it can never widen the pool.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PoolAgreement {
    /// Slots this pool offers. No participant may create a slot index at
    /// or beyond this number.
    pub capacity: u32,
    /// When it was agreed, for an operator reading the file.
    pub agreed_unix_millis: i64,
    /// Why the number is what it is, in the pool for whoever finds it.
    /// Carries no estate, source or Work path: this file is shared
    /// across every estate this uid runs.
    pub note: String,
}

/// What a participant is actually allowed to do in a shared pool, and
/// what it should be told about the difference from what it asked for.
#[derive(Debug, Clone)]
pub struct PoolStanding {
    /// The pool's agreed capacity.
    pub agreed_capacity: u32,
    /// Slots this participant may take: `min(agreed, requested)`.
    pub usable_slots: u32,
    /// Disclosure of any disagreement between the two.
    pub notes: Vec<String>,
}

fn agreement_path(pool: &Path) -> PathBuf {
    pool.join("capacity.json")
}

/// Where this policy's shared expensive-job pool actually lives, for a
/// caller that wants to *inspect* it rather than admit into it.
pub fn host_pool_directory(policy: &ResourcePolicy) -> PathBuf {
    host_slot_dir(policy)
}

/// What an operator can learn about the shared pool **without touching
/// it** (ruling 0251, F1/F3).
///
/// Three different things that were previously conflated into one
/// number, and the whole value of this view is that they stay apart:
///
/// - `configured_*`: what *this estate's* `resources.json` asks for.
///   A preference. Never a host bound, never a shared agreement.
/// - `agreement`: what the pool itself has recorded, if anything. The
///   only number that binds every participant.
/// - `effective_usable_slots`: what this estate would actually be
///   offered right now — `min(agreed, configured)` — and `None` when
///   the pool has not been initialized, because then there is no
///   answer yet and inventing one would be the same false precision
///   this repairs.
///
/// **Reading never initializes.** `pool_standing` writes a first
/// agreement as a side effect of admitting a job, which is correct
/// there and would be indefensible here: an operator asking "what is
/// the shared policy?" must not thereby become the participant that
/// set it. Nothing in this function creates a directory, a lock or a
/// slot.
#[derive(Debug, Clone)]
pub struct PoolInspection {
    pub directory: PathBuf,
    pub configured_max_host_expensive: u32,
    pub configured_capacity_authority: bool,
    /// The pool's own recorded agreement, when it has one.
    pub agreement: Option<PoolAgreement>,
    /// Why there is no agreement to report: `"uninitialized"` (no
    /// record yet) or `"unreadable"` (a record that could not be
    /// parsed — reported, never silently treated as absent).
    pub agreement_status: &'static str,
    pub effective_usable_slots: Option<u32>,
    /// The same disclosure `admit` would produce, in advance: this
    /// caller's restraint, or the deliberate way to change the number.
    pub notes: Vec<String>,
}

/// Inspect a shared pool read-only. See [`PoolInspection`].
pub fn inspect_pool(pool: &Path, requested: u32, authority: bool) -> PoolInspection {
    let path = agreement_path(pool);
    let (agreement, status) = match fs::read_to_string(&path) {
        Err(_) => (None, "uninitialized"),
        Ok(body) => match serde_json::from_str::<PoolAgreement>(&body) {
            Ok(agreement) => (Some(agreement), "initialized"),
            Err(_) => (None, "unreadable"),
        },
    };
    let mut notes = Vec::new();
    let usable = match &agreement {
        None if status == "uninitialized" => {
            notes.push(format!(
                "this shared pool has no agreed capacity yet. The first participant to admit an \
                 expensive job initializes it — at {requested} slot(s) if that is this estate; \
                 reading it here does not create it. Effective shared capacity therefore depends \
                 on which estate starts first, so an operator who wants a particular shared bound \
                 establishes it deliberately: run one expensive job from the estate whose number \
                 should govern before the others start, or set host_pool_capacity_authority in \
                 that estate's resources.json and let it change the agreement later."
            ));
            None
        }
        None => {
            notes.push(format!(
                "this shared pool's capacity record at {} could not be read as an agreement; no \
                 shared bound can be reported from it and the next admission will refuse rather \
                 than run unbounded",
                path.display()
            ));
            None
        }
        Some(agreement) => {
            let usable = agreement.capacity.min(requested);
            if requested > agreement.capacity {
                notes.push(format!(
                    "this estate's max_host_expensive is {requested} but the shared pool's agreed \
                     capacity is {}; the pool's number governs. A participant's own maximum is \
                     not a shared agreement — set host_pool_capacity_authority in this estate's \
                     resources.json to change the pool deliberately.",
                    agreement.capacity
                ));
            } else if requested < agreement.capacity {
                notes.push(format!(
                    "this estate restrains itself to {usable} of the pool's {} slot(s); it \
                     cannot impose that smaller bound on the other participants, which may still \
                     use all {}",
                    agreement.capacity, agreement.capacity
                ));
            }
            if authority {
                notes.push(
                    "this estate holds host_pool_capacity_authority: a differing \
                     max_host_expensive here changes the shared agreement at its next expensive \
                     admission, unless live leases outside the new capacity would be stranded"
                        .to_string(),
                );
            }
            Some(usable)
        }
    };
    PoolInspection {
        directory: pool.to_path_buf(),
        configured_max_host_expensive: requested,
        configured_capacity_authority: authority,
        agreement,
        agreement_status: status,
        effective_usable_slots: usable,
        notes,
    }
}

/// Which slot indexes in `[from, to)` are currently leased.
///
/// Probing is a non-blocking `flock` attempt that is released
/// immediately. A slot file that does not exist was never leased, so it
/// is not even opened — probing must not create the very slots a shrink
/// is trying to retire.
fn live_leases(pool: &Path, from: u32, to: u32) -> Vec<(u32, HolderHint)> {
    let mut live = Vec::new();
    for index in from..to {
        let path = pool.join(format!("slot-{index}"));
        if !path.exists() {
            continue;
        }
        match OwnerLock::try_acquire(&path, "capacity probe") {
            Ok(Ok(_released_immediately)) => {}
            Ok(Err(hint)) => live.push((index, hint)),
            Err(_) => {}
        }
    }
    live
}

/// Read the pool's agreement, creating it if this is the first
/// participant, and return what this caller may actually use.
///
/// Everything here happens under the pool's **own** lock, so two
/// participants initializing concurrently cannot both write a first
/// agreement: the loser reads the winner's and is bound by it.
///
/// `authority` is the operator's explicit statement that this estate's
/// configuration may set policy for the shared pool. It defaults off,
/// and that default is the point: without it, a participant's own
/// maximum is never mistaken for the shared agreement. With it, a
/// deliberate change is still refused when it would strand a live lease.
pub fn pool_standing(
    pool: &Path,
    requested: u32,
    authority: bool,
) -> Result<PoolStanding, Refusal> {
    fs::create_dir_all(pool).map_err(|error| Refusal {
        notes: Vec::new(),
        code: "PoolUnusable",
        message: format!(
            "the shared expensive-job pool at {} could not be created ({error}), so no host-wide \
             bound can be enforced and this job was not admitted rather than run unbounded",
            pool.display()
        ),
    })?;
    let guard = match OwnerLock::acquire_within(
        &pool.join("capacity.lock"),
        "capacity agreement",
        Duration::from_secs(5),
    ) {
        Ok(Ok(guard)) => guard,
        Ok(Err(hint)) => {
            return Err(Refusal {
                notes: Vec::new(),
                code: "PoolBusy",
                message: format!(
                    "another participant is settling this shared pool's capacity ({}); this job \
                     was refused rather than admitted against an unsettled bound",
                    hint.describe()
                ),
            });
        }
        Err(error) => {
            return Err(Refusal {
                notes: Vec::new(),
                code: "PoolUnusable",
                message: format!(
                    "the shared expensive-job pool's capacity lock is unusable ({error}); this \
                     job was not admitted rather than run unbounded"
                ),
            });
        }
    };

    let path = agreement_path(pool);
    let existing: Option<PoolAgreement> = fs::read_to_string(&path)
        .ok()
        .and_then(|body| serde_json::from_str(&body).ok());

    let mut notes = Vec::new();
    let agreed = match existing {
        None => {
            let agreement = PoolAgreement {
                capacity: requested,
                agreed_unix_millis: now_unix_millis(),
                note: "written by the first participant to use this pool; every later \
                       participant is bound by this number and a larger local \
                       max_host_expensive does not widen it"
                    .to_string(),
            };
            write_agreement(&path, &agreement)?;
            notes.push(format!(
                "this shared pool had no agreed capacity; it was initialized at {requested} \
                 slot(s) from this estate's configuration"
            ));
            requested
        }
        Some(agreement) if agreement.capacity == requested => agreement.capacity,
        Some(agreement) if !authority => {
            notes.push(format!(
                "this estate's max_host_expensive is {requested} but the shared pool's agreed \
                 capacity is {}; the pool's number governs. A participant's own maximum is not a \
                 shared agreement — set host_pool_capacity_authority in this estate's \
                 resources.json to change the pool deliberately.",
                agreement.capacity
            ));
            agreement.capacity
        }
        Some(agreement) => {
            // Authority declared: a deliberate change, still checked.
            if requested < agreement.capacity {
                let stranded = live_leases(pool, requested, agreement.capacity);
                if !stranded.is_empty() {
                    let held: Vec<String> = stranded
                        .iter()
                        .map(|(index, hint)| format!("slot-{index} ({})", hint.describe()))
                        .collect();
                    drop(guard);
                    return Err(Refusal {
                        notes: Vec::new(),
                        code: "PoolCapacityInUse",
                        message: format!(
                            "this estate asked to shrink the shared pool from {} to {requested} \
                             slot(s), but {} lease(s) outside the new capacity are live: {}. \
                             Shrinking now would leave running jobs outside the bound while \
                             admitting new ones inside it, so the change was refused and the \
                             existing capacity of {} still applies. Retry once those jobs end.",
                            agreement.capacity,
                            stranded.len(),
                            held.join(", "),
                            agreement.capacity
                        ),
                    });
                }
            }
            let changed = PoolAgreement {
                capacity: requested,
                agreed_unix_millis: now_unix_millis(),
                note: "changed deliberately by a participant configured with \
                       host_pool_capacity_authority"
                    .to_string(),
            };
            write_agreement(&path, &changed)?;
            notes.push(format!(
                "this estate holds pool capacity authority; the shared pool's agreed capacity \
                 was changed from {} to {requested} slot(s)",
                agreement.capacity
            ));
            requested
        }
    };
    drop(guard);

    let usable = agreed.min(requested);
    if usable < agreed {
        notes.push(format!(
            "this estate restrains itself to {usable} of the pool's {agreed} slot(s); it cannot \
             impose that smaller bound on the other participants, which may still use all {agreed}"
        ));
    }
    Ok(PoolStanding {
        agreed_capacity: agreed,
        usable_slots: usable,
        notes,
    })
}

fn write_agreement(path: &Path, agreement: &PoolAgreement) -> Result<(), Refusal> {
    let body = serde_json::to_vec_pretty(agreement).map_err(|error| Refusal {
        notes: Vec::new(),
        code: "PoolUnusable",
        message: format!("the shared pool's capacity record could not be encoded ({error})"),
    })?;
    fs::write(path, body).map_err(|error| Refusal {
        notes: Vec::new(),
        code: "PoolUnusable",
        message: format!(
            "the shared pool's capacity record at {} could not be written ({error}); this job \
             was not admitted rather than run against an unrecorded bound",
            path.display()
        ),
    })
}

/// Admit one expensive job, or refuse it visibly.
///
/// Order matters. The **estate** slot is taken first, because it is the
/// cheap local bound and holding a host slot while queuing for a local
/// one would block every other estate behind this estate's own
/// contention. The **host** slot is taken second and bounds this user
/// across every estate. Both are `flock`s on files, so the bound survives
/// process boundaries — the defect being repaired is precisely that an
/// in-process mutex bounded only one daemon, while worktree
/// materialization happens in a different process entirely.
///
/// Pressure and space are checked *before* either slot is taken: refusing
/// early is cheaper and does not make a queue wait for a job that was
/// never going to be admitted.
pub fn admit(
    estate_root: &Path,
    policy: &ResourcePolicy,
    request: &JobRequest,
) -> Result<Admission, Refusal> {
    let mut notes = Vec::new();
    if let Some(note) = policy.capacity_note() {
        notes.push(note);
    }

    // Space first: a job that cannot fit is refused whatever the slots say.
    if let (Some(path), Some(estimate)) = (&request.space_path, request.estimate_bytes) {
        match available_space_bytes(path) {
            Ok(available) if available < estimate => {
                return Err(Refusal {
                    notes: notes.clone(),
                    code: "InsufficientSpace",
                    message: format!(
                        "{} needs an estimated {estimate} bytes on {} but {available} are \
                         available. The estimate is an estimate, not a measurement: the job may \
                         need more or less, and a write that runs out of space part-way is \
                         still refused and its staging removed rather than left half-written",
                        request.verb,
                        path.display()
                    ),
                });
            }
            Ok(_) => {}
            Err(reason) => notes.push(format!("space not checked: {reason}")),
        }
    }

    // Memory: observed from every scope that applies to this process,
    // and refused as advisory. Reading only the host's MemAvailable
    // answered a question about the machine when the question is what
    // this process may use; under ruling 0246 those differed by more
    // than three times, in the direction that admits work which stalls.
    let memory = observe_memory();
    if memory.is_throttled() {
        notes.push(format!(
            "a cgroup this process runs in ({}) is at its memory.high soft threshold, so it is \
             being actively reclaimed and may run slowly. This is disclosure, not a refusal: a \
             busy cgroup sits at its soft threshold normally, and sustained trouble shows up in \
             the pressure figure instead",
            memory.soft_headroom_from.clone().unwrap_or_default()
        ));
    }
    for reason in &memory.unavailable {
        notes.push(format!(
            "memory not fully observed ({reason}); admission proceeded on what could be read \
             rather than claiming a check it did not make"
        ));
    }
    if let Some(avg10) = memory.admission_some_avg10()
        && avg10 > policy.memory_pressure_avg10_max
    {
        return Err(Refusal {
            notes: notes.clone(),
            code: "MemoryPressure",
            message: format!(
                "{} refused: {} some avg10 is {avg10:.2}, above the configured {:.2}. This is an \
                 advisory sample of the recent past, not a statement that memory is globally \
                 unsafe; raise memory_pressure_avg10_max in .wirk/resources.json to admit anyway",
                request.verb,
                memory.admission_some_avg10_origin(),
                policy.memory_pressure_avg10_max
            ),
        });
    }
    if let Some(available) = memory.effective_available_bytes()
        && available < policy.min_available_memory_bytes
    {
        return Err(Refusal {
            notes: notes.clone(),
            code: "MemoryPressure",
            message: format!(
                "{} refused: {available} bytes are available before the tightest bound that \
                 applies to this process, below the configured floor of {} bytes. That bound is \
                 {}. That number is advisory: an estimate and a sample, not a guarantee that \
                 an admitted job can allocate what it needs",
                request.verb,
                policy.min_available_memory_bytes,
                memory
                    .effective_available_origin()
                    .unwrap_or_else(|| "not identified".to_string()),
            ),
        });
    }

    let wait = Duration::from_secs(policy.admission_wait_secs);
    let started = Instant::now();
    let detail = format!("{} {}", request.verb, request.detail);

    // For an expensive job the shared pool's agreed capacity is settled
    // *first*, because it bounds this estate's own offer too: an estate
    // configured for three expensive jobs in a pool agreed at one is
    // offered one, not three. Settling it before either slot is taken
    // also means a disagreement is reported without having queued.
    let mut host_standing = None;
    if request.class == JobClass::Expensive {
        let standing = pool_standing(
            &host_slot_dir(policy),
            policy.max_host_expensive,
            policy.host_pool_capacity_authority,
        )
        .map_err(|refusal| refusal.with_notes_before(&notes))?;
        notes.extend(standing.notes.iter().cloned());
        host_standing = Some(standing);
    }

    let (pool, slots, code, label) = match request.class {
        JobClass::Expensive => {
            let usable = host_standing
                .as_ref()
                .map(|standing| standing.usable_slots)
                .unwrap_or(policy.max_host_expensive);
            let offered = policy.max_expensive.min(usable);
            if offered < policy.max_expensive {
                notes.push(format!(
                    "max_expensive {} exceeds what the shared host pool allows this estate \
                     ({usable}); this estate is offered {offered} expensive slot(s)",
                    policy.max_expensive
                ));
            }
            (
                estate_slot_dir(estate_root),
                offered.max(1),
                "ExpensiveJobBusy",
                "expensive",
            )
        }
        JobClass::Materialization => (
            estate_root.join(".wirk").join("materialization"),
            policy.max_materialization,
            "MaterializationBusy",
            "worktree materialization",
        ),
    };
    let estate_slot = match wait_for_slot(&pool, slots, &detail, wait) {
        Ok(slot) => slot,
        Err(hint) => {
            return Err(Refusal {
                notes: notes.clone(),
                code,
                message: format!(
                    "{} refused: this estate's {slots} {label} slot(s) are taken ({}). {}",
                    request.verb,
                    hint.describe(),
                    waited_phrase(policy.admission_wait_secs)
                ),
            });
        }
    };

    // The host slot carries **no estate, source or Work identity**. It is
    // shared with every other estate this user runs, and a refusal read by
    // one estate must not disclose another's private paths. Only the verb
    // class, the pid and the start time cross that boundary.
    // Materialization is estate-bounded only: it does not contend for
    // the model runtime the host pool protects.
    if request.class == JobClass::Materialization {
        return Ok(Admission {
            _estate_slot: estate_slot,
            _host_slot: None,
            job_id: new_job_id(),
            waited: started.elapsed(),
            notes,
        });
    }
    // The pool's agreed capacity, never this caller's number: taking
    // `max_host_expensive` slots here is exactly how a wider participant
    // used to create slots a narrower one had never agreed to.
    let standing = host_standing.expect("expensive class settles its pool standing above");
    let host_slot = match wait_for_slot(
        &host_slot_dir(policy),
        standing.usable_slots,
        "expensive",
        wait.saturating_sub(started.elapsed()),
    ) {
        Ok(slot) => Some(slot),
        Err(hint) => {
            return Err(Refusal {
                notes: notes.clone(),
                code: "HostExpensiveBusy",
                message: format!(
                    "{} refused: all {} of the shared expensive slot(s) this estate may use are \
                     taken by another wirk job ({}); the pool's agreed capacity is {}. The host \
                     pool is advisory and scoped to this uid, this host and this runtime \
                     directory: it does not see another user's jobs, a container with its own \
                     /run/user, or any non-wirk consumer. {}",
                    request.verb,
                    standing.usable_slots,
                    hint.describe(),
                    standing.agreed_capacity,
                    waited_phrase(policy.admission_wait_secs)
                ),
            });
        }
    };

    Ok(Admission {
        _estate_slot: estate_slot,
        _host_slot: host_slot,
        job_id: new_job_id(),
        waited: started.elapsed(),
        notes,
    })
}

fn waited_phrase(seconds: u64) -> String {
    if seconds == 0 {
        "Refused immediately rather than waiting invisibly; pass a wait to queue instead."
            .to_string()
    } else {
        format!("Waited the configured {seconds}s before refusing.")
    }
}

fn wait_for_slot(
    directory: &Path,
    slots: u32,
    detail: &str,
    wait: Duration,
) -> Result<OwnerLock, HolderHint> {
    let deadline = Instant::now() + wait;
    loop {
        match take_slot(directory, slots, detail) {
            Ok(Ok(owner)) => return Ok(owner),
            Ok(Err(hint)) => {
                if Instant::now() >= deadline {
                    return Err(hint);
                }
                std::thread::sleep(POLL_INTERVAL);
            }
            Err(error) => {
                return Err(HolderHint {
                    detail: Some(format!("slot pool unusable: {error}")),
                    ..HolderHint::default()
                });
            }
        }
    }
}

fn new_job_id() -> String {
    ulid::Ulid::generate().to_string()
}

// ---------------------------------------------------------------------
// Cancellation
// ---------------------------------------------------------------------

/// A cancel flag shared between one running job and whatever may cancel
/// it, carrying the reason so the job's own end can say who stopped it.
///
/// **One token per job, not one per estate.** The earlier version hung a
/// single token on the store and cloned it into every child, and the
/// flag was sticky: cancelling once cancelled the estate's next job too,
/// and the one after that. A cancellation that poisons subsequent
/// legitimate work is not a usable cancellation, so the token now
/// belongs to the job and dies with it.
#[derive(Debug, Clone, Default)]
pub struct CancelToken {
    flag: Arc<AtomicBool>,
    reason: Arc<std::sync::Mutex<Option<String>>>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.cancel_with("was cancelled");
    }

    /// Cancel, recording why, so the child's end is reported as the
    /// deliberate act it was rather than as a backend failure.
    pub fn cancel_with(&self, reason: impl Into<String>) {
        *self.reason.lock().unwrap_or_else(|p| p.into_inner()) = Some(reason.into());
        self.flag.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    pub fn reason(&self) -> Option<String> {
        self.reason
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
}

/// One expensive job that is running right now, as the process that
/// started it can see it.
#[derive(Debug, Clone)]
pub struct ActiveJob {
    pub job_id: String,
    /// The verb that started it, e.g. `atlas semantic build`.
    pub verb: String,
    /// What it is working on — a source alias, in practice. This is the
    /// *target* half of a cancellation request's scope, and it is what
    /// makes `cancel` addressable rather than a blunt stop-everything.
    pub scope: String,
    /// P4.5 B correction (ruling 0251, F4): **who asked for this job** —
    /// the Work id the daemon itself resolved from the request, or
    /// `None` for a job started administratively (no Work context).
    ///
    /// This is the job-origin information the registry simply did not
    /// carry. Without it a cancellation had nothing to be scoped
    /// *against*: every job looked the same to every caller, so a
    /// narrowly bound actor that could open the estate's socket could
    /// enumerate and stop work belonging to Works it has no relationship
    /// with. It is bound where the job is admitted, from the identity
    /// the daemon resolved against its own journals — never from a grant
    /// the client supplied.
    ///
    /// What it is not: authentication. The same uid runs the actor and
    /// the operator, so this closes the *omission* path (ruling 0117's
    /// own scope) and does not claim to stop a deliberate same-uid
    /// impersonation.
    pub requester: Option<String>,
    pub started_unix_millis: i64,
    pub cancel: CancelToken,
}

/// How a caller names the jobs it wants cancelled.
///
/// There is no implicit default. A cancellation that stops more than the
/// caller meant is the failure mode worth designing against, so the
/// caller states its target and `All` is a thing you have to ask for by
/// name — and even `All` is bounded by this registry, which holds only
/// the jobs *this* process started for *this* estate. Nothing here ever
/// matches on a pid, a name prefix or another owner's cgroup.
#[derive(Debug, Clone)]
pub enum JobSelector {
    /// Exactly one job, by the id the admission reported.
    Job(String),
    /// Every job whose `scope` equals this — one source alias.
    Scope(String),
    /// Every job in this registry.
    All,
}

impl JobSelector {
    fn matches(&self, job: &ActiveJob) -> bool {
        match self {
            JobSelector::Job(id) => job.job_id == *id,
            JobSelector::Scope(scope) => job.scope == *scope,
            JobSelector::All => true,
        }
    }

    pub fn describe(&self) -> String {
        match self {
            JobSelector::Job(id) => format!("job {id}"),
            JobSelector::Scope(scope) => format!("every job on source {scope}"),
            JobSelector::All => "every running expensive job in this estate".to_string(),
        }
    }
}

/// What a cancellation request *acknowledged*, which is deliberately not
/// what it *completed*.
///
/// Signalling a job and the job actually ending are two different events
/// separated by however long the child takes to die. Reporting them as
/// one would be the same "declared done" this increment exists to
/// remove, so they are separate fields and the caller can observe the
/// second by asking again.
#[derive(Debug, Clone)]
pub struct CancelAck {
    pub job_id: String,
    pub verb: String,
    pub scope: String,
    pub running_millis: i64,
}

/// The expensive jobs one process is running, and the only thing an
/// operator's cancellation reaches.
///
/// **Why this exists at all.** The store held a cancel token that
/// nothing in the product ever called: `cancel_jobs` had zero callers,
/// the CLI had no verb, and the only thing that reached a child was the
/// deadline watchdog. Worse, every atlas verb runs behind the daemon's
/// single atlas mutex, so a cancel routed the ordinary way would have
/// queued behind the very build it was trying to stop. This registry is
/// therefore held *beside* that mutex, never inside it: cancelling takes
/// a short lock of its own and returns while the build still holds the
/// atlas.
#[derive(Debug, Clone, Default)]
pub struct JobRegistry(Arc<std::sync::Mutex<std::collections::HashMap<String, ActiveJob>>>);

impl JobRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    fn with<T>(
        &self,
        body: impl FnOnce(&mut std::collections::HashMap<String, ActiveJob>) -> T,
    ) -> T {
        let mut guard = self.0.lock().unwrap_or_else(|p| p.into_inner());
        body(&mut guard)
    }

    pub fn register(&self, job: ActiveJob) {
        self.with(|jobs| jobs.insert(job.job_id.clone(), job));
    }

    pub fn deregister(&self, job_id: &str) {
        self.with(|jobs| jobs.remove(job_id));
    }

    /// Every job running now, oldest first.
    pub fn list(&self) -> Vec<ActiveJob> {
        let mut jobs = self.with(|jobs| jobs.values().cloned().collect::<Vec<_>>());
        jobs.sort_by_key(|job| job.started_unix_millis);
        jobs
    }

    pub fn is_active(&self, job_id: &str) -> bool {
        self.with(|jobs| jobs.contains_key(job_id))
    }

    /// Signal every job the selector names. Returns what was signalled —
    /// an acknowledgement, not a completion.
    ///
    /// Deregistration is left to the job itself, on its own exit path, so
    /// "still in the registry" remains a truthful answer to "has it
    /// actually finished?".
    pub fn cancel(&self, selector: &JobSelector, reason: &str) -> Vec<CancelAck> {
        let now = now_unix_millis();
        self.with(|jobs| {
            jobs.values()
                .filter(|job| selector.matches(job))
                .map(|job| {
                    job.cancel.cancel_with(reason.to_string());
                    CancelAck {
                        job_id: job.job_id.clone(),
                        verb: job.verb.clone(),
                        scope: job.scope.clone(),
                        running_millis: now.saturating_sub(job.started_unix_millis),
                    }
                })
                .collect()
        })
    }
}

// ---------------------------------------------------------------------
// The bounded child
// ---------------------------------------------------------------------

/// How a bounded child ended.
#[derive(Debug)]
pub enum ChildEnd {
    /// It ran to completion. The output is the ordinary
    /// `wait_with_output` result.
    Finished(std::process::Output),
    /// A deadline or an explicit cancel ended it. The group and the job
    /// cgroup were killed; this is not a backend failure and must not be
    /// reported as one.
    Cancelled { reason: String, elapsed: Duration },
    /// It could not be started at all.
    Failed(String),
}

/// What a bounded run is allowed to use, and how it is contained.
pub struct BoundedChild<'a> {
    pub capabilities: &'a JobCapabilities,
    pub policy: &'a ResourcePolicy,
    pub cancel: CancelToken,
    pub job_id: String,
    /// Recorded so a later owner can recover this job even if this
    /// process is killed. `None` skips the record — used by tests that
    /// own their own cleanup.
    pub estate_root: Option<PathBuf>,
    pub staging: Option<PathBuf>,
    pub verb: String,
    /// What this job is working on, carried into [`ActiveJob::scope`] so
    /// a cancellation can name a target instead of stopping everything.
    pub scope: String,
    /// Carried into [`ActiveJob::requester`]: the Work this job is being
    /// run for, as the daemon resolved it. `None` is an administrative
    /// job, controllable only administratively.
    pub requester: Option<String>,
    /// Where this job announces itself while it runs, so an operator's
    /// cancellation can reach it. `None` runs unregistered — used by
    /// tests that drive the token directly.
    pub registry: Option<JobRegistry>,
}

impl BoundedChild<'_> {
    /// Spawn `command` under this process's containment, feed it `write`,
    /// and wait no longer than the policy deadline.
    ///
    /// The parent keeps a blocking `wait` (ruling 0044's posture) on its
    /// own thread; a watchdog thread kills the job cgroup — and the
    /// process group, which is what covers the case where no cgroup was
    /// available — when the deadline passes or the cancel flag is set.
    /// The group and cgroup are killed on **every** exit path, success
    /// included: a backend that exits while a child of its own still runs
    /// would otherwise leave that grandchild behind.
    ///
    /// What this cannot do, and does not claim: if *this* process is
    /// `SIGKILL`ed, none of this runs. The recorded job
    /// ([`record_owned_job`]) is what the next owner recovers from, and
    /// the gap between the two is the restart interval.
    pub fn run(
        &self,
        mut command: Command,
        write: impl FnOnce(&mut std::process::ChildStdin) -> std::io::Result<()> + Send + 'static,
    ) -> ChildEnd {
        let cgroup = JobCgroup::create(self.capabilities, &self.job_id);
        let mut cap_note = None;
        if let (Some(cgroup), Some(bytes)) = (&cgroup, self.policy.job_memory_max_bytes) {
            if let Err(reason) = cgroup.set_memory_max(bytes) {
                cap_note = Some(reason);
            }
        } else if self.policy.job_memory_max_bytes.is_some() {
            cap_note = Some("no job cgroup is available on this host".to_string());
        }
        if let Some(reason) = cap_note {
            // Never silently drop a requested cap.
            eprintln!(
                "wirk: a per-job memory cap was configured but cannot be applied ({reason}); \
                 this job runs uncapped"
            );
        }

        harden_execution_child(&mut command);
        if let Some(cgroup) = &cgroup {
            cgroup.arm(&mut command);
        }

        if let (Some(estate_root), true) = (&self.estate_root, cgroup.is_some()) {
            let _ = record_owned_job(
                estate_root,
                &OwnedJobRecord {
                    job_id: self.job_id.clone(),
                    cgroup: cgroup.as_ref().map(|value| value.dir().to_path_buf()),
                    pid: None,
                    verb: self.verb.clone(),
                    started_unix_millis: now_unix_millis(),
                    staging: self.staging.clone(),
                },
            );
        }

        // Announce before spawning: a cancellation that arrives in the
        // window between fork and registration would otherwise find
        // nothing and report "no such job" about a job that is running.
        // The token is checked again immediately below, so a cancel that
        // lands in that window still stops the child.
        if let Some(registry) = &self.registry {
            registry.register(ActiveJob {
                job_id: self.job_id.clone(),
                verb: self.verb.clone(),
                scope: self.scope.clone(),
                requester: self.requester.clone(),
                started_unix_millis: now_unix_millis(),
                cancel: self.cancel.clone(),
            });
        }

        let started = Instant::now();
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                self.clear();
                return ChildEnd::Failed(format!("could not be started: {error}"));
            }
        };
        let pgid = child.id() as i32;
        let mut stdin = child.stdin.take().expect("stdin was piped by the caller");
        let writer = std::thread::spawn(move || write(&mut stdin));

        let deadline = Duration::from_secs(self.policy.job_deadline_secs);
        let cancel = self.cancel.clone();
        let finished = Arc::new(AtomicBool::new(false));
        let watchdog_finished = finished.clone();
        let watchdog_cgroup = cgroup.as_ref().map(|value| value.dir().to_path_buf());
        let reason = Arc::new(std::sync::Mutex::new(None::<String>));
        let watchdog_reason = reason.clone();
        let watchdog = std::thread::spawn(move || {
            let start = Instant::now();
            while !watchdog_finished.load(Ordering::SeqCst) {
                // Phrased to read correctly where it is printed:
                // "backend <path> <reason> after 1.0s and its job was
                // killed". A refusal an operator cannot parse is a
                // refusal that gets ignored.
                let why = if cancel.is_cancelled() {
                    Some(
                        cancel
                            .reason()
                            .unwrap_or_else(|| "was cancelled".to_string()),
                    )
                } else if start.elapsed() >= deadline {
                    Some(format!("exceeded its {}s deadline", deadline.as_secs()))
                } else {
                    None
                };
                if let Some(why) = why {
                    *watchdog_reason.lock().unwrap_or_else(|p| p.into_inner()) = Some(why);
                    // The cgroup first: it is what reaches a setsid()
                    // descendant. The group kill still runs, because
                    // where no cgroup was available it is all there is.
                    if let Some(dir) = &watchdog_cgroup {
                        let _ = fs::write(dir.join("cgroup.kill"), "1");
                    }
                    kill_process_group(pgid);
                    return;
                }
                std::thread::sleep(POLL_INTERVAL);
            }
        });

        let output = child.wait_with_output();
        finished.store(true, Ordering::SeqCst);
        let _ = watchdog.join();
        let _ = writer.join();
        let elapsed = started.elapsed();

        // Every exit path, success included.
        kill_process_group(pgid);
        drop(cgroup);
        self.clear();

        let cancelled = reason.lock().unwrap_or_else(|p| p.into_inner()).clone();
        if let Some(reason) = cancelled {
            return ChildEnd::Cancelled { reason, elapsed };
        }
        match output {
            Ok(output) => ChildEnd::Finished(output),
            Err(error) => ChildEnd::Failed(format!("failed: {error}")),
        }
    }

    /// Every trace this job leaves behind, removed on every exit path:
    /// the recovery record and the registry entry. Leaving the registry
    /// entry would make a finished job answer "still running" to the
    /// completion question the cancel path asks.
    fn clear(&self) {
        if let Some(estate_root) = &self.estate_root {
            clear_owned_job(estate_root, &self.job_id);
        }
        if let Some(registry) = &self.registry {
            registry.deregister(&self.job_id);
        }
    }
}
