#!/usr/bin/env bash
# run.sh — build and run the GPU compositor-slint with a test client.
#
# Usage: bash crates/compositor-slint/run.sh
#
# This script builds compositor-slint (GPU pipeline via FemtoVGWGPURenderer),
# launches it, waits for the Wayland socket, then launches kitty inside it.
# Screenshot is taken to /tmp/slint-gpu-spike.png for visual verification (D5).
#
# Requirements: kitty, grim (optional for screenshots)
# Note: uses CARGO_TARGET_DIR=/var/tmp/de-prompts-target if disk space is tight.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
LOG="/tmp/slint-gpu.log"
SCREENSHOT="/tmp/slint-gpu-spike.png"

# Use /var/tmp for build artifacts if /home is tight on space
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/var/tmp/de-prompts-target}"

cd "$REPO_ROOT"

echo "=== Building compositor-slint (GPU) ==="
cargo build -p compositor-slint 2>&1

BINARY="$CARGO_TARGET_DIR/debug/compositor-slint"

echo "=== Starting GPU compositor (logs → $LOG) ==="
"$BINARY" > "$LOG" 2>&1 &
COMPOSITOR_PID=$!
echo "Compositor PID: $COMPOSITOR_PID"

# Wait for the Wayland socket to appear
echo "=== Waiting for Wayland socket ==="
WAYLAND_SOCKET=""
for i in $(seq 1 30); do
    sleep 0.5
    WAYLAND_SOCKET=$(grep "WAYLAND_DISPLAY=" "$LOG" 2>/dev/null | tail -1 | sed 's/WAYLAND_DISPLAY=//' || true)
    if [ -n "$WAYLAND_SOCKET" ]; then
        echo "Socket ready: $WAYLAND_SOCKET"
        break
    fi
done

if [ -z "$WAYLAND_SOCKET" ]; then
    echo "ERROR: Wayland socket not found. Logs:"
    cat "$LOG"
    kill $COMPOSITOR_PID 2>/dev/null || true
    exit 1
fi

echo "=== Launching kitty (WAYLAND_DISPLAY=$WAYLAND_SOCKET) ==="
WAYLAND_DISPLAY="$WAYLAND_SOCKET" kitty &
KITTY_PID=$!

echo "=== Waiting 5 seconds for kitty to render ==="
sleep 5

echo "=== Taking screenshot to $SCREENSHOT ==="
if command -v grim &>/dev/null; then
    grim "$SCREENSHOT" && echo "Screenshot saved: $SCREENSHOT"
    echo "Compare with: /tmp/spike-2.png (software renderer reference)"
else
    echo "grim not available — skipping screenshot (D5 incomplete)"
fi

echo "=== Compositor logs ==="
cat "$LOG"

echo ""
echo "=== Done ==="
echo "Compositor PID $COMPOSITOR_PID still running."
echo "Kill with: kill $COMPOSITOR_PID"
echo "Screenshot: $SCREENSHOT"
