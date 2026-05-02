#!/usr/bin/env bash
# playground.sh — debug harness for the nested compositor.
#
# Subcommands:
#   up                Build and start: compositor + panel + dock + notification + portal.
#   down              Kill all playground processes + remove sockets.
#   status            Show pids, wayland socket, log sizes.
#   restart           down + up.
#   logs [name]       Tail a process log (default: compositor-slint).
#                     Names: compositor-slint | notification | portal | client.
#   screenshot [out]  grim screenshot of the host display (default: out.png
#                     under playground/).
#   watch [interval]  Take screenshots + tail logs every <interval> sec
#                     until Ctrl-C. Default 3 s.
#   client <cmd...>   Launch a wayland client against the compositor
#                     (e.g. `playground.sh client kitty -e htop`).
#   ipc <json>        Send a raw ShellRequest line to the compositor IPC
#                     socket via socat. Example:
#                       playground.sh ipc '{"type":"GetAllWindows"}'
#   events            Tail ShellEvents broadcast by the compositor.
#
# All state goes under playground/ (gitignored).
set -uo pipefail

cd "$(dirname "$0")"
DIR="playground"
PIDS="$DIR/pids"
LOGS="$DIR/logs"
SHOTS="$DIR/screenshots"
mkdir -p "$DIR" "$PIDS" "$LOGS" "$SHOTS"

CMD="${1:-status}"
shift || true

PROCS=(compositor-slint notification portal)

socket_name() {
    grep -oE 'Wayland socket: wayland-[0-9]+' "$LOGS/compositor-slint.log" 2>/dev/null \
        | tail -1 | awk '{print $3}'
}

wayland_display() {
    local s; s=$(socket_name)
    [[ -n "$s" ]] && echo "$s"
}

is_alive() { kill -0 "$1" 2>/dev/null; }

