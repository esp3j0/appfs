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

assert_token_completed() {
    token="$1"
    line="$(grep "$token" "$EVENTS" 2>/dev/null | tail -n 1 || true)"
    [ -n "$line" ] || fail "missing event line for token=$token"
    assert_json_expr "$line" 'obj.get("type") == "action.completed"' "token $token did not emit action.completed"
    actual_token="$(printf '%s\n' "$line" | python3 -c 'import json,sys; print(json.loads(sys.stdin.read()).get("client_token", ""))')"
    [ "$actual_token" = "$token" ] || fail "expected client_token=$token, got $actual_token"
}

banner "AppFS v2 CT2-007 ActionLineV2 Parse"
require_cmd python3
ensure_agentfs_bin "$CLI_DIR"

mkdir -p "$CLI_DIR/target"
TMP_ROOT="$(mktemp -d "$CLI_DIR/target/ct2-v2-007.XXXXXX")"
cp -R "$REPO_DIR/examples/appfs/aiim" "$TMP_ROOT/"

APP_DIR="$TMP_ROOT/aiim"
ACTION="$APP_DIR/contacts/zhangsan/send_message.act"
EVENTS="$APP_DIR/_stream/events.evt.jsonl"

assert_file "$ACTION"
assert_file "$EVENTS"

ADAPTER_LOG="$TMP_ROOT/appfs-adapter.log"
ADAPTER_PID="$(start_appfs_v2_adapter "$ADAPTER_LOG" "$AGENTFS_BIN" "$TMP_ROOT" "aiim" 50 1)"
pass "adapter started with APPFS_V2_ACTIONLINE_STRICT=1"

wait_writable "$ACTION" 10 || fail "action sink remained non-writable: $ACTION"

token_1="ct2-007-1-$$"
printf '{"version":"2.0","client_token":"%s","payload":{"text":"hello"}}\n' "$token_1" >> "$ACTION" || fail "failed to submit actionline v2 case 1"
wait_token_event "$token_1" "$EVENTS" 15 || fail "case 1 token event timeout"
assert_token_completed "$token_1"
pass "single-line ActionLineV2 request parsed"

token_2="ct2-007-2-$$"
printf '{"version":"2.0","client_token":"%s","payload":{"text":"hello\\nworld\\t!"}}\n' "$token_2" >> "$ACTION" || fail "failed to submit actionline v2 case 2"
wait_token_event "$token_2" "$EVENTS" 15 || fail "case 2 token event timeout"
assert_token_completed "$token_2"
pass "escaped special characters parsed through payload"

token_3="ct2-007-3-$$"
token_4="ct2-007-4-$$"
printf '{"version":"2.0","client_token":"%s","payload":{"text":"a"}}\n{"version":"2.0","client_token":"%s","payload":{"text":"b"}}\n' "$token_3" "$token_4" >> "$ACTION" || fail "failed to submit multi-line actionline v2 payload"
wait_token_event "$token_3" "$EVENTS" 15 || fail "case 3 token event timeout"
wait_token_event "$token_4" "$EVENTS" 15 || fail "case 4 token event timeout"
assert_token_completed "$token_3"
assert_token_completed "$token_4"
pass "multi-line JSONL action submissions parsed independently"

say "CT2-007 done"
