#!/bin/sh
set -eu

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
# shellcheck source=./lib.sh
. "$SCRIPT_DIR/lib.sh"

banner "AppFS CT-011 Interrupted Submit Does Not Commit"

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

token_partial="ct-interrupt-partial-$$"
before_lines="$(wc -l < "$events" 2>/dev/null || echo 0)"

wait_writable "$action" || fail "action sink remained non-writable: $action"
(
    exec 3>> "$action"
    # Intentionally write a non-terminated payload chunk, then hang.
    printf '{"client_token":"%s","text":"partial' "$token_partial" >&3
    sleep 10
    printf '%s' '-late-body"}' >&3
    printf '\n' >&3
    exec 3>&-
) &
writer_pid=$!

sleep 1
kill -9 "$writer_pid" >/dev/null 2>&1 || true
wait "$writer_pid" 2>/dev/null || true
pass "interrupted writer process terminated"

sleep 2
after_interrupt_lines="$(wc -l < "$events" 2>/dev/null || echo 0)"
[ "$after_interrupt_lines" -eq "$before_lines" ] || fail "interrupted write unexpectedly emitted event"
pass "no event emitted for interrupted write"

token_recover="ct-interrupt-recover-$$"
wait_writable "$action" || fail "action sink remained non-writable after interrupted write: $action"
# Flush poisoned partial line first; runtime will consume the malformed line
# and then parse the next JSONL record normally.
printf '\n{"client_token":"%s","text":"recovered"}\n' "$token_recover" >> "$action" || fail "recovery submit failed"
wait_token_event "$token_recover" || fail "recovery event did not arrive in time"
pass "subsequent valid submit succeeded"

say "CT-011 done"
