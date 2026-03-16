#!/bin/sh
set -eu

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
# shellcheck source=./lib.sh
. "$SCRIPT_DIR/lib.sh"

banner "AppFS CT-002 Action Sink Semantics"

events="$APPFS_APP_DIR/_stream/events.evt.jsonl"
action="$APPFS_TEST_ACTION"

assert_file "$events"
assert_file "$action"

before_lines="$(wc -l < "$events" 2>/dev/null || echo 0)"

printf 'token:ct-002\nhello-from-contract\n' > "$action" || fail "write+close failed: $action"
pass "write+close on action sink"

after_lines="$(wait_for_line_growth "$events" "$before_lines" "$APPFS_TIMEOUT_SEC" || true)"
[ -n "${after_lines:-}" ] || fail "event stream did not grow after action submit"
pass "event stream grew ($before_lines -> $after_lines)"

run_expect_fail cat "$action"
run_expect_fail sh -c "echo appended >> \"$action\""

if command -v jq >/dev/null 2>&1; then
    last_line="$(tail -n 1 "$events")"
    printf '%s\n' "$last_line" | jq -e ".request_id and .type" >/dev/null 2>&1 || fail "last event missing request_id/type"
    pass "last event has request_id and type"
fi

say "CT-002 done"
