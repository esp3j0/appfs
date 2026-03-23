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
    SNAPSHOT_FILE="$APP_DIR/chats/chat-oversize/messages.res.jsonl"
    REFRESH_ACT="$APP_DIR/_snapshot/refresh.act"
    EVENTS="$APP_DIR/_stream/events.evt.jsonl"
}

start_adapter() {
    delay_ms="${1:-0}"
    force_expand="${2:-0}"
    ADAPTER_LOG="$TMP_ROOT/appfs-adapter.log"
    ADAPTER_PID="$(start_appfs_v2_adapter "$ADAPTER_LOG" "$AGENTFS_BIN" "$TMP_ROOT" "aiim" 50 0 "$delay_ms" "" "$force_expand")"
}

assert_snapshot_too_large_event() {
    token="$1"
    line="$(grep "$token" "$EVENTS" 2>/dev/null | grep "\"type\":\"action.failed\"" | tail -n 1 || true)"
    [ -n "$line" ] || fail "missing action event for token=$token"
    assert_json_expr "$line" 'obj.get("type") == "action.failed"' "token=$token should emit action.failed"
    assert_json_expr "$line" 'obj.get("error", {}).get("code") == "SNAPSHOT_TOO_LARGE"' "token=$token should map to SNAPSHOT_TOO_LARGE"
    assert_json_expr "$line" 'obj.get("error", {}).get("size", 0) > obj.get("error", {}).get("max_size", 0)' "token=$token should expose error.size > error.max_size"
    assert_json_expr "$line" 'obj.get("error", {}).get("max_size") == 128' "token=$token should expose error.max_size=128 for chat-oversize"
}

assert_expand_failed_snapshot_too_large() {
    token="$1"
    line="$(grep "$token" "$EVENTS" 2>/dev/null | grep "\"type\":\"cache.expand\"" | tail -n 1 || true)"
    [ -n "$line" ] || fail "missing cache.expand event for token=$token"
    assert_json_expr "$line" 'obj.get("content", {}).get("phase") == "failed"' "token=$token cache.expand phase should be failed"
    assert_json_expr "$line" 'obj.get("content", {}).get("failure_reason") == "snapshot_too_large"' "token=$token cache.expand should report snapshot_too_large"
    assert_json_expr "$line" 'obj.get("content", {}).get("size", 0) > obj.get("content", {}).get("max_size", 0)' "token=$token cache.expand should expose size/max_size details"
}

banner "AppFS v2 CT2-005 Snapshot Too-Large Atomic Mapping"
require_cmd python3
require_cmd sha256sum
ensure_agentfs_bin

mkdir -p "$CLI_DIR/target"
TMP_ROOT="$(mktemp -d "$CLI_DIR/target/ct2-v2-005.XXXXXX")"

reload_fixture_app
assert_file "$SNAPSHOT_FILE"
assert_file "$REFRESH_ACT"
assert_file "$EVENTS"

rm -f "$SNAPSHOT_FILE"
pass "removed $SNAPSHOT_FILE to force cold expansion"

start_adapter 0 0
pass "adapter started for cold-miss oversize scenario"

wait_writable "$REFRESH_ACT" 10 || fail "snapshot refresh sink remained non-writable: $REFRESH_ACT"
token_cold="ct2-005-cold-$$"
printf '{"resource_path":"/chats/chat-oversize/messages.res.jsonl","client_token":"%s"}\n' "$token_cold" >> "$REFRESH_ACT" || fail "failed to submit cold-miss oversize refresh"
wait_token_type_event "$token_cold" "action.failed" "$EVENTS" 20 || fail "cold-miss oversize action.failed timeout"

assert_snapshot_too_large_event "$token_cold"
assert_expand_failed_snapshot_too_large "$token_cold"
[ ! -f "$SNAPSHOT_FILE" ] || fail "cold-miss oversize should not publish oversized snapshot file"
grep -F -q "[cache] snapshot_too_large resource=/chats/chat-oversize/messages.res.jsonl" "$ADAPTER_LOG" || fail "missing snapshot_too_large log anchor"
pass "cold startup oversize is mapped to SNAPSHOT_TOO_LARGE with details"

stop_adapter

reload_fixture_app
cat > "$SNAPSHOT_FILE" <<'EOF'
{"id":"keep-1","text":"old cache line"}
EOF
hash_before="$(sha256sum "$SNAPSHOT_FILE" | awk '{print $1}')"
size_before="$(wc -c < "$SNAPSHOT_FILE" | tr -d ' ')"
[ "$size_before" -lt 128 ] || fail "precondition failed: partial cache should be under max_size"
pass "seeded partial cache bytes=$size_before under max_size=128"

start_adapter 0 1
pass "adapter started for partial-cache continue-expand oversize scenario"

wait_writable "$REFRESH_ACT" 10 || fail "snapshot refresh sink remained non-writable in partial-cache scenario: $REFRESH_ACT"
token_partial="ct2-005-partial-$$"
printf '{"resource_path":"/chats/chat-oversize/messages.res.jsonl","client_token":"%s"}\n' "$token_partial" >> "$REFRESH_ACT" || fail "failed to submit partial-cache oversize refresh"
wait_token_type_event "$token_partial" "action.failed" "$EVENTS" 20 || fail "partial-cache oversize action.failed timeout"

assert_snapshot_too_large_event "$token_partial"
assert_expand_failed_snapshot_too_large "$token_partial"
hash_after="$(sha256sum "$SNAPSHOT_FILE" | awk '{print $1}')"
[ "$hash_before" = "$hash_after" ] || fail "partial cache file changed after oversize failure (expected atomic keep-old)"
grep -F -q "[cache] snapshot_too_large resource=/chats/chat-oversize/messages.res.jsonl" "$ADAPTER_LOG" || fail "missing snapshot_too_large log anchor in partial-cache scenario"
pass "partial-cache continue-expand oversize keeps old cache unchanged (atomicity)"

say "CT2-005 done"
