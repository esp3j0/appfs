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

journal_json() {
    "$AGENTFS_BIN" fs "$AGENT_ID" cat "/aiim/_stream/snapshot-expand.state.res.json" 2>/dev/null || true
}

journal_temp_artifact() {
    json_payload="$(journal_json)"
    [ -n "$json_payload" ] || {
        printf '\n'
        return 0
    }
    printf '%s\n' "$json_payload" | python3 -c 'import json,sys; doc=json.loads(sys.stdin.read()); entry=(doc.get("resources") or {}).get("chats/chat-001/messages.res.jsonl") or {}; print(entry.get("temp_artifact") or "")'
}

assert_journal_entry_cleared() {
    json_payload="$(journal_json)"
    [ -n "$json_payload" ] || return 0
    printf '%s\n' "$json_payload" | python3 -c 'import json,sys; doc=json.loads(sys.stdin.read()); resources=doc.get("resources"); raise SystemExit(1 if not isinstance(resources, dict) or "chats/chat-001/messages.res.jsonl" in resources else 0)'
}

start_mount() {
    publish_delay_ms="${1:-}"
    reuse_existing="${2:-0}"
    MOUNT_LOG="$TMP_ROOT/appfs-mount.log"
    MOUNT_PID="$(start_appfs_v2_mount "$MOUNT_LOG" "$AGENTFS_BIN" "$AGENT_ID" "$TMP_ROOT" "$MOUNTPOINT" "aiim" "" "$publish_delay_ms" "" "$reuse_existing")"
}

wait_mount_log() {
    pattern="$1"
    timeout="${2:-20}"
    i=0
    while [ "$i" -lt "$timeout" ]; do
        if grep -F -q "$pattern" "$MOUNT_LOG" 2>/dev/null; then
            return 0
        fi
        i=$((i + 1))
        sleep 1
    done
    return 1
}

banner "AppFS v2 CT2-006 Journal Recovery for Incomplete Expand"
require_cmd python3
ensure_agentfs_bin "$CLI_DIR"

mkdir -p "$CLI_DIR/target"
TMP_ROOT="$(mktemp -d "$CLI_DIR/target/ct2-v2-006.XXXXXX")"
MOUNTPOINT="/tmp/agentfs-ct2-v2-006-$$"
AGENT_ID="ct2-v2-006-$$"

prepare_fixture
start_mount 8000 0

SNAPSHOT_FILE="$MOUNTPOINT/aiim/chats/chat-001/messages.res.jsonl"
[ -f "$SNAPSHOT_FILE" ] || fail "snapshot file missing before crash simulation"
rm -f "$SNAPSHOT_FILE"
pass "removed $SNAPSHOT_FILE to force cold snapshot miss"

cat "$SNAPSHOT_FILE" >"$TMP_ROOT/inflight.out" &
cat_pid=$!
wait_mount_log "[cache] mount read-through resource=/chats/chat-001/messages.res.jsonl trigger=lookup_miss" 20 || fail "mount did not enter snapshot expand path before kill"
sleep 2

stop_mount_process "${MOUNT_PID:-}" "${MOUNTPOINT:-}"
MOUNT_PID=""
wait "$cat_pid" 2>/dev/null || true
pass "mount killed during publish window"

json_payload="$(journal_json)"
[ -n "$json_payload" ] || fail "journal missing after crash"
printf '%s\n' "$json_payload" | python3 -c 'import json,sys; doc=json.loads(sys.stdin.read()); entry=(doc.get("resources") or {}).get("chats/chat-001/messages.res.jsonl"); raise SystemExit(0 if entry and entry.get("status")=="publishing" else 1)' || fail "journal did not persist publishing state across crash"
temp_artifact_rel="$(journal_temp_artifact)"
[ -n "$temp_artifact_rel" ] || fail "journal missing temp_artifact path after crash"
"$AGENTFS_BIN" fs "$AGENT_ID" cat "$temp_artifact_rel" >/dev/null 2>&1 || fail "expected pending temp artifact to exist after crash"
pass "crash left publishing journal state and pending temp artifact"

start_mount "" 1
SNAPSHOT_FILE="$MOUNTPOINT/aiim/chats/chat-001/messages.res.jsonl"

full_content="$(cat "$SNAPSHOT_FILE")" || fail "ordinary read after recovery should succeed"
[ -n "$full_content" ] || fail "ordinary read after recovery returned empty content"
line_count="$(wc -l < "$SNAPSHOT_FILE" | tr -d ' ')"
[ "$line_count" -eq 100 ] || fail "recovered snapshot should materialize 100 JSONL lines, got $line_count"
grep -F -q "[recovery] mount snapshot expand incomplete resource=/chats/chat-001/messages.res.jsonl" "$MOUNT_LOG" || fail "missing recovery log anchor"
"$AGENTFS_BIN" fs "$AGENT_ID" cat "$temp_artifact_rel" >/dev/null 2>&1 && fail "recovery should clean pending temp artifact"
assert_journal_entry_cleared || fail "recovery should clear journal entry for target resource"
pass "restart recovery cleaned incomplete expansion and cleared journal entry"
pass "post-recovery ordinary read materializes full snapshot"

say "CT2-006 done"
