#!/bin/sh
# Static stand-in for `git`, used only by wirk-atlas::git::batched_read_tests.
#
# This file is checked into the repo and never rewritten at test time.
# The earlier fixture wrote a fresh executable script per test and execed
# it; that races ETXTBSY (Linux denies exec of a file that is open for
# write by anyone, and fork(2) from an unrelated, concurrently running
# test duplicates this process's write fd across the fork, so a sibling
# test's exec of *its own*, already-closed script can still observe the
# inode as busy). A file that is never opened for writing during the test
# run cannot trigger that check, so per-test behaviour is read as plain
# data from the repo directory git.rs already passes via `-C <repo>`.
repo="$2"
stderr_bytes="$(cat "$repo/.stub-stderr-bytes")"
head -c "$stderr_bytes" /dev/zero | tr '\0' 'w' >&2
. "$repo/.stub-body.sh"
