#!/bin/sh
set -eu

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
if REPO_ROOT="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel 2>/dev/null)"; then
    REPO_DIR="$REPO_ROOT"
else
    REPO_DIR="$(CDPATH= cd -- "$SCRIPT_DIR/../../../.." && pwd)"
fi
CLI_DIR="$REPO_DIR/cli"

UV_BIN="${UV_BIN:-uv}"
APPFS_ADAPTER_HTTP_ENDPOINT_SET="${APPFS_ADAPTER_HTTP_ENDPOINT+x}"
APPFS_ADAPTER_HTTP_ENDPOINT="${APPFS_ADAPTER_HTTP_ENDPOINT:-http://127.0.0.1:8080}"
APPFS_TIMEOUT_SEC="${APPFS_TIMEOUT_SEC:-20}"
APPFS_ADAPTER_BRIDGE_MAX_RETRIES="${APPFS_ADAPTER_BRIDGE_MAX_RETRIES:-1}"
APPFS_ADAPTER_BRIDGE_INITIAL_BACKOFF_MS="${APPFS_ADAPTER_BRIDGE_INITIAL_BACKOFF_MS:-50}"
APPFS_ADAPTER_BRIDGE_MAX_BACKOFF_MS="${APPFS_ADAPTER_BRIDGE_MAX_BACKOFF_MS:-200}"
APPFS_ADAPTER_BRIDGE_CIRCUIT_BREAKER_FAILURES="${APPFS_ADAPTER_BRIDGE_CIRCUIT_BREAKER_FAILURES:-2}"
APPFS_ADAPTER_BRIDGE_CIRCUIT_BREAKER_COOLDOWN_MS="${APPFS_ADAPTER_BRIDGE_CIRCUIT_BREAKER_COOLDOWN_MS:-4000}"
APPFS_BRIDGE_RESILIENCE_COOLDOWN_WAIT_SEC="${APPFS_BRIDGE_RESILIENCE_COOLDOWN_WAIT_SEC:-4}"
BRIDGE_LOG="${BRIDGE_LOG:-$CLI_DIR/appfs-http-bridge-conformance.log}"

BRIDGE_PID=""

say() {
    printf '%s\n' "$*"
}

fail() {
    say "FAILED: $*"
    exit 1
}

cleanup() {
    if [ -n "${BRIDGE_PID:-}" ] && kill -0 "$BRIDGE_PID" 2>/dev/null; then
        kill "$BRIDGE_PID" 2>/dev/null || true
        wait "$BRIDGE_PID" 2>/dev/null || true
    fi
}

wait_http_ready() {
    endpoint="$1"
    timeout="${2:-30}"
    i=0
    while [ "$i" -lt "$timeout" ]; do
        if "$UV_BIN" run --project "$SCRIPT_DIR" python - "$endpoint" <<'PY'
import socket
import sys
from urllib.parse import urlparse

endpoint = sys.argv[1]
u = urlparse(endpoint)
host = u.hostname or "127.0.0.1"
port = u.port or 80
s = socket.socket()
s.settimeout(0.5)
try:
    s.connect((host, port))
    sys.exit(0)
except OSError:
    sys.exit(1)
finally:
    s.close()
PY
        then
            return 0
        fi
        i=$((i + 1))
        sleep 1
    done
    return 1
}

parse_http_endpoint() {
    endpoint="$1"
    "$UV_BIN" run --project "$SCRIPT_DIR" python - "$endpoint" <<'PY'
import sys
from urllib.parse import urlparse

endpoint = sys.argv[1]
parsed = urlparse(endpoint)
host = parsed.hostname
port = parsed.port
if host is None or port is None:
    print(f"invalid APPFS_ADAPTER_HTTP_ENDPOINT: {endpoint}", file=sys.stderr)
    sys.exit(1)
print(host)
print(port)
PY
}

endpoint_bindable() {
    host="$1"
    port="$2"
    "$UV_BIN" run --project "$SCRIPT_DIR" python - "$host" "$port" <<'PY'
import socket
import sys

host = sys.argv[1]
port = int(sys.argv[2])
s = socket.socket()
try:
    s.bind((host, port))
    sys.exit(0)
except OSError:
    sys.exit(1)
finally:
    s.close()
PY
}

