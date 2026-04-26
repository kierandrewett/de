#!/usr/bin/env bash
# run.sh — build and run the Slint compositor spike with a test client.
#
# Usage: bash crates/compositor-slint-spike/run.sh
#
# Requirements: kitty, grim (optional for screenshots)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
LOG="/tmp/spike.log"
SCREENSHOT_DIR="/tmp"

cd "$REPO_ROOT"

echo "=== Building compositor-slint-spike ==="
cargo build -p compositor-slint-spike 2>&1

BINARY="$REPO_ROOT/target/debug/compositor-slint-spike"

echo "=== Starting compositor (logs → $LOG) ==="
"$BINARY" > "$LOG" 2>&1 &
COMPOSITOR_PID=$!

echo "Compositor PID: $COMPOSITOR_PID"

# Wait for the wayland socket to appear
echo "=== Waiting for wayland socket ==="
WAYLAND_SOCKET=""
for i in $(seq 1 20); do
    sleep 0.5
    # Parse socket name from log
    WAYLAND_SOCKET=$(grep "WAYLAND_DISPLAY=" "$LOG" 2>/dev/null | tail -1 | sed 's/WAYLAND_DISPLAY=//' || true)
    if [ -n "$WAYLAND_SOCKET" ]; then
        echo "Socket ready: $WAYLAND_SOCKET"
        break
    fi
done

if [ -z "$WAYLAND_SOCKET" ]; then
    echo "ERROR: Wayland socket not found in logs. Check $LOG"
    kill $COMPOSITOR_PID 2>/dev/null || true
    exit 1
fi

echo "=== Launching kitty (WAYLAND_DISPLAY=$WAYLAND_SOCKET) ==="
WAYLAND_DISPLAY="$WAYLAND_SOCKET" kitty &
KITTY_PID=$!

echo "=== Waiting 5 seconds for interaction ==="
echo "Take screenshots with: grim $SCREENSHOT_DIR/spike-\$(date +%s).png"
sleep 5

echo "=== Taking screenshot ==="
if command -v grim &>/dev/null; then
    grim "$SCREENSHOT_DIR/spike-$(date +%s).png" && echo "Screenshot saved"
else
    echo "grim not available — skipping screenshot"
fi

echo "=== Logs ==="
cat "$LOG"

echo "=== Done. Compositor still running (PID $COMPOSITOR_PID). Kill with: kill $COMPOSITOR_PID ==="
