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

cleanup() {
    if [ -n "${ADAPTER_LOG:-}" ]; then
        persist_case_evidence "st4-001" "adapter.log" "$ADAPTER_LOG"
    fi
    stop_adapter_process "${ADAPTER_PID:-}" "${AGENTFS_BIN:-}" "${TMP_ROOT:-}"
    if [ -n "${TMP_ROOT:-}" ] && [ -d "$TMP_ROOT" ]; then
        rm -rf "$TMP_ROOT"
    fi
}
trap cleanup EXIT INT TERM

banner "AppFS ST4-001 Structure Initialize"
require_cmd python3
ensure_agentfs_bin "$CLI_DIR"

mkdir -p "$CLI_DIR/target"
TMP_ROOT="$(mktemp -d "$CLI_DIR/target/st4-001.XXXXXX")"
ADAPTER_LOG="$TMP_ROOT/appfs-adapter.log"

ADAPTER_PID="$(start_appfs_v2_adapter "$ADAPTER_LOG" "$AGENTFS_BIN" "$TMP_ROOT" "aiim" 50 0)"
pass "adapter started on empty root"

APP_DIR="$TMP_ROOT/aiim"
STATE="$APP_DIR/_meta/app-structure-sync.state.res.json"
MANIFEST="$APP_DIR/_meta/manifest.res.json"

wait_path_exists "$MANIFEST" 15 || fail "structure initialize did not materialize manifest"
wait_path_exists "$APP_DIR/_app/enter_scope.act" 15 || fail "missing enter_scope action after initialize"
wait_path_exists "$APP_DIR/_app/refresh_structure.act" 15 || fail "missing refresh_structure action after initialize"
wait_path_exists "$APP_DIR/chats/chat-001" 15 || fail "missing default chat scope after initialize"
wait_log_contains "[structure.sync] op=get_app_structure app=aiim" "$ADAPTER_LOG" 15 || fail "missing get_app_structure log evidence"
wait_log_contains "[structure.sync] result app=aiim changed=true revision=demo-structure-chat-001 active_scope=chat-001" "$ADAPTER_LOG" 15 || fail "missing structure sync result log evidence"

state_json="$(cat "$STATE")"
assert_json_expr "$state_json" "obj.get('active_scope') == 'chat-001'" "initial active_scope should be chat-001"
assert_json_expr "$state_json" "'chats/chat-001/messages.res.jsonl' in obj.get('owned_paths', [])" "initial owned paths should include chat-001 snapshot"

pass "empty root bootstrapped connector-owned structure"
pass "initial structure state revision recorded"
say "ST4-001 done"
