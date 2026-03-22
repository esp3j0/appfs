#!/bin/sh
set -eu

DIR="$(dirname "$0")"

if [ "${APPFS_V2_CONTRACT_TESTS:-0}" != "1" ]; then
    echo "SKIP appfs-v2-contract (set APPFS_V2_CONTRACT_TESTS=1 to enable)"
    exit 0
fi

echo "Running AppFS v2 contract tests (Phase D)..."

required_tests="
$DIR/appfs-v2/test-ct2-001-startup-prewarm.sh
$DIR/appfs-v2/test-ct2-002-snapshot-hit.sh
$DIR/appfs-v2/test-ct2-003-read-miss-expand.sh
$DIR/appfs-v2/test-ct2-004-concurrent-dedupe.sh
$DIR/appfs-v2/test-ct2-005-snapshot-too-large.sh
$DIR/appfs-v2/test-ct2-006-recovery-incomplete-expand.sh
$DIR/appfs-v2/test-ct2-007-actionline-parse.sh
$DIR/appfs-v2/test-ct2-008-submit-reject.sh
$DIR/appfs-v2/test-ct2-009-dual-shape.sh
"

extended_tests="
$DIR/appfs-v2/test-ct2-028-timeout-return-stale.sh
"

status=0
pass_count=0
pending_count=0
required_total=0
required_pass=0
extended_total=0
extended_pass=0

run_test_case() {
    t="$1"
    tier="$2"

    set +e
    sh "$t"
    rc=$?
    set -e

    if [ "$rc" -eq 0 ]; then
        pass_count=$((pass_count + 1))
        if [ "$tier" = "required" ]; then
            required_pass=$((required_pass + 1))
        else
            extended_pass=$((extended_pass + 1))
        fi
        return 0
    fi

    if [ "$rc" -eq 2 ]; then
        pending_count=$((pending_count + 1))
        if [ "$tier" = "required" ]; then
            echo "  FAIL (required set must not be pending): $t"
            status=1
            return 0
        fi

        echo "  PENDING (extended coverage): $t"
        if [ "${APPFS_V2_STRICT:-0}" = "1" ]; then
            status=1
        fi
        return 0
    fi

    echo "  FAIL: $t (exit=$rc)"
    status=1
}

for t in $required_tests; do
    required_total=$((required_total + 1))
    run_test_case "$t" "required"
done

for t in $extended_tests; do
    extended_total=$((extended_total + 1))
    run_test_case "$t" "extended"
done

echo "AppFS v2 contract summary: pass=$pass_count pending=$pending_count strict=${APPFS_V2_STRICT:-0} required_pass=$required_pass/$required_total extended_pass=$extended_pass/$extended_total"

if [ "$status" -ne 0 ]; then
    echo "AppFS v2 contract suite: FAILED"
    exit "$status"
fi

echo "AppFS v2 contract suite: OK"
