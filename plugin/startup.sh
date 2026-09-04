#!/usr/bin/env bash
# Wirk plugin startup hook (P1 item 7; 0032 D99: idempotent across live
# handoff via the state-dir pointer, 0022 D79). Runs once per enabled
# plugin after Herdr's session API socket is ready, and again on live
# handoff -- never at link or enable (plugins.mdx "Startup hooks").
#
# Every branch below prints exactly one line to stdout (Herdr's plugin
# command log, plugins.mdx "Commands and environment") and falls
# through to `exit 0`: a startup hook's failure must not stop the
# server (plugins.mdx "Startup hooks", "A startup failure does not
# stop the server").
set -eu

# 1. No configured estate: no-op. The operator blocker this dissolves
#    (run-brief.md §2/§5) -- a startup fire in any other session that
#    has this plugin linked but never configured an estate does
#    nothing, spawns nothing.
CONFIG_FILE="$HERDR_PLUGIN_CONFIG_DIR/estate"
if [ ! -f "$CONFIG_FILE" ]; then
    echo "wirk startup: no estate configured ($CONFIG_FILE absent); nothing to do"
    exit 0
fi
ESTATE_ROOT="$(cat "$CONFIG_FILE")"
if [ -z "$ESTATE_ROOT" ]; then
    echo "wirk startup: $CONFIG_FILE is empty; nothing to do"
    exit 0
fi

# 2. Binary location (orient/manifest.md §3, R4): an explicit override
#    first, else cargo's own CARGO_TARGET_DIR convention, else the
#    plugin root's own target/ dir (this manifest lives at the
#    workspace root, so HERDR_PLUGIN_ROOT *is* that root).
WIRK_BIN="${WIRK_BIN_PATH:-${CARGO_TARGET_DIR:-$HERDR_PLUGIN_ROOT/target}/debug/wirk}"

# 3. Idempotency (0032 D99): wirkd writes a copy of its pointer file to
#    $HERDR_PLUGIN_STATE_DIR/wirkd.json whenever that variable is set
#    (wirk/src/wirkd/server.rs write_pointer, D79), so its presence
#    here is a cheap first check; `wirk wirkd ping` against the real
#    estate root is the decisive one -- a live wirkd answers it
#    regardless of whether this particular copy is current.
POINTER="$HERDR_PLUGIN_STATE_DIR/wirkd.json"
if [ -f "$POINTER" ] && "$WIRK_BIN" wirkd ping --estate "$ESTATE_ROOT" >/dev/null 2>&1; then
    echo "wirk startup: wirkd already running for $ESTATE_ROOT"
    exit 0
fi

# 4. No live wirkd answered. If a pointer copy exists but is stale
#    (its pid is dead), remove this copy so it does not linger --
#    wirkd's own bind path reclaims the estate-root copy and the
#    socket by pid-liveness before it listens (server.rs bind_socket,
#    0032 D99), this script only tidies its own state-dir copy.
if [ -f "$POINTER" ]; then
    POINTER_PID="$(sed -n 's/.*"pid":[[:space:]]*\([0-9][0-9]*\).*/\1/p' "$POINTER" 2>/dev/null || true)"
    if [ -n "$POINTER_PID" ] && ! kill -0 "$POINTER_PID" 2>/dev/null; then
        rm -f "$POINTER"
        echo "wirk startup: removed stale state-dir pointer (pid $POINTER_PID dead)"
    fi
fi

# 5. Spawn one, detached: new session (setsid, outlives this script's
#    process group), stdio redirected to a state-dir log (never
#    inherited -- a daemon must not hold the startup hook's log pipe
#    open), no wait -- the hook returns immediately.
if [ ! -x "$WIRK_BIN" ]; then
    echo "wirk startup: $WIRK_BIN not found or not executable; nothing to do"
    exit 0
fi
setsid nohup "$WIRK_BIN" wirkd start --estate "$ESTATE_ROOT" \
    >"$HERDR_PLUGIN_STATE_DIR/wirkd.out" 2>&1 </dev/null &
disown
echo "wirk startup: spawned wirkd for $ESTATE_ROOT"
exit 0
