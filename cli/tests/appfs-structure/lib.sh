#!/bin/sh
set -eu

DIR="$(dirname "$0")"
# shellcheck disable=SC1091
. "$DIR/../appfs-connector/lib.sh"

wait_path_exists() {
    path="$1"
    timeout="${2:-10}"
    i=0
    while [ "$i" -lt "$timeout" ]; do
        if [ -e "$path" ]; then
            return 0
        fi
        i=$((i + 1))
        sleep 1
    done
    return 1
}

wait_log_contains() {
    pattern="$1"
    log_path="$2"
    timeout="${3:-10}"
    i=0
    while [ "$i" -lt "$timeout" ]; do
        if [ -f "$log_path" ] && grep -F -q "$pattern" "$log_path"; then
            return 0
        fi
        i=$((i + 1))
        sleep 1
    done
    return 1
}

wait_token_type_count() {
    token="$1"
    event_type="$2"
    min_count="$3"
    file="$4"
    timeout="${5:-15}"
    i=0
    while [ "$i" -lt "$timeout" ]; do
        count="$(grep "$token" "$file" 2>/dev/null | grep -c "\"type\":\"$event_type\"" || true)"
        [ -n "$count" ] || count=0
        if [ "$count" -ge "$min_count" ]; then
            return 0
        fi
        i=$((i + 1))
        sleep 1
    done
    return 1
}

event_line_for_token() {
    token="$1"
    file="$2"
    grep "$token" "$file" 2>/dev/null | tail -n 1 || true
}

assert_json_expr() {
    json_payload="$1"
    expr="$2"
    description="$3"
    if ! printf '%s\n' "$json_payload" | python3 -c 'import json,sys; expr=sys.argv[1]; obj=json.loads(sys.stdin.read()); raise SystemExit(0 if eval(expr, {"obj": obj}) else 1)' "$expr"
    then
        fail "$description"
    fi
}

assert_token_event_type() {
    token="$1"
    file="$2"
    expected_type="$3"
    line="$(event_line_for_token "$token" "$file")"
    [ -n "$line" ] || fail "missing event line for token=$token"
    assert_json_expr "$line" "obj.get('type') == '$expected_type'" "token $token did not emit $expected_type"
}

assert_token_error_code() {
    token="$1"
    file="$2"
    expected_code="$3"
    line="$(event_line_for_token "$token" "$file")"
    [ -n "$line" ] || fail "missing event line for token=$token"
    assert_json_expr "$line" "isinstance(obj.get('error'), dict) and obj.get('error', {}).get('code') == '$expected_code'" "token $token did not emit error code $expected_code"
}
