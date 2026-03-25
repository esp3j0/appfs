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

banner "AppFS v2 CT2-004 Concurrent Cold-Miss Coalescing"
require_cmd python3
ensure_agentfs_bin "$CLI_DIR"

mkdir -p "$CLI_DIR/target"
TMP_ROOT="$(mktemp -d "$CLI_DIR/target/ct2-v2-004.XXXXXX")"
MOUNTPOINT="/tmp/agentfs-ct2-v2-004-$$"

prepare_fixture
MOUNT_LOG="$TMP_ROOT/appfs-mount.log"
MOUNT_PID="$(start_appfs_v2_mount "$MOUNT_LOG" "$AGENTFS_BIN" "ct2-v2-004-$$" "$TMP_ROOT" "$MOUNTPOINT" "aiim" 200 "" "")"

SNAPSHOT_FILE="$MOUNTPOINT/aiim/chats/chat-001/messages.res.jsonl"
[ -f "$SNAPSHOT_FILE" ] || fail "snapshot file missing before concurrent cold-miss probe"
rm -f "$SNAPSHOT_FILE"
pass "removed $SNAPSHOT_FILE to force concurrent ordinary-read cold miss"

out_a="$TMP_ROOT/out-a.jsonl"
out_b="$TMP_ROOT/out-b.jsonl"
out_c="$TMP_ROOT/out-c.jsonl"
cat "$SNAPSHOT_FILE" >"$out_a" &
pid_a=$!
cat "$SNAPSHOT_FILE" >"$out_b" &
pid_b=$!
cat "$SNAPSHOT_FILE" >"$out_c" &
pid_c=$!

wait "$pid_a" || fail "concurrent reader A failed"
wait "$pid_b" || fail "concurrent reader B failed"
wait "$pid_c" || fail "concurrent reader C failed"

cmp -s "$out_a" "$out_b" || fail "concurrent readers A/B returned different snapshot bytes"
cmp -s "$out_a" "$out_c" || fail "concurrent readers A/C returned different snapshot bytes"
assert_file "$SNAPSHOT_FILE"
line_count="$(wc -l < "$SNAPSHOT_FILE" | tr -d ' ')"
[ "$line_count" -eq 100 ] || fail "materialized snapshot should contain 100 JSONL lines, got $line_count"

fetch_count="$(grep -F -c "[cache.expand] fetch_snapshot_chunk resource=/chats/chat-001/messages.res.jsonl" "$MOUNT_LOG" 2>/dev/null || true)"
[ "$fetch_count" -eq 1 ] || fail "expected exactly one upstream fetch_snapshot_chunk call, got $fetch_count"

pass "single-flight ordinary-read cold miss verified with one upstream fetch shared across readers"
say "CT2-004 done"
