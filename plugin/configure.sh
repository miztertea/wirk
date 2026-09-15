#!/usr/bin/env bash
# Choose this installation's estate and harness, from inside Herdr.
#
# The `configure` action opens a tab and runs this script in it, because
# a Herdr plugin action takes no arguments and has no terminal of its
# own: the action is the reachable entry point, and this pane is where
# the questions can actually be asked and answered.
#
# It writes nothing itself. Every write goes through
# `wirk plugin init`, which owns what those files mean; this script asks
# the questions, shows Herdr's own list of harnesses, and reports what
# is missing when it cannot get that far.
set -eu

ROOT="${HERDR_PLUGIN_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
# shellcheck source=plugin/wirk-bin.sh
. "$ROOT/plugin/wirk-bin.sh"
HERDR="${HERDR_BIN_PATH:-herdr}"

# A Herdr plugin action runs with no terminal: its stdout goes to the
# plugin command log and there is nothing to read an answer from. So
# when this script is started that way it puts itself in a pane of its
# own and hands the conversation over to that copy. Run from a terminal
# -- a pane, or a shell -- it just carries on below.
if [ ! -t 0 ]; then
    set -- --cwd "$ROOT" --label "Wirk configure" --focus \
        --env "HERDR_PLUGIN_ROOT=$ROOT" \
        --env "HERDR_PLUGIN_CONFIG_DIR=${HERDR_PLUGIN_CONFIG_DIR:-}" \
        --env "HERDR_PLUGIN_STATE_DIR=${HERDR_PLUGIN_STATE_DIR:-}" \
        --env WIRK_ESTATE_ROOT= --env WIRK_WORK_ID= --env WIRK_RUN_ID=
    if [ -n "${HERDR_WORKSPACE_ID:-}" ]; then
        set -- --workspace "$HERDR_WORKSPACE_ID" "$@"
    fi
    if ! TAB_JSON="$("$HERDR" tab create "$@" 2>&1)"; then
        echo "wirk configure: Herdr did not create a tab; nothing was opened."
        echo "    $TAB_JSON"
        exit 1
    fi
    PANE="$(printf '%s' "$TAB_JSON" | grep -o '"pane_id":"[^"]*"' | head -1 | cut -d'"' -f4)"
    TAB="$(printf '%s' "$TAB_JSON" | grep -o '"tab_id":"[^"]*"' | head -1 | cut -d'"' -f4)"
    if [ -z "$PANE" ]; then
        echo "wirk configure: Herdr reported no pane for the tab it created."
        echo "    $TAB_JSON"
        exit 1
    fi
    if ! RUN_ERR="$("$HERDR" pane run "$PANE" bash "$ROOT/plugin/configure.sh" 2>&1 >/dev/null)"; then
        echo "wirk configure: the pane opened but the configure program did not start."
        echo "    $RUN_ERR"
        echo "Left behind: tab $TAB (pane $PANE). Close it with:"
        echo "    $HERDR tab close $TAB"
        exit 1
    fi
    echo "wirk configure: answer the questions in pane $PANE of tab $TAB."
    exit 0
fi

echo "Wirk -- configure this installation"
echo

# 1. The binary. Without one there is nothing to configure with, and
#    that is the single most common reason this action is being run at
#    all, so it is the first thing answered and the explanation is the
#    shared one every other entry point prints.
if ! WIRK_BIN="$(wirk_resolve_bin)"; then
    wirk_bin_explain_missing
    echo
    echo "Nothing was configured. Fix the above, then run this action again."
    exit 1
fi
echo "wirk executable: $WIRK_BIN"

if [ -z "${HERDR_PLUGIN_CONFIG_DIR:-}" ]; then
    echo
    echo "This pane has no HERDR_PLUGIN_CONFIG_DIR, so there is no per-plugin"
    echo "configuration directory to write into. Run the plugin's 'Configure Wirk'"
    echo "action rather than this script directly."
    exit 1
fi

echo
echo "Current configuration:"
"$WIRK_BIN" plugin show || true

# 2. The estate. An empty answer leaves whatever is already there,
#    so re-running this to change only the harness costs one Enter.
echo
CURRENT_ESTATE="$(head -1 "$HERDR_PLUGIN_CONFIG_DIR/estate" 2>/dev/null || true)"
echo "An estate is the directory wirk works in -- it holds the Work, the Trail and"
echo "the outputs. It is not the repository you want changed; a Work names that"
echo "separately."
if [ -n "$CURRENT_ESTATE" ]; then
    printf 'Estate root [%s]: ' "$CURRENT_ESTATE"
else
    printf 'Estate root: '
fi
read -r ESTATE || ESTATE=""

# 3. The harness, from Herdr's own structured list rather than its help
#    text. `herdr status server` names the socket of the session this
#    pane belongs to; asking Herdr where to ask Herdr keeps session
#    resolution Herdr's business.
echo
SOCKET="${HERDR_SOCKET_PATH:-$("$HERDR" status server 2>/dev/null | sed -n 's/^socket: //p' | head -1)}"
if [ -z "$SOCKET" ]; then
    echo "Could not find the Herdr socket ('$HERDR status server' named none), so the"
    echo "list of harnesses cannot be read. Skipping the harness question; set it later"
    echo "with: wirk plugin init --harness <kind>"
    HARNESS=""
else
    echo "Harnesses this Herdr reports:"
    echo
    if ! "$WIRK_BIN" plugin harnesses --socket "$SOCKET"; then
        echo
        echo "Skipping the harness question; set it later with:"
        echo "    wirk plugin init --harness <kind>"
        HARNESS=""
    else
        echo
        CURRENT_HARNESS="$(head -1 "$HERDR_PLUGIN_CONFIG_DIR/harness" 2>/dev/null || true)"
        if [ -n "$CURRENT_HARNESS" ]; then
            printf 'Harness [%s]: ' "$CURRENT_HARNESS"
        else
            printf 'Harness: '
        fi
        read -r HARNESS || HARNESS=""
    fi
fi

# 4. The harness's own arguments, which are the operator's choice of
#    model, effort or anything else that harness takes. wirk supplies
#    none of its own: name nothing here and the assistant action starts
#    the harness with no extra arguments at all.
#
#    Typed on one line and split on whitespace by `read -a` -- the
#    shell's own splitting, not an unquoted expansion, which would also
#    glob a `*` against this directory. Each word becomes one
#    `--harness-arg`, and `wirk plugin init` stores it verbatim, so an
#    argument that must itself contain a space is the one case this
#    prompt cannot express; the command named below can.
echo
CURRENT_ARGS="$("$WIRK_BIN" plugin show 2>/dev/null | sed -n 's/^harness-args *//p' | grep -v '^(none)' | tr '\n' ' ' || true)"
echo "Harness arguments are passed through to the harness itself when a Wirk"
echo "conversation starts -- for example '--model claude-sonnet-5 --effort medium'."
echo "Press Enter to leave them as they are. Enter '-' to set none."
echo "For an argument containing a space, run:"
echo "    $WIRK_BIN plugin init --harness-arg '<one argument>'"
if [ -n "$CURRENT_ARGS" ]; then
    printf 'Harness arguments [%s]: ' "${CURRENT_ARGS% }"
else
    printf 'Harness arguments (none set): '
fi
read -r HARNESS_ARGS_LINE || HARNESS_ARGS_LINE=""

# 5. One write, through the CLI that owns these files. Nothing to say
#    and nothing to write is a legitimate answer: the operator looked.
#
#    An empty arguments answer adds no flag and so leaves the stored
#    list alone; a bare '-' is `--clear-harness-args`, which is the
#    operator saying "none" rather than saying nothing.
set --
[ -n "${ESTATE:-}" ] && set -- "$@" --estate "$ESTATE"
[ -n "${HARNESS:-}" ] && set -- "$@" --harness "$HARNESS"
if [ "$HARNESS_ARGS_LINE" = "-" ]; then
    set -- "$@" --clear-harness-args
elif [ -n "$HARNESS_ARGS_LINE" ]; then
    read -r -a HARNESS_ARGS_WORDS <<<"$HARNESS_ARGS_LINE"
    for word in "${HARNESS_ARGS_WORDS[@]}"; do
        set -- "$@" --harness-arg "$word"
    done
fi
echo
if [ "$#" -eq 0 ]; then
    echo "Nothing entered; configuration left as it was."
else
    "$WIRK_BIN" plugin init "$@"
fi

echo
echo "Configuration now:"
"$WIRK_BIN" plugin show || true
echo
echo "Next: the plugin's 'Talk to Wirk' action opens a Wirk conversation."
echo "Press Enter to close this pane."
read -r _ || true
