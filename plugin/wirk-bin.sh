# Where this plugin's `wirk` executable is, resolved in one place.
#
# Sourced by every entry point the manifest declares (startup hook,
# actions, status pane) and by the scripts under `plugin/`, so an
# installation resolves the same binary whichever surface it touches.
# Nothing here runs on its own; it defines two functions and returns.
#
# The order matters, and each step exists for a different kind of
# installation:
#
#   1. `WIRK_BIN_PATH` -- an explicit choice by whoever launched Herdr.
#      Set but not executable is an error, not a silent fall-through:
#      somebody named a file and it is not there. Set to an executable
#      that is not a usable wirk is the same kind of error, for the
#      same reason: somebody named a file and it cannot do the job.
#
# Every rung is checked with `wirk_bin_is_usable` before it is
# returned. "Exists and is executable" is not the question an entry
# point is asking -- it is asking for something that can run the
# commands this plugin invokes -- and the two answers differ in the
# case that matters most: an executable that is not wirk makes the
# `claim` action exit 0 having claimed nothing, which reads as success.
#   2. `<target>/release/wirk` -- what this plugin's own `[[build]]`
#      step produces during `herdr plugin install`. This is the path an
#      ordinary installation ends up using, and it is why an install
#      needs no knowledge of anyone's development tree.
#   3. `wirk` on `PATH` -- a wirk installed by any other means.
#   4. `<target>/debug/wirk` -- a developer's own unoptimized build,
#      last, so a stale debug binary never shadows a released one.
#
# `<target>` is `$CARGO_TARGET_DIR` when set, else `$HERDR_PLUGIN_ROOT/
# target`: the manifest sits at the repository root, so the plugin root
# is the Cargo workspace root and its `target/` is cargo's own default.

