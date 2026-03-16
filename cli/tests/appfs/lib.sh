#!/bin/sh
set -eu

APPFS_ROOT="${APPFS_ROOT:-/app}"
APPFS_APP_ID="${APPFS_APP_ID:-aiim}"
APPFS_APP_DIR="${APPFS_ROOT%/}/${APPFS_APP_ID}"
APPFS_TIMEOUT_SEC="${APPFS_TIMEOUT_SEC:-10}"
APPFS_TEST_ACTION="${APPFS_TEST_ACTION:-${APPFS_APP_DIR}/contacts/zhangsan/send_message.act}"
APPFS_PAGEABLE_RESOURCE="${APPFS_PAGEABLE_RESOURCE:-${APPFS_APP_DIR}/chats/chat-001/messages.res.json}"
APPFS_LONG_HANDLE_RESOURCE="${APPFS_LONG_HANDLE_RESOURCE:-${APPFS_APP_DIR}/chats/chat-long/messages.res.json}"

say() {
    printf '%s\n' "$*"
}

pass() {
    say "  OK   $*"
}

fail() {
    say "  FAIL $*"
    exit 1
}

skip() {
    say "  SKIP $*"
    exit 0
}

require_cmd() {
    command -v "$1" >/dev/null 2>&1 || fail "missing command: $1"
}

assert_exists() {
    path="$1"
    [ -e "$path" ] || fail "missing path: $path"
    pass "exists: $path"
}

assert_file() {
    path="$1"
    [ -f "$path" ] || fail "missing file: $path"
    pass "file: $path"
}

run_expect_fail() {
    if "$@"; then
        fail "command should fail: $*"
    fi
    pass "expected failure: $*"
}

wait_for_line_growth() {
    file="$1"
    before="$2"
    timeout="${3:-$APPFS_TIMEOUT_SEC}"
    i=0
    while [ "$i" -lt "$timeout" ]; do
        now="$(wc -l < "$file" 2>/dev/null || echo 0)"
        if [ "$now" -gt "$before" ]; then
            printf '%s\n' "$now"
            return 0
        fi
        i=$((i + 1))
        sleep 1
    done
    return 1
}

assert_json_key() {
    file="$1"
    jq_expr="$2"
    require_cmd jq
    jq -e "$jq_expr" "$file" >/dev/null 2>&1 || fail "missing json key '$jq_expr' in $file"
    pass "json key '$jq_expr' in $file"
}

banner() {
    say "================================================"
    say "  $1"
    say "================================================"
}
