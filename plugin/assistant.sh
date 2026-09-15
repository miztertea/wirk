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
# launches inherits it, so the triple is cleared explicitly at step 4.
#
# Exit status is the operator's only machine-readable signal -- Herdr
# records it in the plugin command log as `exit_code` and `status`, and
# a plugin action that printed a complaint and exited 0 is logged as
# `succeeded`. So: a launch that did not happen exits non-zero and
# names what it left behind; configuration that has not been done yet
# exits 0, because nothing failed and nothing was created.
set -eu

HERDR="${HERDR_BIN_PATH:-herdr}"
ROOT="${HERDR_PLUGIN_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
# shellcheck source=plugin/wirk-bin.sh
. "$ROOT/plugin/wirk-bin.sh"

# 1. The executable. Resolved in the one place every entry point shares,
#    and named in the opening prompt below, so a conversation is never
#    told about a wirk that is not there. No binary is a launch that did
#    not happen, and it exits non-zero saying so.
if ! WIRK_BIN="$(wirk_resolve_bin)"; then
    echo "wirk assistant: no conversation was opened."
    wirk_bin_explain_missing
    exit 1
fi

# 2. The estate. Same one line the other entrypoints read; absent means
#    nothing is configured yet, and the honest answer is the action that
#    configures it -- not a guess at which directory was meant.
ESTATE="$(cat "$HERDR_PLUGIN_CONFIG_DIR/estate" 2>/dev/null || true)"
if [ -z "$ESTATE" ]; then
    echo "wirk assistant: no estate configured. Run this plugin's 'Configure Wirk'"
    echo "action, which asks for an estate and a harness and writes both."
    echo "(the file it writes here is $HERDR_PLUGIN_CONFIG_DIR/estate)"
    exit 0
fi

# 3. The harness. Which kinds exist is Herdr's to say, read from Herdr
#    at the moment of use, so this plugin neither blesses nor blocks any
#    of them -- a kind Herdr learns to start is startable here the same
#    day, and a kind it drops stops being offered without an edit here.
#
#    The list comes from Herdr's own `server.agent_manifests` over the
#    session socket (`wirk plugin harnesses`), a structured answer
#    rather than a parse of help output that changes shape whenever the
#    help does. `herdr status server` names the socket of the session
#    this action belongs to.
#
#    That list is what Herdr carries detection manifests for, which is
#    not quite the same set `agent start --kind` accepts -- it can be
#    smaller. So it is used to *offer* choices, never to veto one: a
#    configured kind is passed to Herdr and Herdr's own answer decides.
HARNESS_FILE="$HERDR_PLUGIN_CONFIG_DIR/harness"
KIND="$(head -1 "$HARNESS_FILE" 2>/dev/null | tr -d '[:space:]' || true)"
if [ -z "$KIND" ]; then
    SOCKET="${HERDR_SOCKET_PATH:-$("$HERDR" status server 2>/dev/null | sed -n 's/^socket: //p' | head -1)}"
    echo "wirk assistant: no harness chosen. Run this plugin's 'Configure Wirk'"
    echo "action to pick one; it lists what Herdr reports and which of those"
    echo "resolve to an executable here."
    if [ -n "$SOCKET" ]; then
        echo
        "$WIRK_BIN" plugin harnesses --socket "$SOCKET" || true
    fi
    exit 0
fi

# 3b. The operator's own harness arguments, if they chose any.
#
#    Which model, effort or flags a conversation runs at is the
#    operator's choice, and there is no product default here: nothing is
#    passed unless they said so, and this script never invents a model.
#    They travel on Herdr's own passthrough, `agent start ... --
#    <agent-args>`.
#
#    Two places, most local first:
#
#      WIRK_ASSISTANT_HARNESS_ARGS in this process's environment, for a
#      single run, a test or a CI job. Split on whitespace by `read -a`,
#      which is the shell's own splitting primitive -- not an unquoted
#      expansion, which would additionally glob a `*` against this
#      directory, and not `eval`. An argument that must itself contain a
#      space cannot be written this way; the file below can.
#
#      Otherwise $HERDR_PLUGIN_CONFIG_DIR/harness-args, written by
#      `wirk plugin init --harness-arg` (which the 'Configure Wirk'
#      action calls): one argument per line, taken exactly as written,
#      so spaces and globs inside an argument survive and nothing is
#      expanded. Blank lines and `#` comments are skipped so the file
#      can say what it is. `wirk plugin show` prints the same list.
HARNESS_ARGS=()
wirk_read_harness_arg_lines() {
    local line
    while IFS= read -r line || [ -n "$line" ]; do
        case "$line" in
            '' | '#'*) continue ;;
        esac
        HARNESS_ARGS+=("$line")
    done
}
HARNESS_ARGS_FROM=
HARNESS_ARGS_FILE="${HERDR_PLUGIN_CONFIG_DIR:-}/harness-args"
if [ -n "${WIRK_ASSISTANT_HARNESS_ARGS:-}" ]; then
    read -r -a HARNESS_ARGS <<<"$WIRK_ASSISTANT_HARNESS_ARGS"
    HARNESS_ARGS_FROM="WIRK_ASSISTANT_HARNESS_ARGS"
