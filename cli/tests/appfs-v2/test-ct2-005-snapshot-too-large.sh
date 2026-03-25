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

start_mount() {
    force_expand="${1:-}"
    MOUNT_LOG="$TMP_ROOT/appfs-mount.log"
    MOUNT_PID="$(start_appfs_v2_mount "$MOUNT_LOG" "$AGENTFS_BIN" "ct2-v2-005-$$" "$TMP_ROOT" "$MOUNTPOINT" "aiim" "" "" "$force_expand")"
}

banner "AppFS v2 CT2-005 Snapshot Too-Large Atomic Mapping"
require_cmd python3
require_cmd sha256sum
ensure_agentfs_bin "$CLI_DIR"

mkdir -p "$CLI_DIR/target"
TMP_ROOT="$(mktemp -d "$CLI_DIR/target/ct2-v2-005.XXXXXX")"
MOUNTPOINT="/tmp/agentfs-ct2-v2-005-$$"

prepare_fixture
start_mount ""

SNAPSHOT_FILE="$MOUNTPOINT/aiim/chats/chat-oversize/messages.res.jsonl"
[ -f "$SNAPSHOT_FILE" ] || fail "oversize snapshot file missing before cold probe"
rm -f "$SNAPSHOT_FILE"
pass "removed $SNAPSHOT_FILE to force cold oversize expansion"

if cat "$SNAPSHOT_FILE" >/dev/null 2>"$TMP_ROOT/ct2-005-cold.stderr"; then
    fail "cold oversize ordinary read should fail"
fi
[ ! -f "$SNAPSHOT_FILE" ] || fail "cold oversize ordinary read must not publish snapshot file"
grep -F -q "[cache] snapshot_too_large resource=/chats/chat-oversize/messages.res.jsonl" "$MOUNT_LOG" || fail "missing snapshot_too_large log anchor"
pass "cold ordinary read maps oversize snapshot to failure without publishing bytes"

stop_mount_process "${MOUNT_PID:-}" "${MOUNTPOINT:-}"
MOUNT_PID=""

prepare_fixture
cat >"$TMP_ROOT/aiim/chats/chat-oversize/messages.res.jsonl" <<'EOF'
{"id":"keep-1","text":"old cache line"}
EOF
hash_before="$(sha256sum "$TMP_ROOT/aiim/chats/chat-oversize/messages.res.jsonl" | awk '{print $1}')"
size_before="$(wc -c < "$TMP_ROOT/aiim/chats/chat-oversize/messages.res.jsonl" | tr -d ' ')"
start_mount 1

SNAPSHOT_FILE="$MOUNTPOINT/aiim/chats/chat-oversize/messages.res.jsonl"
[ "$size_before" -lt 128 ] || fail "precondition failed: partial cache should be under max_size"
pass "seeded partial cache bytes=$size_before under max_size=128"

if cat "$SNAPSHOT_FILE" >/dev/null 2>"$TMP_ROOT/ct2-005-partial.stderr"; then
    fail "partial-cache oversize ordinary read should fail"
fi
hash_after="$(sha256sum "$TMP_ROOT/aiim/chats/chat-oversize/messages.res.jsonl" | awk '{print $1}')"
[ "$hash_before" = "$hash_after" ] || fail "partial cache file changed after oversize failure (expected atomic keep-old)"
grep -F -q "[cache] snapshot_too_large resource=/chats/chat-oversize/messages.res.jsonl" "$MOUNT_LOG" || fail "missing snapshot_too_large log anchor in partial-cache scenario"
pass "partial-cache forced ordinary read keeps old cache unchanged on oversize failure"

say "CT2-005 done"
