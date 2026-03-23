#!/bin/sh
set -eu

DIR="$(dirname "$0")"

if [ "${APPFS_V2_CONTRACT_TESTS:-0}" != "1" ]; then
    echo "SKIP appfs-v2-contract (set APPFS_V2_CONTRACT_TESTS=1 to enable)"
    exit 0
fi

echo "Running AppFS v2 contract tests (Phase D)..."

required_case_map="
ct2-001 $DIR/appfs-v2/test-ct2-001-startup-prewarm.sh
ct2-002 $DIR/appfs-v2/test-ct2-002-snapshot-hit.sh
ct2-003 $DIR/appfs-v2/test-ct2-003-read-miss-expand.sh
ct2-004 $DIR/appfs-v2/test-ct2-004-concurrent-dedupe.sh
ct2-005 $DIR/appfs-v2/test-ct2-005-snapshot-too-large.sh
ct2-006 $DIR/appfs-v2/test-ct2-006-recovery-incomplete-expand.sh
ct2-007 $DIR/appfs-v2/test-ct2-007-actionline-parse.sh
ct2-008 $DIR/appfs-v2/test-ct2-008-submit-reject.sh
ct2-009 $DIR/appfs-v2/test-ct2-009-dual-shape.sh
"

extended_case_map="
ct2-028 $DIR/appfs-v2/test-ct2-028-timeout-return-stale.sh
"

status=0
pass_count=0
pending_count=0
required_total=0
required_pass=0
extended_total=0
extended_pass=0

normalize_selector_token() {
    raw="$1"
    token="$(printf '%s' "$raw" | tr '[:upper:]' '[:lower:]')"
    case_id="$(printf '%s' "$token" | sed -n 's#.*\(ct2-[0-9][0-9][0-9]\).*#\1#p')"
    [ -n "$case_id" ] || return 1
    printf '%s\n' "$case_id"
}

lookup_case_line() {
    case_map="$1"
    wanted_id="$2"
    while IFS= read -r line; do
        [ -n "$line" ] || continue
        case_id="${line%% *}"
        if [ "$case_id" = "$wanted_id" ]; then
            printf '%s\n' "$line"
            return 0
        fi
    done <<EOF
$case_map
EOF
    return 1
}

select_case_map() {
    case_map="$1"
    selector="$2"
    tier="$3"

    if [ -z "$selector" ]; then
        printf '%s' "$case_map"
        return 0
    fi

    selector_lower="$(printf '%s' "$selector" | tr '[:upper:]' '[:lower:]')"
    if [ "$selector_lower" = "none" ] || [ "$selector_lower" = "off" ]; then
        printf ''
        return 0
    fi

    selected_ids=""
    for raw_token in $(printf '%s' "$selector" | tr ',;' '  '); do
        [ -n "$raw_token" ] || continue
        case_id="$(normalize_selector_token "$raw_token")" || {
            echo "Invalid $tier case selector token: $raw_token" >&2
            exit 1
        }
        case " $selected_ids " in
            *" $case_id "*) ;;
            *) selected_ids="$selected_ids $case_id" ;;
        esac
    done

    [ -n "$selected_ids" ] || {
        echo "Empty $tier case selector: $selector" >&2
        exit 1
    }

    selected_map=""
    for case_id in $selected_ids; do
        line="$(lookup_case_line "$case_map" "$case_id" || true)"
        [ -n "$line" ] || {
            echo "Unknown $tier case selector: $case_id" >&2
            exit 1
        }
        selected_map="${selected_map}${line}
"
    done

    printf '%s' "$selected_map"
}

case_map_to_tests() {
    case_map="$1"
    tests=""
    while IFS= read -r line; do
        [ -n "$line" ] || continue
        path="${line#* }"
        tests="${tests}
$path"
    done <<EOF
$case_map
EOF
    printf '%s\n' "$tests"
}

case_map_ids() {
    case_map="$1"
    ids=""
    sep=""
    while IFS= read -r line; do
        [ -n "$line" ] || continue
        case_id="${line%% *}"
        ids="${ids}${sep}${case_id}"
        sep=","
    done <<EOF
$case_map
EOF
    if [ -n "$ids" ]; then
        printf '%s\n' "$ids"
    else
        printf 'none\n'
    fi
}

selected_required_case_map="$(select_case_map "$required_case_map" "${APPFS_V2_REQUIRED_CASES:-}" "required")"
selected_extended_case_map="$(select_case_map "$extended_case_map" "${APPFS_V2_EXTENDED_CASES:-}" "extended")"
required_tests="$(case_map_to_tests "$selected_required_case_map")"
extended_tests="$(case_map_to_tests "$selected_extended_case_map")"

echo "Case selection: required=$(case_map_ids "$selected_required_case_map") extended=$(case_map_ids "$selected_extended_case_map")"

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
