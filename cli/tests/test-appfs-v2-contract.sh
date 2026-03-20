#!/bin/sh
set -eu

DIR="$(dirname "$0")"

if [ "${APPFS_V2_CONTRACT_TESTS:-0}" != "1" ]; then
    echo "SKIP appfs-v2-contract (set APPFS_V2_CONTRACT_TESTS=1 to enable)"
    exit 0
fi

echo "Running AppFS v2 contract skeleton tests..."

tests="
$DIR/appfs-v2/test-ct2-002-snapshot-hit.sh
$DIR/appfs-v2/test-ct2-003-read-miss-expand.sh
$DIR/appfs-v2/test-ct2-005-snapshot-too-large.sh
$DIR/appfs-v2/test-ct2-007-actionline-parse.sh
$DIR/appfs-v2/test-ct2-008-submit-reject.sh
$DIR/appfs-v2/test-ct2-009-dual-shape.sh
"

status=0
pass_count=0
pending_count=0

for t in $tests; do
    set +e
    sh "$t"
    rc=$?
    set -e

    if [ "$rc" -eq 0 ]; then
        pass_count=$((pass_count + 1))
        continue
    fi

    if [ "$rc" -eq 2 ]; then
        pending_count=$((pending_count + 1))
        echo "  XFAIL (skeleton pending): $t"
        if [ "${APPFS_V2_STRICT:-0}" = "1" ]; then
            status=1
        fi
        continue
    fi

    echo "  FAIL: $t (exit=$rc)"
    status=1
done

echo "AppFS v2 skeleton summary: pass=$pass_count pending=$pending_count strict=${APPFS_V2_STRICT:-0}"

if [ "$status" -ne 0 ]; then
    echo "AppFS v2 contract skeleton: FAILED"
    exit "$status"
fi

echo "AppFS v2 contract skeleton: OK"