find_free_port() {
    host="$1"
    "$UV_BIN" run --project "$SCRIPT_DIR" python - "$host" <<'PY'
import socket
import sys

host = sys.argv[1]
s = socket.socket()
s.bind((host, 0))
port = s.getsockname()[1]
s.close()
print(port)
PY
}

trap cleanup EXIT INT TERM

command -v "$UV_BIN" >/dev/null 2>&1 || fail "missing uv binary: $UV_BIN"

say "Running Python HTTP bridge unit tests (uv)..."
"$UV_BIN" run --project "$SCRIPT_DIR" python -m unittest discover -s "$SCRIPT_DIR/tests" -t "$SCRIPT_DIR" -p "test_*.py"

set -- $(parse_http_endpoint "$APPFS_ADAPTER_HTTP_ENDPOINT")
BRIDGE_HOST="$1"
BRIDGE_PORT="$2"
if ! endpoint_bindable "$BRIDGE_HOST" "$BRIDGE_PORT"; then
    if [ -z "$APPFS_ADAPTER_HTTP_ENDPOINT_SET" ]; then
        old_port="$BRIDGE_PORT"
        BRIDGE_PORT="$(find_free_port "$BRIDGE_HOST")"
        APPFS_ADAPTER_HTTP_ENDPOINT="http://${BRIDGE_HOST}:${BRIDGE_PORT}"
        say "Default port ${old_port} is busy; switched endpoint to ${APPFS_ADAPTER_HTTP_ENDPOINT}"
    else
        fail "configured endpoint is busy: ${APPFS_ADAPTER_HTTP_ENDPOINT}; choose a free APPFS_ADAPTER_HTTP_ENDPOINT"
    fi
fi

say "Starting Python HTTP bridge (uv) on ${BRIDGE_HOST}:${BRIDGE_PORT}..."
APPFS_BRIDGE_HOST="$BRIDGE_HOST" \
APPFS_BRIDGE_PORT="$BRIDGE_PORT" \
"$UV_BIN" run --project "$SCRIPT_DIR" python "$SCRIPT_DIR/bridge_server.py" >"$BRIDGE_LOG" 2>&1 &
BRIDGE_PID=$!

sleep 1
if ! kill -0 "$BRIDGE_PID" 2>/dev/null; then
    tail -n 80 "$BRIDGE_LOG" 2>/dev/null || true
    fail "bridge process exited early"
fi

wait_http_ready "$APPFS_ADAPTER_HTTP_ENDPOINT" 30 || {
    tail -n 80 "$BRIDGE_LOG" 2>/dev/null || true
    fail "bridge did not become ready at $APPFS_ADAPTER_HTTP_ENDPOINT"
}

say "Running live AppFS contract suite with HTTP bridge..."
APPFS_CONTRACT_TESTS=1 \
APPFS_TIMEOUT_SEC="$APPFS_TIMEOUT_SEC" \
APPFS_ADAPTER_HTTP_ENDPOINT="$APPFS_ADAPTER_HTTP_ENDPOINT" \
APPFS_ADAPTER_BRIDGE_MAX_RETRIES="$APPFS_ADAPTER_BRIDGE_MAX_RETRIES" \
APPFS_ADAPTER_BRIDGE_INITIAL_BACKOFF_MS="$APPFS_ADAPTER_BRIDGE_INITIAL_BACKOFF_MS" \
APPFS_ADAPTER_BRIDGE_MAX_BACKOFF_MS="$APPFS_ADAPTER_BRIDGE_MAX_BACKOFF_MS" \
APPFS_ADAPTER_BRIDGE_CIRCUIT_BREAKER_FAILURES="$APPFS_ADAPTER_BRIDGE_CIRCUIT_BREAKER_FAILURES" \
APPFS_ADAPTER_BRIDGE_CIRCUIT_BREAKER_COOLDOWN_MS="$APPFS_ADAPTER_BRIDGE_CIRCUIT_BREAKER_COOLDOWN_MS" \
APPFS_BRIDGE_RESILIENCE_CONTRACT=1 \
APPFS_BRIDGE_RESILIENCE_COOLDOWN_WAIT_SEC="$APPFS_BRIDGE_RESILIENCE_COOLDOWN_WAIT_SEC" \
sh "$CLI_DIR/tests/appfs/run-live-with-adapter.sh"

say "HTTP bridge conformance passed."
