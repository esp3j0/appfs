#!/bin/sh
set -eu

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
# shellcheck source=./lib.sh
. "$SCRIPT_DIR/lib.sh"

banner "AppFS CT-014 Concurrent Submit Stress"

events="$APPFS_APP_DIR/_stream/events.evt.jsonl"
count="${APPFS_STRESS_SUBMITS:-12}"
assert_file "$events"
require_cmd jq

before_lines="$(wc -l < "$events" 2>/dev/null || echo 0)"

tokens_file="$(mktemp)"
pids_file="$(mktemp)"
i=1
while [ "$i" -le "$count" ]; do
    token="ct-conc-$i-$$"
    action="$APPFS_APP_DIR/contacts/load-$i/send_message.act"
    mkdir -p "$(dirname "$action")"
    printf '%s\n' "$token" >> "$tokens_file"
    (
        printf '{"client_token":"%s","text":"stress-%s"}\n' "$token" "$i" >> "$action"
    ) &
    printf '%s\n' "$!" >> "$pids_file"
    i=$((i + 1))
done

while read -r pid; do
    wait "$pid" || fail "concurrent writer failed (pid=$pid)"
done < "$pids_file"
pass "all concurrent writes completed"

deadline=$(( $(date +%s) + $APPFS_TIMEOUT_SEC ))
while :; do
    complete=1
    while read -r token; do
        hits="$(grep -c "$token" "$events" 2>/dev/null || true)"
        [ -n "$hits" ] || hits=0
        if [ "$hits" -lt 1 ]; then
            complete=0
            break
        fi
    done < "$tokens_file"
    if [ "$complete" -eq 1 ]; then
        break
    fi
    now="$(date +%s)"
    [ "$now" -lt "$deadline" ] || fail "not all concurrent submit events arrived in time"
    sleep 1
done

after_lines="$(wc -l < "$events" 2>/dev/null || echo 0)"
[ "$after_lines" -ge $((before_lines + count)) ] || fail "stream growth too small for concurrent submits ($before_lines->$after_lines, expected >= +$count)"
pass "event stream grew for concurrent submits"

request_ids_tmp="$(mktemp)"
while read -r token; do
    token_lines="$(mktemp)"
    grep "$token" "$events" > "$token_lines"
    line_count="$(wc -l < "$token_lines" | tr -d ' ')"
    [ "$line_count" = "1" ] || fail "token $token expected exactly 1 terminal event, got $line_count"
    etype="$(jq -r '.type' "$token_lines")"
    [ "$etype" = "action.completed" ] || fail "token $token expected action.completed, got $etype"
    rid="$(jq -r '.request_id' "$token_lines")"
    printf '%s\n' "$rid" >> "$request_ids_tmp"
    rm -f "$token_lines"
done < "$tokens_file"
pass "each concurrent submit produced one completed terminal event"

rid_unique="$(sort -u "$request_ids_tmp" | wc -l | tr -d ' ')"
[ "$rid_unique" = "$count" ] || fail "expected $count unique request_ids, got $rid_unique"
pass "concurrent submits have distinct request_id"

rm -f "$tokens_file" "$pids_file" "$request_ids_tmp"
say "CT-014 done"
