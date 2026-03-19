#!/bin/sh
set -eu

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
# shellcheck source=./lib.sh
. "$SCRIPT_DIR/lib.sh"

banner "AppFS CT-022 Snapshot Too-Large Error Mapping"

events="$APPFS_APP_DIR/_stream/events.evt.jsonl"
refresh_act="$APPFS_APP_DIR/_snapshot/refresh.act"
oversize_resource="${APPFS_OVERSIZE_SNAPSHOT_RESOURCE:-$APPFS_APP_DIR/chats/chat-oversize/messages.res.jsonl}"

assert_file "$events"
assert_file "$refresh_act"
assert_file "$oversize_resource"
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

resource_rel="${oversize_resource#${APPFS_APP_DIR}/}"
resource_rel="/${resource_rel}"
token="ct-snapshot-too-large-$$"

wait_writable "$refresh_act" || fail "snapshot refresh sink remained non-writable: $refresh_act"
printf '{"resource_path":"%s","client_token":"%s"}\n' "$resource_rel" "$token" >> "$refresh_act" || fail "snapshot refresh submit failed"
wait_for_token_event "$token" || fail "snapshot too-large event did not arrive in time"

tmp_file="$(mktemp)"
grep "$token" "$events" > "$tmp_file" || true
[ -s "$tmp_file" ] || fail "token lines missing for snapshot too-large check"
tail -n 1 "$tmp_file" | jq -e '.type=="action.failed"' >/dev/null 2>&1 || fail "snapshot too-large did not emit action.failed"
actual_code="$(tail -n 1 "$tmp_file" | jq -r '.error.code')"
rm -f "$tmp_file"
[ "$actual_code" = "SNAPSHOT_TOO_LARGE" ] || fail "expected SNAPSHOT_TOO_LARGE, got $actual_code"
pass "snapshot too-large mapped to action.failed/SNAPSHOT_TOO_LARGE"

say "CT-022 done"
