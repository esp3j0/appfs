#!/bin/sh
set -eu

DIR="$(dirname "$0")"
# shellcheck disable=SC1091
. "$DIR/lib.sh"

SCRIPT_DIR="$(CDPATH= cd -- "$DIR" && pwd)"
CLI_DIR="$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)"
REPO_DIR="$(CDPATH= cd -- "$CLI_DIR/.." && pwd)"
AGENTFS_BIN="${AGENTFS_BIN:-}"

TMP_ROOT=""
ADAPTER_PID=""
ADAPTER_LOG=""

APP_DIR=""
SNAPSHOT_FILE=""
LIVE_FILE=""
SEND_ACT=""
REFRESH_ACT=""
FETCH_NEXT_ACT=""
EVENTS_FILE=""
SUMMARY_JSON=""

cleanup() {
    stop_adapter_process "${ADAPTER_PID:-}" "${AGENTFS_BIN:-}" "${TMP_ROOT:-}"
    if [ -n "${TMP_ROOT:-}" ] && [ -d "$TMP_ROOT" ]; then
        rm -rf "$TMP_ROOT"
    fi
}
trap cleanup EXIT INT TERM

stop_adapter() {
    stop_adapter_process "${ADAPTER_PID:-}" "${AGENTFS_BIN:-}" "${TMP_ROOT:-}"
    ADAPTER_PID=""
}

wait_writable() {
    path="$1"
    timeout="${2:-10}"
    i=0
    while [ "$i" -lt "$timeout" ]; do
        if [ -w "$path" ]; then
            return 0
        fi
        i=$((i + 1))
        sleep 1
    done
    return 1
}

wait_token_type_event() {
    token="$1"
    event_type="$2"
    file="$3"
    timeout="${4:-20}"
    i=0
    while [ "$i" -lt "$timeout" ]; do
        if grep "$token" "$file" 2>/dev/null | grep -q "\"type\":\"$event_type\""; then
            return 0
        fi
        i=$((i + 1))
        sleep 1
    done
    return 1
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

detect_platform() {
    kernel="$(uname -s 2>/dev/null || echo unknown)"
    case "$kernel" in
        Linux*) printf '%s\n' "linux" ;;
        Darwin*) printf '%s\n' "macos" ;;
        CYGWIN*|MINGW*|MSYS*) printf '%s\n' "windows" ;;
        *) printf '%s\n' "unknown" ;;
    esac
}

reload_fixture_app() {
    rm -rf "$TMP_ROOT/aiim"
    cp -R "$REPO_DIR/examples/appfs/aiim" "$TMP_ROOT/"
    APP_DIR="$TMP_ROOT/aiim"
    SNAPSHOT_FILE="$APP_DIR/chats/chat-001/messages.res.jsonl"
    LIVE_FILE="$APP_DIR/feed/recommendations.res.json"
    SEND_ACT="$APP_DIR/contacts/zhangsan/send_message.act"
    REFRESH_ACT="$APP_DIR/_snapshot/refresh.act"
    FETCH_NEXT_ACT="$APP_DIR/_paging/fetch_next.act"
    EVENTS_FILE="$APP_DIR/_stream/events.evt.jsonl"
}

start_adapter() {
    strict_actionline="${1:-0}"
    ADAPTER_LOG="$TMP_ROOT/appfs-adapter.log"
    runtime_root="$TMP_ROOT"
    case "$AGENTFS_BIN" in
        *.exe)
            win_bin="$AGENTFS_BIN"
            if command -v wslpath >/dev/null 2>&1; then
                runtime_root="$(wslpath -w "$TMP_ROOT")"
                win_bin="$(wslpath -w "$AGENTFS_BIN")"
            elif command -v cygpath >/dev/null 2>&1; then
                runtime_root="$(cygpath -w "$TMP_ROOT")"
                win_bin="$(cygpath -w "$AGENTFS_BIN")"
            fi
            if [ "$strict_actionline" = "1" ]; then
                cmd.exe /C "set APPFS_V2_ACTIONLINE_STRICT=1&& $win_bin serve appfs --root $runtime_root --app-id aiim --poll-ms 50" >"$ADAPTER_LOG" 2>&1 &
            else
                cmd.exe /C "$win_bin serve appfs --root $runtime_root --app-id aiim --poll-ms 50" >"$ADAPTER_LOG" 2>&1 &
            fi
            ;;
        *)
            if [ "$strict_actionline" = "1" ]; then
                APPFS_V2_ACTIONLINE_STRICT=1 "$AGENTFS_BIN" serve appfs --root "$runtime_root" --app-id aiim --poll-ms 50 >"$ADAPTER_LOG" 2>&1 &
            else
                "$AGENTFS_BIN" serve appfs --root "$runtime_root" --app-id aiim --poll-ms 50 >"$ADAPTER_LOG" 2>&1 &
            fi
            ;;
    esac
    ADAPTER_PID=$!
    sleep 1
    if ! kill -0 "$ADAPTER_PID" 2>/dev/null; then
        tail -n 120 "$ADAPTER_LOG" 2>/dev/null || true
        fail "appfs adapter failed to start"
    fi
}