clean_pidfiles() {
    for f in "$PIDS"/*.pid; do
        [[ -e "$f" ]] || continue
        local pid; pid=$(cat "$f")
        if ! is_alive "$pid"; then rm -f "$f"; fi
    done
}

case "$CMD" in
    up)
        # Make sure nothing stale is running.
        "$0" down >/dev/null 2>&1 || true

        echo "[playground] building binaries..."
        cargo build --message-format=short \
            -p compositor-slint -p notification -p portal 2>&1 | tail -3

        echo "[playground] launching compositor..."
        stdbuf -oL ./target/debug/compositor-slint \
            > "$LOGS/compositor-slint.log" 2>&1 &
        echo $! > "$PIDS/compositor-slint.pid"

        echo -n "[playground] waiting for wayland socket"
        for _ in $(seq 1 80); do
            if ! is_alive "$(cat "$PIDS/compositor-slint.pid")"; then
                echo " — DEAD"
                tail -20 "$LOGS/compositor-slint.log"
                exit 1
            fi
            local_sock=$(socket_name)
            if [[ -n "${local_sock:-}" ]]; then
                echo " → $local_sock"
                break
            fi
            sleep 0.25
            echo -n "."
        done

        local_sock=$(socket_name)
        if [[ -z "${local_sock:-}" ]]; then
            echo "[playground] ERROR: socket never appeared. Last log:"
            tail -20 "$LOGS/compositor-slint.log"
            exit 1
        fi

        export WAYLAND_DISPLAY="$local_sock"
        echo "[playground] WAYLAND_DISPLAY=$WAYLAND_DISPLAY"

        for proc in notification portal; do
            stdbuf -oL "./target/debug/$proc" \
                > "$LOGS/$proc.log" 2>&1 &
            echo $! > "$PIDS/$proc.pid"
            echo "[playground] $proc pid=$(cat $PIDS/$proc.pid)"
        done

        echo
        echo "[playground] All up. Try:"
        echo "  ./playground.sh client kitty"
        echo "  ./playground.sh logs compositor"
        echo "  ./playground.sh watch 2"
        echo "  ./playground.sh screenshot"
        echo "  ./playground.sh down"
        ;;

    down)
        # Clients first
        if [[ -f "$PIDS/client.pid" ]]; then
            pid=$(cat "$PIDS/client.pid")
            kill "$pid" 2>/dev/null || true
            rm -f "$PIDS/client.pid"
        fi
        for proc in "${PROCS[@]}"; do
            f="$PIDS/$proc.pid"
            if [[ -f "$f" ]]; then
                pid=$(cat "$f")
                kill "$pid" 2>/dev/null || true
                rm -f "$f"
            fi
        done
        # Belt-and-braces — kill any stragglers from prior crashes.
        pkill -f "target/debug/compositor-slint" 2>/dev/null || true
        pkill -f "target/debug/notification"     2>/dev/null || true
        pkill -f "target/debug/portal"           2>/dev/null || true
        rm -f "${XDG_RUNTIME_DIR:-/tmp}/myDE.sock"
        echo "[playground] stopped."
        ;;

    status)
        clean_pidfiles
        printf "%-15s %-10s %-12s %s\n" "PROCESS" "PID" "ALIVE" "LOG SIZE"
        for proc in "${PROCS[@]}" client; do
            f="$PIDS/$proc.pid"
            if [[ -f "$f" ]]; then
                pid=$(cat "$f")
                alive=$(is_alive "$pid" && echo yes || echo no)
            else
                pid="-"; alive="-"
            fi
            log="$LOGS/$proc.log"
            sz=$([[ -f "$log" ]] && wc -c < "$log" || echo 0)
            printf "%-15s %-10s %-12s %s bytes\n" "$proc" "$pid" "$alive" "$sz"
        done
        echo
        local_sock=$(socket_name)
        if [[ -n "${local_sock:-}" ]]; then
            echo "Wayland socket: $local_sock"
            echo "  -> WAYLAND_DISPLAY=$local_sock <wayland-client>"
        else
            echo "Wayland socket: not yet announced"
        fi
        if [[ -S "${XDG_RUNTIME_DIR:-/tmp}/myDE.sock" ]]; then
            echo "IPC socket:     ${XDG_RUNTIME_DIR:-/tmp}/myDE.sock"
        else
            echo "IPC socket:     missing"
        fi
        ;;

    restart)
        "$0" down
        sleep 0.5
        "$0" up
        ;;

    logs)
        name="${1:-compositor-slint}"
        log="$LOGS/$name.log"
        if [[ ! -f "$log" ]]; then
            echo "no log at $log"; exit 1
        fi
        exec tail -f "$log"
        ;;

    screenshot)
        out="${1:-$SHOTS/$(date +%H%M%S).png}"
        # Pick the first available screenshot tool that works against the
        # host session. grim needs wlr-screencopy (sway/wlroots/Hyprland —
        # NOT GNOME mutter or our own compositor). gnome-screenshot uses
        # the xdg-desktop-portal screenshot interface (works on GNOME).
        # ImageMagick `import` is an X11/XWayland fallback.
        host_display="${HOST_WAYLAND_DISPLAY:-wayland-0}"
        if command -v grim >/dev/null \
           && WAYLAND_DISPLAY="$host_display" grim "$out" 2>/dev/null; then
            echo "$out (grim)"
        elif command -v gnome-screenshot >/dev/null \
             && gnome-screenshot -f "$out" 2>/dev/null; then
            echo "$out (gnome-screenshot)"
        elif command -v import >/dev/null \
             && import -window root "$out" 2>/dev/null; then
            echo "$out (import)"
        else
            echo "ERROR: no working screenshot tool" >&2
            exit 1
        fi
        ;;

    watch)
        interval="${1:-3}"
        echo "watching every ${interval}s — Ctrl-C to stop"
        echo "screenshots will be saved to $SHOTS/"
        i=0
        while true; do
            i=$((i + 1))
            stamp=$(date +%H:%M:%S)
            echo
            echo "===== tick $i  ($stamp) ====="
            "$0" status
            for log in "$LOGS"/*.log; do
                [[ -e "$log" ]] || continue
                name=$(basename "$log" .log)
                tail_text=$(tail -3 "$log" 2>/dev/null)
                if [[ -n "$tail_text" ]]; then
                    echo
                    echo "-- $name (last 3) --"
                    echo "$tail_text"
                fi
            done
            shot="$SHOTS/tick-$(printf %03d $i).png"
            if "$0" screenshot "$shot" >/dev/null 2>&1; then
                echo "screenshot: $shot"
            fi
            sleep "$interval"
        done
        ;;

    client)
        local_sock=$(socket_name)
        if [[ -z "${local_sock:-}" ]]; then
            echo "compositor isn't up — run ./playground.sh up first" >&2
            exit 1
        fi
        if [[ $# -lt 1 ]]; then
            echo "usage: playground.sh client <command> [args...]"; exit 1
        fi
        echo "[playground] launching client under WAYLAND_DISPLAY=$local_sock"
        WAYLAND_DISPLAY="$local_sock" "$@" \
            > "$LOGS/client.log" 2>&1 &
        echo $! > "$PIDS/client.pid"
        echo "[playground] client pid=$(cat $PIDS/client.pid) — log: $LOGS/client.log"
        ;;

    ipc)
        if [[ $# -lt 1 ]]; then
            echo "usage: playground.sh ipc '<json-shellrequest>'"; exit 1
        fi
        sock="${XDG_RUNTIME_DIR:-/tmp}/myDE.sock"
        if [[ ! -S "$sock" ]]; then
            echo "compositor IPC socket not found at $sock" >&2; exit 1
        fi
        if ! command -v socat >/dev/null; then
            echo "socat not installed — can't talk to the IPC socket" >&2
            exit 1
        fi
        printf '%s\n' "$1" | socat - "UNIX-CONNECT:$sock"
        ;;

    events)
        sock="${XDG_RUNTIME_DIR:-/tmp}/myDE.sock"
        if ! command -v socat >/dev/null; then
            echo "socat not installed — can't tail IPC events" >&2; exit 1
        fi
        echo "tailing ShellEvents from $sock — Ctrl-C to stop"
        socat - "UNIX-CONNECT:$sock"
        ;;

    inner-screenshot)
        # Ask the compositor itself to read back its framebuffer to PNG.
        # Avoids host-side screenshot tools entirely. Output is copied
        # into playground/screenshots/ for the conversation.
        sock="${XDG_RUNTIME_DIR:-/tmp}/myDE.sock"
        produced="${XDG_RUNTIME_DIR:-/tmp}/myDE-screenshot.png"
        if [[ ! -S "$sock" ]]; then
            echo "compositor IPC socket not found at $sock" >&2; exit 1
        fi
        if ! command -v socat >/dev/null; then
            echo "socat not installed — can't talk to IPC" >&2; exit 1
        fi
        rm -f "$produced"
        printf '%s\n' '{"type":"TakeScreenshot","region":null}' \
            | socat - "UNIX-CONNECT:$sock" >/dev/null &
        # Wait up to 3 s for the file to appear (one render frame).
        for _ in 1 2 3 4 5 6; do
            [[ -f "$produced" ]] && break
            sleep 0.5
        done
        if [[ ! -f "$produced" ]]; then
            echo "ERROR: compositor produced no screenshot at $produced" >&2
            exit 1
        fi
        out="${1:-$SHOTS/inner-$(date +%H%M%S).png}"
        cp "$produced" "$out"
        echo "$out"
        ;;

    type)
        # Inject keyboard text into the focused surface of the nested
        # compositor via wtype (zwp_virtual_keyboard_manager_v1).
        if ! command -v wtype >/dev/null; then
            echo "wtype not installed (sudo dnf install wtype)" >&2; exit 1
        fi
        local_sock=$(socket_name)
        if [[ -z "${local_sock:-}" ]]; then
            echo "compositor not up — run ./playground.sh up" >&2; exit 1
        fi
        if [[ $# -lt 1 ]]; then
            echo "usage: playground.sh type 'text to send'"; exit 1
        fi
        WAYLAND_DISPLAY="$local_sock" wtype "$@"
        ;;

    key)
        # Send a single keysym (Return, Escape, Left, BackSpace, etc.) via
        # wtype's -P (press+release) shortcut.
        if ! command -v wtype >/dev/null; then
            echo "wtype not installed (sudo dnf install wtype)" >&2; exit 1
        fi
        local_sock=$(socket_name)
        if [[ -z "${local_sock:-}" ]]; then
            echo "compositor not up — run ./playground.sh up" >&2; exit 1
        fi
        if [[ $# -lt 1 ]]; then
            echo "usage: playground.sh key Return | Escape | Left | …"; exit 1
        fi
        WAYLAND_DISPLAY="$local_sock" wtype -P "$1"
        ;;

    click)
        # ydotool talks to /dev/uinput so events go to whichever surface
        # has system-wide focus on the HOST. To target the nested
        # compositor: focus its window first (alt-tab to it manually or
        # use this command after the compositor window is foreground).
        if ! command -v ydotool >/dev/null; then
            echo "ydotool not installed" >&2; exit 1
        fi
        if ! pgrep -x ydotoold >/dev/null; then
            echo "ydotoold daemon not running. Start it with:" >&2
            echo "  sudo systemctl start ydotool   # or:" >&2
            echo "  sudo ydotoold &" >&2
            exit 1
        fi
        case "${1:-}" in
            "")        ydotool click 0xC0 ;;          # left click at current
            left)      ydotool click 0xC0 ;;
            right)     ydotool click 0xC1 ;;
            middle)    ydotool click 0xC2 ;;
            move)      shift; ydotool mousemove --absolute "$1" "$2" ;;
            *)         echo "usage: playground.sh click [left|right|middle|move x y]"; exit 1 ;;
        esac
        ;;

    *)
        sed -n '2,30p' "$0"
        ;;
esac
