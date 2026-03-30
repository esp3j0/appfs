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

cleanup() {
    stop_mount_process "${MOUNT_PID:-}" "${MOUNTPOINT:-}"
    if [ -n "${TMP_ROOT:-}" ] && [ -d "$TMP_ROOT" ]; then
        rm -rf "$TMP_ROOT"
    fi
}
trap cleanup EXIT INT TERM

prepare_fixture() {
    rm -rf "$TMP_ROOT/aiim"
    cp -R "$REPO_DIR/examples/appfs/aiim" "$TMP_ROOT/"
}

patch_manifest_timeout_return_stale() {
    python3 - "$TMP_ROOT/aiim/_meta/manifest.res.json" <<'PY'
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

start_mount() {
    MOUNT_LOG="$TMP_ROOT/appfs-mount.log"
    MOUNT_PID="$(start_appfs_connector_mount "$MOUNT_LOG" "$AGENTFS_BIN" "ct2-connector-028-$$" "$TMP_ROOT" "$MOUNTPOINT" "aiim" 200 "" 1)"
}

banner "AppFS Connector CT2-028 Timeout Return-Stale Fallback"
require_cmd python3
require_cmd sha256sum
ensure_agentfs_bin "$CLI_DIR"

mkdir -p "$CLI_DIR/target"
TMP_ROOT="$(mktemp -d "$CLI_DIR/target/ct2-connector-028.XXXXXX")"
MOUNTPOINT="/tmp/agentfs-ct2-connector-028-$$"

prepare_fixture
patch_manifest_timeout_return_stale
hash_before="$(sha256sum "$TMP_ROOT/aiim/chats/chat-001/messages.res.jsonl" | awk '{print $1}')"
start_mount

SNAPSHOT_FILE="$MOUNTPOINT/aiim/chats/chat-001/messages.res.jsonl"
stale_output="$TMP_ROOT/stale-output.jsonl"
cat "$SNAPSHOT_FILE" >"$stale_output" || fail "timeout-return_stale with healthy cache should succeed"
hash_after="$(sha256sum "$TMP_ROOT/aiim/chats/chat-001/messages.res.jsonl" | awk '{print $1}')"
[ "$hash_before" = "$hash_after" ] || fail "healthy stale fallback should keep cached bytes unchanged"
cmp -s "$SNAPSHOT_FILE" "$stale_output" || fail "healthy stale fallback should return cached bytes"
grep -F -q "[cache] timeout_return_stale resource=/chats/chat-001/messages.res.jsonl trigger=open" "$MOUNT_LOG" || fail "missing timeout_return_stale log anchor"
pass "timeout-return_stale with healthy cache returns degraded success and keeps stale bytes"

stop_mount_process "${MOUNT_PID:-}" "${MOUNTPOINT:-}"
MOUNT_PID=""

prepare_fixture
patch_manifest_timeout_return_stale
cat >"$TMP_ROOT/aiim/chats/chat-001/messages.res.jsonl" <<'EOF'
{"id":"ok-1","text":"valid line"}
{"id":
EOF
bad_hash_before="$(sha256sum "$TMP_ROOT/aiim/chats/chat-001/messages.res.jsonl" | awk '{print $1}')"
start_mount

SNAPSHOT_FILE="$MOUNTPOINT/aiim/chats/chat-001/messages.res.jsonl"
if cat "$SNAPSHOT_FILE" >/dev/null 2>"$TMP_ROOT/ct2-028-bad.stderr"; then
    fail "timeout-return_stale with malformed stale cache should fail"
fi
bad_hash_after="$(sha256sum "$TMP_ROOT/aiim/chats/chat-001/messages.res.jsonl" | awk '{print $1}')"
[ "$bad_hash_before" = "$bad_hash_after" ] || fail "malformed stale timeout should not mutate stale cache bytes"
grep -F -q "[cache] expand failed resource=/chats/chat-001/messages.res.jsonl phase=timeout on_timeout=return_stale stale_reason=stale_cache_unhealthy trigger=open" "$MOUNT_LOG" || fail "missing malformed stale timeout log"
pass "malformed stale cache is rejected and ordinary read fails without mutating bytes"

say "CT2-028 done"
