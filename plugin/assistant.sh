#!/usr/bin/env bash
# The human entry: open a pane, start the harness the operator chose,
# and point it at the Wirk persona this repository already ships.
#
# Everything here is Herdr's own surface, called through `herdr` --
# `tab create`, `agent start`, `agent prompt`. Herdr owns which
# harnesses exist and how they run; this script only reads that list,
# checks the operator's choice against it, and launches. It never
# writes into the estate, into a target repository, or into any user
# configuration.
#
# The pane's working directory is this plugin's own root, which is the
# wirk repository root, because that is where the canonical persona
# (AGENTS.md, with CLAUDE.md a symlink to it) lives and every harness
# that discovers project instructions discovers them from its working
# directory. It is deliberately NOT the configured estate: the estate
# is a place wirk is pointed at, usually with instructions of its own
# that are none of our business, and the persona is named to the pane
# by path in the opening prompt rather than copied anywhere.
#
# This pane is a conversation, not a Run, so it must not carry an
# execution triple. It does not get one by default: this script runs
# with whatever environment the Herdr server has, and everything Herdr
# launches inherits it, so the triple is cleared explicitly at step 3.
#
# Exit status is the operator's only machine-readable signal -- Herdr
# records it in the plugin command log as `exit_code` and `status`, and
# a plugin action that printed a complaint and exited 0 is logged as
# `succeeded`. So: a launch that did not happen exits non-zero and
# names what it left behind; configuration that has not been done yet
# exits 0, because nothing failed and nothing was created.
set -eu

HERDR="${HERDR_BIN_PATH:-herdr}"
WIRK_BIN="${WIRK_BIN_PATH:-${CARGO_TARGET_DIR:-$HERDR_PLUGIN_ROOT/target}/debug/wirk}"

# 1. The estate. Same one line the other entrypoints read; absent means
#    nothing is configured yet, and the honest answer is the command
#    that configures it -- not a guess at which directory was meant.
ESTATE="$(cat "$HERDR_PLUGIN_CONFIG_DIR/estate" 2>/dev/null || true)"
if [ -z "$ESTATE" ]; then
    echo "wirk assistant: no estate configured. Run, from a Herdr plugin action:"
    echo "    wirk plugin init --estate <root>"
    echo "(the file it writes is $HERDR_PLUGIN_CONFIG_DIR/estate)"
    exit 0
fi

# 2. The harness. The list of kinds is Herdr's, read from Herdr at the
#    moment of use, so this plugin neither blesses nor blocks any of
#    them -- a kind Herdr learns to start is startable here the same
#    day, and a kind it drops stops being offered without an edit here.
#
#    Herdr exposes no structured listing of agent kinds; `agent start
#    --help` is where it publishes them, so that is what is read. This
#    is a one-line config file, not a chooser: picking a harness is
#    still done by editing it.
KINDS="$("$HERDR" agent start --help 2>&1 |
    sed -n 's/.*possible values: \(.*\)\]/\1/p' | tr -d ' ' | head -1)"
if [ -z "$KINDS" ]; then
    echo "wirk assistant: could not read the agent kinds Herdr supports from"
    echo "    $HERDR agent start --help"
    echo "Not guessing a harness; nothing was started."
    exit 1
fi

HARNESS_FILE="$HERDR_PLUGIN_CONFIG_DIR/harness"
KIND="$(head -1 "$HARNESS_FILE" 2>/dev/null | tr -d '[:space:]' || true)"
if [ -z "$KIND" ]; then
    echo "wirk assistant: no harness chosen. Write one of these into"
    echo "    $HARNESS_FILE"
    echo "$KINDS" | tr ',' '\n' | sed 's/^/    /'
    exit 0
fi
case ",$KINDS," in
    *",$KIND,"*) ;;
    *)
        echo "wirk assistant: '$KIND' (from $HARNESS_FILE) is not a harness this"
        echo "Herdr can start. It starts: $KINDS"
        exit 0
        ;;
esac

