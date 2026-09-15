#!/usr/bin/env bash
# The "Browser view" action: open a browser on the Work whose pane this
# action was chosen from, and leave a way back to that pane.
#
# `wirk browser serve` blocks for as long as the bridge is live, the way
# the status pane's own program does, so this action needs a pane to
# block in. A Herdr plugin action has no terminal of its own, so -- the
# same move `plugin/configure.sh` makes -- this script re-launches
# itself in a fresh tab when it has no terminal, and runs the server
# directly once it does. What the operator sees is the browser that
# `--open` launches; the pane exists so the server has somewhere to keep
# running and something to point Ctrl-C at.
#
# Which Work this action is about is not in the environment. Herdr runs
# a plugin action from its own server environment plus its own
# variables -- it does not export the invoking pane's `WIRK_*` -- so
# reading `$WIRK_WORK_ID` here would find whatever the Herdr server was
# started with, or nothing (checked against herdr 0.9.0: a plugin
# action receives HERDR_PLUGIN_ROOT/CONFIG_DIR/STATE_DIR, HERDR_PANE_ID,
# HERDR_WORKSPACE_ID, HERDR_BIN_PATH and HERDR_PLUGIN_CONTEXT_JSON).
#
# What Herdr does supply is the invoking pane's working directory, in
# HERDR_PLUGIN_CONTEXT_JSON. A Work's Actor pane runs in the checkout
# wirk materialized for it, at `<estate>/worktrees/<work-id>` -- wirk's
# own layout, not a guess about Herdr -- so that path names the Work,
# and the name is then confirmed against `<estate>/works/<work-id>`
# before it is used. A pane somewhere else is not a Work's pane, and
# this says so rather than serving something it inferred.
#
# So `$WIRK_WORK_ID` must not win here. Inside a plugin action it is
# whatever the Herdr *server* was started with, which on this estate is
# a real Work id -- just not the invoking pane's. Preferring it silently
# serves the wrong Work and looks like it worked. A plugin action is
# recognised by HERDR_PLUGIN_CONTEXT_JSON: when that is set, the pane
# context decides and an inherited value is ignored. Run from a terminal
# that really does carry a Work (a person running this script inside a
# Work's own pane, no plugin context), `$WIRK_WORK_ID` is that answer.
#
# $HERDR_SOCKET_PATH is carried forward explicitly: the focus action
# talks to the Herdr session this action came from, and a pane that
# guessed a different session would focus the wrong place or nothing at
# all. $WIRK_RUN_ID is cleared -- the server is not that Run and must
# not present itself as one; it names its scope on the command line.
#
# The same discriminator settles which estate to use. Inside a plugin
# action, $WIRK_ESTATE_ROOT is the Herdr *server's* own environment, not
# the invoking pane's -- exactly the reasoning above for $WIRK_WORK_ID --
# so it must not silently win over the operator's own configured estate
# either: a server started inside some other estate would otherwise send
# this action there instead. Run from a real Work's own pane (no plugin
# context), $WIRK_ESTATE_ROOT is that pane's own intentional context and
# is used directly.
set -eu

ROOT="${HERDR_PLUGIN_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
# shellcheck source=plugin/wirk-bin.sh
. "$ROOT/plugin/wirk-bin.sh"
HERDR="${HERDR_BIN_PATH:-herdr}"

# One string field out of Herdr's own `serde_json::to_string` output.
# Compact, so `"key":"value"` with no spaces -- but a value is JSON, and
# a path with a quote or a backslash in it arrives escaped. Matching
# `[^"]*` truncates such a path silently and then names the wrong Work,
# so the match accepts escaped pairs and the value is unescaped after.
json_field() {
    printf '%s' "${2:-}" \
        | grep -o "\"$1\":\"\(\\\\.\|[^\"\\\\]\)*\"" \
        | head -1 \
        | sed -e "s/^\"$1\":\"//" -e 's/"$//' \
              -e 's|\\/|/|g' -e 's/\\"/"/g' -e 's/\\\\/\\/g'
}

if [ -n "${HERDR_PLUGIN_CONTEXT_JSON:-}" ]; then
    ESTATE=""
    if [ -n "${HERDR_PLUGIN_CONFIG_DIR:-}" ]; then
        ESTATE="$(cat "$HERDR_PLUGIN_CONFIG_DIR/estate" 2>/dev/null || true)"
    fi
else
    ESTATE="${WIRK_ESTATE_ROOT:-}"
    if [ -z "$ESTATE" ] && [ -n "${HERDR_PLUGIN_CONFIG_DIR:-}" ]; then
        ESTATE="$(cat "$HERDR_PLUGIN_CONFIG_DIR/estate" 2>/dev/null || true)"
    fi
