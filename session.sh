#!/usr/bin/env bash
# Session launcher (release build). Same architecture as dev.sh — runs
# compositor-slint nested in the host's wayland session plus the
# notification + portal D-Bus services. compositor-slint is currently a
# wayland client (winit/wgpu); a bare-TTY DRM/KMS path is not yet wired.
#
# Usage: ./session.sh
#
# Logs go to $XDG_RUNTIME_DIR/myDE-session/.
set -euo pipefail

cd "$(dirname "$0")"

export XDG_CURRENT_DESKTOP=myDE
LOG_DIR="${XDG_RUNTIME_DIR:-/tmp}/myDE-session"
mkdir -p "$LOG_DIR"

cargo build --release -p compositor-slint -p notification -p portal \
            --message-format=short

./target/release/compositor-slint > "$LOG_DIR/compositor-slint.log" 2>&1 &
COMPOSITOR_PID=$!

# Wait for the compositor to bind its wayland socket.
for _ in $(seq 1 80); do
    if SOCK=$(grep -oE 'Wayland socket: wayland-[0-9]+' "$LOG_DIR/compositor-slint.log" 2>/dev/null | tail -1 | awk '{print $3}'); then
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
for proc in notification portal; do
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
