#!/bin/sh
set -eu

DIR="$(dirname "$0")"
# shellcheck disable=SC1091
. "$DIR/lib.sh"

SCRIPT_DIR="$(CDPATH= cd -- "$DIR" && pwd)"
CLI_DIR="$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)"
REPO_DIR="$(CDPATH= cd -- "$CLI_DIR/.." && pwd)"
AGENTFS_BIN="${AGENTFS_BIN:-$CLI_DIR/target/debug/agentfs}"

TMP_ROOT=""
ADAPTER_PID=""
ADAPTER_LOG=""

APP_DIR=""
SNAPSHOT_FILE=""
REFRESH_ACT=""
EVENTS=""
JOURNAL=""

cleanup() {
    stop_adapter
    if [ -n "${TMP_ROOT:-}" ] && [ -d "$TMP_ROOT" ]; then
        rm -rf "$TMP_ROOT"
    fi
}
trap cleanup EXIT INT TERM

stop_adapter() {
    stop_adapter_process "${ADAPTER_PID:-}" "${AGENTFS_BIN:-}" "${TMP_ROOT:-}"
    ADAPTER_PID=""
}

wait_writable() {
    path="$1"
    timeout="${2:-10}"
    i=0
    while [ "$i" -lt "$timeout" ]; do
        if [ -w "$path" ]; then
            return 0
        fi
        i=$((i + 1))
        sleep 1
    done
    return 1
}

wait_token_event() {
    token="$1"
    file="$2"
    timeout="${3:-20}"
    i=0
    while [ "$i" -lt "$timeout" ]; do
        count="$(grep -c "$token" "$file" 2>/dev/null || true)"
        [ -n "$count" ] || count=0
        if [ "$count" -ge 1 ]; then
            return 0
        fi
        i=$((i + 1))
        sleep 1
    done
    return 1
}

ensure_agentfs_bin() {
    if command -v cargo >/dev/null 2>&1; then
        say "Building Linux agentfs binary for CT2 v2 tests..."
        if (cd "$CLI_DIR" && cargo build --quiet); then
            if [ -f "$CLI_DIR/target/debug/agentfs" ]; then
                AGENTFS_BIN="$CLI_DIR/target/debug/agentfs"
                return 0
            fi
            say "Linux build command succeeded but target/debug/agentfs is missing; trying Windows fallback binary..."
        else
            say "Linux build unavailable; trying Windows fallback binary..."
        fi
    fi

    if command -v cargo.exe >/dev/null 2>&1; then
        say "Building Windows agentfs binary for CT2 v2 tests..."
        if (cd "$CLI_DIR" && cargo.exe build --quiet); then
            if [ -f "$CLI_DIR/target/debug/agentfs.exe" ]; then
                AGENTFS_BIN="$CLI_DIR/target/debug/agentfs.exe"
                return 0
            fi
            say "Windows build command succeeded but target/debug/agentfs.exe is missing; falling back to existing binaries..."
        else
            say "Windows build unavailable; falling back to existing binaries..."
        fi
    fi

    if [ -f "$CLI_DIR/target/debug/agentfs" ]; then
        AGENTFS_BIN="$CLI_DIR/target/debug/agentfs"
        return 0
    fi

    if [ -f "$CLI_DIR/target/debug/agentfs.exe" ]; then
        AGENTFS_BIN="$CLI_DIR/target/debug/agentfs.exe"
        return 0
    fi

    fail "missing cargo/cargo.exe; set AGENTFS_BIN to an existing binary"
}

reload_fixture_app() {
    rm -rf "$TMP_ROOT/aiim"
    cp -R "$REPO_DIR/examples/appfs/aiim" "$TMP_ROOT/"
    APP_DIR="$TMP_ROOT/aiim"
    SNAPSHOT_FILE="$APP_DIR/chats/chat-001/messages.res.jsonl"
    REFRESH_ACT="$APP_DIR/_snapshot/refresh.act"
    EVENTS="$APP_DIR/_stream/events.evt.jsonl"
    JOURNAL="$APP_DIR/_stream/snapshot-expand.state.res.json"
}

start_adapter() {
    expand_delay_ms="${1:-0}"
    publish_delay_ms="${2:-0}"
    ADAPTER_LOG="$TMP_ROOT/appfs-adapter.log"
    runtime_root="$TMP_ROOT"
    case "$AGENTFS_BIN" in
        *.exe)
            win_bin="$AGENTFS_BIN"
            if command -v wslpath >/dev/null 2>&1; then
                runtime_root="$(wslpath -w "$TMP_ROOT")"
                win_bin="$(wslpath -w "$AGENTFS_BIN")"
            fi
            cmd.exe /C "set APPFS_V2_SNAPSHOT_EXPAND_DELAY_MS=$expand_delay_ms&& set APPFS_V2_SNAPSHOT_PUBLISH_DELAY_MS=$publish_delay_ms&& $win_bin serve appfs --root $runtime_root --app-id aiim --poll-ms 50" >"$ADAPTER_LOG" 2>&1 &
            ;;
        *)
            APPFS_V2_SNAPSHOT_EXPAND_DELAY_MS="$expand_delay_ms" APPFS_V2_SNAPSHOT_PUBLISH_DELAY_MS="$publish_delay_ms" "$AGENTFS_BIN" serve appfs --root "$runtime_root" --app-id aiim --poll-ms 50 >"$ADAPTER_LOG" 2>&1 &
            ;;
    esac
    ADAPTER_PID=$!
    sleep 1
    if ! kill -0 "$ADAPTER_PID" 2>/dev/null; then
        tail -n 120 "$ADAPTER_LOG" 2>/dev/null || true
        fail "appfs adapter failed to start"
    fi
}

