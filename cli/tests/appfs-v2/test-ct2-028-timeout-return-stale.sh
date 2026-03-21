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

wait_token_type_event() {
    token="$1"
    event_type="$2"
    file="$3"
    timeout="${4:-20}"
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
    MANIFEST="$APP_DIR/_meta/manifest.res.json"
    SNAPSHOT_FILE="$APP_DIR/chats/chat-001/messages.res.jsonl"
    REFRESH_ACT="$APP_DIR/_snapshot/refresh.act"
    EVENTS="$APP_DIR/_stream/events.evt.jsonl"
}

patch_manifest_timeout_return_stale() {
    python3 - "$MANIFEST" <<'PY'
import json
import sys

manifest_path = sys.argv[1]
with open(manifest_path, "r", encoding="utf-8") as f:
    doc = json.load(f)

node = doc["nodes"]["chats/{chat_id}/messages.res.jsonl"]
snapshot = dict(node.get("snapshot") or {})
snapshot["read_through_timeout_ms"] = 50
snapshot["on_timeout"] = "return_stale"
node["snapshot"] = snapshot

with open(manifest_path, "w", encoding="utf-8") as f:
    json.dump(doc, f, ensure_ascii=False, indent=2)
    f.write("\n")
PY
}

start_adapter() {
    expand_delay_ms="${1:-0}"
    force_expand="${2:-0}"
    ADAPTER_LOG="$TMP_ROOT/appfs-adapter.log"
    runtime_root="$TMP_ROOT"
    case "$AGENTFS_BIN" in
        *.exe)
            win_bin="$AGENTFS_BIN"
            if command -v wslpath >/dev/null 2>&1; then
                runtime_root="$(wslpath -w "$TMP_ROOT")"
                win_bin="$(wslpath -w "$AGENTFS_BIN")"
            fi
            cmd.exe /C "set APPFS_V2_SNAPSHOT_EXPAND_DELAY_MS=$expand_delay_ms&& set APPFS_V2_SNAPSHOT_REFRESH_FORCE_EXPAND=$force_expand&& $win_bin serve appfs --root $runtime_root --app-id aiim --poll-ms 50" >"$ADAPTER_LOG" 2>&1 &
            ;;
        *)
            APPFS_V2_SNAPSHOT_EXPAND_DELAY_MS="$expand_delay_ms" APPFS_V2_SNAPSHOT_REFRESH_FORCE_EXPAND="$force_expand" "$AGENTFS_BIN" serve appfs --root "$runtime_root" --app-id aiim --poll-ms 50 >"$ADAPTER_LOG" 2>&1 &
            ;;
    esac
    ADAPTER_PID=$!
    sleep 1
    if ! kill -0 "$ADAPTER_PID" 2>/dev/null; then
        tail -n 120 "$ADAPTER_LOG" 2>/dev/null || true
        fail "appfs adapter failed to start"
    fi
}

banner "AppFS v2 CT2-028 Timeout Return-Stale Fallback"
require_cmd python3
require_cmd sha256sum
ensure_agentfs_bin

mkdir -p "$CLI_DIR/target"
TMP_ROOT="$(mktemp -d "$CLI_DIR/target/ct2-v2-028.XXXXXX")"

reload_fixture_app
patch_manifest_timeout_return_stale
assert_file "$MANIFEST"
assert_file "$SNAPSHOT_FILE"
assert_file "$REFRESH_ACT"
assert_file "$EVENTS"

hash_before="$(sha256sum "$SNAPSHOT_FILE" | awk '{print $1}')"
start_adapter 200 1
pass "adapter started for timeout-return_stale with stale cache available"

wait_writable "$REFRESH_ACT" 10 || fail "snapshot refresh sink remained non-writable: $REFRESH_ACT"
token_stale="ct2-028-stale-$$"
printf '{"resource_path":"/chats/chat-001/messages.res.jsonl","client_token":"%s"}\n' "$token_stale" >> "$REFRESH_ACT" || fail "failed to submit refresh for stale-available scenario"
wait_token_event "$token_stale" "$EVENTS" 20 || fail "stale-available token event timeout"
wait_token_type_event "$token_stale" "action.completed" "$EVENTS" 20 || fail "stale-available action.completed timeout"

event_stale="$(grep "$token_stale" "$EVENTS" 2>/dev/null | grep "\"type\":\"action.completed\"" | tail -n 1 || true)"
[ -n "$event_stale" ] || fail "missing stale-available action event"
assert_json_expr "$event_stale" 'obj.get("type") == "action.completed"' "stale-available should emit action.completed"
assert_json_expr "$event_stale" 'obj.get("content", {}).get("stale") is True' "stale-available should mark stale=true"
assert_json_expr "$event_stale" 'obj.get("content", {}).get("degrade_reason") == "timeout_return_stale"' "stale-available should expose degrade_reason"
assert_json_expr "$event_stale" 'obj.get("content", {}).get("cached") is True' "stale-available should reuse cached snapshot"
if grep "$token_stale" "$EVENTS" 2>/dev/null | grep -q "\"type\":\"action.failed\""; then
    fail "stale-available must not emit action.failed"