# The repository root, which is this plugin's root. Herdr sets
# `HERDR_PLUGIN_ROOT` for everything it launches; when this file is
# sourced outside Herdr the directory above `plugin/` is the same
# answer.
wirk_plugin_root() {
    if [ -n "${HERDR_PLUGIN_ROOT:-}" ]; then
        printf '%s\n' "$HERDR_PLUGIN_ROOT"
        return 0
    fi
    (cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
}

wirk_target_dir() {
    if [ -n "${CARGO_TARGET_DIR:-}" ]; then
        case "$CARGO_TARGET_DIR" in
            /*) printf '%s\n' "$CARGO_TARGET_DIR" ;;
            # A relative CARGO_TARGET_DIR is resolved by cargo against
            # its own cwd at build time, which install's [[build]] step
            # pins to the plugin root (the fresh checkout, before it is
            # renamed into place -- see plugin/build.sh). Every other
            # entry point (actions, the startup hook) also runs with
            # cwd at the plugin root, but as its own separate process:
            # left as a bare string, "$CARGO_TARGET_DIR/release/wirk"
            # would only be correct by coincidence of both callers
            # agreeing to cd there first. Anchoring it here to the
            # plugin root explicitly removes that coincidence, so a
            # relative value resolves the same way wherever this
            # function is called from.
            *) printf '%s/%s\n' "$(wirk_plugin_root)" "$CARGO_TARGET_DIR" ;;
        esac
    else
        printf '%s/target\n' "$(wirk_plugin_root)"
    fi
}

# The verbs this plugin actually invokes through the binary it
# resolves: `claim` (the claim action), `wirkd` (the startup hook, the
# status action and the status pane) and `plugin` (configure and
# assistant). A candidate that does not offer all three cannot serve
# this manifest, whatever else it is.
WIRK_REQUIRED_VERBS="claim wirkd plugin"

# Is this executable a wirk that can run what this plugin asks of it?
#
# Bounded on purpose: one invocation of the candidate's own usage
# output, which touches no estate, starts no daemon, writes nothing and
# needs no network. wirk has no --version or build-identity output to
# compare against (checked at 48d888f), so this does not attempt to
# decide whether a binary is the *right* wirk -- only whether it is
# wirk enough to run these commands. A stale wirk that still offers all
# three verbs passes here and fails later on its own terms, which is
# the honest limit of a capability check and is named as such rather
# than papered over with a version framework.
#
# `wirk --help` writes its usage to stderr and exits non-zero, so both
# streams are read and the status is deliberately not consulted.
wirk_bin_is_usable() {
    local candidate="$1" usage verb
    [ -x "$candidate" ] || return 1

    # A candidate that hangs is a failed check, not a hung action.
    if command -v timeout >/dev/null 2>&1; then
        usage="$(timeout 10 "$candidate" --help 2>&1)" || true
    else
        usage="$("$candidate" --help 2>&1)" || true
    fi

    for verb in $WIRK_REQUIRED_VERBS; do
        case "$usage" in
            *"wirk $verb"*) ;;
            *) return 1 ;;
        esac
    done
    return 0
}

# Prints the resolved executable and returns 0, or prints nothing and
# returns 1. Callers that need a binary follow a 1 with
# `wirk_bin_explain_missing`.
wirk_resolve_bin() {
    local target found

    # An explicit value is the operator's own answer, so an unusable one
    # is refused rather than quietly replaced by a binary they did not
    # name. The discovered rungs below are this plugin's own guesses, so
    # an unusable candidate there is simply not the answer and the next
    # rung is tried.
    if [ -n "${WIRK_BIN_PATH:-}" ]; then
        if wirk_bin_is_usable "$WIRK_BIN_PATH"; then
            printf '%s\n' "$WIRK_BIN_PATH"
            return 0
        fi
        return 1
    fi

    target="$(wirk_target_dir)"
    if wirk_bin_is_usable "$target/release/wirk"; then
        printf '%s\n' "$target/release/wirk"
        return 0
    fi
    if found="$(command -v wirk 2>/dev/null)" && [ -n "$found" ] \
        && wirk_bin_is_usable "$found"; then
        printf '%s\n' "$found"
        return 0
    fi
    if wirk_bin_is_usable "$target/debug/wirk"; then
        printf '%s\n' "$target/debug/wirk"
        return 0
    fi
    return 1
}

# What an operator can actually do about it. Written to stdout, because
# Herdr records a plugin command's stdout in its own log and that log is
# where somebody looks when an action did nothing.
wirk_bin_explain_missing() {
    local target
    target="$(wirk_target_dir)"

    if [ -n "${WIRK_BIN_PATH:-}" ]; then
        if [ -x "$WIRK_BIN_PATH" ]; then
            echo "wirk: WIRK_BIN_PATH is set to '$WIRK_BIN_PATH', which is an executable file"
            echo "but does not offer the commands this plugin runs ($WIRK_REQUIRED_VERBS)."
            echo "Its own 'wirk --help' output is what was checked, so a wirk too old to"
            echo "list one of those verbs fails here too."
            echo "Either point it at a wirk binary, or unset it and let this plugin resolve its own."
            return 0
        fi
        echo "wirk: WIRK_BIN_PATH is set to '$WIRK_BIN_PATH', which is not an executable file."
        echo "Either point it at a wirk binary, or unset it and let this plugin resolve its own."
        return 0
    fi

    echo "wirk: no wirk executable found. Looked, in order, at:"
    echo "    \$WIRK_BIN_PATH                (unset)"
    echo "    $target/release/wirk          (what this plugin's install-time build produces)"
    echo "    wirk on \$PATH                 (not found)"
    echo "    $target/debug/wirk            (a local development build)"
    echo
    echo "A path above can also have been skipped for being an executable that is not"
    echo "a usable wirk: each one is checked for the commands this plugin runs"
    echo "($WIRK_REQUIRED_VERBS) via its own 'wirk --help'."
    echo
    echo "To get one:"
    echo "  * Reinstall the plugin so its build step runs:"
    echo "        herdr plugin install <owner>/<repo>"
    echo "    That step compiles wirk from this repository with cargo and needs a"
    echo "    Rust toolchain (rustup: https://rustup.rs). It is not run by"
    echo "    'herdr plugin link' -- a linked plugin is expected to be built by hand."
    echo "  * Or build it yourself in $(wirk_plugin_root):"
    echo "        cargo build --release --locked -p wirk --bin wirk"
    echo "  * Or point this plugin at a wirk you already have:"
    echo "        WIRK_BIN_PATH=/path/to/wirk, in the environment Herdr runs in."
}
