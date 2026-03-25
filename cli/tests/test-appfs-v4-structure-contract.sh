#!/bin/sh
set -eu

DIR="$(dirname "$0")"

structure_tests="${APPFS_V4_STRUCTURE_TESTS:-0}"
require_evidence="${APPFS_V4_REQUIRE_EVIDENCE:-1}"
evidence_file="${APPFS_V4_EVIDENCE_FILE:-}"
evidence_dir="${APPFS_V4_EVIDENCE_DIR:-}"
build_before_run="${APPFS_V4_BUILD_BEFORE_RUN:-1}"

if [ "$structure_tests" != "1" ]; then
    echo "SKIP appfs-v4-structure-contract (set APPFS_V4_STRUCTURE_TESTS=1 to enable)"
    exit 0
fi

required_case_map="
st4-001 $DIR/appfs-v4/test-st4-001-initialize.sh
st4-002 $DIR/appfs-v4/test-st4-002-enter-scope.sh
st4-003 $DIR/appfs-v4/test-st4-003-failure-recovery.sh
"

status=0
pass_count=0
required_total=0
required_pass=0

if [ -z "$evidence_file" ]; then
    if [ -n "${TMPDIR:-}" ]; then
        evidence_file="$(mktemp "$TMPDIR/appfs-v4-evidence.XXXXXX")"
    else
        evidence_file="$(mktemp "/tmp/appfs-v4-evidence.XXXXXX")"
    fi
fi
export APPFS_V4_EVIDENCE_FILE="$evidence_file"
: >"$evidence_file"
export APPFS_V2_EVIDENCE_FILE="$evidence_file"
export APPFS_V3_BUILD_BEFORE_RUN="$build_before_run"
export APPFS_V2_BUILD_BEFORE_RUN="$build_before_run"

if [ -z "$evidence_dir" ]; then
    if [ -n "${TMPDIR:-}" ]; then
        evidence_dir="$(mktemp -d "$TMPDIR/appfs-v4-evidence-dir.XXXXXX")"
    else
        evidence_dir="$(mktemp -d "/tmp/appfs-v4-evidence-dir.XXXXXX")"
    fi
fi
export APPFS_V4_EVIDENCE_DIR="$evidence_dir"
export APPFS_V2_EVIDENCE_DIR="$evidence_dir"

record_v4_evidence() {
    key="$1"
    value="${2:-}"
    if [ -n "$value" ]; then
        printf '%s=%s\n' "$key" "$value" >>"$APPFS_V4_EVIDENCE_FILE"
    else
        printf '%s\n' "$key" >>"$APPFS_V4_EVIDENCE_FILE"
    fi
}

evidence_has_key() {
    key="$1"
    while IFS= read -r line; do
        [ -n "$line" ] || continue
        case "$line" in
            "$key"|"$key"=*)
                return 0
                ;;
        esac
    done <"$APPFS_V4_EVIDENCE_FILE"
    return 1
}

assert_required_evidence() {
    expected_key="$1"
    reason="$2"
    if evidence_has_key "$expected_key"; then
        return 0
    fi
    echo "  FAIL (required v4 evidence missing): $expected_key ($reason)"
    echo "  Evidence file: $APPFS_V4_EVIDENCE_FILE"
    echo "  Collected evidence:"
    sed 's/^/    /' "$APPFS_V4_EVIDENCE_FILE"
    status=1
}

extract_runtime_evidence_for_case() {
    case_id="$1"
    case "$case_id" in
        st4-001)
            log="$APPFS_V4_EVIDENCE_DIR/st4-001.adapter.log"
            if [ -f "$log" ] && grep -F -q "[structure.sync] op=get_app_structure app=aiim" "$log"; then
                record_v4_evidence "connector.get_app_structure" "app=aiim"
            fi
            ;;
        st4-002)
            log="$APPFS_V4_EVIDENCE_DIR/st4-002.adapter.log"
            if [ -f "$log" ] && grep -F -q "[structure.sync] op=refresh_app_structure app=aiim reason=enter_scope target_scope=chat-long" "$log"; then
                record_v4_evidence "connector.refresh_app_structure" "reason=enter_scope target_scope=chat-long"
            fi
            ;;
    esac
}

run_test_case() {
    case_id="$1"
    t="$2"
    set +e
    sh "$t"
    rc=$?
    set -e

    if [ "$rc" -eq 0 ]; then
        pass_count=$((pass_count + 1))
        required_pass=$((required_pass + 1))
        extract_runtime_evidence_for_case "$case_id"
        return 0
    fi

    echo "  FAIL: $t (exit=$rc)"
    status=1
}

echo "Running AppFS v4 structure contract tests..."
while IFS= read -r line; do
    [ -n "$line" ] || continue
    case_id="${line%% *}"
    t="${line#* }"
    required_total=$((required_total + 1))
    run_test_case "$case_id" "$t"
done <<EOF
$required_case_map
EOF

if [ "$required_total" -gt 0 ] && [ "$require_evidence" = "1" ]; then
    assert_required_evidence "connector.get_app_structure" "initial bootstrap should hit get_app_structure"
    assert_required_evidence "connector.refresh_app_structure" "enter_scope should hit refresh_app_structure"
fi

echo "AppFS v4 structure contract summary: pass=$pass_count required_pass=$required_pass/$required_total"
if [ "$status" -ne 0 ]; then
    echo "AppFS v4 structure contract suite: FAILED"
    exit "$status"
fi

echo "AppFS v4 structure contract suite: OK"