banner "AppFS v2 CT2-010 Minimal Cross-Platform Consistency Matrix"
require_cmd python3
ensure_agentfs_bin "$CLI_DIR"

mkdir -p "$CLI_DIR/target"
TMP_ROOT="$(mktemp -d "$CLI_DIR/target/ct2-v2-010.XXXXXX")"
SUMMARY_JSON="$TMP_ROOT/ct2-010-summary.json"

reload_fixture_app
assert_file "$SNAPSHOT_FILE"
assert_file "$LIVE_FILE"
assert_file "$SEND_ACT"
assert_file "$REFRESH_ACT"
assert_file "$FETCH_NEXT_ACT"
assert_file "$EVENTS_FILE"

# 1) ActionLineV2 accept/reject basics (strict mode for deterministic submit-time semantics)
start_adapter 1
pass "adapter started (strict ActionLineV2 mode)"

wait_writable "$SEND_ACT" 10 || fail "action sink remained non-writable: $SEND_ACT"
token_accept="ct2-010-accept-$$"
printf '{"version":"2.0","client_token":"%s","payload":{"text":"cross-platform hello"}}\n' "$token_accept" >> "$SEND_ACT" || fail "failed to append ActionLineV2 accept payload"
wait_token_type_event "$token_accept" "action.completed" "$EVENTS_FILE" 20 || fail "ActionLineV2 accept did not emit action.completed"
accept_event="$(grep "$token_accept" "$EVENTS_FILE" 2>/dev/null | grep "\"type\":\"action.completed\"" | tail -n 1 || true)"
[ -n "$accept_event" ] || fail "missing accept event line"
pass "ActionLineV2 accept path is consistent"

# 3) ActionLineV2 reject basics (must not emit action.accepted/completed for rejected submit)
token_reject="ct2-010-reject-$$"
before_reject_count="$(grep -c "$token_reject" "$EVENTS_FILE" 2>/dev/null || true)"
[ -n "$before_reject_count" ] || before_reject_count=0
printf '{"version":"2.0","client_token":"%s"}\n' "$token_reject" >> "$SEND_ACT" || fail "failed to append ActionLineV2 reject payload"
sleep 2
after_reject_count="$(grep -c "$token_reject" "$EVENTS_FILE" 2>/dev/null || true)"
[ -n "$after_reject_count" ] || after_reject_count=0
[ "$before_reject_count" = "$after_reject_count" ] || fail "rejected ActionLineV2 payload should not emit token-correlated events"
pass "ActionLineV2 reject path is consistent (no token event emitted)"

stop_adapter

# 2) dual-shape + error surface + windows backslash path (default runtime mode)
start_adapter 0
pass "adapter restarted (default mode)"

# 2.1) snapshot/live dual-shape minimal consistency
first_snapshot_line="$(head -n 1 "$SNAPSHOT_FILE")"
assert_json_expr "$first_snapshot_line" 'isinstance(obj, dict) and "page" not in obj and "id" in obj' "snapshot shape must be pure JSONL object line"
live_payload="$(cat "$LIVE_FILE")"
assert_json_expr "$live_payload" 'isinstance(obj, dict) and isinstance(obj.get("items"), list) and isinstance(obj.get("page"), dict) and obj.get("page", {}).get("mode") == "live"' "live shape must expose {items,page} envelope with mode=live"
LIVE_MODE="$(printf '%s\n' "$live_payload" | python3 -c 'import json,sys; print(json.loads(sys.stdin.read()).get("page", {}).get("mode", ""))')"
pass "dual-shape baseline is consistent"

