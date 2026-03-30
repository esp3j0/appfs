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
        persist_case_evidence "st4-002" "adapter.log" "$ADAPTER_LOG"
    fi
    if [ -n "${EVENTS:-}" ]; then
        persist_case_evidence "st4-002" "events.evt.jsonl" "$EVENTS"
    fi
    stop_adapter_process "${ADAPTER_PID:-}" "${AGENTFS_BIN:-}" "${TMP_ROOT:-}"
    if [ -n "${TMP_ROOT:-}" ] && [ -d "$TMP_ROOT" ]; then
        rm -rf "$TMP_ROOT"
    fi
}
trap cleanup EXIT INT TERM

banner "AppFS ST4-002 Enter Scope Refresh"
require_cmd python3
ensure_agentfs_bin "$CLI_DIR"

mkdir -p "$CLI_DIR/target"
TMP_ROOT="$(mktemp -d "$CLI_DIR/target/st4-002.XXXXXX")"
ADAPTER_LOG="$TMP_ROOT/appfs-adapter.log"

ADAPTER_PID="$(start_appfs_connector_adapter "$ADAPTER_LOG" "$AGENTFS_BIN" "$TMP_ROOT" "aiim" 50 0)"
pass "adapter started on empty root"

APP_DIR="$TMP_ROOT/aiim"
EVENTS="$APP_DIR/_stream/events.evt.jsonl"
ACTION="$APP_DIR/_app/enter_scope.act"
STATE="$APP_DIR/_meta/app-structure-sync.state.res.json"
RUNTIME_OWNED="$APP_DIR/_stream/custom-runtime.log"

wait_path_exists "$ACTION" 15 || fail "enter_scope action sink not created"
wait_path_exists "$EVENTS" 15 || fail "events stream not created"
printf 'keep\n' >"$RUNTIME_OWNED"

token="st4-enter-$$"
printf '{"target_scope":"chat-long","client_token":"%s"}\n' "$token" >>"$ACTION" || fail "failed to submit enter_scope action"
wait_token_type_count "$token" "action.completed" 1 "$EVENTS" 15 || fail "enter_scope did not complete"
assert_token_event_type "$token" "$EVENTS" "action.completed"

wait_path_exists "$APP_DIR/chats/chat-long" 15 || fail "chat-long scope was not materialized"
[ ! -e "$APP_DIR/chats/chat-001" ] || fail "chat-001 scope should be pruned after scope switch"
[ -f "$RUNTIME_OWNED" ] || fail "runtime-owned _stream file must survive structure refresh"
wait_log_contains "[structure.sync] op=refresh_app_structure app=aiim reason=enter_scope target_scope=chat-long" "$ADAPTER_LOG" 15 || fail "missing refresh_app_structure enter_scope log evidence"

state_json="$(cat "$STATE")"
assert_json_expr "$state_json" "obj.get('active_scope') == 'chat-long'" "active_scope should switch to chat-long"
assert_json_expr "$state_json" "'chats/chat-long/messages.res.jsonl' in obj.get('owned_paths', [])" "owned paths should include chat-long snapshot"
assert_json_expr "$state_json" "'chats/chat-001/messages.res.jsonl' not in obj.get('owned_paths', [])" "owned paths should no longer include chat-001 snapshot"

pass "enter_scope reconciled connector-owned nodes"
pass "runtime-owned paths remained protected"
say "ST4-002 done"
