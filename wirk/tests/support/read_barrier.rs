//! A test-side scheduler that parks the **real daemon** inside one of
//! its own production reads, so a two-read window can be *ordered*
//! rather than raced.
//!
//! Why this exists. Two W-C2 proofs used to be race amplification:
//! an 8 MB artifact swapped by `rename` in a loop while six reservations
//! assembled ("the mutant happened to die on reservation 2"), and 40 000
//! planted `.tmp-` residues to make a startup sweep long enough that a
//! filesystem assertion would land inside it. Both prove that a thread
//! ran, not that the decisive interleaving occurred; both can pass or
//! fail on scheduling. `CLAUDE.md`: "a test is deterministic and has
//! been watched fail, or it is not a test."
//!
//! Why no product instrumentation. R2, and `git_gate.rs`'s own
//! precedent: the daemon is parked at a point *inside* its shipped code
//! using a native facility only, with no flag, no environment variable
//! read by wirk, and nothing that could exist in a release runtime.
//! `git_gate` uses a process's own `PATH` (R4) because the window it
//! orders happens to straddle a `Command::new("git")`. The windows here
//! straddle a `std::fs::read`, so the native facility is the other one:
//! a **FIFO** (R4, `mkfifo(1)` from coreutils, R5). Replacing the file
//! the daemon is about to read with a named pipe makes that read block
//! in `open(2)` until this side opens the write end, and block again in
//! `read(2)` until this side closes it. The test therefore decides,
//! causally:
//!
//! * **when** the read starts — `park()` returns exactly when the daemon
//!   has opened the path, which is the observation, not a guess;
//! * **which bytes** the read receives — this side is the pipe's only
//!   writer, so the captured bytes are known exactly;
//! * **when** the read completes — the daemon's `read_to_end` sees EOF
//!   only when `release()` closes the write end.
//!
//! That third point is what makes an atomic rewrite airtight rather than
//! probabilistic. Anything done between `supply()` and `release()` —
//! a `rename` over the path, for instance — *happens-before* the read
//! returns, and therefore happens-before any second read of the same
//! path can possibly open it. A faithful "read twice" mutant cannot miss
//! the window; it is not a window any more, it is an order.
//!
//! It is a scheduler, not a fake (ruling 0040): the daemon under test is
//! the shipped daemon, the read is its own real read, and the bytes it
//! receives are the real bytes its own Claim recorded a digest over.
//! Only *when* the read returns is the test's to decide.
//!
//! The bound in `park` is a termination bound in the sense of ruling
//! 0044: its exhaustion is reported as "the state this test schedules
//! against was never observed", and it fails the test. It is never a
//! success and never a verdict.

// Included by `#[path]` into the test binaries that need it; the
// module-level allow is `route_fixture.rs`'s own, same reason.
#![allow(dead_code)]

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

/// A path the daemon is about to read, replaced by a named pipe.
pub struct ReadBarrier {
    path: PathBuf,
}

impl ReadBarrier {
    /// Replaces `path` with a FIFO. From here the next production read
    /// of that path blocks until [`ReadBarrier::park`] is answered.
    ///
    /// The caller is responsible for arming a path whose *only* reader
    /// in the window under test is the one being ordered.
    pub fn arm(path: &Path) -> ReadBarrier {
        let _ = std::fs::remove_file(path);
        let status = Command::new("mkfifo")
            .arg(path)
            .status()
            .expect("mkfifo(1) runs");
        assert!(status.success(), "mkfifo {} failed", path.display());
        ReadBarrier {
            path: path.to_path_buf(),
        }
    }

    /// Blocks until the daemon has actually opened the armed path for
    /// reading, and hands back the held write end.
    ///
    /// The return is the causal observation every assertion below it is
    /// made against: the reader is now inside `read(2)` on this pipe and
    /// cannot make progress until the returned [`Held`] is released.
    pub fn park(&self, what: &str) -> Held {
        let path = self.path.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(OpenOptions::new().write(true).open(&path));
        });
        let opened = rx.recv_timeout(Duration::from_secs(120)).unwrap_or_else(|_| {
            panic!(
                "nothing ever opened {} for reading while {what}: the state this test schedules \
                 against was never observed, so nothing below it would prove anything",
                self.path.display()
            )
        });
        Held {
            file: opened.unwrap_or_else(|error| {
                panic!("open the write end of {}: {error}", self.path.display())
            }),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// The held write end of an armed path: the reader is blocked until this
/// is released.
pub struct Held {
    file: File,
}

impl Held {
    /// The exact bytes this read will receive. Nothing else can write
    /// them, so what the daemon captured is known, not inferred.
    pub fn supply(&mut self, bytes: &[u8]) {
        self.file
            .write_all(bytes)
            .expect("supply the captured bytes");
    }

    /// Closes the write end: the daemon's read sees EOF and returns.
    /// Everything the caller did before this happens-before the read
    /// completes.
    pub fn release(self) {
        drop(self.file);
    }
}
