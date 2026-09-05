#!/bin/sh
# A fake `wirk` binary for `opencode_hook_plugin.rs`: records its argv
# and a few env keys the real `wirk claim` reads (`main.rs`'s
# `TRIPLE_VARS`), so the test can assert the plugin invoked exactly
# `wirk claim` with the pane's env still attached (execFile's default:
# inherit process.env, no explicit env passed by the plugin). Named by
# `WIRK_FAKE_RECORD`; writes nothing if that is unset (never silently
# no-ops in a way that hides a real miss -- the test itself requires
# the file to exist).
set -eu
[ -n "${WIRK_FAKE_RECORD:-}" ] || exit 0
{
    printf 'ARGV:%s\n' "$*"
    printf 'WIRK_ESTATE_ROOT=%s\n' "${WIRK_ESTATE_ROOT:-}"
    printf 'WIRK_WORK_ID=%s\n' "${WIRK_WORK_ID:-}"
    printf 'WIRK_RUN_ID=%s\n' "${WIRK_RUN_ID:-}"
} >"$WIRK_FAKE_RECORD"