fi

expand_timeout_line="$(grep "$token_stale" "$EVENTS" 2>/dev/null | grep "\"type\":\"cache.expand\"" | tail -n 1 || true)"
[ -n "$expand_timeout_line" ] || fail "missing cache.expand timeout evidence for stale-available scenario"
assert_json_expr "$expand_timeout_line" 'obj.get("content", {}).get("phase") == "timeout"' "stale-available cache.expand phase should be timeout"
assert_json_expr "$expand_timeout_line" 'obj.get("content", {}).get("fallback") == "return_stale"' "stale-available cache.expand should show return_stale fallback"

stale_line="$(grep "$token_stale" "$EVENTS" 2>/dev/null | grep "\"type\":\"cache.stale\"" | tail -n 1 || true)"
[ -n "$stale_line" ] || fail "missing cache.stale evidence for stale-available scenario"
assert_json_expr "$stale_line" 'obj.get("content", {}).get("reason") == "timeout"' "cache.stale reason should be timeout"

hash_after="$(sha256sum "$SNAPSHOT_FILE" | awk '{print $1}')"
[ "$hash_before" = "$hash_after" ] || fail "stale-available fallback should keep old cache bytes unchanged"
grep -F -q "[cache] timeout_return_stale resource=/chats/chat-001/messages.res.jsonl" "$ADAPTER_LOG" || fail "missing timeout_return_stale log anchor"
pass "timeout-return_stale with stale cache returns degraded success and diagnostic evidence"

stop_adapter

reload_fixture_app
patch_manifest_timeout_return_stale
rm -f "$SNAPSHOT_FILE"
pass "prepared timeout-return_stale scenario without stale cache"

start_adapter 200 0
pass "adapter started for timeout-return_stale without stale cache"

wait_writable "$REFRESH_ACT" 10 || fail "snapshot refresh sink remained non-writable in no-stale scenario: $REFRESH_ACT"
token_no_stale="ct2-028-no-stale-$$"
printf '{"resource_path":"/chats/chat-001/messages.res.jsonl","client_token":"%s"}\n' "$token_no_stale" >> "$REFRESH_ACT" || fail "failed to submit refresh for no-stale scenario"
wait_token_event "$token_no_stale" "$EVENTS" 20 || fail "no-stale token event timeout"
wait_token_type_event "$token_no_stale" "action.failed" "$EVENTS" 20 || fail "no-stale action.failed timeout"

event_fail="$(grep "$token_no_stale" "$EVENTS" 2>/dev/null | grep "\"type\":\"action.failed\"" | tail -n 1 || true)"
[ -n "$event_fail" ] || fail "missing no-stale action event"
assert_json_expr "$event_fail" 'obj.get("type") == "action.failed"' "no-stale should emit action.failed"
assert_json_expr "$event_fail" 'obj.get("error", {}).get("code") == "CACHE_MISS_EXPAND_FAILED"' "no-stale should keep deterministic CACHE_MISS_EXPAND_FAILED"

expand_fail_line="$(grep "$token_no_stale" "$EVENTS" 2>/dev/null | grep "\"type\":\"cache.expand\"" | tail -n 1 || true)"
[ -n "$expand_fail_line" ] || fail "missing cache.expand evidence for no-stale scenario"
assert_json_expr "$expand_fail_line" 'obj.get("content", {}).get("phase") == "failed"' "no-stale cache.expand phase should be failed"
assert_json_expr "$expand_fail_line" 'obj.get("content", {}).get("failure_reason") == "timeout"' "no-stale cache.expand reason should be timeout"
assert_json_expr "$expand_fail_line" 'obj.get("content", {}).get("on_timeout") == "return_stale"' "no-stale cache.expand should expose on_timeout=return_stale"

if grep "$token_no_stale" "$EVENTS" 2>/dev/null | grep -q "\"type\":\"cache.stale\""; then
    fail "no-stale scenario must not emit cache.stale"
fi
[ ! -f "$SNAPSHOT_FILE" ] || fail "no-stale timeout should not publish snapshot file"
grep -F -q "[cache] timeout_return_stale unavailable resource=/chats/chat-001/messages.res.jsonl reason=no_stale_cache" "$ADAPTER_LOG" || fail "missing no-stale timeout_return_stale unavailable log"
pass "timeout-return_stale without stale cache remains deterministic failure"

say "CT2-028 done"
