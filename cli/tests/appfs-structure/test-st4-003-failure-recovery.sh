#!/bin/sh
set -eu

DIR="$(dirname "$0")"
# shellcheck disable=SC1091
. "$DIR/lib.sh"

SCRIPT_DIR="$(CDPATH= cd -- "$DIR" && pwd)"
CLI_DIR="$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)"
AGENTFS_BIN="${AGENTFS_BIN:-$CLI_DIR/target/debug/agentfs}"

TMP_ROOT=""
ADAPTER_PID=""
ADAPTER_LOG=""
EVENTS=""

cleanup() {
    if [ -n "${ADAPTER_LOG:-}" ]; then
        persist_case_evidence "st4-003" "adapter.log" "$ADAPTER_LOG"
    fi
    if [ -n "${EVENTS:-}" ]; then
        persist_case_evidence "st4-003" "events.evt.jsonl" "$EVENTS"
    fi
    stop_adapter_process "${ADAPTER_PID:-}" "${AGENTFS_BIN:-}" "${TMP_ROOT:-}"
    if [ -n "${TMP_ROOT:-}" ] && [ -d "$TMP_ROOT" ]; then
        rm -rf "$TMP_ROOT"
    fi
}
trap cleanup EXIT INT TERM

banner "AppFS ST4-003 Structure Refresh Failure Recovery"
require_cmd python3
ensure_agentfs_bin "$CLI_DIR"

mkdir -p "$CLI_DIR/target"
TMP_ROOT="$(mktemp -d "$CLI_DIR/target/st4-003.XXXXXX")"
ADAPTER_LOG="$TMP_ROOT/appfs-adapter.log"

ADAPTER_PID="$(start_appfs_connector_adapter "$ADAPTER_LOG" "$AGENTFS_BIN" "$TMP_ROOT" "aiim" 50 0)"
pass "adapter started on empty root"

APP_DIR="$TMP_ROOT/aiim"
EVENTS="$APP_DIR/_stream/events.evt.jsonl"
ENTER_SCOPE_ACTION="$APP_DIR/_app/enter_scope.act"
REFRESH_ACTION="$APP_DIR/_app/refresh_structure.act"

wait_path_exists "$ENTER_SCOPE_ACTION" 15 || fail "enter_scope action sink not created"
wait_path_exists "$REFRESH_ACTION" 15 || fail "refresh_structure action sink not created"

bad_token="st4-bad-$$"
printf '{"target_scope":"missing-scope","client_token":"%s"}\n' "$bad_token" >>"$ENTER_SCOPE_ACTION" || fail "failed to submit invalid enter_scope action"
wait_token_type_count "$bad_token" "action.failed" 1 "$EVENTS" 15 || fail "invalid enter_scope did not fail"
assert_token_event_type "$bad_token" "$EVENTS" "action.failed"
assert_token_error_code "$bad_token" "$EVENTS" "STRUCTURE_SYNC_FAILED"

[ -d "$APP_DIR/chats/chat-001" ] || fail "failed refresh should preserve existing chat-001 scope"
[ ! -e "$APP_DIR/chats/chat-long" ] || fail "failed refresh must not publish chat-long scope"

recover_token="st4-recover-$$"
printf '{"target_scope":"chat-long","client_token":"%s"}\n' "$recover_token" >>"$REFRESH_ACTION" || fail "failed to submit recovery refresh action"
wait_token_type_count "$recover_token" "action.completed" 1 "$EVENTS" 15 || fail "recovery refresh did not complete"
assert_token_event_type "$recover_token" "$EVENTS" "action.completed"
wait_log_contains "[structure.sync] op=refresh_app_structure app=aiim reason=refresh target_scope=chat-long" "$ADAPTER_LOG" 15 || fail "missing refresh recovery log evidence"

[ -d "$APP_DIR/chats/chat-long" ] || fail "recovery refresh should publish chat-long scope"
[ ! -e "$APP_DIR/chats/chat-001" ] || fail "recovery refresh should prune stale chat-001 scope"

pass "failed refresh left previous structure intact"
pass "subsequent refresh recovered and published new scope"
say "ST4-003 done"
