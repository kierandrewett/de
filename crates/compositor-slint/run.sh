#!/usr/bin/env bash
# run.sh — smoke test for compositor-slint.
#
# Behaviour:
#   1. Launches compositor-slint binary (pre-built; this script does NOT build).
#   2. Waits up to 3 s for the Wayland socket to appear in the log.
#   3. Launches kitty against the socket.
#   4. Waits 15 s for visual inspection / stability check.
#   5. Cleans up (kills compositor + kitty).
#   6. Returns 0 if no "panicked at" was found in the log, non-zero otherwise.
#
# Build before running:
#   CARGO_TARGET_DIR=/var/tmp/de-prompts-target cargo build -p compositor-slint
#
# Usage:
#   bash crates/compositor-slint/run.sh
#
# Requirements: kitty, optional: grim (for screenshot)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
LOG="/tmp/slint-compositor.log"
SCREENSHOT="/tmp/slint-prod.png"

export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/var/tmp/de-prompts-target}"
BINARY="$CARGO_TARGET_DIR/debug/compositor-slint"

if [ ! -f "$BINARY" ]; then
    echo "ERROR: binary not found at $BINARY"
    echo "Run: CARGO_TARGET_DIR=/var/tmp/de-prompts-target cargo build -p compositor-slint"
    exit 1
fi

cd "$REPO_ROOT"

echo "=== Starting compositor-slint (logs → $LOG) ==="
RUST_LOG="compositor_slint=debug,wgpu=warn" "$BINARY" > "$LOG" 2>&1 &
COMPOSITOR_PID=$!
echo "Compositor PID: $COMPOSITOR_PID"

# ── Wait up to 3 s for the Wayland socket ────────────────────────────────────
echo "=== Waiting up to 3 s for Wayland socket ==="
WAYLAND_SOCKET=""
for i in $(seq 1 6); do
    sleep 0.5
    WAYLAND_SOCKET=$(grep "WAYLAND_DISPLAY=" "$LOG" 2>/dev/null | tail -1 | sed 's/WAYLAND_DISPLAY=//' || true)
    if [ -n "$WAYLAND_SOCKET" ]; then
        echo "Socket ready: $WAYLAND_SOCKET"
        break
    fi
done

if [ -z "$WAYLAND_SOCKET" ]; then
    echo "ERROR: Wayland socket not found after 3 s. Log tail:"
    tail -30 "$LOG"
    kill "$COMPOSITOR_PID" 2>/dev/null || true
    exit 1
fi

# ── Launch kitty ──────────────────────────────────────────────────────────────
echo "=== Launching kitty (WAYLAND_DISPLAY=$WAYLAND_SOCKET) ==="
WAYLAND_DISPLAY="$WAYLAND_SOCKET" kitty &
KITTY_PID=$!

# ── 15 s visual inspection window ────────────────────────────────────────────
echo "=== Waiting 15 s for visual inspection ==="
sleep 15

# ── Optional screenshot ───────────────────────────────────────────────────────
if command -v grim &>/dev/null; then
    echo "=== Taking screenshot to $SCREENSHOT ==="
    grim "$SCREENSHOT" && echo "Screenshot saved: $SCREENSHOT" || true
else
    echo "(grim not available — skipping screenshot)"
fi

# ── Cleanup ───────────────────────────────────────────────────────────────────
echo "=== Cleaning up ==="
kill "$KITTY_PID" 2>/dev/null || true
kill "$COMPOSITOR_PID" 2>/dev/null || true
# Give them a moment to exit
sleep 1
kill -0 "$COMPOSITOR_PID" 2>/dev/null && kill -9 "$COMPOSITOR_PID" 2>/dev/null || true

# ── Check for panics ─────────────────────────────────────────────────────────
echo "=== Log tail (last 30 lines) ==="
tail -30 "$LOG"

if grep -q "panicked at" "$LOG" 2>/dev/null; then
    echo ""
    echo "RESULT: FAIL — panic detected in log"
    exit 1
else
    echo ""
    echo "RESULT: PASS — no panic detected"
    exit 0
fi
