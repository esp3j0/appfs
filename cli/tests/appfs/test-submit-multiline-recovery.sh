#!/bin/sh
set -eu

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
# shellcheck source=./lib.sh
. "$SCRIPT_DIR/lib.sh"

banner "AppFS CT-020 Multiline JSON Recovery"

events="$APPFS_APP_DIR/_stream/events.evt.jsonl"
action="${APPFS_TEST_ACTION:-$APPFS_APP_DIR/contacts/zhangsan/send_message.act}"
require_cmd jq

assert_file "$events"
assert_file "$action"

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
    i=0
    while [ "$i" -lt "$APPFS_TIMEOUT_SEC" ]; do
        count="$(grep -c "$token" "$events" 2>/dev/null || true)"
        [ -n "$count" ] || count=0
        if [ "$count" -ge 1 ]; then
            return 0
        fi
        i=$((i + 1))
        sleep 1
    done
    return 1
}

token1="ct-multiline-1-$$"
token2="ct-multiline-2-$$"

before_lines="$(wc -l < "$events" 2>/dev/null || echo 0)"

wait_writable "$action" || fail "action sink remained non-writable: $action"
cat >> "$action" <<EOF
{"client_token":"$token1","text":"你好
hello
好！"}
EOF
pass "first multiline payload appended"

wait_writable "$action" || fail "action sink remained non-writable before second multiline submit: $action"
cat >> "$action" <<EOF
{"client_token":"$token2","text":"line-1
line-2
line-3"}
EOF
pass "second multiline payload appended"

wait_token_event "$token1" || fail "first multiline token missing in stream"
wait_token_event "$token2" || fail "second multiline token missing in stream"
pass "both multiline tokens observed in stream"

after_lines="$(wc -l < "$events" 2>/dev/null || echo 0)"
[ "$after_lines" -gt "$before_lines" ] || fail "event stream did not grow for multiline submits"
pass "event stream grew ($before_lines -> $after_lines)"

line1="$(grep "$token1" "$events" | tail -n 1)"
line2="$(grep "$token2" "$events" | tail -n 1)"

echo "$line1" | jq -e '.type == "action.completed"' >/dev/null 2>&1 || fail "token1 terminal type mismatch"
echo "$line2" | jq -e '.type == "action.completed"' >/dev/null 2>&1 || fail "token2 terminal type mismatch"
pass "multiline submits produced completed terminal events"

seq1="$(echo "$line1" | jq -r '.seq')"
seq2="$(echo "$line2" | jq -r '.seq')"
[ "$seq1" -lt "$seq2" ] || fail "multiline submit order mismatch: seq1=$seq1 seq2=$seq2"
pass "multiline submit order preserved by stream sequence"

say "CT-020 done"
