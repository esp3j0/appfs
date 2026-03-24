#!/bin/sh
set -eu

DIR="$(dirname "$0")"

env_pick() {
    var_v3="$1"
    var_v2="$2"
    default_value="$3"
    eval "value_v3=\${$var_v3:-}"
    if [ -n "$value_v3" ]; then
        printf '%s\n' "$value_v3"
        return 0
    fi
    eval "value_v2=\${$var_v2:-}"
    if [ -n "$value_v2" ]; then
        printf '%s\n' "$value_v2"
        return 0
    fi
    printf '%s\n' "$default_value"
}

contract_tests="$(env_pick APPFS_V3_CONTRACT_TESTS APPFS_V2_CONTRACT_TESTS 0)"
strict_mode="$(env_pick APPFS_V3_STRICT APPFS_V2_STRICT 0)"
build_before_run="$(env_pick APPFS_V3_BUILD_BEFORE_RUN APPFS_V2_BUILD_BEFORE_RUN 1)"
required_selector="$(env_pick APPFS_V3_REQUIRED_CASES APPFS_V2_REQUIRED_CASES "")"
extended_selector="$(env_pick APPFS_V3_EXTENDED_CASES APPFS_V2_EXTENDED_CASES "")"
require_evidence="$(env_pick APPFS_V3_REQUIRE_EVIDENCE APPFS_V2_REQUIRE_EVIDENCE 1)"
evidence_file="$(env_pick APPFS_V3_EVIDENCE_FILE APPFS_V2_EVIDENCE_FILE "")"
evidence_dir="$(env_pick APPFS_V3_EVIDENCE_DIR APPFS_V2_EVIDENCE_DIR "")"

export APPFS_V3_CONTRACT_TESTS="$contract_tests"
export APPFS_V2_CONTRACT_TESTS="$contract_tests"
export APPFS_V3_STRICT="$strict_mode"
export APPFS_V2_STRICT="$strict_mode"
export APPFS_V3_BUILD_BEFORE_RUN="$build_before_run"
export APPFS_V2_BUILD_BEFORE_RUN="$build_before_run"
export APPFS_V3_REQUIRED_CASES="$required_selector"
export APPFS_V2_REQUIRED_CASES="$required_selector"
export APPFS_V3_EXTENDED_CASES="$extended_selector"
export APPFS_V2_EXTENDED_CASES="$extended_selector"
export APPFS_V3_REQUIRE_EVIDENCE="$require_evidence"
export APPFS_V2_REQUIRE_EVIDENCE="$require_evidence"

if [ "$contract_tests" != "1" ]; then
    echo "SKIP appfs-v3-contract (set APPFS_V3_CONTRACT_TESTS=1 to enable; APPFS_V2_CONTRACT_TESTS is alias)"
    exit 0
fi

echo "Running AppFS v3 contract tests (v2-compatible runner surface)..."

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

if [ -z "$evidence_file" ]; then
    if [ -n "${TMPDIR:-}" ]; then
        evidence_file="$(mktemp "$TMPDIR/appfs-v2-evidence.XXXXXX")"
    else
        evidence_file="$(mktemp "/tmp/appfs-v2-evidence.XXXXXX")"
    fi
fi
export APPFS_V3_EVIDENCE_FILE="$evidence_file"
export APPFS_V2_EVIDENCE_FILE="$evidence_file"
: >"$evidence_file"

if [ -z "$evidence_dir" ]; then
    if [ -n "${TMPDIR:-}" ]; then
        evidence_dir="$(mktemp -d "$TMPDIR/appfs-v2-evidence-dir.XXXXXX")"
    else
        evidence_dir="$(mktemp -d "/tmp/appfs-v2-evidence-dir.XXXXXX")"
    fi
fi
export APPFS_V3_EVIDENCE_DIR="$evidence_dir"
export APPFS_V2_EVIDENCE_DIR="$evidence_dir"

record_v2_evidence() {
    key="$1"
    value="${2:-}"
    if [ -n "$value" ]; then
        printf '%s=%s\n' "$key" "$value" >>"$APPFS_V2_EVIDENCE_FILE"
    else
        printf '%s\n' "$key" >>"$APPFS_V2_EVIDENCE_FILE"
    fi
}

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

case_map_has_id() {
    case_map="$1"
    wanted_id="$2"
    while IFS= read -r line; do
        [ -n "$line" ] || continue
        case_id="${line%% *}"
        if [ "$case_id" = "$wanted_id" ]; then
            return 0
        fi
    done <<EOF
$case_map
EOF
    return 1
}

evidence_has_key() {
    key="$1"
    if [ ! -f "$APPFS_V2_EVIDENCE_FILE" ]; then
        return 1
    fi
    while IFS= read -r line; do
        [ -n "$line" ] || continue
        case "$line" in
            "$key"|"$key"=*)
                return 0
                ;;
        esac
    done <"$APPFS_V2_EVIDENCE_FILE"
    return 1
}

assert_required_evidence() {
    expected_key="$1"
    reason="$2"
    if evidence_has_key "$expected_key"; then
        return 0
    fi

    echo "  FAIL (required v2 evidence missing): $expected_key ($reason)"
    echo "  Evidence file: $APPFS_V2_EVIDENCE_FILE"
    echo "  Runtime transport env: http=${APPFS_ADAPTER_HTTP_ENDPOINT:-none} grpc=${APPFS_ADAPTER_GRPC_ENDPOINT:-none}"
    if [ -f "$APPFS_V2_EVIDENCE_FILE" ]; then
        echo "  Collected evidence:"
        sed 's/^/    /' "$APPFS_V2_EVIDENCE_FILE"
    else
        echo "  Collected evidence: <missing file>"
    fi
    status=1
}

