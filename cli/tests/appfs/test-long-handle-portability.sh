#!/bin/sh
set -eu

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
# shellcheck source=./lib.sh
. "$SCRIPT_DIR/lib.sh"

banner "AppFS CT-015 Long Handle Normalization"

events="$APPFS_APP_DIR/_stream/events.evt.jsonl"
resource="${APPFS_LONG_HANDLE_RESOURCE:-$APPFS_APP_DIR/chats/chat-long/messages.res.json}"
fetch_next="$APPFS_APP_DIR/_paging/fetch_next.act"
close_act="$APPFS_APP_DIR/_paging/close.act"

assert_file "$events"
assert_file "$resource"
assert_file "$fetch_next"
assert_file "$close_act"
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

wait_for_token_event() {
    token="$1"
    deadline=$(( $(date +%s) + ${APPFS_TIMEOUT_SEC} ))
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

long_handle="$(jq -r '.page.handle_id' "$resource")"
[ "$long_handle" != "null" ] || fail "missing long handle in $resource"
[ -n "$long_handle" ] || fail "empty long handle in $resource"

long_len="$(printf '%s' "$long_handle" | wc -c | tr -d ' ')"
[ "$long_len" -gt 255 ] || fail "fixture handle is not overlong (len=$long_len)"
pass "fixture handle length > 255 bytes ($long_len)"

submit_fetch() {
    handle="$1"
    token="$2"
    wait_writable "$fetch_next" || fail "action sink remained non-writable: $fetch_next"
    printf '{"handle_id":"%s","client_token":"%s"}\n' "$handle" "$token" >> "$fetch_next" || fail "fetch_next submit failed"
    wait_for_token_event "$token" || fail "token event not observed: $token"

    tmp_file="$(mktemp)"
    grep "$token" "$events" > "$tmp_file" || true
    [ -s "$tmp_file" ] || fail "token lines missing: $token"
    line="$(tail -n 1 "$tmp_file")"
    event_type="$(printf '%s\n' "$line" | jq -r '.type')"
    if [ "$event_type" != "action.completed" ]; then
        error_code="$(printf '%s\n' "$line" | jq -r '.error.code // "UNKNOWN"')"
        rm -f "$tmp_file"
        fail "token $token expected action.completed, got $event_type (error.code=$error_code)"
    fi
    handle_out="$(printf '%s\n' "$line" | jq -r '.content.page.handle_id')"
    rm -f "$tmp_file"

    [ "$handle_out" != "null" ] || fail "token $token missing content.page.handle_id"
    [ -n "$handle_out" ] || fail "token $token empty normalized handle"
    SUBMIT_FETCH_RESULT="$handle_out"
}

token_a="ct-long-handle-a-$$"
submit_fetch "$long_handle" "$token_a"
norm_a="$SUBMIT_FETCH_RESULT"
norm_a_len="$(printf '%s' "$norm_a" | wc -c | tr -d ' ')"
[ "$norm_a_len" -le 255 ] || fail "normalized handle too long ($norm_a_len)"
[ "$norm_a" != "$long_handle" ] || fail "overlong handle should be normalized"
pass "overlong handle normalized to <=255 bytes"

token_b="ct-long-handle-b-$$"
submit_fetch "$long_handle" "$token_b"
norm_b="$SUBMIT_FETCH_RESULT"
[ "$norm_b" = "$norm_a" ] || fail "normalization is not deterministic"
pass "normalization is deterministic"

token_close="ct-long-handle-close-$$"
wait_writable "$close_act" || fail "action sink remained non-writable: $close_act"
printf '{"handle_id":"%s","client_token":"%s"}\n' "$long_handle" "$token_close" >> "$close_act" || fail "close submit failed"
wait_for_token_event "$token_close" || fail "close token event not observed"

tmp_close="$(mktemp)"
grep "$token_close" "$events" > "$tmp_close" || true
[ -s "$tmp_close" ] || fail "close token lines missing"
tail -n 1 "$tmp_close" | jq -e '.type=="action.completed"' >/dev/null 2>&1 || fail "close did not complete"
close_id="$(tail -n 1 "$tmp_close" | jq -r '.content.handle_id')"
closed="$(tail -n 1 "$tmp_close" | jq -r '.content.closed')"
rm -f "$tmp_close"
[ "$closed" = "true" ] || fail "close content.closed expected true"
[ "$close_id" = "$norm_a" ] || fail "close returned unexpected handle id"
pass "close accepts overlong handle input via canonical alias"

say "CT-015 done"
