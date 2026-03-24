#!/bin/sh
set -eu

DIR="$(dirname "$0")"
# shellcheck disable=SC1091
. "$DIR/lib.sh"

SCRIPT_DIR="$(CDPATH= cd -- "$DIR" && pwd)"
CLI_DIR="$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)"
REPO_DIR="$(CDPATH= cd -- "$CLI_DIR/.." && pwd)"
AGENTFS_BIN="${AGENTFS_BIN:-}"

TMP_ROOT=""
ADAPTER_PID=""
ADAPTER_LOG=""

APP_DIR=""
MANIFEST=""
SNAPSHOT_FILE=""

cleanup() {
    stop_adapter_process "${ADAPTER_PID:-}" "${AGENTFS_BIN:-}" "${TMP_ROOT:-}"
    if [ -n "${TMP_ROOT:-}" ] && [ -d "$TMP_ROOT" ]; then
        rm -rf "$TMP_ROOT"
    fi
}
trap cleanup EXIT INT TERM

stop_adapter() {
    stop_adapter_process "${ADAPTER_PID:-}" "${AGENTFS_BIN:-}" "${TMP_ROOT:-}"
    ADAPTER_PID=""
}

wait_log_contains() {
    needle="$1"
    file="$2"
    timeout="${3:-10}"
    i=0
    while [ "$i" -lt "$timeout" ]; do
        if grep -F -q "$needle" "$file" 2>/dev/null; then
            return 0
        fi
        i=$((i + 1))
        sleep 1
    done
    return 1
}

reload_fixture_app() {
    rm -rf "$TMP_ROOT/aiim"
    cp -R "$REPO_DIR/examples/appfs/aiim" "$TMP_ROOT/"
    APP_DIR="$TMP_ROOT/aiim"
    MANIFEST="$APP_DIR/_meta/manifest.res.json"
    SNAPSHOT_FILE="$APP_DIR/chats/chat-001/messages.res.jsonl"
}

patch_manifest_for_prewarm() {
    timeout_ms="$1"
    python3 - "$MANIFEST" "$timeout_ms" <<'PY'
import copy
import json
import sys

manifest_path = sys.argv[1]
timeout_ms = int(sys.argv[2])

with open(manifest_path, "r", encoding="utf-8") as f:
    doc = json.load(f)

nodes = doc["nodes"]
template_key = "chats/{chat_id}/messages.res.jsonl"
target_key = "chats/chat-001/messages.res.jsonl"

template_node = copy.deepcopy(nodes[template_key])
template_snapshot = dict(template_node.get("snapshot") or {})
template_snapshot["prewarm"] = False
template_node["snapshot"] = template_snapshot
nodes[template_key] = template_node

if "chats/chat-oversize/messages.res.jsonl" in nodes:
    oversize_node = copy.deepcopy(nodes["chats/chat-oversize/messages.res.jsonl"])
    oversize_snapshot = dict(oversize_node.get("snapshot") or {})
    oversize_snapshot["prewarm"] = False
    oversize_node["snapshot"] = oversize_snapshot
    nodes["chats/chat-oversize/messages.res.jsonl"] = oversize_node

target_node = copy.deepcopy(template_node)
target_snapshot = dict(target_node.get("snapshot") or {})
target_snapshot["prewarm"] = True
target_snapshot["prewarm_timeout_ms"] = timeout_ms
target_node["snapshot"] = target_snapshot
nodes[target_key] = target_node

with open(manifest_path, "w", encoding="utf-8") as f:
    json.dump(doc, f, ensure_ascii=False, indent=2)
    f.write("\n")
PY
}

start_adapter_with_prewarm_delay() {
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
            cmd.exe /C "set APPFS_V2_PREWARM_DELAY_MS=$delay_ms&& $win_bin serve appfs --root $runtime_root --app-id aiim --poll-ms 50" >"$ADAPTER_LOG" 2>&1 &
            ;;
        *)
            APPFS_V2_PREWARM_DELAY_MS="$delay_ms" "$AGENTFS_BIN" serve appfs --root "$runtime_root" --app-id aiim --poll-ms 50 >"$ADAPTER_LOG" 2>&1 &
            ;;
    esac
    ADAPTER_PID=$!
    sleep 1
    if ! kill -0 "$ADAPTER_PID" 2>/dev/null; then
        tail -n 120 "$ADAPTER_LOG" 2>/dev/null || true
        fail "appfs adapter failed to start"
    fi
}

banner "AppFS v2 CT2-001 Startup Prewarm"
require_cmd python3
ensure_agentfs_bin "$CLI_DIR"

mkdir -p "$CLI_DIR/target"
TMP_ROOT="$(mktemp -d "$CLI_DIR/target/ct2-v2-001.XXXXXX")"

reload_fixture_app
rm -f "$SNAPSHOT_FILE"
pass "removed $SNAPSHOT_FILE to start from cold snapshot state"
patch_manifest_for_prewarm 5000
start_adapter_with_prewarm_delay 0
pass "adapter started for prewarm-success scenario"

wait_log_contains "[prewarm] resource=/chats/chat-001/messages.res.jsonl state=hot" "$ADAPTER_LOG" 10 || fail "missing prewarm success log evidence"
record_v2_evidence "connector.prewarm_snapshot_meta" "resource=/chats/chat-001/messages.res.jsonl"
pass "startup prewarm success marks snapshot hot with explicit log evidence"

stop_adapter

reload_fixture_app
rm -f "$SNAPSHOT_FILE"
pass "removed $SNAPSHOT_FILE for prewarm-timeout scenario"
patch_manifest_for_prewarm 1000
start_adapter_with_prewarm_delay 1500
pass "adapter started for prewarm-timeout scenario"

wait_log_contains "[prewarm] timeout resource=/chats/chat-001/messages.res.jsonl" "$ADAPTER_LOG" 10 || fail "missing prewarm timeout log evidence"
grep -F -q "[prewarm] timeout resource=/chats/chat-001/messages.res.jsonl state=cold" "$ADAPTER_LOG" || fail "timeout path should keep snapshot state cold"
pass "prewarm timeout does not block startup and keeps snapshot cold"

say "CT2-001 done"
