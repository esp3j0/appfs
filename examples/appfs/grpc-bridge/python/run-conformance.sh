#!/bin/sh
set -eu

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
REPO_DIR="$(CDPATH= cd -- "$SCRIPT_DIR/../../../.." && pwd)"
CLI_DIR="$REPO_DIR/cli"

PYTHON_BIN="${PYTHON_BIN:-python3}"
APPFS_ADAPTER_GRPC_ENDPOINT="${APPFS_ADAPTER_GRPC_ENDPOINT:-http://127.0.0.1:50051}"
APPFS_TIMEOUT_SEC="${APPFS_TIMEOUT_SEC:-20}"
BRIDGE_LOG="${BRIDGE_LOG:-$CLI_DIR/appfs-grpc-bridge-conformance.log}"

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

wait_tcp_ready() {
    endpoint="$1"
    timeout="${2:-30}"
    i=0
    while [ "$i" -lt "$timeout" ]; do
        if "$PYTHON_BIN" - "$endpoint" <<'PY'
import socket
import sys
from urllib.parse import urlparse

endpoint = sys.argv[1]
u = urlparse(endpoint)
host = u.hostname or "127.0.0.1"
port = u.port or 50051
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

trap cleanup EXIT INT TERM

command -v "$PYTHON_BIN" >/dev/null 2>&1 || fail "missing python interpreter: $PYTHON_BIN"
[ -f "$SCRIPT_DIR/appfs_adapter_v1_pb2.py" ] || fail "missing generated protobuf stubs: appfs_adapter_v1_pb2.py"
[ -f "$SCRIPT_DIR/appfs_adapter_v1_pb2_grpc.py" ] || fail "missing generated protobuf stubs: appfs_adapter_v1_pb2_grpc.py"

say "Starting Python gRPC bridge..."
"$PYTHON_BIN" "$SCRIPT_DIR/grpc_server.py" >"$BRIDGE_LOG" 2>&1 &
BRIDGE_PID=$!

sleep 1
if ! kill -0 "$BRIDGE_PID" 2>/dev/null; then
    tail -n 80 "$BRIDGE_LOG" 2>/dev/null || true
    fail "gRPC bridge process exited early"
fi

wait_tcp_ready "$APPFS_ADAPTER_GRPC_ENDPOINT" 30 || {
    tail -n 80 "$BRIDGE_LOG" 2>/dev/null || true
    fail "gRPC bridge did not become ready at $APPFS_ADAPTER_GRPC_ENDPOINT"
}

say "Running live AppFS contract suite with gRPC bridge..."
APPFS_CONTRACT_TESTS=1 \
APPFS_TIMEOUT_SEC="$APPFS_TIMEOUT_SEC" \
APPFS_ADAPTER_GRPC_ENDPOINT="$APPFS_ADAPTER_GRPC_ENDPOINT" \
sh "$CLI_DIR/tests/appfs/run-live-with-adapter.sh"

say "gRPC bridge conformance passed."
