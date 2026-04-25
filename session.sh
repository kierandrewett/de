#!/usr/bin/env bash
# Session launcher (production). Run from a TTY login.
#   chmod +x session.sh
#   ./session.sh   # from a bare tty
#
# Logs go to $XDG_RUNTIME_DIR/myDE-session/.
set -euo pipefail

cd "$(dirname "$0")"

export XDG_CURRENT_DESKTOP=myDE
LOG_DIR="${XDG_RUNTIME_DIR:-/tmp}/myDE-session"
mkdir -p "$LOG_DIR"

cargo build --release -p compositor -p shell-panel -p shell-dock \
            -p shell-launcher -p notification -p portal --message-format=short

# Compositor on the bare TTY (DRM/KMS + libinput).
./target/release/compositor --tty-udev > "$LOG_DIR/compositor.log" 2>&1 &
COMPOSITOR_PID=$!

# Wait for the compositor to bind its wayland socket.
for _ in $(seq 1 80); do
    if SOCK=$(grep -oE 'Wayland socket: wayland-[0-9]+' "$LOG_DIR/compositor.log" 2>/dev/null | tail -1 | awk '{print $3}'); then
        if [[ -n "${SOCK:-}" ]]; then break; fi
    fi
    sleep 0.25
done

if [[ -z "${SOCK:-}" ]]; then
    echo "ERROR: compositor never advertised a wayland socket" >&2
    kill "$COMPOSITOR_PID" 2>/dev/null || true
    exit 1
fi
export WAYLAND_DISPLAY="$SOCK"

declare -A pids
for proc in shell-panel shell-dock notification portal; do
    ./target/release/"$proc" > "$LOG_DIR/$proc.log" 2>&1 &
    pids[$proc]=$!
done

cleanup() {
    for proc in "${!pids[@]}"; do
        kill "${pids[$proc]}" 2>/dev/null || true
    done
    kill "$COMPOSITOR_PID" 2>/dev/null || true
    wait 2>/dev/null || true
}
trap cleanup INT TERM EXIT

wait "$COMPOSITOR_PID"
