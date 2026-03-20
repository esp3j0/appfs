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

cleanup() {
    if [ -n "${ADAPTER_PID:-}" ] && kill -0 "$ADAPTER_PID" 2>/dev/null; then
        kill "$ADAPTER_PID" 2>/dev/null || true
        wait "$ADAPTER_PID" 2>/dev/null || true
    fi
    if [ -n "${TMP_ROOT:-}" ] && [ -d "$TMP_ROOT" ]; then
        rm -rf "$TMP_ROOT"
    fi
}
trap cleanup EXIT INT TERM

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
    timeout="${3:-15}"
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

assert_json_expr() {
    json_payload="$1"
    expr="$2"
    description="$3"
    if ! printf '%s\n' "$json_payload" | python3 -c 'import json,sys; expr=sys.argv[1]; obj=json.loads(sys.stdin.read()); raise SystemExit(0 if eval(expr, {"obj": obj}) else 1)' "$expr"
    then
        fail "$description"
    fi
}

ensure_agentfs_bin() {
    if [ -f "$CLI_DIR/target/debug/agentfs" ]; then
        AGENTFS_BIN="$CLI_DIR/target/debug/agentfs"
        return 0
    fi

    if [ -f "$CLI_DIR/target/debug/agentfs.exe" ]; then
        AGENTFS_BIN="$CLI_DIR/target/debug/agentfs.exe"
        return 0
    fi

    if command -v cargo >/dev/null 2>&1; then
        build_cmd="cargo"
        say "Building Linux agentfs binary for CT2 v2 tests..."
        if (cd "$CLI_DIR" && "$build_cmd" build --quiet); then
            if [ ! -f "$CLI_DIR/target/debug/agentfs" ]; then
                fail "linux cargo build succeeded but $CLI_DIR/target/debug/agentfs is missing"
            fi
            AGENTFS_BIN="$CLI_DIR/target/debug/agentfs"
            return 0
        fi
        say "Linux build unavailable; trying Windows fallback binary..."
    fi

    if command -v cargo.exe >/dev/null 2>&1; then
        build_cmd="cargo.exe"
        say "Building Windows agentfs binary for CT2 v2 tests..."
        (cd "$CLI_DIR" && "$build_cmd" build --quiet)
        if [ ! -f "$CLI_DIR/target/debug/agentfs.exe" ]; then
            fail "windows cargo build succeeded but $CLI_DIR/target/debug/agentfs.exe is missing"
        fi
        AGENTFS_BIN="$CLI_DIR/target/debug/agentfs.exe"
        return 0
    fi

    fail "missing cargo/cargo.exe; set AGENTFS_BIN to an existing binary"
}

banner "AppFS v2 CT2-009 Snapshot/Live Dual Shape"
require_cmd python3

ensure_agentfs_bin

mkdir -p "$CLI_DIR/target"
TMP_ROOT="$(mktemp -d "$CLI_DIR/target/ct2-v2-009.XXXXXX")"
cp -R "$REPO_DIR/examples/appfs/aiim" "$TMP_ROOT/"

APP_DIR="$TMP_ROOT/aiim"
SNAPSHOT_RESOURCE="$APP_DIR/chats/chat-001/messages.res.jsonl"
LIVE_RESOURCE="$APP_DIR/feed/recommendations.res.json"
FETCH_NEXT_ACT="$APP_DIR/_paging/fetch_next.act"
EVENTS="$APP_DIR/_stream/events.evt.jsonl"

assert_file "$SNAPSHOT_RESOURCE"
assert_file "$LIVE_RESOURCE"
assert_file "$FETCH_NEXT_ACT"
assert_file "$EVENTS"

ADAPTER_LOG="$TMP_ROOT/appfs-adapter.log"
runtime_root="$TMP_ROOT"
case "$AGENTFS_BIN" in
    *.exe)
        if command -v wslpath >/dev/null 2>&1; then
            runtime_root="$(wslpath -w "$TMP_ROOT")"
        fi
        ;;
esac
"$AGENTFS_BIN" serve appfs --root "$runtime_root" --app-id aiim --poll-ms 50 >"$ADAPTER_LOG" 2>&1 &
ADAPTER_PID=$!
sleep 1
if ! kill -0 "$ADAPTER_PID" 2>/dev/null; then
    tail -n 120 "$ADAPTER_LOG" 2>/dev/null || true
    fail "appfs adapter failed to start"
fi
pass "adapter started"

line_count="$(wc -l < "$SNAPSHOT_RESOURCE" | tr -d ' ')"
[ "$line_count" -ge 2 ] || fail "snapshot resource should contain JSONL lines"
first_line="$(head -n 1 "$SNAPSHOT_RESOURCE")"
assert_json_expr "$first_line" 'isinstance(obj, dict) and "id" in obj and "text" in obj and "items" not in obj and "page" not in obj' "snapshot resource must be pure JSONL without envelope"
pass "snapshot shape is pure JSONL"

assert_json_expr "$(cat "$LIVE_RESOURCE")" 'isinstance(obj.get("items"), list) and isinstance(obj.get("page"), dict) and isinstance(obj.get("page", {}).get("handle_id"), str) and obj.get("page", {}).get("mode") == "live"' "live resource must expose {items,page} with page.mode=live"
pass "live shape exposes {items,page} envelope"

handle_id="$(python3 - "$LIVE_RESOURCE" <<'PY'
import json
import sys

with open(sys.argv[1], "r", encoding="utf-8") as f:
    obj = json.load(f)
print(obj.get("page", {}).get("handle_id", ""))
PY
)"
initial_page_no="$(python3 - "$LIVE_RESOURCE" <<'PY'
import json
import sys

with open(sys.argv[1], "r", encoding="utf-8") as f:
    obj = json.load(f)
print(obj.get("page", {}).get("page_no", -1))
PY
)"
[ -n "$handle_id" ] || fail "live resource missing page.handle_id"

token="ct2-009-fetch-$$"
wait_writable "$FETCH_NEXT_ACT" 10 || fail "paging fetch_next sink remained non-writable: $FETCH_NEXT_ACT"
printf '{"handle_id":"%s","client_token":"%s"}\n' "$handle_id" "$token" >> "$FETCH_NEXT_ACT" || fail "failed to submit fetch_next request"
wait_token_event "$token" "$EVENTS" 15 || fail "fetch_next action event did not arrive in time"

event_line="$(grep "$token" "$EVENTS" 2>/dev/null | tail -n 1 || true)"
[ -n "$event_line" ] || fail "missing fetch_next event line"
assert_json_expr "$event_line" 'obj.get("type") == "action.completed"' "fetch_next should emit action.completed"
assert_json_expr "$event_line" 'isinstance(obj.get("content", {}).get("items"), list)' "fetch_next response missing content.items array"
assert_json_expr "$event_line" 'obj.get("content", {}).get("page", {}).get("mode") == "live"' "fetch_next page.mode must remain live"
next_page_no="$(printf '%s\n' "$event_line" | python3 -c 'import json,sys; print(json.loads(sys.stdin.read()).get("content", {}).get("page", {}).get("page_no", -1))')"
expected_page_no=$((initial_page_no + 1))
[ "$next_page_no" -eq "$expected_page_no" ] || fail "expected page_no=$expected_page_no after fetch_next, got $next_page_no"
pass "fetch_next minimal flow works with page_no increment"

say "CT2-009 done"
