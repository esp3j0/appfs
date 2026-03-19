#!/bin/sh
set -eu

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
# shellcheck source=./lib.sh
. "$SCRIPT_DIR/lib.sh"

banner "AppFS CT-021 Snapshot Full-File Semantics"

resource="${APPFS_SNAPSHOT_RESOURCE:-$APPFS_APP_DIR/chats/chat-001/messages.res.jsonl}"
assert_file "$resource"

line_count="$(wc -l < "$resource" | tr -d ' ')"
[ "$line_count" -ge 2 ] || fail "snapshot JSONL should contain multiple lines"
pass "snapshot JSONL has $line_count lines"

if command -v jq >/dev/null 2>&1; then
    first_line="$(head -n 1 "$resource")"
    printf '%s\n' "$first_line" | jq -e '.id and .text' >/dev/null 2>&1 || fail "snapshot JSONL line is not a message item"
    if printf '%s\n' "$first_line" | jq -e '.page' >/dev/null 2>&1; then
        fail "snapshot JSONL must not be wrapped in {items,page} envelope"
    fi
    pass "snapshot uses full-file JSONL item semantics"
else
    skip "jq not found; skip snapshot JSON structure checks"
fi

if command -v rg >/dev/null 2>&1; then
    rg -n "snapshot file" "$resource" >/dev/null 2>&1 || fail "rg did not match expected snapshot content"
    pass "rg can query snapshot JSONL directly"
else
    grep -n "snapshot file" "$resource" >/dev/null 2>&1 || fail "grep did not match expected snapshot content"
    pass "grep can query snapshot JSONL directly"
fi

say "CT-021 done"
