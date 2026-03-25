#!/bin/sh
set -eu

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
# shellcheck source=./lib.sh
. "$SCRIPT_DIR/lib.sh"

banner "AppFS CT-021 Snapshot Full-File Semantics"

resource="${APPFS_SNAPSHOT_RESOURCE:-$APPFS_APP_DIR/chats/chat-001/messages.res.jsonl}"
app_dir="$(dirname "$(dirname "$(dirname "$resource")")")"
events="$app_dir/_stream/events.evt.jsonl"

assert_file "$events"
[ -f "$resource" ] && rm -f "$resource"
pass "removed snapshot file to force ordinary-read cold miss"

full_content="$(cat "$resource")" || fail "ordinary snapshot read should auto-expand cold miss"
[ -n "$full_content" ] || fail "ordinary snapshot read returned empty content"
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
    query_text="$(printf '%s\n' "$first_line" | jq -r '.text')"
    [ -n "$query_text" ] || fail "snapshot first line is missing text"
else
    skip "jq not found; skip snapshot JSON structure checks"
fi

if command -v rg >/dev/null 2>&1; then
    rg -n -F "$query_text" "$resource" >/dev/null 2>&1 || fail "rg did not match expanded snapshot content"
    pass "rg can query snapshot JSONL directly"
else
    grep -n -F "$query_text" "$resource" >/dev/null 2>&1 || fail "grep did not match expanded snapshot content"
    pass "grep can query snapshot JSONL directly"
fi

say "CT-021 done"
