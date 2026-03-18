#!/bin/sh
set -eu

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
# shellcheck source=./lib.sh
. "$SCRIPT_DIR/lib.sh"

banner "AppFS CT-013 Duplicate Consumption (At-Least-Once)"

events="$APPFS_APP_DIR/_stream/events.evt.jsonl"
from_seq_dir="$APPFS_APP_DIR/_stream/from-seq"
action="${APPFS_TEST_ACTION:-$APPFS_APP_DIR/contacts/zhangsan/send_message.act}"
require_cmd jq

assert_file "$events"
assert_exists "$from_seq_dir"
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

token="ct-dup-$$"
wait_writable "$action" || fail "action sink remained non-writable: $action"
printf '{"client_token":"%s","text":"dup-check"}\n' "$token" >> "$action" || fail "duplicate-consumption submit failed"
wait_token_event "$token" || fail "token event did not arrive in time"
pass "live event observed for duplicate-consumption probe"

tmp_live="$(mktemp)"
grep "$token" "$events" > "$tmp_live"
[ -s "$tmp_live" ] || fail "token event not found in live stream"

live_line="$(tail -n 1 "$tmp_live")"
seq="$(printf '%s\n' "$live_line" | jq -r '.seq')"
event_id="$(printf '%s\n' "$live_line" | jq -r '.event_id')"
request_id="$(printf '%s\n' "$live_line" | jq -r '.request_id')"
[ "$seq" != "null" ] || fail "seq missing on live event"
[ "$event_id" != "null" ] || fail "event_id missing on live event"
[ "$request_id" != "null" ] || fail "request_id missing on live event"
pass "live event has seq/event_id/request_id"

replay_file="$from_seq_dir/$seq.evt.jsonl"
assert_file "$replay_file"
replay_line="$(tail -n 1 "$replay_file")"
replay_event_id="$(printf '%s\n' "$replay_line" | jq -r '.event_id')"
replay_request_id="$(printf '%s\n' "$replay_line" | jq -r '.request_id')"
[ "$replay_event_id" = "$event_id" ] || fail "replay event_id mismatch: $replay_event_id vs $event_id"
[ "$replay_request_id" = "$request_id" ] || fail "replay request_id mismatch: $replay_request_id vs $request_id"
pass "replay preserves same event identity"

tmp_dupe="$(mktemp)"
printf '%s\n' "$live_line" > "$tmp_dupe"
printf '%s\n' "$replay_line" >> "$tmp_dupe"
dup_count="$(jq -r --arg id "$event_id" 'select(.event_id==$id) | .event_id' "$tmp_dupe" | wc -l | tr -d ' ')"
[ "$dup_count" -ge 2 ] || fail "expected duplicate consumption sample count>=2, got $dup_count"
pass "same event can be consumed from live and replay (at-least-once)"

rm -f "$tmp_live" "$tmp_dupe"
say "CT-013 done"