elif [ -n "${HERDR_PLUGIN_CONFIG_DIR:-}" ] && [ -s "$HARNESS_ARGS_FILE" ]; then
    wirk_read_harness_arg_lines <"$HARNESS_ARGS_FILE"
    # A file that is only comments is a file that chose nothing; it must
    # not be reported as a selection, and `set -e` must not see a bare
    # failing test here.
    if [ "${#HARNESS_ARGS[@]}" -gt 0 ]; then
        HARNESS_ARGS_FROM="$HARNESS_ARGS_FILE"
    fi
fi

# 4. The pane. A tab of its own in the workspace the action was invoked
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
set -- --cwd "$ROOT" --label Wirk --focus \
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

# 5. The harness itself, started by Herdr in that pane and named so the
#    operator (and this script's next step) can address it. A second
#    conversation gets a second name rather than colliding with the
#    first.
NAME=wirk
if "$HERDR" agent get "$NAME" >/dev/null 2>&1; then
    NAME="wirk-$(printf '%s' "$PANE" | tr ':' '-')"
fi
START_OK=1
if [ "${#HARNESS_ARGS[@]}" -gt 0 ]; then
    START_ERR="$("$HERDR" agent start "$NAME" --kind "$KIND" --pane "$PANE" \
        -- "${HARNESS_ARGS[@]}" 2>&1 >/dev/null)" || START_OK=0
else
    START_ERR="$("$HERDR" agent start "$NAME" --kind "$KIND" --pane "$PANE" 2>&1 >/dev/null)" || START_OK=0
fi
if [ "$START_OK" -eq 0 ]; then
    echo "wirk assistant: $KIND did not come up in pane $PANE; no conversation was opened."
    echo "    $START_ERR"
    # The most common cause, and the one Herdr's own message cannot
    # name: the harness is simply not installed. Only said when it is
    # actually true here -- Herdr chooses the executable for a kind and
    # for a few it is not the kind's own name, so this is an addition to
    # Herdr's answer, never a replacement for it.
    if ! command -v "$KIND" >/dev/null 2>&1; then
        echo "There is no '$KIND' executable on PATH, so it may not be installed."
        echo "Install it, or choose another harness with this plugin's 'Configure Wirk' action."
    fi
    # The other named cause: the harness itself is up but stuck behind
    # its own first-run prompt (a fresh checkout's folder-trust question
    # is the one observed here) and never reaches interactive-ready
    # within the start wait -- Herdr reports that as "agent_not_ready" /
    # "blocked during startup" (refs/herdr-0.9.0 src/cli/agent.rs
    # wait_for_named_agent), a status this script cannot answer on the
    # operator's behalf. This is the P6.5 friction (ruling 0255); it is
    # named here, not resolved here.
    case "$START_ERR" in
        *agent_not_ready*|*"blocked during startup"*)
            echo "This looks like $KIND coming up but stopping at its own first-run prompt"
            echo "(for example, a fresh checkout's folder-trust question) rather than failing"
            echo "to start. Open pane $PANE and answer whatever it is waiting on, then run"
            echo "this action again."
            ;;
    esac
    echo "Left behind, for you to look at or close: tab $TAB (pane $PANE)."
    echo "    $HERDR tab close $TAB"
    exit 1
fi

# 6. What the persona cannot know: where it is, and what it is not.
#    The persona itself is the repository's own AGENTS.md and is not
#    repeated here. A harness that came up but never received this is
#    not a Wirk conversation, so a failed prompt is a failed launch.
if ! PROMPT_ERR="$("$HERDR" agent prompt "$NAME" "You are Wirk. Your persona is AGENTS.md in this \
pane's working directory, $ROOT/AGENTS.md (CLAUDE.md is a symlink to it); read it now \
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
if [ -n "$HARNESS_ARGS_FROM" ]; then
    echo "    harness arguments from $HARNESS_ARGS_FROM: ${HARNESS_ARGS[*]}"
fi