extract_runtime_evidence_for_case() {
    case_id="$1"
    case "$case_id" in
        ct2-001)
            log="$APPFS_V2_EVIDENCE_DIR/ct2-001.adapter.log"
            if [ -f "$log" ] && grep -F -q "[prewarm] resource=/chats/chat-001/messages.res.jsonl state=hot" "$log"; then
                record_v2_evidence "connector.prewarm_snapshot_meta" "resource=/chats/chat-001/messages.res.jsonl"
            fi
            ;;
        ct2-003)
            log="$APPFS_V2_EVIDENCE_DIR/ct2-003.adapter.log"
            if [ -f "$log" ] && grep -F -q "[cache.expand] fetch_snapshot_chunk resource=/chats/chat-001/messages.res.jsonl" "$log"; then
                record_v2_evidence "connector.fetch_snapshot_chunk" "resource=/chats/chat-001/messages.res.jsonl"
            fi
            ;;
        ct2-007)
            events="$APPFS_V2_EVIDENCE_DIR/ct2-007.events.evt.jsonl"
            if [ -f "$events" ] && python3 - "$events" <<'PY' >/dev/null 2>&1
import json
import sys

path = sys.argv[1]
with open(path, "r", encoding="utf-8") as f:
    for line in f:
        line = line.strip()
        if not line:
            continue
        try:
            obj = json.loads(line)
        except Exception:
            continue
        if obj.get("type") != "action.completed":
            continue
        content = obj.get("content")
        if isinstance(content, dict) and isinstance(content.get("echo"), dict):
            sys.exit(0)
sys.exit(1)
PY
            then
                record_v2_evidence "connector.submit_action" "path=/contacts/zhangsan/send_message.act"
            fi
            ;;
        ct2-009)
            events="$APPFS_V2_EVIDENCE_DIR/ct2-009.events.evt.jsonl"
            if [ -f "$events" ] && python3 - "$events" <<'PY' >/dev/null 2>&1
import json
import sys

path = sys.argv[1]
with open(path, "r", encoding="utf-8") as f:
    for line in f:
        line = line.strip()
        if not line:
            continue
        try:
            obj = json.loads(line)
        except Exception:
            continue
        if obj.get("type") != "action.completed":
            continue
        content = obj.get("content")
        if not isinstance(content, dict):
            continue
        page = content.get("page")
        if not isinstance(page, dict):
            continue
        if page.get("mode") == "live" and isinstance(page.get("handle_id"), str) and page.get("handle_id"):
            sys.exit(0)
sys.exit(1)
PY
            then
                record_v2_evidence "connector.fetch_live_page" "resource=/feed/recommendations.res.json"
            fi
            ;;
    esac
}

selected_required_case_map="$(select_case_map "$required_case_map" "$required_selector" "required")"
selected_extended_case_map="$(select_case_map "$extended_case_map" "$extended_selector" "extended")"

echo "Case selection: required=$(case_map_ids "$selected_required_case_map") extended=$(case_map_ids "$selected_extended_case_map")"

run_test_case() {
    case_id="$1"
    t="$2"
    tier="$3"

    set +e
    sh "$t"
    rc=$?
    set -e

    if [ "$rc" -eq 0 ]; then
        pass_count=$((pass_count + 1))
        if [ "$tier" = "required" ]; then
            required_pass=$((required_pass + 1))
            extract_runtime_evidence_for_case "$case_id"
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
        if [ "$strict_mode" = "1" ]; then
            status=1
        fi
        return 0
    fi

    echo "  FAIL: $t (exit=$rc)"
    status=1
}

while IFS= read -r line; do
    [ -n "$line" ] || continue
    case_id="${line%% *}"
    t="${line#* }"
    required_total=$((required_total + 1))
    run_test_case "$case_id" "$t" "required"
done <<EOF
$selected_required_case_map
EOF

while IFS= read -r line; do
    [ -n "$line" ] || continue
    case_id="${line%% *}"
    t="${line#* }"
    extended_total=$((extended_total + 1))
    run_test_case "$case_id" "$t" "extended"
done <<EOF
$selected_extended_case_map
EOF

if [ "$required_total" -gt 0 ] && [ "$require_evidence" = "1" ]; then
    if case_map_has_id "$selected_required_case_map" "ct2-001"; then
        assert_required_evidence "connector.prewarm_snapshot_meta" "CT2-001 should hit startup prewarm via V2 connector"
    fi
    if case_map_has_id "$selected_required_case_map" "ct2-003"; then
        assert_required_evidence "connector.fetch_snapshot_chunk" "CT2-003 should expand snapshot through V2 connector"
    fi
    if case_map_has_id "$selected_required_case_map" "ct2-007"; then
        assert_required_evidence "connector.submit_action" "CT2-007 should submit actions through V2 connector"
    fi
    if case_map_has_id "$selected_required_case_map" "ct2-009"; then
        assert_required_evidence "connector.fetch_live_page" "CT2-009 should page live resource through V2 connector"
    fi
fi

echo "AppFS v3 contract summary: pass=$pass_count pending=$pending_count strict=$strict_mode required_pass=$required_pass/$required_total extended_pass=$extended_pass/$extended_total"

if [ "$status" -ne 0 ]; then
    echo "AppFS v3 contract suite: FAILED"
    exit "$status"
fi

echo "AppFS v3 contract suite: OK"
