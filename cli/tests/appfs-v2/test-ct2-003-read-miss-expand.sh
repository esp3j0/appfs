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
MANIFEST=""
SNAPSHOT_FILE=""
REFRESH_ACT=""
EVENTS=""

cleanup() {
    if [ -n "${ADAPTER_LOG:-}" ]; then
        persist_case_evidence "ct2-003" "adapter.final.log" "$ADAPTER_LOG"
    fi
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

wait_token_type_event() {
    token="$1"
    event_type="$2"
    file="$3"
    timeout="${4:-15}"
    i=0
    while [ "$i" -lt "$timeout" ]; do
        if grep "$token" "$file" 2>/dev/null | grep -q "\"type\":\"$event_type\""; then
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

reload_fixture_app() {
    rm -rf "$TMP_ROOT/aiim"
    cp -R "$REPO_DIR/examples/appfs/aiim" "$TMP_ROOT/"
    APP_DIR="$TMP_ROOT/aiim"
    MANIFEST="$APP_DIR/_meta/manifest.res.json"
    SNAPSHOT_FILE="$APP_DIR/chats/chat-001/messages.res.jsonl"
    REFRESH_ACT="$APP_DIR/_snapshot/refresh.act"
    EVENTS="$APP_DIR/_stream/events.evt.jsonl"
}

start_adapter_with_delay() {
    delay_ms="${1:-0}"
    ADAPTER_LOG="$TMP_ROOT/appfs-adapter.log"
    ADAPTER_PID="$(start_appfs_v2_adapter "$ADAPTER_LOG" "$AGENTFS_BIN" "$TMP_ROOT" "aiim" 50 0 "$delay_ms" "" "")"
}

patch_manifest_timeout_fail() {
    python3 - "$MANIFEST" <<'PY'
import json
import sys

manifest_path = sys.argv[1]
with open(manifest_path, "r", encoding="utf-8") as f:
    doc = json.load(f)

node = doc["nodes"]["chats/{chat_id}/messages.res.jsonl"]
snapshot = dict(node.get("snapshot") or {})
snapshot["read_through_timeout_ms"] = 50
snapshot["on_timeout"] = "fail"
node["snapshot"] = snapshot

with open(manifest_path, "w", encoding="utf-8") as f:
    json.dump(doc, f, ensure_ascii=False, indent=2)
    f.write("\n")
PY
}

banner "AppFS v2 CT2-003 Read Miss Expand"
require_cmd python3
ensure_agentfs_bin

mkdir -p "$CLI_DIR/target"
TMP_ROOT="$(mktemp -d "$CLI_DIR/target/ct2-v2-003.XXXXXX")"

reload_fixture_app
assert_file "$SNAPSHOT_FILE"
assert_file "$REFRESH_ACT"
assert_file "$EVENTS"

rm -f "$SNAPSHOT_FILE"
pass "removed $SNAPSHOT_FILE to force cold snapshot miss"

start_adapter_with_delay 0
pass "adapter started for expand-success scenario"

wait_writable "$REFRESH_ACT" 10 || fail "snapshot refresh sink remained non-writable: $REFRESH_ACT"
token_success="ct2-003-expand-$$"
printf '{"resource_path":"/chats/chat-001/messages.res.jsonl","client_token":"%s"}\n' "$token_success" >> "$REFRESH_ACT" || fail "failed to submit snapshot refresh for expand-success scenario"
wait_token_event "$token_success" "$EVENTS" 20 || fail "expand-success token event timeout"

event_ok="$(grep "$token_success" "$EVENTS" 2>/dev/null | tail -n 1 || true)"
[ -n "$event_ok" ] || fail "missing action event for expand-success scenario"
assert_json_expr "$event_ok" 'obj.get("type") == "action.completed"' "expand-success should emit action.completed"

assert_file "$SNAPSHOT_FILE"
line_count="$(wc -l < "$SNAPSHOT_FILE" | tr -d ' ')"
[ "$line_count" -eq 100 ] || fail "expanded snapshot should materialize 100 JSONL lines, got $line_count"
first_line="$(head -n 1 "$SNAPSHOT_FILE")"
assert_json_expr "$first_line" 'isinstance(obj, dict) and "id" in obj and "text" in obj and "page" not in obj' "expanded snapshot line is not pure JSONL item"

expand_done_line="$(grep "\"type\":\"cache.expand\"" "$EVENTS" 2>/dev/null | grep "$token_success" | tail -n 1 || true)"
[ -n "$expand_done_line" ] || fail "missing cache.expand event for expand-success scenario"
assert_json_expr "$expand_done_line" 'obj.get("content", {}).get("phase") == "completed"' "cache.expand phase should be completed"
assert_json_expr "$expand_done_line" 'obj.get("content", {}).get("path") == "/chats/chat-001/messages.res.jsonl"' "cache.expand path mismatch"

grep -F -q "[cache] state resource=/chats/chat-001/messages.res.jsonl from=cold to=warming" "$ADAPTER_LOG" || fail "missing cold->warming state transition log"
grep -F -q "[cache] state resource=/chats/chat-001/messages.res.jsonl from=warming to=hot" "$ADAPTER_LOG" || fail "missing warming->hot state transition log"
grep -F -q "[cache] expanded resource=/chats/chat-001/messages.res.jsonl bytes=" "$ADAPTER_LOG" || fail "missing expansion completion log"
persist_case_evidence "ct2-003" "adapter.log" "$ADAPTER_LOG"
pass "cold -> warming -> hot with cache.expand evidence is materialized"

stop_adapter

reload_fixture_app
patch_manifest_timeout_fail
rm -f "$SNAPSHOT_FILE"
pass "prepared timeout-fail scenario with read_through_timeout_ms=50 and on_timeout=fail"

start_adapter_with_delay 200
pass "adapter started for timeout-fail scenario"

wait_writable "$REFRESH_ACT" 10 || fail "snapshot refresh sink remained non-writable in timeout scenario: $REFRESH_ACT"
token_timeout="ct2-003-timeout-$$"
printf '{"resource_path":"/chats/chat-001/messages.res.jsonl","client_token":"%s"}\n' "$token_timeout" >> "$REFRESH_ACT" || fail "failed to submit snapshot refresh for timeout-fail scenario"
wait_token_type_event "$token_timeout" "action.failed" "$EVENTS" 20 || fail "timeout-fail action.failed timeout"

event_fail="$(grep "$token_timeout" "$EVENTS" 2>/dev/null | grep "\"type\":\"action.failed\"" | tail -n 1 || true)"
[ -n "$event_fail" ] || fail "missing action event for timeout-fail scenario"
assert_json_expr "$event_fail" 'obj.get("type") == "action.failed"' "timeout-fail should emit action.failed"
assert_json_expr "$event_fail" 'obj.get("error", {}).get("code") == "CACHE_MISS_EXPAND_FAILED"' "timeout-fail should map to CACHE_MISS_EXPAND_FAILED"

expand_fail_line="$(grep "\"type\":\"cache.expand\"" "$EVENTS" 2>/dev/null | grep "$token_timeout" | tail -n 1 || true)"
[ -n "$expand_fail_line" ] || fail "missing cache.expand event for timeout-fail scenario"
assert_json_expr "$expand_fail_line" 'obj.get("content", {}).get("phase") == "failed"' "timeout-fail cache.expand phase should be failed"
assert_json_expr "$expand_fail_line" 'obj.get("content", {}).get("failure_reason") == "timeout"' "timeout-fail cache.expand reason should be timeout"

grep -F -q "[cache] expand failed resource=/chats/chat-001/messages.res.jsonl phase=timeout" "$ADAPTER_LOG" || fail "missing timeout-fail expand log"
pass "on_timeout=fail returns CACHE_MISS_EXPAND_FAILED with cache.expand failure evidence"

say "CT2-003 done"
