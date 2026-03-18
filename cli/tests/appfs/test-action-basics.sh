#!/bin/sh
set -eu

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
# shellcheck source=./lib.sh
. "$SCRIPT_DIR/lib.sh"

banner "AppFS CT-002 Action Sink JSONL Semantics"

events="$APPFS_APP_DIR/_stream/events.evt.jsonl"
action="$APPFS_TEST_ACTION"

assert_file "$events"
assert_file "$action"

before_lines="$(wc -l < "$events" 2>/dev/null || echo 0)"
printf '{"client_token":"ct-002","text":"hello-from-contract"}\n' >> "$action" || fail "append submit failed: $action"
pass "append+newline submit on action sink"

after_lines="$(wait_for_line_growth "$events" "$before_lines" "$APPFS_TIMEOUT_SEC" || true)"
[ -n "${after_lines:-}" ] || fail "event stream did not grow after action submit"
pass "event stream grew ($before_lines -> $after_lines)"

before_overwrite="$(wc -l < "$events" 2>/dev/null || echo 0)"
printf '{"client_token":"ct-002-overwrite","text":"overwrite-without-newline"}' > "$action" || fail "overwrite probe write failed"
sleep 2
after_overwrite="$(wc -l < "$events" 2>/dev/null || echo 0)"
[ "$after_overwrite" -eq "$before_overwrite" ] || fail "overwrite probe unexpectedly produced stream event"
pass "overwrite/truncate probe did not commit a request"

if command -v jq >/dev/null 2>&1; then
    last_line="$(tail -n 1 "$events")"
    printf '%s\n' "$last_line" | jq -e ".request_id and .type" >/dev/null 2>&1 || fail "last event missing request_id/type"
    pass "last event has request_id and type"
fi

say "CT-002 done"