fi

# The Work this action is about. An explicit one in the environment
# wins; otherwise the invoking pane's own directory has to be a
# checkout this estate materialized, and has to still be registered.
if [ -n "${HERDR_PLUGIN_CONTEXT_JSON:-}" ]; then
    WORK=""
else
    WORK="${WIRK_WORK_ID:-}"
fi
if [ -z "$WORK" ] && [ -n "${HERDR_PLUGIN_CONTEXT_JSON:-}" ] && [ -n "$ESTATE" ]; then
    PANE_CWD="$(json_field focused_pane_cwd "$HERDR_PLUGIN_CONTEXT_JSON")"
    [ -n "$PANE_CWD" ] || PANE_CWD="$(json_field workspace_cwd "$HERDR_PLUGIN_CONTEXT_JSON")"
    case "$PANE_CWD" in
        "$ESTATE/worktrees/"*)
            CANDIDATE="${PANE_CWD#"$ESTATE/worktrees/"}"
            CANDIDATE="${CANDIDATE%%/*}"
            if [ -n "$CANDIDATE" ] && [ -d "$ESTATE/works/$CANDIDATE" ]; then
                WORK="$CANDIDATE"
            fi
            ;;
    esac
fi

if [ ! -t 0 ]; then
    set -- --cwd "$ROOT" --label "Wirk browser" --focus \
        --env "HERDR_PLUGIN_ROOT=$ROOT" \
        --env "HERDR_PLUGIN_CONFIG_DIR=${HERDR_PLUGIN_CONFIG_DIR:-}" \
        --env "HERDR_PLUGIN_STATE_DIR=${HERDR_PLUGIN_STATE_DIR:-}" \
        --env "HERDR_SOCKET_PATH=${HERDR_SOCKET_PATH:-}" \
        --env "HERDR_SESSION=${HERDR_SESSION:-}" \
        --env "HERDR_BIN_PATH=${HERDR_BIN_PATH:-}" \
        --env "WIRK_ESTATE_ROOT=$ESTATE" \
        --env "WIRK_WORK_ID=$WORK" \
        --env WIRK_RUN_ID=
    if [ -n "${HERDR_WORKSPACE_ID:-}" ]; then
        set -- --workspace "$HERDR_WORKSPACE_ID" "$@"
    fi
    if ! TAB_JSON="$("$HERDR" tab create "$@" 2>&1)"; then
        echo "wirk browser: Herdr did not create a tab; nothing was opened."
        echo "    $TAB_JSON"
        exit 1
    fi
    PANE="$(printf '%s' "$TAB_JSON" | grep -o '"pane_id":"[^"]*"' | head -1 | cut -d'"' -f4)"
    TAB="$(printf '%s' "$TAB_JSON" | grep -o '"tab_id":"[^"]*"' | head -1 | cut -d'"' -f4)"
    if [ -z "$PANE" ]; then
        echo "wirk browser: Herdr reported no pane for the tab it created."
        echo "    $TAB_JSON"
        exit 1
    fi
    if ! RUN_ERR="$("$HERDR" pane run "$PANE" bash "$ROOT/plugin/browser.sh" 2>&1 >/dev/null)"; then
        echo "wirk browser: the pane opened but the browser bridge did not start."
        echo "    $RUN_ERR"
        echo "Left behind: tab $TAB (pane $PANE). Close it with:"
        echo "    $HERDR tab close $TAB"
        exit 1
    fi
    echo "wirk browser: opening in pane $PANE of tab $TAB."
    exit 0
fi

if ! WIRK_BIN="$(wirk_resolve_bin)"; then
    wirk_bin_explain_missing
    exit 1
fi

if [ -z "$ESTATE" ]; then
    echo "wirk browser: no estate configured. Run this plugin's 'Configure Wirk' action first."
    exit 0
fi

# The invoking pane's own Work, asked about as itself. A pane that
# carried no Work has nothing for this action to serve: the estate map
# is the operator's administrative read, and this action does not
# quietly widen into it.
if [ -n "$WORK" ]; then
    echo "wirk browser: serving Work $WORK"
    exec "$WIRK_BIN" browser serve --estate "$ESTATE" --work "$WORK" \
        --requesting-work "$WORK" --open
fi
echo "wirk browser: this pane is not a Work's own checkout under $ESTATE,"
echo "so there is nothing for this action to serve."
echo "Run it from a Work's pane. For the whole estate, run:"
echo "    $WIRK_BIN browser view --estate \"$ESTATE\" --admin --out <path.html>"
