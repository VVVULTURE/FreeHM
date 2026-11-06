#!/usr/bin/env bash
# End-to-end check of the Linux updater on a headless CI machine. A release served from this
# machine plays GitHub's part:
#   a real TLS request to GitHub; a copy that cannot replace itself (.deb, .tar.gz) only tells
#   the user; an AppImage refuses a file that does not match its checksum, then replaces itself
#   and restarts.
# Usage: tools/ci/linux-update-test.sh path/to/FreeHM.AppImage path/to/freehm [output-dir]
set -euo pipefail

SRC=$(realpath "$1")
BIN=$(realpath "$2")
OUT=$(realpath -m "${3:-ci-out}")
mkdir -p "$OUT"

export XDG_RUNTIME_DIR; XDG_RUNTIME_DIR=$(mktemp -d); chmod 700 "$XDG_RUNTIME_DIR"
export FREEHM_CONFIG_DIR; FREEHM_CONFIG_DIR=$(mktemp -d)
export XDG_CONFIG_HOME; XDG_CONFIG_HOME=$(mktemp -d)
export XDG_SESSION_TYPE=x11 DISPLAY=:97
unset WAYLAND_DISPLAY
# No FUSE on CI machines. Keep the extracted copy: the old instance would remove it on exit.
export APPIMAGE_EXTRACT_AND_RUN=1 NO_CLEANUP=1

PORT=18378
WEB=$(mktemp -d)
DIR=$(mktemp -d)
NAME=FreeHM-9.9.9-x86_64.AppImage
APP=""

cleanup() {
    "$BIN" --quit 2>/dev/null || true
    cp "$FREEHM_CONFIG_DIR"/logs/*.log "$OUT/" 2>/dev/null || true
    kill "${SERVER:-}" "${XVFB:-}" 2>/dev/null || true
}
trap cleanup EXIT

fail() { echo "FAIL: $*" >&2; "$BIN" --status 2>/dev/null | jq -c .update >&2 || true; exit 1; }
pass() { echo "PASS: $*"; }
update() { "$BIN" --status | jq -r ".update.$1 // empty"; }
wait_for() { # seconds description command...
    local n=$(($1 * 5)) what=$2; shift 2
    for _ in $(seq 1 "$n"); do if "$@" >/dev/null 2>&1; then return 0; fi; sleep 0.2; done
    fail "timed out waiting for $what"
}
phase_is() { [ "$(update phase)" = "$1" ]; }
message_has() { update message | grep -q "$1"; }
stop_app() {
    "$BIN" --quit || true
    for _ in $(seq 1 100); do "$BIN" --status >/dev/null 2>&1 || return 0; sleep 0.1; done
    fail "FreeHM did not exit"
}

Xvfb :97 -screen 0 1280x720x24 & XVFB=$!
wait_for 10 "Xvfb" xdpyinfo
echo '{"schema_version":1,"general":{"start_with_system":false},"permissions":{"first_run_done":true}}' > "$FREEHM_CONFIG_DIR/config.json"

# A real request: the certificate is checked against the system's certificates. An answer of
# any kind from GitHub proves that (a rate limit or no release yet is an answer).
dbus-run-session -- "$BIN" --background & APP=$!
wait_for 15 "FreeHM to start" "$BIN" --status
"$BIN" --check-update
wait_for 40 "the check to end" sh -c "[ \"\$($BIN --status | jq -r .update.phase)\" != Checking ]"
case "$(update phase)" in
    UpToDate | Available) pass "GitHub answered over TLS: $(update phase)" ;;
    *) message_has "http status" || fail "the request to GitHub failed: $(update message)"
       pass "GitHub answered over TLS: $(update message)" ;;
esac
stop_app

# GitHub's part from here on.
python3 -m http.server "$PORT" --bind 127.0.0.1 --directory "$WEB" >/dev/null 2>&1 & SERVER=$!
wait_for 10 "the test server" sh -c "python3 -c \"import socket; socket.create_connection(('127.0.0.1', $PORT), 1)\""
cat > "$WEB/release.json" <<EOF
{"tag_name": "v9.9.9", "html_url": "https://github.com/VVVULTURE/FreeHM/releases/tag/v9.9.9", "draft": false,
 "assets": [{"name": "$NAME", "browser_download_url": "http://127.0.0.1:$PORT/$NAME"},
            {"name": "SHA256SUMS.txt", "browser_download_url": "http://127.0.0.1:$PORT/SHA256SUMS.txt"}]}
EOF
# A different file that still runs: an AppImage ignores what follows its filesystem.
cp "$SRC" "$WEB/$NAME"
printf 'the next version' >> "$WEB/$NAME"
WANT=$(sha256sum "$WEB/$NAME" | cut -d' ' -f1)
echo "$(printf '0%.0s' $(seq 64))  $NAME" > "$WEB/SHA256SUMS.txt"
export FREEHM_UPDATE_URL="http://127.0.0.1:$PORT/release.json"

# A plain binary cannot replace itself.
dbus-run-session -- "$BIN" --background & APP=$!
wait_for 15 "FreeHM to start" "$BIN" --status
"$BIN" --check-update
wait_for 20 "the update to be found" phase_is Available
[ "$(update method)" = Manual ] || fail "a plain binary should be updated by hand, got $(update method)"
[ "$(update page_url)" = "https://github.com/VVVULTURE/FreeHM/releases/tag/v9.9.9" ] || fail "no link to the release"
if msg=$("$BIN" --install-update 2>&1); then fail "--install-update accepted on a plain binary"; fi
echo "$msg" | grep -q "get the new version from" || fail "unexpected answer: $msg"
pass "a copy that cannot replace itself points to the release: $msg"
stop_app

# The AppImage. The first file offered does not match its checksum.
cp "$SRC" "$DIR/FreeHM.AppImage"
OLD=$(sha256sum "$DIR/FreeHM.AppImage" | cut -d' ' -f1)
dbus-run-session -- "$DIR/FreeHM.AppImage" --background & APP=$!
wait_for 30 "the AppImage to start" "$BIN" --status
[ "$(update method)" = AppImage ] || fail "the AppImage was not recognised: $(update method)"
wait_for 40 "the checksum refusal" message_has "does not match"
[ "$(update phase)" = Available ] || fail "wrong state after a bad checksum"
[ "$(sha256sum "$DIR/FreeHM.AppImage" | cut -d' ' -f1)" = "$OLD" ] || fail "a refused file replaced the AppImage"
[ "$(ls -A "$DIR")" = FreeHM.AppImage ] || fail "a refused download was left behind: $(ls -A "$DIR")"
pass "the AppImage throws away a file that does not match its checksum: $(update message)"

# Now the right file. Nothing is left to find once it has restarted.
echo "$WANT  $NAME" > "$WEB/SHA256SUMS.txt"
"$BIN" --install-update
replaced() { [ "$(sha256sum "$DIR/FreeHM.AppImage" | cut -d' ' -f1)" = "$WANT" ]; }
wait_for 30 "the AppImage to be replaced" replaced
rm "$WEB/release.json"
[ -x "$DIR/FreeHM.AppImage" ] || fail "the new AppImage is not executable"
[ ! -e "$DIR/.FreeHM-update.AppImage" ] || fail "the download was left behind"
pass "the AppImage replaced itself"
wait_for 20 "the new version to start" pgrep -f "FreeHM.AppImage --restart-after|--restart-after"
wait_for 30 "the new version to answer" "$BIN" --status
[ "$("$BIN" --status | jq -r .update.method)" = AppImage ] || fail "the restarted copy is not the AppImage"
pass "the AppImage restarted as the new file"
stop_app
echo "all updater checks passed"
