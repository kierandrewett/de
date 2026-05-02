#!/usr/bin/env bash
# Dev launcher — builds + runs compositor-slint nested in the host wayland
# session, plus the notification and portal D-Bus services as IPC peers.
# Usage: ./dev.sh
#
# Compositor logs go to stdout; each peer process logs to session-logs/dev/.
set -euo pipefail

cd "$(dirname "$0")"

LOG_DIR="session-logs/dev"
mkdir -p "$LOG_DIR"

# Make sure release-style env doesn't sneak in.
unset XDG_CURRENT_DESKTOP

# Build the binaries we'll launch.
cargo build -p compositor-slint -p notification -p portal --message-format=short

# Run the compositor directly (not via `cargo run`) so we get the actual
# process pid + line-buffered stdout, which the wayland-socket discovery
# loop below depends on.
stdbuf -oL ./target/debug/compositor-slint \
    > "$LOG_DIR/compositor-slint.log" 2>&1 &
COMPOSITOR_PID=$!
echo "compositor-slint pid=$COMPOSITOR_PID — log: $LOG_DIR/compositor-slint.log"

# Wait for the compositor to bind its wayland socket.
for _ in $(seq 1 80); do
    if ! kill -0 "$COMPOSITOR_PID" 2>/dev/null; then
        echo "ERROR: compositor exited early. Last log lines:" >&2
        tail -20 "$LOG_DIR/compositor-slint.log" >&2
        exit 1
    fi
    if SOCK=$(grep -oE 'Wayland socket: wayland-[0-9]+' "$LOG_DIR/compositor-slint.log" 2>/dev/null | tail -1 | awk '{print $3}'); then
        if [[ -n "${SOCK:-}" ]]; then break; fi
    fi
    sleep 0.25
done

if [[ -z "${SOCK:-}" ]]; then
    echo "ERROR: compositor never advertised a wayland socket" >&2
    kill "$COMPOSITOR_PID" 2>/dev/null || true
    tail -20 "$LOG_DIR/compositor-slint.log"
    exit 1
fi

export WAYLAND_DISPLAY="$SOCK"
echo "WAYLAND_DISPLAY=$WAYLAND_DISPLAY"

# Spawn the D-Bus services. They connect to the compositor's IPC socket
# ($XDG_RUNTIME_DIR/myDE.sock) on their own.
declare -A pids
for proc in notification portal; do
    stdbuf -oL ./target/debug/"$proc" > "$LOG_DIR/$proc.log" 2>&1 &
    pids[$proc]=$!
    echo "$proc pid=${pids[$proc]} — log: $LOG_DIR/$proc.log"
done

cleanup() {
    echo
    echo "shutting down..."
    for proc in "${!pids[@]}"; do
        kill "${pids[$proc]}" 2>/dev/null || true
    done
    kill "$COMPOSITOR_PID" 2>/dev/null || true
    wait 2>/dev/null || true
}
trap cleanup INT TERM EXIT

echo
echo "Stack is live. Try launching a wayland client, e.g.:"
echo "  WAYLAND_DISPLAY=$WAYLAND_DISPLAY alacritty"
echo
echo "Press Ctrl-C to tear it all down."

wait "$COMPOSITOR_PID"
