#!/usr/bin/env bash
# End-to-end checks of the Linux overlay backends on a headless CI machine:
#   X11     — Xvfb: overlay window mapped, click-through (input shape), Ctrl+Shift+O grab,
#             screenshot.
#   Wayland — sway (wlroots) headless: layer-shell backend, output enumeration, honest
#             shortcut status without a GlobalShortcuts portal, screenshot.
# Usage: tools/ci/linux-overlay-test.sh [path/to/freehm] [output-dir]
set -euo pipefail

BIN=$(realpath "${1:-target/release/freehm}")
OUT=$(realpath -m "${2:-ci-out}")
mkdir -p "$OUT"

fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "PASS: $*"; }
wait_for() { for _ in $(seq 1 75); do if "$@" >/dev/null 2>&1; then return 0; fi; sleep 0.2; done; return 1; }

fresh_env() {
    export XDG_RUNTIME_DIR; XDG_RUNTIME_DIR=$(mktemp -d); chmod 700 "$XDG_RUNTIME_DIR"
    export FREEHM_CONFIG_DIR; FREEHM_CONFIG_DIR=$(mktemp -d)
    export XDG_CONFIG_HOME; XDG_CONFIG_HOME=$(mktemp -d)
}

st() { "$BIN" --status; }

start_app() {
    dbus-run-session -- "$BIN" --mock &
    APP=$!
    wait_for "$BIN" --status || { cat "$FREEHM_CONFIG_DIR"/logs/*.log >&2 || true; fail "$1: FreeHM did not start"; }
}

stop_app() {
    "$BIN" --quit || true
    for _ in $(seq 1 50); do kill -0 "$APP" 2>/dev/null || break; sleep 0.1; done
    if kill -0 "$APP" 2>/dev/null; then fail "$1: FreeHM did not exit after --quit"; fi
    [ ! -e "$XDG_RUNTIME_DIR/freehm/app.sock" ] || fail "$1: IPC socket left behind"
    pass "$1: --quit exits cleanly and removes its socket"
    cp "$FREEHM_CONFIG_DIR"/logs/*.log "$OUT/" 2>/dev/null || true
}

x11_test() {
    fresh_env
    unset WAYLAND_DISPLAY
    export XDG_SESSION_TYPE=x11 DISPLAY=:99
    Xvfb :99 -screen 0 1920x1080x24 +extension RANDR &
    XVFB=$!
    wait_for xdpyinfo || fail "Xvfb did not start"
    # A normal window underneath the overlay, for the click-through check.
    xterm -geometry 120x40+0+0 -e sleep 600 &
    XT=$!
    wait_for sh -c "xwininfo -root -tree | grep -q XTerm"

    start_app X11
    "$BIN" --show
    sleep 1
    st > "$OUT/x11-status.json"
    [ "$(jq -r .overlay.visible "$OUT/x11-status.json")" = true ] || fail "X11: overlay not visible: $(jq -r .overlay.reason "$OUT/x11-status.json")"
    jq -r .overlay.backend "$OUT/x11-status.json" | grep -q "X11 override-redirect" || fail "X11: unexpected backend"
    pass "X11: overlay visible with backend: $(jq -r .overlay.backend "$OUT/x11-status.json")"

    WID=$(xwininfo -root -tree | awk '/"FreeHM overlay"/ {print $1; exit}')
    [ -n "$WID" ] || fail "X11: overlay window not found"
    xwininfo -id "$WID" > "$OUT/x11-overlay-window.txt"
    grep -q "IsViewable" "$OUT/x11-overlay-window.txt" || fail "X11: overlay window not viewable"
    grep -q "Override Redirect State: yes" "$OUT/x11-overlay-window.txt" || fail "X11: not override-redirect"
    pass "X11: override-redirect overlay window $WID is mapped"

    # Click-through: with an empty input shape the pointer "falls through" to the window below.
    read -r X Y < <(awk '/Absolute upper-left X/ {x=$4} /Absolute upper-left Y/ {y=$4} END {print x+6, y+6}' "$OUT/x11-overlay-window.txt")
    xdotool mousemove "$X" "$Y"
    sleep 0.2
    UNDER=$(xdotool getmouselocation --shell | sed -n 's/^WINDOW=//p')
    [ "$((UNDER))" != "$((WID))" ] || fail "X11: the pointer hits the overlay (not click-through)"
    pass "X11: click-through (pointer at $X,$Y resolves to window $UNDER, not the overlay)"

    import -window root "$OUT/x11-screenshot.png" || true

    [ "$(jq -r .hotkey.registered "$OUT/x11-status.json")" = true ] || fail "X11: shortcut not registered: $(jq -r .hotkey.error "$OUT/x11-status.json")"
    xdotool key ctrl+shift+o
    sleep 0.6
    [ "$(st | jq -r .overlay.visible)" = false ] || fail "X11: Ctrl+Shift+O did not hide the overlay"
    xwininfo -id "$WID" | grep -q "IsUnMapped" || fail "X11: window still mapped after hide"
    xdotool key ctrl+shift+o
    sleep 0.6
    [ "$(st | jq -r .overlay.visible)" = true ] || fail "X11: Ctrl+Shift+O did not show the overlay again"
    pass "X11: Ctrl+Shift+O (XGrabKey) toggles the overlay"

    # Conflict handling: a second, separate instance on the same display cannot grab the
    # same key and must say so instead of pretending.
    (
        export XDG_RUNTIME_DIR; XDG_RUNTIME_DIR=$(mktemp -d); chmod 700 "$XDG_RUNTIME_DIR"
        export FREEHM_CONFIG_DIR; FREEHM_CONFIG_DIR=$(mktemp -d)
        dbus-run-session -- "$BIN" --mock &
        P2=$!
        wait_for "$BIN" --status || fail "X11: second instance did not start"
        "$BIN" --status > "$OUT/x11-conflict-status.json"
        "$BIN" --quit || true
        wait "$P2" 2>/dev/null || true
    )
    [ "$(jq -r .hotkey.registered "$OUT/x11-conflict-status.json")" = false ] || fail "X11: conflicting grab reported as registered"
    jq -r .hotkey.error "$OUT/x11-conflict-status.json" | grep -q "already used" || fail "X11: conflict not explained"
    pass "X11: shortcut conflict reported: $(jq -r .hotkey.error "$OUT/x11-conflict-status.json")"

    # Settings changes go through the same IPC request the settings window sends.
    CFG="$FREEHM_CONFIG_DIR/edited.json"
    st | jq '.config | .hotkeys.toggle_overlay = "Ctrl+Shift+Alt+K" | .general.refresh_interval_ms = 500
             | .general.start_with_system = true | .overlay.anchor = "BottomLeft" | .overlay.font_size = 21
             | (.metrics[] | select(.id == "CpuClock") | .enabled) = true | .detection.manual_games = ["mygame"]' > "$CFG"
    "$BIN" --apply-config "$CFG" || fail "X11: --apply-config failed"
    [ "$(st | jq -r '.hotkey.registered and .hotkey.shortcut == "Ctrl+Shift+Alt+K"')" = true ] || fail "X11: new shortcut not active: $(st | jq -c .hotkey)"
    xdotool key ctrl+shift+o; sleep 0.6
    [ "$(st | jq -r .overlay.visible)" = true ] || fail "X11: the old shortcut still works after changing it"
    xdotool key ctrl+shift+alt+k; sleep 0.6
    [ "$(st | jq -r .overlay.visible)" = false ] || fail "X11: the new shortcut does not toggle the overlay"
    xdotool key ctrl+shift+alt+k; sleep 0.6
    pass "X11: shortcut changed at runtime: Ctrl+Shift+Alt+K toggles, Ctrl+Shift+O released"
    AUTO="$XDG_CONFIG_HOME/autostart/freehm.desktop"
    grep -q '^Exec=".*/freehm" --background$' "$AUTO" || fail "X11: enabling startup did not write $AUTO"
    [ "$(st | jq -r .autostart.enabled)" = true ] || fail "X11: autostart not reported enabled"
    pass "X11: start with system on: $AUTO ($(grep ^Exec= "$AUTO"))"
    jq '.general.start_with_system = false' "$CFG" > "$CFG.2" && mv "$CFG.2" "$CFG"
    "$BIN" --apply-config "$CFG" || fail "X11: --apply-config failed"
    [ "$(st | jq -r .autostart.enabled)" = false ] || fail "X11: disabling startup left autostart enabled"
    pass "X11: start with system off: $( [ -e "$AUTO" ] && echo "entry disabled" || echo "entry removed")"
    jq '.general.refresh_interval_ms = 5' "$CFG" > "$CFG.bad"
    if MSG=$("$BIN" --apply-config "$CFG.bad" 2>&1); then fail "X11: an out-of-range interval was accepted"; fi
    [ "$(st | jq -r .config.general.refresh_interval_ms)" = 500 ] || fail "X11: a rejected configuration changed the settings"
    pass "X11: invalid configuration rejected without changing anything: $MSG"

    stop_app X11
    # The edited settings survive a restart.
    start_app X11-restart
    st > "$OUT/x11-restart-status.json"
    jq -e '.config | .hotkeys.toggle_overlay == "Ctrl+Shift+Alt+K" and .general.refresh_interval_ms == 500 and .overlay.anchor == "BottomLeft"
           and .overlay.font_size == 21 and (.metrics[] | select(.id == "CpuClock") | .enabled) and .detection.manual_games == ["mygame"]
           and (.general.start_with_system | not)' "$OUT/x11-restart-status.json" > /dev/null || fail "X11: settings lost across restart: $(jq -c .config "$OUT/x11-restart-status.json")"
    [ "$(jq -r .hotkey.registered "$OUT/x11-restart-status.json")" = true ] || fail "X11: saved shortcut not registered after restart"
    pass "X11: settings persisted across restart (shortcut, interval, position, font size, metric selection, manual games, startup)"
    stop_app X11-restart
    kill "$XT" "$XVFB" 2>/dev/null || true
    wait "$XVFB" 2>/dev/null || true
}

wayland_test() {
    fresh_env
    unset DISPLAY
    export XDG_SESSION_TYPE=wayland WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=pixman
    SWAYCFG=$(mktemp)
    printf 'output HEADLESS-1 resolution 1920x1080\nxwayland disable\n' > "$SWAYCFG"
    sway -c "$SWAYCFG" > "$OUT/sway.log" 2>&1 &
    SWAY=$!
    wait_for sh -c 'ls "$XDG_RUNTIME_DIR"/wayland-* | grep -v lock' || { cat "$OUT/sway.log"; fail "sway did not start"; }
    WAYLAND_DISPLAY=$(basename "$(ls "$XDG_RUNTIME_DIR"/wayland-* | grep -v lock | head -1)")
    export WAYLAND_DISPLAY
    SWAYSOCK=$(ls "$XDG_RUNTIME_DIR"/sway-ipc.*.sock 2>/dev/null | head -1 || true)
    export SWAYSOCK

    start_app Wayland
    "$BIN" --show
    sleep 1
    st > "$OUT/wayland-status.json"
    jq -r .overlay.backend "$OUT/wayland-status.json" | grep -q "Wayland layer-shell" || fail "Wayland: unexpected backend $(jq -r .overlay.backend "$OUT/wayland-status.json")"
    [ "$(jq -r .overlay.visible "$OUT/wayland-status.json")" = true ] || fail "Wayland: overlay not visible: $(jq -r .overlay.reason "$OUT/wayland-status.json")"
    pass "Wayland: overlay visible with backend: $(jq -r .overlay.backend "$OUT/wayland-status.json")"
    [ "$(jq '.monitors | length' "$OUT/wayland-status.json")" -ge 1 ] || fail "Wayland: no outputs reported"
    pass "Wayland: outputs reported: $(jq -c '[.monitors[] | {name, width, height, scale}]' "$OUT/wayland-status.json")"

    grim "$OUT/wayland-screenshot.png" || true

    # No GlobalShortcuts portal here: the status must say so instead of claiming success.
    [ "$(jq -r .hotkey.registered "$OUT/wayland-status.json")" = false ] || fail "Wayland: shortcut claims to be registered without a portal"
    pass "Wayland: shortcut honestly reported unavailable: $(jq -r .hotkey.error "$OUT/wayland-status.json")"
    "$BIN" --toggle
    sleep 0.5
    [ "$(st | jq -r .overlay.visible)" = false ] || fail "Wayland: --toggle did not hide the overlay"
    pass "Wayland: \`freehm --toggle\` fallback hides the overlay"

    stop_app Wayland
    kill "$SWAY" 2>/dev/null || true
}

x11_test
wayland_test
echo "all Linux overlay checks passed"
