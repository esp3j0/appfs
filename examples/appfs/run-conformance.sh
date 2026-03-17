#!/bin/sh
set -eu

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
REPO_DIR="$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)"
CLI_DIR="$REPO_DIR/cli"

mode="${1:-inprocess}"

usage() {
    cat <<'EOF'
Usage:
  sh ./run-conformance.sh [inprocess|http-python|grpc-python]

Modes:
  inprocess   Run live AppFS contract suite with built-in demo adapter
  http-python Run live AppFS contract suite with Python HTTP bridge adapter
  grpc-python Run live AppFS contract suite with Python gRPC bridge adapter
EOF
}

case "$mode" in
    inprocess)
        APPFS_CONTRACT_TESTS=1 sh "$CLI_DIR/tests/appfs/run-live-with-adapter.sh"
        ;;
    http-python)
        sh "$SCRIPT_DIR/http-bridge/python/run-conformance.sh"
        ;;
    grpc-python)
        sh "$SCRIPT_DIR/grpc-bridge/python/run-conformance.sh"
        ;;
    -h|--help|help)
        usage
        ;;
    *)
        usage
        printf '\nUnknown mode: %s\n' "$mode" >&2
        exit 2
        ;;
esac
