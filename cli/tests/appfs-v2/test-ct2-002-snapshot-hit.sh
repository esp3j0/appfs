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
    stop_adapter_process "${ADAPTER_PID:-}" "${AGENTFS_BIN:-}" "${TMP_ROOT:-}"
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

banner "AppFS v2 CT2-002 Snapshot Read Hit + Miss Hook"
require_cmd python3

ensure_agentfs_bin "$CLI_DIR"

mkdir -p "$CLI_DIR/target"
TMP_ROOT="$(mktemp -d "$CLI_DIR/target/ct2-v2-002.XXXXXX")"
cp -R "$REPO_DIR/examples/appfs/aiim" "$TMP_ROOT/"

APP_DIR="$TMP_ROOT/aiim"
SNAPSHOT_HIT="$APP_DIR/chats/chat-001/messages.res.jsonl"
SNAPSHOT_MISS="$APP_DIR/chats/chat-long/messages.res.jsonl"
SNAPSHOT_MISS_REL="/chats/chat-long/messages.res.jsonl"
REFRESH_ACT="$APP_DIR/_snapshot/refresh.act"
EVENTS="$APP_DIR/_stream/events.evt.jsonl"

assert_file "$SNAPSHOT_HIT"
assert_file "$SNAPSHOT_MISS"
assert_file "$REFRESH_ACT"
assert_file "$EVENTS"

rm -f "$SNAPSHOT_MISS"
pass "removed $SNAPSHOT_MISS to force declared snapshot miss"

ADAPTER_LOG="$TMP_ROOT/appfs-adapter.log"
ADAPTER_PID="$(start_appfs_v2_adapter "$ADAPTER_LOG" "$AGENTFS_BIN" "$TMP_ROOT" "aiim" 50 0)"
pass "adapter started"

line_count="$(wc -l < "$SNAPSHOT_HIT" | tr -d ' ')"
[ "$line_count" -ge 2 ] || fail "snapshot hit should expose multi-line JSONL"
first_line="$(head -n 1 "$SNAPSHOT_HIT")"
assert_json_expr "$first_line" 'isinstance(obj, dict) and "id" in obj and "text" in obj and "page" not in obj' "snapshot hit line is not a pure JSONL item"
grep -q "snapshot file" "$SNAPSHOT_HIT" || fail "snapshot hit file cannot be searched by grep"
pass "snapshot hit path returns pure JSONL and remains grep-friendly"

token="ct2-002-miss-$$"
wait_writable "$REFRESH_ACT" 10 || fail "snapshot refresh sink remained non-writable: $REFRESH_ACT"
printf '{"resource_path":"%s","client_token":"%s"}\n' "$SNAPSHOT_MISS_REL" "$token" >> "$REFRESH_ACT" || fail "failed to submit snapshot refresh for miss path"
wait_token_event "$token" "$EVENTS" 15 || fail "snapshot miss hook event did not arrive in time"

event_line="$(grep "$token" "$EVENTS" 2>/dev/null | tail -n 1 || true)"
[ -n "$event_line" ] || fail "missing snapshot miss event line"
assert_json_expr "$event_line" 'obj.get("type") == "action.failed"' "snapshot miss should emit action.failed"
code="$(printf '%s\n' "$event_line" | python3 -c 'import json,sys; print(json.loads(sys.stdin.read()).get("error", {}).get("code", ""))')"
[ "$code" = "CACHE_MISS_EXPAND_FAILED" ] || fail "expected CACHE_MISS_EXPAND_FAILED, got $code"
message="$(printf '%s\n' "$event_line" | python3 -c 'import json,sys; print(json.loads(sys.stdin.read()).get("error", {}).get("message", ""))')"
printf '%s\n' "$message" | grep -q "resource=$SNAPSHOT_MISS_REL" || fail "snapshot miss diagnostics missing resource path"
printf '%s\n' "$message" | grep -q "phase=expand_hook" || fail "snapshot miss diagnostics missing phase"
pass "snapshot miss enters unified expansion hook with diagnosable payload"

say "CT2-002 done"
