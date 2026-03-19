#!/bin/sh
set -eu

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
CLI_DIR="$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)"
REPO_DIR="$(CDPATH= cd -- "$CLI_DIR/.." && pwd)"

APPFS_FIXTURE_DIR="${APPFS_FIXTURE_DIR:-$REPO_DIR/examples/appfs}"
APPFS_LIVE_AGENT_ID="${APPFS_LIVE_AGENT_ID:-appfs-live-$$}"
APPFS_LIVE_MOUNTPOINT="${APPFS_LIVE_MOUNTPOINT:-/tmp/agentfs-appfs-live-$$}"
APPFS_APP_ID="${APPFS_APP_ID:-aiim}"
APPFS_ADAPTER_POLL_MS="${APPFS_ADAPTER_POLL_MS:-100}"
APPFS_ADAPTER_RECONCILE_POLL_MS="${APPFS_ADAPTER_RECONCILE_POLL_MS:-1000}"
APPFS_ADAPTER_HTTP_ENDPOINT="${APPFS_ADAPTER_HTTP_ENDPOINT:-}"
APPFS_ADAPTER_HTTP_TIMEOUT_MS="${APPFS_ADAPTER_HTTP_TIMEOUT_MS:-5000}"
APPFS_ADAPTER_GRPC_ENDPOINT="${APPFS_ADAPTER_GRPC_ENDPOINT:-}"
APPFS_ADAPTER_GRPC_TIMEOUT_MS="${APPFS_ADAPTER_GRPC_TIMEOUT_MS:-5000}"
APPFS_ADAPTER_BRIDGE_MAX_RETRIES="${APPFS_ADAPTER_BRIDGE_MAX_RETRIES:-2}"
APPFS_ADAPTER_BRIDGE_INITIAL_BACKOFF_MS="${APPFS_ADAPTER_BRIDGE_INITIAL_BACKOFF_MS:-100}"
APPFS_ADAPTER_BRIDGE_MAX_BACKOFF_MS="${APPFS_ADAPTER_BRIDGE_MAX_BACKOFF_MS:-1000}"
APPFS_ADAPTER_BRIDGE_CIRCUIT_BREAKER_FAILURES="${APPFS_ADAPTER_BRIDGE_CIRCUIT_BREAKER_FAILURES:-5}"
APPFS_ADAPTER_BRIDGE_CIRCUIT_BREAKER_COOLDOWN_MS="${APPFS_ADAPTER_BRIDGE_CIRCUIT_BREAKER_COOLDOWN_MS:-3000}"
APPFS_BRIDGE_RESILIENCE_CONTRACT="${APPFS_BRIDGE_RESILIENCE_CONTRACT:-0}"
APPFS_BRIDGE_RESILIENCE_COOLDOWN_WAIT_SEC="${APPFS_BRIDGE_RESILIENCE_COOLDOWN_WAIT_SEC:-4}"
APPFS_BRIDGE_RESILIENCE_CONTACT_PREFIX="${APPFS_BRIDGE_RESILIENCE_CONTACT_PREFIX:-resilience-}"
APPFS_BRIDGE_FAULT_CONFIG_PATH="${APPFS_BRIDGE_FAULT_CONFIG_PATH:-/tmp/appfs-bridge-fault-config.json}"
APPFS_BRIDGE_RESILIENCE_MIN_BREAKER_COOLDOWN_MS="${APPFS_BRIDGE_RESILIENCE_MIN_BREAKER_COOLDOWN_MS:-4000}"
APPFS_TIMEOUT_SEC="${APPFS_TIMEOUT_SEC:-20}"
APPFS_MOUNT_WAIT_SEC="${APPFS_MOUNT_WAIT_SEC:-20}"
APPFS_MOUNT_LOG="${APPFS_MOUNT_LOG:-$CLI_DIR/appfs-mount-live.log}"
APPFS_ADAPTER_LOG="${APPFS_ADAPTER_LOG:-$CLI_DIR/appfs-adapter-live.log}"
APPFS_TEST_ACTION_LIVE=""
APPFS_STREAMING_ACTION_LIVE=""
APPFS_PAGEABLE_RESOURCE_LIVE=""
APPFS_EXPIRED_PAGEABLE_RESOURCE_LIVE=""
APPFS_LONG_HANDLE_RESOURCE_LIVE=""
APPFS_SNAPSHOT_RESOURCE_LIVE=""
APPFS_OVERSIZE_SNAPSHOT_RESOURCE_LIVE=""

