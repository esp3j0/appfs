#!/bin/sh
set -eu

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
# shellcheck source=./lib.sh
. "$SCRIPT_DIR/lib.sh"

banner "AppFS CT-004 Paging Handle Protocol"

resource="$APPFS_PAGEABLE_RESOURCE"
fetch_next="$APPFS_APP_DIR/_paging/fetch_next.act"
close_act="$APPFS_APP_DIR/_paging/close.act"
events="$APPFS_APP_DIR/_stream/events.evt.jsonl"

assert_file "$fetch_next"
assert_file "$close_act"
assert_file "$events"

[ -f "$resource" ] || skip "pageable resource not found: $resource"
require_cmd jq

page_json="$(cat "$resource")"
printf '%s\n' "$page_json" | jq -e '.items and .page and .page.handle_id' >/dev/null 2>&1 || fail "pageable resource does not return {items,page.handle_id}"
pass "pageable resource returns handle envelope"

handle_id="$(printf '%s\n' "$page_json" | jq -r '.page.handle_id')"
[ "$handle_id" != "null" ] || fail "handle_id is null"
[ -n "$handle_id" ] || fail "handle_id is empty"
pass "handle_id: $handle_id"

before_lines="$(wc -l < "$events" 2>/dev/null || echo 0)"
printf '{"handle_id":"%s"}\n' "$handle_id" >> "$fetch_next" || fail "fetch_next action failed"
pass "fetch_next accepted"

after_lines="$(wait_for_line_growth "$events" "$before_lines" "$APPFS_TIMEOUT_SEC" || true)"
[ -n "${after_lines:-}" ] || fail "no stream growth after fetch_next"
pass "stream grew after fetch_next"

printf '{"handle_id":"%s"}\n' "$handle_id" >> "$close_act" || fail "close paging handle failed"
pass "close handle submitted"

say "CT-004 done"