journal_temp_artifact() {
    python3 - "$JOURNAL" <<'PY'
import json
import os
import sys

journal = sys.argv[1]
if not os.path.exists(journal):
    print("")
    raise SystemExit(0)

with open(journal, "r", encoding="utf-8") as f:
    doc = json.load(f)
entry = (doc.get("resources") or {}).get("chats/chat-001/messages.res.jsonl") or {}
print(entry.get("temp_artifact") or "")
PY
}

wait_journal_publishing() {
    timeout="${1:-20}"
    i=0
    while [ "$i" -lt "$timeout" ]; do
        if python3 - "$JOURNAL" <<'PY'
import json
import os
import sys

journal = sys.argv[1]
if not os.path.exists(journal):
    raise SystemExit(1)
with open(journal, "r", encoding="utf-8") as f:
    doc = json.load(f)
entry = (doc.get("resources") or {}).get("chats/chat-001/messages.res.jsonl")
if not entry:
    raise SystemExit(1)
if entry.get("status") != "publishing":
    raise SystemExit(1)
if not entry.get("temp_artifact"):
    raise SystemExit(1)
raise SystemExit(0)
PY
        then
            return 0
        fi
        i=$((i + 1))
        sleep 1
    done
    return 1
}

wait_recovery_event() {
    timeout="${1:-20}"
    i=0
    while [ "$i" -lt "$timeout" ]; do
        if grep -q "\"type\":\"cache.recovery\"" "$EVENTS" 2>/dev/null; then
            if grep "\"type\":\"cache.recovery\"" "$EVENTS" 2>/dev/null | grep -q "\"path\":\"/chats/chat-001/messages.res.jsonl\""; then
                return 0
            fi
        fi
        i=$((i + 1))
        sleep 1
    done
    return 1
}

banner "AppFS v2 CT2-006 Journal Recovery for Incomplete Expand"
require_cmd python3
ensure_agentfs_bin

mkdir -p "$CLI_DIR/target"
TMP_ROOT="$(mktemp -d "$CLI_DIR/target/ct2-v2-006.XXXXXX")"

reload_fixture_app
assert_file "$SNAPSHOT_FILE"
assert_file "$REFRESH_ACT"
assert_file "$EVENTS"

rm -f "$SNAPSHOT_FILE"
pass "removed $SNAPSHOT_FILE to force cold snapshot miss"

start_adapter 0 5000
pass "adapter started with delayed publish window for crash simulation"

wait_writable "$REFRESH_ACT" 10 || fail "snapshot refresh sink remained non-writable: $REFRESH_ACT"
token_crash="ct2-006-crash-$$"
printf '{"resource_path":"/chats/chat-001/messages.res.jsonl","client_token":"%s"}\n' "$token_crash" >> "$REFRESH_ACT" || fail "failed to submit refresh for crash simulation"

wait_journal_publishing 20 || fail "journal did not enter publishing state before kill"
temp_artifact_rel="$(journal_temp_artifact)"
[ -n "$temp_artifact_rel" ] || fail "journal missing temp_artifact path"
temp_artifact_abs="$APP_DIR/${temp_artifact_rel#/}"
[ -f "$temp_artifact_abs" ] || fail "expected pending temp artifact to exist before kill"
pass "journal captured publishing phase and pending temp artifact"

stop_adapter
pass "adapter killed during publish window"

[ ! -f "$SNAPSHOT_FILE" ] || fail "incomplete expansion must not expose final snapshot file"
[ -f "$temp_artifact_abs" ] || fail "expected temp artifact to remain after crash"
pass "crash leaves only temp artifact and no published half-product"

start_adapter 0 0
pass "adapter restarted for recovery scan"

wait_recovery_event 20 || fail "missing cache.recovery evidence after restart"
grep -F -q "[recovery] snapshot expand incomplete resource=/chats/chat-001/messages.res.jsonl" "$ADAPTER_LOG" || fail "missing recovery log anchor"
[ ! -f "$temp_artifact_abs" ] || fail "recovery should clean pending temp artifact"
pass "restart recovery cleaned incomplete expansion and emitted recovery evidence"

token_recover="ct2-006-recover-$$"
printf '{"resource_path":"/chats/chat-001/messages.res.jsonl","client_token":"%s"}\n' "$token_recover" >> "$REFRESH_ACT" || fail "failed to submit refresh after recovery"
wait_token_event "$token_recover" "$EVENTS" 20 || fail "recovery follow-up request timeout"

event_line="$(grep "$token_recover" "$EVENTS" 2>/dev/null | grep "\"type\":\"action.completed\"" | tail -n 1 || true)"
[ -n "$event_line" ] || fail "missing action.completed after recovery"

assert_file "$SNAPSHOT_FILE"
line_count="$(wc -l < "$SNAPSHOT_FILE" | tr -d ' ')"
[ "$line_count" -eq 100 ] || fail "recovered expansion should materialize full 100-line snapshot, got $line_count"
pass "post-recovery request materializes full snapshot correctly"

say "CT2-006 done"