MOUNT_PID=""
ADAPTER_PID=""

if [ "${APPFS_BRIDGE_RESILIENCE_CONTRACT:-0}" = "1" ]; then
    if [ "$APPFS_ADAPTER_BRIDGE_CIRCUIT_BREAKER_COOLDOWN_MS" -lt "$APPFS_BRIDGE_RESILIENCE_MIN_BREAKER_COOLDOWN_MS" ]; then
        APPFS_ADAPTER_BRIDGE_CIRCUIT_BREAKER_COOLDOWN_MS="$APPFS_BRIDGE_RESILIENCE_MIN_BREAKER_COOLDOWN_MS"
    fi
fi

say() {
    printf '%s\n' "$*"
}

pass() {
    say "  OK   $*"
}

fail() {
    say "FAILED: $*"
    exit 1
}

banner() {
    say "================================================"
    say "  $1"
    say "================================================"
}

endpoint_host_port() {
    endpoint="$1"
    trimmed="${endpoint#*://}"
    authority="${trimmed%%/*}"
    host="${authority%%:*}"
    port="${authority##*:}"
    if [ -z "$host" ] || [ -z "$port" ] || [ "$port" = "$authority" ]; then
        fail "invalid bridge endpoint (expected scheme://host:port): $endpoint"
    fi
    printf '%s %s\n' "$host" "$port"
}

wait_tcp_ready() {
    host="$1"
    port="$2"
    timeout="${3:-20}"
    i=0
    while [ "$i" -lt "$timeout" ]; do
        if command -v python3 >/dev/null 2>&1; then
            if python3 - "$host" "$port" <<'PY' >/dev/null 2>&1
import socket
import sys

host = sys.argv[1]
port = int(sys.argv[2])
sock = socket.socket()
sock.settimeout(1.0)
try:
    sock.connect((host, port))
    sys.exit(0)
except OSError:
    sys.exit(1)
finally:
    sock.close()
PY
            then
                return 0
            fi
        elif command -v nc >/dev/null 2>&1; then
            if nc -z "$host" "$port" >/dev/null 2>&1; then
                return 0
            fi
        else
            fail "missing python3 or nc for bridge readiness check"
        fi
        i=$((i + 1))
        sleep 1
    done
    return 1
}

