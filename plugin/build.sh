#!/usr/bin/env bash
# The manifest's `[[build]]` step: how an installation gets a runnable
# `wirk` of its own.
#
# Herdr runs this once, in the freshly fetched checkout, during
# `herdr plugin install` and after the operator has confirmed the
# install. It is deliberately **not** run by `herdr plugin link`, so a
# developer working from their own checkout keeps building by hand.
#
# What it does is compile wirk from source with cargo. That is the whole
# route today: wirk publishes no prebuilt binaries, so there is nothing
# to download, and this script does not pretend otherwise. It says so
# out loud before it starts, because a source build is a real cost --
# minutes of CPU and a `target/` directory of a few gigabytes -- and an
# installation that is about to pay it should be told, not surprised.
# If the toolchain it needs is absent it stops and says what to install,
# rather than registering a plugin whose every action will fail later.
set -eu

ROOT="${HERDR_PLUGIN_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
# shellcheck source=plugin/wirk-bin.sh
. "$ROOT/plugin/wirk-bin.sh"
TARGET="$(wirk_target_dir)"
OUT="$TARGET/release/wirk"

echo "wirk build: this plugin builds the wirk binary from source with cargo."
echo "wirk build: repository $ROOT"
echo "wirk build: output      $OUT"

# An explicit binary means the operator has already answered the
# question this step exists to answer. Say which one is being honored;
# a skipped build that says nothing is indistinguishable from a build
# that silently did nothing.
#
# A *set but not executable* WIRK_BIN_PATH is the same case
# wirk-bin.sh's own resolver refuses rather than falls through
# (wirk_resolve_bin: "set-but-not-executable is an error, never a
# fall-through: somebody named a file and it is not there"). This step
# must not repeat that name silently as a several-minute source build
# with no explanation of what happened to the value the operator set.
# "Already names an executable" is not the question either: this step
# is deciding whether the installation already has a wirk that can run
# what the manifest's actions invoke. An executable that is not one
# would skip the build and register a plugin whose `claim` action exits
# 0 having claimed nothing, so the same bounded check the resolver
# applies is applied here, before the build is skipped on its word.
if [ -n "${WIRK_BIN_PATH:-}" ]; then
    if wirk_bin_is_usable "$WIRK_BIN_PATH"; then
        echo "wirk build: skipped -- WIRK_BIN_PATH already names a usable wirk ($WIRK_BIN_PATH)."
        exit 0
    fi
    if [ -x "$WIRK_BIN_PATH" ]; then
        echo "wirk build: WIRK_BIN_PATH is set to '$WIRK_BIN_PATH', which is executable but does"
        echo "            not offer the commands this plugin runs ($WIRK_REQUIRED_VERBS)."
    else
        echo "wirk build: WIRK_BIN_PATH is set to '$WIRK_BIN_PATH', which is not an executable file."
    fi
    echo "wirk build: ignoring it and building from source instead. Fix or unset it if that"
    echo "            was not intended."
fi

if ! command -v cargo >/dev/null 2>&1; then
    echo "wirk build: no 'cargo' on PATH, so wirk cannot be built here."
    echo "wirk build: wirk is a Rust program and ships no prebuilt binary today, so a"
    echo "            Rust toolchain is what an install needs. Install one from"
    echo "            https://rustup.rs and run 'herdr plugin install' again, or set"
    echo "            WIRK_BIN_PATH to a wirk you already have and install again."
    exit 1
fi

# The pinned toolchain is `rust-toolchain.toml` in this repository;
# cargo and rustup honor it without being told. Naming the version that
# will actually be used makes a toolchain-download pause during install
# explicable rather than mysterious.
echo "wirk build: cargo   $(cargo --version 2>&1 | head -1)"
if [ -f "$ROOT/rust-toolchain.toml" ]; then
    echo "wirk build: pinned  $(sed -n 's/^ *channel *= *//p' "$ROOT/rust-toolchain.toml" | head -1) (rust-toolchain.toml)"
fi
echo "wirk build: running cargo build --release --locked -p wirk --bin wirk"
echo "wirk build: this takes several minutes on a cold cache and writes to $TARGET."

started=$SECONDS
cd "$ROOT"
cargo build --release --locked -p wirk --bin wirk
elapsed=$(( SECONDS - started ))

if [ ! -x "$OUT" ]; then
    echo "wirk build: cargo reported success but $OUT is not there; nothing usable was produced."
    exit 1
fi

# One real invocation, not just a stat, so a file that cannot execute is
# caught here rather than at first use. Run with no arguments wirk
# prints its own usage and exits non-zero, which is exactly the signal
# wanted: it depends on nothing in the environment -- this step runs
# during `herdr plugin install`, which is not a plugin action and has
# no plugin config directory -- and no other program prints that line.
if ! "$OUT" 2>&1 | grep -q '^usage: wirk '; then
    echo "wirk build: $OUT was produced but does not run as wirk."
    exit 1
fi

echo "wirk build: built in ${elapsed}s -- $OUT ($(wc -c <"$OUT") bytes)"
echo "wirk build: next, choose an estate and a harness with the plugin's 'Configure Wirk' action."
