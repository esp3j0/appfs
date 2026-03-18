#!/bin/sh
set -eu

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
# shellcheck source=./lib.sh
. "$SCRIPT_DIR/lib.sh"

banner "AppFS CT-018 Burst Append JSONL Queueing"

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

wait_writable "$action" || fail "action sink remained non-writable: $action"

token1="ct-burst-1-$$"
token2="ct-burst-2-$$"
token3="ct-burst-3-$$"
before_lines="$(wc -l < "$events" 2>/dev/null || echo 0)"

# Do not wait for prior event; submit multiple appends on one sink.
printf '{"client_token":"%s","text":"burst-1"}\n' "$token1" >> "$action" || fail "burst submit #1 failed"
printf '{"client_token":"%s","text":"burst-2"}\n' "$token2" >> "$action" || fail "burst submit #2 failed"
printf '{"client_token":"%s","text":"burst-3"}\n' "$token3" >> "$action" || fail "burst submit #3 failed"

wait_for_line_growth "$events" "$before_lines" "$APPFS_TIMEOUT_SEC" >/dev/null || fail "event stream did not grow"
sleep 2

tmp="$(mktemp)"
grep -E "$token1|$token2|$token3" "$events" > "$tmp" || true

count1="$(grep -c "$token1" "$tmp" 2>/dev/null || true)"
count2="$(grep -c "$token2" "$tmp" 2>/dev/null || true)"
count3="$(grep -c "$token3" "$tmp" 2>/dev/null || true)"
[ "${count1:-0}" -ge 1 ] || fail "burst token1 missing in stream"
[ "${count2:-0}" -ge 1 ] || fail "burst token2 missing in stream"
[ "${count3:-0}" -ge 1 ] || fail "burst token3 missing in stream"
pass "all burst tokens observed in stream"

line1="$(grep "$token1" "$tmp" | tail -n 1)"
line2="$(grep "$token2" "$tmp" | tail -n 1)"
line3="$(grep "$token3" "$tmp" | tail -n 1)"

echo "$line1" | jq -e '.type == "action.completed"' >/dev/null 2>&1 || fail "token1 terminal type mismatch"
echo "$line2" | jq -e '.type == "action.completed"' >/dev/null 2>&1 || fail "token2 terminal type mismatch"
echo "$line3" | jq -e '.type == "action.completed"' >/dev/null 2>&1 || fail "token3 terminal type mismatch"
pass "all burst submits produced completed terminal events"

seq1="$(echo "$line1" | jq -r '.seq')"
seq2="$(echo "$line2" | jq -r '.seq')"
seq3="$(echo "$line3" | jq -r '.seq')"
[ "$seq1" -lt "$seq2" ] || fail "burst order mismatch: seq1=$seq1 seq2=$seq2"
[ "$seq2" -lt "$seq3" ] || fail "burst order mismatch: seq2=$seq2 seq3=$seq3"
pass "burst submit order preserved by stream sequence"

rm -f "$tmp"
say "CT-018 done"