start_adapter() {
    poll_ms="${1:-$APPFS_ADAPTER_POLL_MS}"
    if [ -n "$APPFS_ADAPTER_HTTP_ENDPOINT" ]; then
        set -- $(endpoint_host_port "$APPFS_ADAPTER_HTTP_ENDPOINT")
        if ! wait_tcp_ready "$1" "$2" "$APPFS_TIMEOUT_SEC"; then
            fail "http bridge endpoint not ready: $APPFS_ADAPTER_HTTP_ENDPOINT"
        fi
    fi
    if [ -n "$APPFS_ADAPTER_GRPC_ENDPOINT" ]; then
        set -- $(endpoint_host_port "$APPFS_ADAPTER_GRPC_ENDPOINT")
        if ! wait_tcp_ready "$1" "$2" "$APPFS_TIMEOUT_SEC"; then
            fail "grpc bridge endpoint not ready: $APPFS_ADAPTER_GRPC_ENDPOINT"
        fi
    fi
    say "Starting AppFS adapter runtime..."
    set -- "$AGENTFS_BIN" serve appfs --root "$APPFS_LIVE_MOUNTPOINT" --app-id "$APPFS_APP_ID" --poll-ms "$poll_ms" \
        --adapter-bridge-max-retries "$APPFS_ADAPTER_BRIDGE_MAX_RETRIES" \
        --adapter-bridge-initial-backoff-ms "$APPFS_ADAPTER_BRIDGE_INITIAL_BACKOFF_MS" \
        --adapter-bridge-max-backoff-ms "$APPFS_ADAPTER_BRIDGE_MAX_BACKOFF_MS" \
        --adapter-bridge-circuit-breaker-failures "$APPFS_ADAPTER_BRIDGE_CIRCUIT_BREAKER_FAILURES" \
        --adapter-bridge-circuit-breaker-cooldown-ms "$APPFS_ADAPTER_BRIDGE_CIRCUIT_BREAKER_COOLDOWN_MS"
    if [ -n "$APPFS_ADAPTER_HTTP_ENDPOINT" ]; then
        set -- "$@" --adapter-http-endpoint "$APPFS_ADAPTER_HTTP_ENDPOINT" --adapter-http-timeout-ms "$APPFS_ADAPTER_HTTP_TIMEOUT_MS"
    fi
    if [ -n "$APPFS_ADAPTER_GRPC_ENDPOINT" ]; then
        set -- "$@" --adapter-grpc-endpoint "$APPFS_ADAPTER_GRPC_ENDPOINT" --adapter-grpc-timeout-ms "$APPFS_ADAPTER_GRPC_TIMEOUT_MS"
    fi
    "$@" >"$APPFS_ADAPTER_LOG" 2>&1 &
    ADAPTER_PID=$!
    sleep 1
    if ! kill -0 "$ADAPTER_PID" 2>/dev/null; then
        tail -n 80 "$APPFS_ADAPTER_LOG" 2>/dev/null || true
        fail "adapter failed to start"
    fi
}

stop_adapter() {
    if [ -n "${ADAPTER_PID:-}" ] && kill -0 "$ADAPTER_PID" 2>/dev/null; then
        kill "$ADAPTER_PID" 2>/dev/null || true
        wait "$ADAPTER_PID" 2>/dev/null || true
    fi
    ADAPTER_PID=""
}

