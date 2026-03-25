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
MOUNT_PID=""
MOUNT_LOG=""
MOUNTPOINT=""
AGENT_ID=""

cleanup() {
    if [ -n "${MOUNT_LOG:-}" ]; then
        persist_case_evidence "ct2-003" "mount.final.log" "$MOUNT_LOG"
    fi
    stop_mount_process "${MOUNT_PID:-}" "${MOUNTPOINT:-}"
    if [ -n "${TMP_ROOT:-}" ] && [ -d "$TMP_ROOT" ]; then
        rm -rf "$TMP_ROOT"
    fi
}
trap cleanup EXIT INT TERM

assert_json_expr() {
    json_payload="$1"
    expr="$2"
    description="$3"
    if ! printf '%s\n' "$json_payload" | python3 -c 'import json,sys; expr=sys.argv[1]; obj=json.loads(sys.stdin.read()); raise SystemExit(0 if eval(expr, {"obj": obj}) else 1)' "$expr"
    then
        fail "$description"
    fi
}

prepare_fixture() {
    rm -rf "$TMP_ROOT/aiim"
    cp -R "$REPO_DIR/examples/appfs/aiim" "$TMP_ROOT/"
}

patch_manifest_timeout_fail() {
    python3 - "$TMP_ROOT/aiim/_meta/manifest.res.json" <<'PY'
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

start_mount() {
    delay_ms="${1:-}"
    publish_delay_ms="${2:-}"
    force_expand="${3:-}"
    MOUNT_LOG="$TMP_ROOT/appfs-mount.log"
    AGENT_ID="ct2-v2-003-$$"
    MOUNT_PID="$(start_appfs_v2_mount "$MOUNT_LOG" "$AGENTFS_BIN" "$AGENT_ID" "$TMP_ROOT" "$MOUNTPOINT" "aiim" "$delay_ms" "$publish_delay_ms" "$force_expand")"
}

banner "AppFS v2 CT2-003 Read Miss Expand"
require_cmd python3
ensure_agentfs_bin "$CLI_DIR"

mkdir -p "$CLI_DIR/target"
TMP_ROOT="$(mktemp -d "$CLI_DIR/target/ct2-v2-003.XXXXXX")"
MOUNTPOINT="/tmp/agentfs-ct2-v2-003-$$"

prepare_fixture
start_mount "" "" ""

SNAPSHOT_FILE="$MOUNTPOINT/aiim/chats/chat-001/messages.res.jsonl"
[ -f "$SNAPSHOT_FILE" ] || fail "snapshot file missing before cold-miss probe"
rm -f "$SNAPSHOT_FILE"
pass "removed $SNAPSHOT_FILE to force ordinary-read cold miss"

full_content="$(cat "$SNAPSHOT_FILE")" || fail "ordinary snapshot read should auto-expand cold miss"
[ -n "$full_content" ] || fail "ordinary snapshot read returned empty content"
assert_file "$SNAPSHOT_FILE"

line_count="$(wc -l < "$SNAPSHOT_FILE" | tr -d ' ')"
[ "$line_count" -eq 100 ] || fail "expanded snapshot should materialize 100 JSONL lines, got $line_count"
first_line="$(head -n 1 "$SNAPSHOT_FILE")"
assert_json_expr "$first_line" 'isinstance(obj, dict) and "id" in obj and "text" in obj and "page" not in obj' "expanded snapshot line is not pure JSONL item"

grep -F -q "[cache] mount read-through resource=/chats/chat-001/messages.res.jsonl trigger=lookup_miss" "$MOUNT_LOG" || fail "missing ordinary-read cold miss log"
grep -F -q "[cache.expand] fetch_snapshot_chunk resource=/chats/chat-001/messages.res.jsonl trigger=read" "$MOUNT_LOG" || fail "missing fetch_snapshot_chunk log"
grep -F -q "[cache] expanded resource=/chats/chat-001/messages.res.jsonl bytes=" "$MOUNT_LOG" || fail "missing expansion completion log"
persist_case_evidence "ct2-003" "mount.log" "$MOUNT_LOG"
pass "cold miss ordinary read expanded through V2 connector and materialized JSONL"

stop_mount_process "${MOUNT_PID:-}" "${MOUNTPOINT:-}"
MOUNT_PID=""

prepare_fixture
patch_manifest_timeout_fail
start_mount 200 "" ""

SNAPSHOT_FILE="$MOUNTPOINT/aiim/chats/chat-001/messages.res.jsonl"
[ -f "$SNAPSHOT_FILE" ] || fail "snapshot file missing before timeout-fail probe"
rm -f "$SNAPSHOT_FILE"
pass "prepared timeout-fail scenario with read_through_timeout_ms=50 and on_timeout=fail"

if cat "$SNAPSHOT_FILE" >/dev/null 2>"$TMP_ROOT/ct2-003-timeout.stderr"; then
    fail "timeout-fail ordinary read should not succeed"
fi
[ ! -f "$SNAPSHOT_FILE" ] || fail "timeout-fail ordinary read must not publish snapshot file"
grep -F -q "[cache] expand failed resource=/chats/chat-001/messages.res.jsonl phase=timeout on_timeout=fail" "$MOUNT_LOG" || fail "missing timeout-fail expand log"
pass "ordinary read timeout with on_timeout=fail surfaces failure and keeps snapshot unpublished"

say "CT2-003 done"
