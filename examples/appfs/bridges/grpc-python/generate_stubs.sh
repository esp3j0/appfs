#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
PROTO_DIR="$SCRIPT_DIR/proto"
LEGACY_PROTO_DIR="$SCRIPT_DIR/../../legacy/v1/grpc/proto"

python3 -m grpc_tools.protoc \
  -I "$PROTO_DIR" \
  --python_out="$SCRIPT_DIR" \
  --grpc_python_out="$SCRIPT_DIR" \
  "$PROTO_DIR/appfs_connector.proto" \
  "$PROTO_DIR/appfs_structure.proto"

if [ -f "$LEGACY_PROTO_DIR/appfs_adapter_v1.proto" ]; then
  python3 -m grpc_tools.protoc \
    -I "$LEGACY_PROTO_DIR" \
    --python_out="$SCRIPT_DIR" \
    --grpc_python_out="$SCRIPT_DIR" \
    "$LEGACY_PROTO_DIR/appfs_adapter_v1.proto"
fi

echo "Generated stubs:"
echo "  $SCRIPT_DIR/appfs_connector_pb2.py"
echo "  $SCRIPT_DIR/appfs_connector_pb2_grpc.py"
echo "  $SCRIPT_DIR/appfs_structure_pb2.py"
echo "  $SCRIPT_DIR/appfs_structure_pb2_grpc.py"
if [ -f "$SCRIPT_DIR/appfs_adapter_v1_pb2.py" ]; then
  echo "  $SCRIPT_DIR/appfs_adapter_v1_pb2.py"
  echo "  $SCRIPT_DIR/appfs_adapter_v1_pb2_grpc.py"
fi