wait_token_in_events() {
    token="$1"
    file="$2"
    timeout="${3:-20}"
    i=0
    while [ "$i" -lt "$timeout" ]; do
        count="$(grep -c "$token" "$file" 2>/dev/null || true)"
        [ -n "$count" ] || count=0
        if [ "$count" -ge 1 ]; then
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
    timeout="${5:-20}"
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

token_terminal_count() {
    token="$1"
    file="$2"
    grep "$token" "$file" 2>/dev/null | grep -E -c '"type":"action\.(completed|failed|canceled)"' || true
}

assert_token_completed() {
    token="$1"
    file="$2"
    line="$(grep "$token" "$file" 2>/dev/null | tail -n 1 || true)"
    [ -n "$line" ] || fail "missing event line for token: $token"
    printf '%s\n' "$line" | grep -q '"type":"action.completed"' || fail "token $token did not emit action.completed"
}

wait_writable() {
    path="$1"
    timeout="${2:-20}"
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

wait_log_token() {
    pattern="$1"
    file="$2"
    timeout="${3:-20}"
    i=0
    while [ "$i" -lt "$timeout" ]; do
        if grep -q "$pattern" "$file" 2>/dev/null; then
            return 0
        fi
        i=$((i + 1))
        sleep 1
    done
    return 1
}

run_bridge_resilience_probe() {
    if [ "${APPFS_BRIDGE_RESILIENCE_CONTRACT:-0}" != "1" ]; then
        return 0
    fi
    if [ -z "$APPFS_ADAPTER_HTTP_ENDPOINT" ] && [ -z "$APPFS_ADAPTER_GRPC_ENDPOINT" ]; then
        fail "CT-017 requires APPFS_ADAPTER_HTTP_ENDPOINT or APPFS_ADAPTER_GRPC_ENDPOINT"
    fi

    banner "AppFS CT-017 Bridge Retry/Circuit/Recovery"
    events_file="$APPFS_LIVE_MOUNTPOINT/$APPFS_APP_ID/_stream/events.evt.jsonl"
    resilience_action_1="$APPFS_LIVE_MOUNTPOINT/$APPFS_APP_ID/contacts/${APPFS_BRIDGE_RESILIENCE_CONTACT_PREFIX}1/send_message.act"
    resilience_action_2="$APPFS_LIVE_MOUNTPOINT/$APPFS_APP_ID/contacts/${APPFS_BRIDGE_RESILIENCE_CONTACT_PREFIX}2/send_message.act"
    resilience_action_3="$APPFS_LIVE_MOUNTPOINT/$APPFS_APP_ID/contacts/${APPFS_BRIDGE_RESILIENCE_CONTACT_PREFIX}3/send_message.act"
    resilience_action_4="$APPFS_LIVE_MOUNTPOINT/$APPFS_APP_ID/contacts/${APPFS_BRIDGE_RESILIENCE_CONTACT_PREFIX}4/send_message.act"
    for action_path in "$resilience_action_1" "$resilience_action_2" "$resilience_action_3" "$resilience_action_4"; do
        mkdir -p "$(dirname "$action_path")"
        if [ ! -f "$action_path" ]; then
            : > "$action_path" || fail "failed to initialize resilience action sink: $action_path"
        fi
        wait_writable "$action_path" "$APPFS_TIMEOUT_SEC" || fail "resilience sink not writable: $action_path"
    done
    pass "resilience action sinks initialized"

    mkdir -p "$(dirname "$APPFS_BRIDGE_FAULT_CONFIG_PATH")"
    if [ -n "$APPFS_ADAPTER_GRPC_ENDPOINT" ]; then
        cat > "$APPFS_BRIDGE_FAULT_CONFIG_PATH" <<EOF
{"fail_next_submit_action":4,"fail_path_prefix":"/contacts/${APPFS_BRIDGE_RESILIENCE_CONTACT_PREFIX}","fail_grpc_code":"UNAVAILABLE"}
EOF
    else
        cat > "$APPFS_BRIDGE_FAULT_CONFIG_PATH" <<EOF
{"fail_next_submit_action":4,"fail_path_prefix":"/contacts/${APPFS_BRIDGE_RESILIENCE_CONTACT_PREFIX}","fail_http_status":503}
EOF
    fi
    pass "bridge fault config prepared at $APPFS_BRIDGE_FAULT_CONFIG_PATH"
    sleep 1

    token_retry_1="ct-resilience-1-$$"
    printf '{"client_token":"%s","text":"resilience-1"}\n' "$token_retry_1" >> "$resilience_action_1" || fail "resilience request 1 submit failed"
    wait_log_token "bridge .* retry" "$APPFS_ADAPTER_LOG" "$APPFS_TIMEOUT_SEC" || fail "bridge retry log not observed in adapter log"
    wait_log_token "circuit opened" "$APPFS_ADAPTER_LOG" "$APPFS_TIMEOUT_SEC" || fail "bridge circuit did not open in adapter log"
    pass "retry and circuit-open logs observed"

    wait_token_type_count "$token_retry_1" "action.completed" 1 "$events_file" "$APPFS_TIMEOUT_SEC" || fail "resilience request 1 missing action.completed after retries"
    assert_token_completed "$token_retry_1" "$events_file"
    pass "request 1 completed after transient bridge failures"

    sleep "$APPFS_BRIDGE_RESILIENCE_COOLDOWN_WAIT_SEC"

    token_recovered="ct-resilience-4-$$"
    printf '{"client_token":"%s","text":"resilience-4"}\n' "$token_recovered" >> "$resilience_action_4" || fail "resilience request 4 submit failed"
    wait_token_type_count "$token_recovered" "action.completed" 1 "$events_file" "$APPFS_TIMEOUT_SEC" || fail "resilience recovery request missing action.completed"
    assert_token_completed "$token_recovered" "$events_file"
    pass "request 4 completed after cooldown"

    say "CT-017 done"
}

cleanup() {
    set +e

    stop_adapter

    if mountpoint -q "$APPFS_LIVE_MOUNTPOINT" 2>/dev/null; then
        fusermount -u "$APPFS_LIVE_MOUNTPOINT" 2>/dev/null || true
    fi

    if [ -n "${MOUNT_PID:-}" ] && kill -0 "$MOUNT_PID" 2>/dev/null; then
        kill "$MOUNT_PID" 2>/dev/null || true
        wait "$MOUNT_PID" 2>/dev/null || true
    fi

    if mountpoint -q "$APPFS_LIVE_MOUNTPOINT" 2>/dev/null; then
        umount "$APPFS_LIVE_MOUNTPOINT" 2>/dev/null || true
    fi

    rmdir "$APPFS_LIVE_MOUNTPOINT" 2>/dev/null || true

    rm -f "$CLI_DIR/.agentfs/${APPFS_LIVE_AGENT_ID}.db"
    rm -f "$CLI_DIR/.agentfs/${APPFS_LIVE_AGENT_ID}.db-shm"
    rm -f "$CLI_DIR/.agentfs/${APPFS_LIVE_AGENT_ID}.db-wal"
    rm -f "$APPFS_BRIDGE_FAULT_CONFIG_PATH"
}

trap cleanup EXIT INT TERM

command -v cargo >/dev/null 2>&1 || fail "missing command: cargo"
command -v mountpoint >/dev/null 2>&1 || fail "missing command: mountpoint"
command -v fusermount >/dev/null 2>&1 || fail "missing command: fusermount"

[ -d "$APPFS_FIXTURE_DIR" ] || fail "missing fixture directory: $APPFS_FIXTURE_DIR"

cd "$CLI_DIR"

say "Building agentfs binary..."
cargo build >/dev/null
AGENTFS_BIN="$CLI_DIR/target/debug/agentfs"
[ -x "$AGENTFS_BIN" ] || fail "agentfs binary not found: $AGENTFS_BIN"

say "Preparing live mountpoint: $APPFS_LIVE_MOUNTPOINT"
mkdir -p "$APPFS_LIVE_MOUNTPOINT"
if mountpoint -q "$APPFS_LIVE_MOUNTPOINT" 2>/dev/null; then
    fusermount -u "$APPFS_LIVE_MOUNTPOINT" 2>/dev/null || true
fi
find "$APPFS_LIVE_MOUNTPOINT" -mindepth 1 -maxdepth 1 -exec rm -rf {} + 2>/dev/null || true

say "Initializing test agent: $APPFS_LIVE_AGENT_ID"
"$AGENTFS_BIN" init "$APPFS_LIVE_AGENT_ID" --force >/dev/null

say "Starting foreground mount process..."
"$AGENTFS_BIN" mount "$APPFS_LIVE_AGENT_ID" "$APPFS_LIVE_MOUNTPOINT" --backend fuse --foreground >"$APPFS_MOUNT_LOG" 2>&1 &
MOUNT_PID=$!

waited=0
while [ "$waited" -lt "$APPFS_MOUNT_WAIT_SEC" ]; do
    if mountpoint -q "$APPFS_LIVE_MOUNTPOINT" 2>/dev/null; then
        break
    fi
    sleep 1
    waited=$((waited + 1))
done
if ! mountpoint -q "$APPFS_LIVE_MOUNTPOINT" 2>/dev/null; then
    tail -n 80 "$APPFS_MOUNT_LOG" 2>/dev/null || true
    fail "mount did not become ready within ${APPFS_MOUNT_WAIT_SEC}s"
fi

say "Copying AppFS fixture into mounted filesystem..."
cp -a "$APPFS_FIXTURE_DIR"/. "$APPFS_LIVE_MOUNTPOINT"/
APPFS_TEST_ACTION_LIVE="$APPFS_LIVE_MOUNTPOINT/$APPFS_APP_ID/contacts/zhangsan/send_message.act"
APPFS_STREAMING_ACTION_LIVE="$APPFS_LIVE_MOUNTPOINT/$APPFS_APP_ID/files/file-001/download.act"
APPFS_PAGEABLE_RESOURCE_LIVE="$APPFS_LIVE_MOUNTPOINT/$APPFS_APP_ID/feed/recommendations.res.json"
APPFS_EXPIRED_PAGEABLE_RESOURCE_LIVE="$APPFS_LIVE_MOUNTPOINT/$APPFS_APP_ID/feed/recommendations-expired.res.json"
APPFS_LONG_HANDLE_RESOURCE_LIVE="$APPFS_LIVE_MOUNTPOINT/$APPFS_APP_ID/feed/recommendations-long.res.json"
APPFS_SNAPSHOT_RESOURCE_LIVE="$APPFS_LIVE_MOUNTPOINT/$APPFS_APP_ID/chats/chat-001/messages.res.jsonl"
APPFS_OVERSIZE_SNAPSHOT_RESOURCE_LIVE="$APPFS_LIVE_MOUNTPOINT/$APPFS_APP_ID/chats/chat-oversize/messages.res.jsonl"

start_adapter

say "Running AppFS contract tests against live adapter..."
if ! APPFS_CONTRACT_TESTS=1 \
    APPFS_ROOT="$APPFS_LIVE_MOUNTPOINT" \
    APPFS_APP_ID="$APPFS_APP_ID" \
    APPFS_TIMEOUT_SEC="$APPFS_TIMEOUT_SEC" \
    APPFS_TEST_ACTION="$APPFS_TEST_ACTION_LIVE" \
    APPFS_STREAMING_ACTION="$APPFS_STREAMING_ACTION_LIVE" \
    APPFS_PAGEABLE_RESOURCE="$APPFS_PAGEABLE_RESOURCE_LIVE" \
    APPFS_EXPIRED_PAGEABLE_RESOURCE="$APPFS_EXPIRED_PAGEABLE_RESOURCE_LIVE" \
    APPFS_LONG_HANDLE_RESOURCE="$APPFS_LONG_HANDLE_RESOURCE_LIVE" \
    APPFS_SNAPSHOT_RESOURCE="$APPFS_SNAPSHOT_RESOURCE_LIVE" \
    APPFS_OVERSIZE_SNAPSHOT_RESOURCE="$APPFS_OVERSIZE_SNAPSHOT_RESOURCE_LIVE" \
    sh "$CLI_DIR/tests/test-appfs-contract.sh"; then
    say "---- mount log tail ----"
    tail -n 80 "$APPFS_MOUNT_LOG" 2>/dev/null || true
    say "---- adapter log tail ----"
    tail -n 80 "$APPFS_ADAPTER_LOG" 2>/dev/null || true
    fail "live AppFS contract tests failed"
fi

banner "AppFS CT-016 Restart Reconciliation"
pass "lifecycle probe: graceful stop + restart + post-restart submit"
if ! kill -0 "$ADAPTER_PID" 2>/dev/null; then
    fail "adapter not alive before lifecycle probe"
fi
stop_adapter
if [ -n "${ADAPTER_PID:-}" ] && kill -0 "$ADAPTER_PID" 2>/dev/null; then
    fail "adapter still alive after stop signal"
fi

start_adapter
events_file="$APPFS_LIVE_MOUNTPOINT/$APPFS_APP_ID/_stream/events.evt.jsonl"
probe_action="$APPFS_LIVE_MOUNTPOINT/$APPFS_APP_ID/contacts/lifecycle/send_message.act"
probe_token="ct-lifecycle-$$"
mkdir -p "$(dirname "$probe_action")"
printf '{"client_token":"%s","text":"restart-ok"}\n' "$probe_token" >> "$probe_action" || fail "lifecycle probe submit failed"
wait_token_in_events "$probe_token" "$events_file" "$APPFS_TIMEOUT_SEC" || fail "lifecycle probe event not observed after adapter restart"
pass "lifecycle probe passed"

pass "restart reconciliation for accepted-but-not-terminal streaming request"
stop_adapter
start_adapter "$APPFS_ADAPTER_RECONCILE_POLL_MS"
reconcile_action="$APPFS_STREAMING_ACTION_LIVE"
reconcile_token="ct-reconcile-$$"
wait_writable "$reconcile_action" "$APPFS_TIMEOUT_SEC" || fail "reconcile action sink not writable: $reconcile_action"
printf '{"target":"/tmp/reconcile.bin","client_token":"%s"}\n' "$reconcile_token" >> "$reconcile_action" || fail "reconcile submit failed"
wait_token_type_count "$reconcile_token" "action.accepted" 1 "$events_file" "$APPFS_TIMEOUT_SEC" || fail "reconcile accepted event missing before restart"
pass "reconcile accepted event observed before restart"
terminal_before="$(token_terminal_count "$reconcile_token" "$events_file")"
[ "$terminal_before" -eq 0 ] || fail "reconcile terminal emitted too early before restart"

stop_adapter
start_adapter "$APPFS_ADAPTER_RECONCILE_POLL_MS"
wait_token_type_count "$reconcile_token" "action.progress" 1 "$events_file" "$APPFS_TIMEOUT_SEC" || fail "reconcile progress missing after restart"
wait_token_type_count "$reconcile_token" "action.completed" 1 "$events_file" "$APPFS_TIMEOUT_SEC" || fail "reconcile terminal missing after restart"
terminal_after="$(token_terminal_count "$reconcile_token" "$events_file")"
[ "$terminal_after" -eq 1 ] || fail "reconcile request emitted unexpected terminal count: $terminal_after"
pass "reconcile emitted progress and single terminal after restart"
say "CT-016 done"

banner "AppFS CT-019 Restart Cursor Recovery"
stop_adapter
recovery_action="$APPFS_TEST_ACTION_LIVE"
token_rc1="ct-restart-cursor-1-$$"
token_rc2="ct-restart-cursor-2-$$"
token_rc3="ct-restart-cursor-3-$$"
wait_writable "$recovery_action" "$APPFS_TIMEOUT_SEC" || fail "restart-cursor action sink not writable: $recovery_action"
printf '{"client_token":"%s","text":"restart-cursor-1"}\n' "$token_rc1" >> "$recovery_action" || fail "restart-cursor submit #1 failed"
printf '{"client_token":"%s","text":"restart-cursor-2"}\n' "$token_rc2" >> "$recovery_action" || fail "restart-cursor submit #2 failed"
printf '{"client_token":"%s","text":"restart-cursor-3"}\n' "$token_rc3" >> "$recovery_action" || fail "restart-cursor submit #3 failed"
start_adapter
wait_token_type_count "$token_rc1" "action.completed" 1 "$events_file" "$APPFS_TIMEOUT_SEC" || fail "restart-cursor token1 not completed after restart"
wait_token_type_count "$token_rc2" "action.completed" 1 "$events_file" "$APPFS_TIMEOUT_SEC" || fail "restart-cursor token2 not completed after restart"
wait_token_type_count "$token_rc3" "action.completed" 1 "$events_file" "$APPFS_TIMEOUT_SEC" || fail "restart-cursor token3 not completed after restart"
pass "restart cursor consumed queued append-jsonl submits after restart"
say "CT-019 done"

run_bridge_resilience_probe

say "LIVE AppFS contract tests passed."
