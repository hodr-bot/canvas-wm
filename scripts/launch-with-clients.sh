#!/usr/bin/env bash
# Launch canvas-wm, wait until it is really ready, then start clients.
#
# Usage: scripts/launch-with-clients.sh [client command ...]
#   With no client args, just starts the compositor and prints the
#   environment clients need.
#
# Why not `sleep N`? A fixed sleep races slow builds (nix fetch on first
# run) and wastes time on fast ones. This polls for the two readiness
# signals the compositor itself emits instead:
#   1. `wayland socket ready on "wayland-N"` in the log  -> WAYLAND_DISPLAY
#   2. a new socket in /tmp/.X11-unix (Xwayland picks a free display
#      number at runtime, so hardcoding DISPLAY=:2 misses) -> DISPLAY
#
# The compositor runs in the background: this script exits once the
# clients are started and never waits for the compositor to quit.
# Stop it with: kill <compositor pid printed below>.
set -u

RT="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
LOG="${CANVAS_LOG:-/tmp/canvas-wm-launch.log}"
X11DIR="${X11_DIR:-/tmp/.X11-unix}"
TIMEOUT="${CANVAS_TIMEOUT:-60}"
# Command that starts the compositor (overridable for tests).
CANVAS_CMD="${CANVAS_CMD:-nix run github:hodr-bot/canvas-wm#compositor -- --winit}"

export XDG_RUNTIME_DIR="$RT"

# Poll a probe command until it succeeds or the timeout expires.
wait_for() { # $1 = description, $2 = timeout_s, $3... = probe
    local desc="$1" timeout="$2"
    shift 2
    local i=0
    while ! "$@" >/dev/null 2>&1; do
        sleep 0.2
        i=$((i + 1))
        if [ "$i" -ge "$((timeout * 5))" ]; then
            echo "launch-with-clients: timeout waiting for $desc (log: $LOG)" >&2
            kill "$COMP_PID" 2>/dev/null
            exit 1
        fi
    done
}

# Echo the display number of an X socket that was not there before.
new_x_display() {
    local before="$1" f
    for f in "$X11DIR"/X[0-9]*; do
        [ -e "$f" ] || continue
        f="${f##*/X}"
        case " $before " in
            *" $f "*) ;;
            *) echo "$f"; return 0 ;;
        esac
    done
    return 1
}

X_BEFORE="$(ls "$X11DIR" 2>/dev/null | sed 's/^X//' || true)"
: > "$LOG"
# shellcheck disable=SC2086
$CANVAS_CMD >>"$LOG" 2>&1 &
COMP_PID=$!

wait_for "wayland socket" "$TIMEOUT" grep -q 'wayland socket ready' "$LOG"
SOCK="$(grep -o 'wayland socket ready on "[^"]*"' "$LOG" | head -1 | cut -d'"' -f2)"
export WAYLAND_DISPLAY="$SOCK"

NUM=""
i=0
while [ -z "$NUM" ]; do
    NUM="$(new_x_display "$X_BEFORE" || true)"
    if [ -z "$NUM" ]; then
        sleep 0.2
        i=$((i + 1))
        if [ "$i" -ge "$((TIMEOUT * 5))" ]; then
            echo "launch-with-clients: timeout waiting for Xwayland socket (log: $LOG)" >&2
            kill "$COMP_PID" 2>/dev/null
            exit 1
        fi
    fi
done
export DISPLAY=":$NUM"

echo "launch-with-clients: compositor pid $COMP_PID, WAYLAND_DISPLAY=$WAYLAND_DISPLAY DISPLAY=$DISPLAY"
if [ "$#" -eq 0 ]; then
    echo "launch-with-clients: no clients given; run clients with that env, stop with: kill $COMP_PID"
    exit 0
fi

for client in "$@"; do
    # shellcheck disable=SC2086
    $client &
    echo "launch-with-clients: started '$client' (pid $!)"
done
exit 0
