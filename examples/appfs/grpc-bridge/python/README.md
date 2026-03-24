# gRPC Bridge Example (Python)

This example provides:

1. `grpc_server.py`: gRPC implementation of AppFS connector bridge services.
2. `http_gateway.py`: legacy HTTP gateway exposing `/v1/submit-action` and `/v1/submit-control-action` (auxiliary example only).

For v0.3 runtime main path, use direct gRPC endpoint with `--adapter-grpc-endpoint`.

## 1. Install dependencies

```bash
cd examples/appfs/grpc-bridge/python
python3 -m venv .venv
source .venv/bin/activate
pip install -r requirements.txt
```

## 2. Generate Python stubs

```bash
./generate_stubs.sh
```

This generates:

1. `appfs_adapter_v1_pb2.py`
2. `appfs_adapter_v1_pb2_grpc.py`
3. `appfs_connector_v2_pb2.py`
4. `appfs_connector_v2_pb2_grpc.py`

## 3. Start gRPC server

```bash
python3 grpc_server.py
```

Default listen: `127.0.0.1:50051`.

## 4. Start AppFS runtime (v0.3 main path: gRPC V2 connector)

```bash
cd cli
agentfs serve appfs \
  --root /app \
  --app-id aiim \
  --adapter-grpc-endpoint http://127.0.0.1:50051 \
  --adapter-bridge-max-retries 2 \
  --adapter-bridge-initial-backoff-ms 100 \
  --adapter-bridge-max-backoff-ms 1000 \
  --adapter-bridge-circuit-breaker-failures 5 \
  --adapter-bridge-circuit-breaker-cooldown-ms 3000
```

For live harness:

```bash
cd cli
APPFS_ADAPTER_GRPC_ENDPOINT=http://127.0.0.1:50051 \
APPFS_CONTRACT_TESTS=1 \
sh ./tests/appfs/run-live-with-adapter.sh
```

## 5. Optional legacy gateway

`http_gateway.py` remains as a compatibility/auxiliary example for V1 HTTP surface and is not the current runtime main path.

```bash
python3 http_gateway.py
```
