#!/bin/sh
set -eu

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
# shellcheck source=./lib.sh
. "$SCRIPT_DIR/lib.sh"

banner "AppFS CT-007 Submit-Time Reject Without Stream Accept"

events="$APPFS_APP_DIR/_stream/events.evt.jsonl"
json_action="${APPFS_STREAMING_ACTION:-$APPFS_APP_DIR/files/file-001/download.act}"
fetch_next="$APPFS_APP_DIR/_paging/fetch_next.act"

assert_file "$events"
assert_file "$json_action"
assert_file "$fetch_next"

wait_writable() {
    path="$1"
    i=0
    while [ "$i" -lt "$APPFS_TIMEOUT_SEC" ]; do
        if [ -w "$path" ]; then
            return 0
        fi
        i=$((i + 1))
        sleep 1
    done
    return 1
}

wait_writable "$json_action" || fail "action sink remained non-writable: $json_action"
before_json="$(wc -l < "$events" 2>/dev/null || echo 0)"
printf '{"target":\n' >> "$json_action" || fail "malformed json write failed unexpectedly"
sleep 2
after_json="$(wc -l < "$events" 2>/dev/null || echo 0)"
[ "$after_json" -eq "$before_json" ] || fail "malformed json unexpectedly produced stream event"
pass "malformed json payload rejected without stream event"

wait_writable "$fetch_next" || fail "action sink remained non-writable: $fetch_next"
before_handle="$(wc -l < "$events" 2>/dev/null || echo 0)"
printf '{"handle_id":"bad/handle"}\n' >> "$fetch_next" || fail "invalid handle write failed unexpectedly"
sleep 2
after_handle="$(wc -l < "$events" 2>/dev/null || echo 0)"
[ "$after_handle" -eq "$before_handle" ] || fail "invalid handle unexpectedly produced stream event"
pass "invalid paging handle rejected without stream event"

say "CT-007 done"
