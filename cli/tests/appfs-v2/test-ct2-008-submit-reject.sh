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

event_count() {
    wc -l < "$EVENTS" | tr -d ' '
}

log_line_count() {
    if [ -f "$ADAPTER_LOG" ]; then
        wc -l < "$ADAPTER_LOG" | tr -d ' '
    else
        printf '0\n'
    fi
}

wait_log_pattern_after() {
    pattern="$1"
    start_line="$2"
    timeout="${3:-10}"
    i=0
    while [ "$i" -lt "$timeout" ]; do
        if [ -f "$ADAPTER_LOG" ] && tail -n +"$((start_line + 1))" "$ADAPTER_LOG" | grep -F -q "$pattern"; then
            return 0
        fi
        i=$((i + 1))
        sleep 1
    done
    return 1
}

assert_no_token_event() {
    token="$1"
    if grep -q "$token" "$EVENTS" 2>/dev/null; then
        fail "token=$token should not produce events for rejected payload"
    fi
}

banner "AppFS v2 CT2-008 Submit Reject Rules"
require_cmd python3
ensure_agentfs_bin "$CLI_DIR"

mkdir -p "$CLI_DIR/target"
TMP_ROOT="$(mktemp -d "$CLI_DIR/target/ct2-v2-008.XXXXXX")"
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

events_before="$(event_count)"
log_before="$(log_line_count)"
printf 'hello world\n' >> "$ACTION" || fail "failed to submit raw text payload"
wait_log_pattern_after "validation=INVALID_PAYLOAD" "$log_before" 10 || fail "raw text rejection log missing INVALID_PAYLOAD"
[ "$(event_count)" -eq "$events_before" ] || fail "raw text should not emit any action event"
pass "raw text is rejected at submit-time"

events_before="$(event_count)"
log_before="$(log_line_count)"
printf '["array","not","allowed"]\n' >> "$ACTION" || fail "failed to submit non-object json payload"
wait_log_pattern_after "validation=INVALID_ARGUMENT" "$log_before" 10 || fail "non-object JSON rejection log missing INVALID_ARGUMENT"
[ "$(event_count)" -eq "$events_before" ] || fail "non-object JSON should not emit any action event"
pass "non-object JSON is rejected"

token_mode="ct2-008-mode-$$"
events_before="$(event_count)"
log_before="$(log_line_count)"
printf '{"version":"2.0","mode":"text","client_token":"%s","payload":{"text":"x"}}\n' "$token_mode" >> "$ACTION" || fail "failed to submit mode-field payload"
wait_log_pattern_after "validation=INVALID_ARGUMENT" "$log_before" 10 || fail "mode field rejection log missing INVALID_ARGUMENT"
assert_no_token_event "$token_mode"
[ "$(event_count)" -eq "$events_before" ] || fail "mode field payload should not emit events"
pass "mode field is rejected deterministically"

events_before="$(event_count)"
log_before="$(log_line_count)"
printf '{"version":"2.0","payload":{"text":"x"}}\n' >> "$ACTION" || fail "failed to submit missing-client_token payload"
wait_log_pattern_after "validation=INVALID_ARGUMENT" "$log_before" 10 || fail "missing client_token rejection log missing INVALID_ARGUMENT"
[ "$(event_count)" -eq "$events_before" ] || fail "missing client_token should not emit events"
pass "missing required client_token is rejected"

token_payload="ct2-008-missing-payload-$$"
events_before="$(event_count)"
log_before="$(log_line_count)"
printf '{"version":"2.0","client_token":"%s"}\n' "$token_payload" >> "$ACTION" || fail "failed to submit missing-payload payload"
wait_log_pattern_after "validation=INVALID_ARGUMENT" "$log_before" 10 || fail "missing payload rejection log missing INVALID_ARGUMENT"
assert_no_token_event "$token_payload"
[ "$(event_count)" -eq "$events_before" ] || fail "missing payload should not emit events"
pass "missing required payload object is rejected"

token_ok="ct2-008-ok-$$"
printf '{"version":"2.0","client_token":"%s","payload":{"text":"ok"}}\n' "$token_ok" >> "$ACTION" || fail "failed to submit control valid payload"
wait_token_event "$token_ok" "$EVENTS" 15 || fail "valid payload did not emit token event"
line_ok="$(grep "$token_ok" "$EVENTS" 2>/dev/null | tail -n 1 || true)"
[ -n "$line_ok" ] || fail "missing token event for valid payload"
assert_json_expr "$line_ok" 'obj.get("type") == "action.completed"' "valid ActionLineV2 payload should still pass in strict mode"
pass "strict gate is active and allows valid ActionLineV2 payload"

say "CT2-008 done"
