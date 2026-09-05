#!/bin/sh
# wirk's scripted actor (P2.5 W1, ruling 0049 D148): a deterministic
# stand-in for a real coding agent, installed by a *test* as `opencode`
# in a temp directory it prepends to the pane's own PATH -- never on a
# real actor's PATH, never under /var/tmp/wirk-target. Same env-guard,
# shell-plus-python3-over-unix-socket idiom every
# refs/herdr/src/integration/assets/*/herdr-agent-state.sh reference
# hook already uses on this box (R3/R5). This script itself stays the
# pane's one long-lived foreground process the whole time: Herdr's own
# process-based agent detection reads the foreground job's own argv[0]
# (measured live, `w1/BUILD.md` -- an earlier draft that `exec`'d into
# a python3 driver made "python3", not "opencode", the foreground
# process's own name, and `agent.start` never completed), so every
# python3 invocation here is a brief, dialed-and-closed child, never a
# replacement of this process (no `exec`) and never the thing that
# blocks waiting for the next prompt.
#
# Script step grammar, one line consumed per turn (one prompt sent by
# `agent.prompt`, typed into this pane's own foreground stdin exactly
# as a human's terminal input would land):
#   idle               -- do nothing; end the turn with no worktree change
#   edit:<relpath>:<text> -- write <text> to <relpath> in cwd; end the turn
#   claim:<argv...>    -- run `wirk claim <argv...>` in cwd; end the turn
#   block              -- report blocked and wait; nothing here releases
#                         it -- the test's own pane.release_agent /
#                         pane.clear_agent_authority is the human/test's
#                         job (row 15), unchanged by this program
# A missing or exhausted script file is idle forever: never a defect,
# the test's own control of what happens next -- no report at all is
# sent for a turn with no script step left.
#
# `compose_first_prompt` (`wirk-herdr/src/run_loop.rs`) composes one
# `agent.prompt` call's text with embedded blank lines between its
# intent/artifacts/claim-instruction paragraphs; each embedded newline
# becomes its own line break once typed into this canonical-mode pty,
# so one prompt call can legitimately arrive as more than one physical
# line. `drain_extra_lines` (python3's `select`, a short bounded
# window -- this program's own turn-boundary heuristic, not a product
# wait; 0044 D134 governs wirk itself, never a test fixture standing
# in for an actor's own reading of its terminal) consumes any further
# lines the same prompt already delivered before a turn is treated as
# consumed, so a multi-line prompt is one turn, never several.
set -eu

report() {
    # $1: idle | working | blocked -- the exact `pane.report_agent`
    # shape every real hook on this box sends (source "herdr:opencode",
    # agent "opencode": wirk's ActorKind::Opencode always sends
    # kind_str "opencode", 0041 D129). Measured live against a real
    # Herdr session (`w1/BUILD.md`): Herdr's own full-lifecycle
    # hook-authority gate (`refs/herdr/src/terminal/state.rs::
    # route_full_lifecycle_hook_report`) refuses every
    # `pane.report_agent` for a `("herdr:opencode","opencode")` source
    # until a session is anchored, and the only session-start source
    # its own `session_start_source_is_recognized` accepts from a
    # fresh terminal is `"startup"` -- so `pane.report_agent_session`
    # carrying that value goes out immediately before every state
    # report, one dialed connection per call (0036 D112's per-request
    # pattern), re-anchored each time so the anchor never lapses.
    HERDR_REPORT_STATE="$1" HERDR_REPORT_SESSION_ID="$WIRK_SCRIPTED_ACTOR_SESSION_ID" python3 - <<'PY'
import json
import os
import socket
import time

pane_id = os.environ.get("HERDR_PANE_ID")
socket_path = os.environ.get("HERDR_SOCKET_PATH")
state = os.environ["HERDR_REPORT_STATE"]
session_id = os.environ["HERDR_REPORT_SESSION_ID"]
if not pane_id or not socket_path:
    raise SystemExit(0)


def send(request):
    try:
        client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        client.settimeout(2.0)
        client.connect(socket_path)
        client.sendall((json.dumps(request) + "\n").encode())
        try:
            client.recv(4096)
        except Exception:
            pass
        client.close()
    except Exception:
        pass


send(
    {
        "id": "herdr:opencode:session:%d:%d" % (int(time.time() * 1000), os.getpid()),
        "method": "pane.report_agent_session",
        "params": {
            "pane_id": pane_id,
            "source": "herdr:opencode",
            "agent": "opencode",
            "seq": time.time_ns(),
            "agent_session_id": session_id,
            "session_start_source": "startup",
        },
    }
)
send(
    {
        "id": "herdr:opencode:%d:%d" % (int(time.time() * 1000), os.getpid()),
        "method": "pane.report_agent",
        "params": {
            "pane_id": pane_id,
            "source": "herdr:opencode",
            "agent": "opencode",
            "seq": time.time_ns(),
            "state": state,
        },
    }
)
PY
}

# A short, bounded settle window (module doc): drains any further
# lines the pty already has buffered for this same `agent.prompt` call
# before this turn is treated as consumed. A brief child, never the
# process that blocks waiting for the *next* prompt. `-c` (never `python3
# - <<HEREDOC`, which would redirect *this* fd 0 to the heredoc's own
# text instead of leaving it as the pane's pty -- a defect found live,
# `w1/BUILD.md`): the program text is a command-line argument, so
# `sys.stdin` stays the inherited pty and genuinely sees what is still
# buffered there.
drain_extra_lines() {
    python3 -c '
import select
import sys

while True:
    ready, _, _ = select.select([sys.stdin], [], [], 0.5)
    if not ready:
        break
    if sys.stdin.readline() == "":
        break
'
}

[ "${HERDR_ENV:-}" = "1" ] || exit 0
[ -n "${HERDR_PANE_ID:-}" ] || exit 0
[ -n "${HERDR_SOCKET_PATH:-}" ] || exit 0
command -v python3 >/dev/null 2>&1 || exit 0

# A stable session id for this program's whole lifetime (`report`'s own
# doc comment).
WIRK_SCRIPTED_ACTOR_SESSION_ID="wirk-scripted-actor-$$"

script_file="${WIRK_SCRIPTED_ACTOR_SCRIPT:-}"
step_index=0
total_steps=0
if [ -n "$script_file" ] && [ -f "$script_file" ]; then
    total_steps=$(grep -c '' "$script_file")
fi

report idle

while IFS= read -r _line; do
    step=""
    if [ "$step_index" -lt "$total_steps" ]; then
        step_index=$((step_index + 1))
        step=$(sed -n "${step_index}p" "$script_file")
    fi

    # Exhausted or missing: idle forever -- no report, no action.
    [ -n "$step" ] || continue

    drain_extra_lines

    case "$step" in
        idle)
            report working
            report idle
            ;;
        edit:*)
            rest=${step#edit:}
            relpath=${rest%%:*}
            text=${rest#*:}
            report working
            printf '%s' "$text" >"$relpath"
            report idle
            ;;
        claim:*)
            argv=${step#claim:}
            report working
            eval "wirk claim $argv" || true
            report idle
            ;;
        block)
            report blocked
            ;;
        *)
            report working
            report idle
            ;;
    esac
done