# 3. The pane. A tab of its own in the workspace the action was invoked
#    from; with no workspace in the environment (the CLI path) the
#    flag is omitted and Herdr's own `tab create` default -- the
#    focused workspace -- decides, rather than this script
#    reconstructing that choice by hand out of `workspace list`.
#
#    The execution triple is cleared here, on the pane Herdr launches,
#    because this process inherited it from the Herdr server and the
#    pane shell (and the harness started inside it) would inherit it in
#    turn. Herdr's `--env` only sets: `--env KEY` with no value is
#    rejected ("env must use KEY=VALUE"), so each is set to empty.
#    Empty is enough for the client that matters: the public `wirk` CLI
#    filters a blank triple variable out exactly as it filters an
#    absent one and reports it missing. Anything else in the server's
#    environment is still inherited; this clears the identity, not the
#    environment.
set -- --cwd "$HERDR_PLUGIN_ROOT" --label Wirk --focus \
    --env WIRK_ESTATE_ROOT= --env WIRK_WORK_ID= --env WIRK_RUN_ID=
if [ -n "${HERDR_WORKSPACE_ID:-}" ]; then
    set -- --workspace "$HERDR_WORKSPACE_ID" "$@"
fi
if ! TAB_JSON="$("$HERDR" tab create "$@" 2>&1)"; then
    echo "wirk assistant: Herdr did not create a tab; nothing was started."
    echo "    $TAB_JSON"
    exit 1
fi
PANE="$(printf '%s' "$TAB_JSON" | grep -o '"pane_id":"[^"]*"' | head -1 | cut -d'"' -f4)"
TAB="$(printf '%s' "$TAB_JSON" | grep -o '"tab_id":"[^"]*"' | head -1 | cut -d'"' -f4)"
if [ -z "$PANE" ]; then
    echo "wirk assistant: Herdr reported no pane for the tab it created."
    echo "    $TAB_JSON"
    exit 1
fi

# 4. The harness itself, started by Herdr in that pane and named so the
#    operator (and this script's next step) can address it. A second
#    conversation gets a second name rather than colliding with the
#    first.
NAME=wirk
if "$HERDR" agent get "$NAME" >/dev/null 2>&1; then
    NAME="wirk-$(printf '%s' "$PANE" | tr ':' '-')"
fi
if ! START_ERR="$("$HERDR" agent start "$NAME" --kind "$KIND" --pane "$PANE" 2>&1 >/dev/null)"; then
    echo "wirk assistant: $KIND did not come up in pane $PANE; no conversation was opened."
    echo "    $START_ERR"
    echo "Left behind, for you to look at or close: tab $TAB (pane $PANE)."
    echo "    $HERDR tab close $TAB"
    exit 1
fi

# 5. What the persona cannot know: where it is, and what it is not.
#    The persona itself is the repository's own AGENTS.md and is not
#    repeated here. A harness that came up but never received this is
#    not a Wirk conversation, so a failed prompt is a failed launch.
if ! PROMPT_ERR="$("$HERDR" agent prompt "$NAME" "You are Wirk. Your persona is AGENTS.md in this \
pane's working directory, $HERDR_PLUGIN_ROOT/AGENTS.md (CLAUDE.md is a symlink to it); read it now \
if your harness has not already loaded it.

This pane is a conversation with a person, not a dispatched Run. It has no Work id, no Run id and \
no Work boundary, and you must not adopt one.

Configured estate: $ESTATE
wirk executable: $WIRK_BIN

This working directory is the wirk repository itself -- not the estate, and not any repository \
the work will touch. Name repository bindings and a base explicitly when you admit Work; do not \
let this directory stand in for them.

Ask what outcome they want." 2>&1 >/dev/null)"; then
    echo "wirk assistant: $KIND is running as '$NAME' in pane $PANE, but Herdr did not deliver the"
    echo "opening prompt, so it is a bare $KIND and not a Wirk conversation."
    echo "    $PROMPT_ERR"
    echo "Left behind: tab $TAB (pane $PANE). Prompt it yourself, or close it:"
    echo "    $HERDR tab close $TAB"
    exit 1
fi

echo "wirk assistant: $KIND started as '$NAME' in pane $PANE of tab $TAB (estate $ESTATE)"
