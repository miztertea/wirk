//! Deterministic (child/docker) executors: wirk-owned, in the `wirk`
//! bin behind `wirk_core::Executor`, never a Herdr pane (0001 D4; 0022
//! D78: no fifth crate for these). Carved from sergeant-rs v0.3.0 as a
//! way (0023 D83; item 5 `orient/child.md`, `orient/docker.md`).
//!
//! W1 (`orient/build-brief.md` §3 W1): `child` — `ChildExecutor`, a
//! real OS process per Run, own process group, `PR_SET_PDEATHSIG` death
//! signal, bounded stderr tail, files the Claim itself on exit 0 (no
//! actor exists for a Deterministic Waypoint).
//!
//! W2 (this wave, `orient/build-brief.md` §3 W2): `docker` —
//! `DockerExecutor`, the same `Executor` contract over a real Docker
//! container (`docker create`/`start`, `--rm`, `--init`, `--network
//! none`, label-and-adopt in place of the death-signal mechanism a
//! container's process tree cannot carry, `orient/docker.md` §1).
//!
//! W3 (`orient/build-brief.md` §3 W3; `orient/child.md` §7 item 2,
//! "wirkd's own Route-runner owns the loop, not `ChildExecutor`"): the
//! Route-runner is `wirk run-deterministic` in `main.rs` — a small
//! driver, not `wirk-core` (R1, no fifth crate) — which constructs one
//! of these two executors by `--executor child|docker` and drives its
//! `launch`/`poll` loop against a `World` it reads back from wirkd's
//! own `status` verb. `main.rs` is the only caller today; a handful of
//! items here (`ChildExecutor::child_pid`, `DockerExecutor::
//! container_name`/`remove_owned`) are exercised only by
//! `wirk/tests/child_executor.rs` and `wirk/tests/docker_executor.rs`,
//! which compile this module into their own crate root via `#[path]`
//! (the same move `wirk/tests/wirkd_client.rs` makes for `wirkd`, R2)
//! — so the bin's own dead-code analysis would still flag those few
//! against `main.rs`'s own narrower use. Allowed at the module level
//! (R2, reusing `wirkd/mod.rs`'s own precedent and rationale verbatim),
//! not scattered per item.
#![allow(dead_code)]

pub mod child;
pub mod docker;