# 2.2) events/error surface minimal consistency
wait_writable "$FETCH_NEXT_ACT" 10 || fail "paging fetch_next sink remained non-writable: $FETCH_NEXT_ACT"
token_error="ct2-010-error-$$"
printf '{"handle_id":"ph_missing_001","client_token":"%s"}\n' "$token_error" >> "$FETCH_NEXT_ACT" || fail "failed to submit paging request for missing handle"
wait_token_type_event "$token_error" "action.failed" "$EVENTS_FILE" 20 || fail "missing-handle paging request should emit action.failed"
error_event="$(grep "$token_error" "$EVENTS_FILE" 2>/dev/null | grep "\"type\":\"action.failed\"" | tail -n 1 || true)"
[ -n "$error_event" ] || fail "missing missing-handle paging error event"
assert_json_expr "$error_event" 'obj.get("error", {}).get("code") == "PAGER_HANDLE_NOT_FOUND"' "missing-handle paging error code mismatch"
ERROR_CODE="$(printf '%s\n' "$error_event" | python3 -c 'import json,sys; print(json.loads(sys.stdin.read()).get("error", {}).get("code", ""))')"
pass "error surface is consistent (PAGER_HANDLE_NOT_FOUND)"

# 2.3) Windows path separator scenario (\ -> / normalization)
token_winpath="ct2-010-winpath-$$"
win_resource_path='chats\\chat-001\\messages.res.jsonl'
printf '{"resource_path":"%s","client_token":"%s"}\n' "$win_resource_path" "$token_winpath" >> "$REFRESH_ACT" || fail "failed to submit backslash resource path"
wait_token_type_event "$token_winpath" "action.completed" "$EVENTS_FILE" 20 || fail "backslash resource path should complete"
winpath_event="$(grep "$token_winpath" "$EVENTS_FILE" 2>/dev/null | grep "\"type\":\"action.completed\"" | tail -n 1 || true)"
[ -n "$winpath_event" ] || fail "missing backslash normalization event"
assert_json_expr "$winpath_event" 'obj.get("content", {}).get("resource_path") == "/chats/chat-001/messages.res.jsonl"' "backslash resource path should normalize to forward slash path"
WINDOWS_PATH_OBSERVED="$(printf '%s\n' "$winpath_event" | python3 -c 'import json,sys; print(json.loads(sys.stdin.read()).get("content", {}).get("resource_path", ""))')"
pass "Windows path separator behavior is consistent"

PLATFORM="$(detect_platform)"
python3 - "$SUMMARY_JSON" "$PLATFORM" "$LIVE_MODE" "$ERROR_CODE" "$WINDOWS_PATH_OBSERVED" <<'PY'
import json
import sys

summary_path, platform, live_mode, error_code, windows_path = sys.argv[1:6]
doc = {
    "platform": platform,
    "checks": {
        "actionline_accept": True,
        "actionline_reject_no_event": True,
        "dual_shape_snapshot_jsonl": True,
        "dual_shape_live_envelope": True,
        "error_surface_minimal": True,
        "windows_backslash_normalized": True,
    },
    "observed": {
        "live_mode": live_mode,
        "error_code": error_code,
        "windows_normalized_path": windows_path,
    },
}
with open(summary_path, "w", encoding="utf-8") as f:
    json.dump(doc, f, ensure_ascii=False, indent=2)
    f.write("\n")
PY

if [ -n "${APPFS_V2_CT2_010_REFERENCE:-}" ] && [ -f "${APPFS_V2_CT2_010_REFERENCE:-}" ]; then
    if ! python3 - "$SUMMARY_JSON" "$APPFS_V2_CT2_010_REFERENCE" <<'PY'
import json
import sys

current_path, reference_path = sys.argv[1:3]
with open(current_path, "r", encoding="utf-8") as f:
    current = json.load(f)
with open(reference_path, "r", encoding="utf-8") as f:
    ref = json.load(f)

diffs = []
for key in ("live_mode", "error_code", "windows_normalized_path"):
    cur_v = current.get("observed", {}).get(key)
    ref_v = ref.get("observed", {}).get(key)
    if cur_v != ref_v:
        diffs.append(f"observed.{key}: current={cur_v!r} reference={ref_v!r}")

if diffs:
    print("CT2-010 cross-platform diff detected:")
    for line in diffs:
        print(f"  - {line}")
    sys.exit(1)
PY
    then
        fail "CT2-010 minimal matrix differs from reference: $APPFS_V2_CT2_010_REFERENCE"
    fi
    pass "reference comparison passed: $APPFS_V2_CT2_010_REFERENCE"
fi

if [ -n "${APPFS_V2_CT2_010_REFERENCE_OUT:-}" ]; then
    cp "$SUMMARY_JSON" "$APPFS_V2_CT2_010_REFERENCE_OUT"
    pass "wrote CT2-010 reference summary: $APPFS_V2_CT2_010_REFERENCE_OUT"
fi

pass "CT2-010 minimal matrix summary: $SUMMARY_JSON"
say "CT2-010 done"
