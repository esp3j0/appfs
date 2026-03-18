#!/bin/sh
set -eu

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
# shellcheck source=./lib.sh
. "$SCRIPT_DIR/lib.sh"

banner "AppFS CT-010 Submit Atomicity (In-Progress Write)"

events="$APPFS_APP_DIR/_stream/events.evt.jsonl"
action="${APPFS_TEST_ACTION:-$APPFS_APP_DIR/contacts/zhangsan/send_message.act}"

assert_file "$events"
assert_file "$action"
require_cmd jq

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

wait_token_event() {
    token="$1"
    deadline=$(( $(date +%s) + $APPFS_TIMEOUT_SEC ))
    while :; do
        count="$(grep -c "$token" "$events" 2>/dev/null || true)"
        [ -n "$count" ] || count=0
        if [ "$count" -ge 1 ]; then
            return 0
        fi
        now="$(date +%s)"
        [ "$now" -lt "$deadline" ] || return 1
        sleep 1
    done
}

token="ct-atomic-$$"
before_lines="$(wc -l < "$events" 2>/dev/null || echo 0)"

wait_writable "$action" || fail "action sink remained non-writable: $action"
(
    printf '{"client_token":"%s","text":"atomic' "$token"
    sleep 1
    printf '-finish"}\n'
) >> "$action" &
writer_pid=$!

sleep 1
mid_lines="$(wc -l < "$events" 2>/dev/null || echo 0)"
[ "$mid_lines" -eq "$before_lines" ] || fail "event emitted before in-progress write finished"
pass "no early event during in-progress write"

wait "$writer_pid" || fail "staged writer failed"
pass "staged writer completed"

wait_token_event "$token" || fail "atomic submit event did not arrive in time"
after_lines="$(wc -l < "$events" 2>/dev/null || echo 0)"
[ "$after_lines" -gt "$before_lines" ] || fail "event stream did not grow after staged write completed"
pass "event stream grew after completed write"

tmp_file="$(mktemp)"
grep "$token" "$events" > "$tmp_file"
count="$(wc -l < "$tmp_file" | tr -d ' ')"
[ "$count" = "1" ] || fail "expected exactly one token event, got $count"
etype="$(jq -r '.type' "$tmp_file")"
[ "$etype" = "action.completed" ] || fail "expected action.completed, got $etype"
rm -f "$tmp_file"

pass "staged write produced one terminal event after completion"
say "CT-010 done"
