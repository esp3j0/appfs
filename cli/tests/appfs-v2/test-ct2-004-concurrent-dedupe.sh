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

start_adapter_with_delay() {
    delay_ms="${1:-0}"
    ADAPTER_LOG="$TMP_ROOT/appfs-adapter.log"
    runtime_root="$TMP_ROOT"
    case "$AGENTFS_BIN" in
        *.exe)
            win_bin="$AGENTFS_BIN"
            if command -v wslpath >/dev/null 2>&1; then
                runtime_root="$(wslpath -w "$TMP_ROOT")"
                win_bin="$(wslpath -w "$AGENTFS_BIN")"
            fi
            cmd.exe /C "set APPFS_V2_SNAPSHOT_EXPAND_DELAY_MS=$delay_ms&& $win_bin serve appfs --root $runtime_root --app-id aiim --poll-ms 50" >"$ADAPTER_LOG" 2>&1 &
            ;;
        *)
            APPFS_V2_SNAPSHOT_EXPAND_DELAY_MS="$delay_ms" "$AGENTFS_BIN" serve appfs --root "$runtime_root" --app-id aiim --poll-ms 50 >"$ADAPTER_LOG" 2>&1 &
            ;;
    esac
    ADAPTER_PID=$!
    sleep 1
    if ! kill -0 "$ADAPTER_PID" 2>/dev/null; then
        tail -n 120 "$ADAPTER_LOG" 2>/dev/null || true
        fail "appfs adapter failed to start"
    fi
}

banner "AppFS v2 CT2-004 Concurrent Cold-Miss Coalescing"
require_cmd python3
ensure_agentfs_bin

mkdir -p "$CLI_DIR/target"
TMP_ROOT="$(mktemp -d "$CLI_DIR/target/ct2-v2-004.XXXXXX")"
cp -R "$REPO_DIR/examples/appfs/aiim" "$TMP_ROOT/"

APP_DIR="$TMP_ROOT/aiim"
SNAPSHOT_FILE="$APP_DIR/chats/chat-001/messages.res.jsonl"
REFRESH_ACT="$APP_DIR/_snapshot/refresh.act"
EVENTS="$APP_DIR/_stream/events.evt.jsonl"

assert_file "$REFRESH_ACT"
assert_file "$EVENTS"
assert_file "$SNAPSHOT_FILE"
rm -f "$SNAPSHOT_FILE"
pass "removed $SNAPSHOT_FILE to force cold miss"

token_a="ct2-004-a-$$"
token_b="ct2-004-b-$$"
token_c="ct2-004-c-$$"
printf '{"resource_path":"/chats/chat-001/messages.res.jsonl","client_token":"%s"}\n' "$token_a" >> "$REFRESH_ACT"
printf '{"resource_path":"/chats/chat-001/messages.res.jsonl","client_token":"%s"}\n' "$token_b" >> "$REFRESH_ACT"
printf '{"resource_path":"/chats/chat-001/messages.res.jsonl","client_token":"%s"}\n' "$token_c" >> "$REFRESH_ACT"
pass "queued three concurrent refresh requests for same cold snapshot resource"

start_adapter_with_delay 200
pass "adapter started"

wait_token_event "$token_a" "$EVENTS" 20 || fail "token_a event timeout"
wait_token_event "$token_b" "$EVENTS" 20 || fail "token_b event timeout"
wait_token_event "$token_c" "$EVENTS" 20 || fail "token_c event timeout"

event_a="$(grep "$token_a" "$EVENTS" 2>/dev/null | tail -n 1 || true)"
event_b="$(grep "$token_b" "$EVENTS" 2>/dev/null | tail -n 1 || true)"
event_c="$(grep "$token_c" "$EVENTS" 2>/dev/null | tail -n 1 || true)"
[ -n "$event_a" ] || fail "missing event for token_a"
[ -n "$event_b" ] || fail "missing event for token_b"
[ -n "$event_c" ] || fail "missing event for token_c"

assert_json_expr "$event_a" 'obj.get("type") == "action.completed"' "token_a should complete"
assert_json_expr "$event_b" 'obj.get("type") == "action.completed"' "token_b should complete"
assert_json_expr "$event_c" 'obj.get("type") == "action.completed"' "token_c should complete"

assert_json_expr "$event_a" 'obj.get("content", {}).get("cached") is False' "leader request should materialize from upstream"
assert_json_expr "$event_a" 'obj.get("content", {}).get("coalesced") is False' "leader request should not be marked coalesced"
assert_json_expr "$event_b" 'obj.get("content", {}).get("cached") is True' "follower request B should reuse cache"
assert_json_expr "$event_b" 'obj.get("content", {}).get("coalesced") is True' "follower request B should be marked coalesced"
assert_json_expr "$event_c" 'obj.get("content", {}).get("cached") is True' "follower request C should reuse cache"
assert_json_expr "$event_c" 'obj.get("content", {}).get("coalesced") is True' "follower request C should be marked coalesced"

assert_file "$SNAPSHOT_FILE"
line_count="$(wc -l < "$SNAPSHOT_FILE" | tr -d ' ')"
[ "$line_count" -eq 100 ] || fail "materialized snapshot should contain 100 JSONL lines, got $line_count"
first_line="$(head -n 1 "$SNAPSHOT_FILE")"
assert_json_expr "$first_line" 'isinstance(obj, dict) and "id" in obj and "text" in obj and "page" not in obj' "materialized snapshot must be pure JSONL"

expand_event_count="$(grep -c "\"type\":\"cache.expand\"" "$EVENTS" 2>/dev/null || true)"
[ "$expand_event_count" -eq 1 ] || fail "expected exactly one cache.expand event, got $expand_event_count"
expand_event_line="$(grep "\"type\":\"cache.expand\"" "$EVENTS" 2>/dev/null | tail -n 1 || true)"
[ -n "$expand_event_line" ] || fail "missing cache.expand event line"
assert_json_expr "$expand_event_line" 'obj.get("content", {}).get("phase") == "completed"' "cache.expand event should be completed"
assert_json_expr "$expand_event_line" 'obj.get("content", {}).get("upstream_calls") == 1' "cache.expand should expose upstream_calls=1"

fetch_count="$(grep -F -c "[cache.expand] fetch_snapshot_chunk resource=/chats/chat-001/messages.res.jsonl" "$ADAPTER_LOG" 2>/dev/null || true)"
[ "$fetch_count" -eq 1 ] || fail "expected exactly one upstream fetch_snapshot_chunk call, got $fetch_count"
coalesced_count="$(grep -F -c "[cache] coalesced concurrent miss resource=/chats/chat-001/messages.res.jsonl" "$ADAPTER_LOG" 2>/dev/null || true)"
[ "$coalesced_count" -ge 2 ] || fail "expected coalesced concurrent miss log for followers, got count=$coalesced_count"

pass "single-flight/coalescing verified: upstream_calls=1, cache.expand=1, followers reused leader result"
say "CT2-004 done"
