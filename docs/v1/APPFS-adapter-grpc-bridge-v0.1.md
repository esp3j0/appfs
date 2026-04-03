# AppFS Adapter gRPC Bridge v0.1 (Reference)

- Version: `0.1-draft`
- Date: `2026-03-17`
- Status: `Draft`
- Scope: Optional transport example mapped to frozen `AppAdapterV1` semantics

## 1. Purpose

This document defines a gRPC bridge contract for adapter implementations in any language.

It does not change AppFS Core protocol semantics. Runtime-side guarantees still remain in `agentfs serve appfs`.

## 2. Proto Contract

Reference proto:

- `examples/appfs/legacy/v1/grpc/proto/appfs_adapter_v1.proto`

Service:

1. `SubmitAction`
2. `SubmitControlAction`

## 3. JSON Payload Encoding Rule

To keep bridge messages language-neutral:

1. `Completed`/`Streaming`/`ControlCompleted` content fields are encoded as JSON strings in proto.
2. Bridge implementations must encode valid JSON text for these fields.
3. Runtime-side bridge adapters (or HTTP gateways) parse/forward the JSON as `AdapterSubmitOutcomeV1` / `AdapterControlOutcomeV1`.

## 4. Error Mapping

gRPC service returns bridge-level `Error` in response `oneof`:

1. `code`
2. `message`
3. `retryable`

Mapping target:

1. `AdapterErrorV1::Rejected` for domain validation/permission/type errors.
2. Transport/internal failures map to `AdapterErrorV1::Internal`.

## 5. Deployment Shapes

1. Direct runtime gRPC adapter (supported via `--adapter-grpc-endpoint`).
2. gRPC backend + HTTP gateway sidecar (supported via `--adapter-http-endpoint`).

## 6. Runtime Option

```bash
agentfs serve appfs \
  --root /app \
  --app-id aiim \
  --adapter-grpc-endpoint http://127.0.0.1:50051 \
  --adapter-grpc-timeout-ms 5000 \
  --adapter-bridge-max-retries 2 \
  --adapter-bridge-initial-backoff-ms 100 \
  --adapter-bridge-max-backoff-ms 1000 \
  --adapter-bridge-circuit-breaker-failures 5 \
  --adapter-bridge-circuit-breaker-cooldown-ms 3000
```

`--adapter-grpc-endpoint` is mutually exclusive with `--adapter-http-endpoint`.

Equivalent env vars:

1. `APPFS_ADAPTER_GRPC_ENDPOINT`
2. `APPFS_ADAPTER_GRPC_TIMEOUT_MS`
3. `APPFS_ADAPTER_BRIDGE_MAX_RETRIES`
4. `APPFS_ADAPTER_BRIDGE_INITIAL_BACKOFF_MS`
5. `APPFS_ADAPTER_BRIDGE_MAX_BACKOFF_MS`
6. `APPFS_ADAPTER_BRIDGE_CIRCUIT_BREAKER_FAILURES`
7. `APPFS_ADAPTER_BRIDGE_CIRCUIT_BREAKER_COOLDOWN_MS`

## 7. Python Reference

Reference files:

1. `examples/appfs/bridges/grpc-python/grpc_server.py`
2. `examples/appfs/legacy/v1/grpc/python/http_gateway.py`
3. `examples/appfs/bridges/grpc-python/README.md`

`http_gateway.py` exposes:

1. `POST /v1/submit-action`
2. `POST /v1/submit-control-action`

and forwards to gRPC backend.

## 8. Bridge Resilience and Metrics

Runtime-side gRPC bridge dispatch includes:

1. bounded retry with exponential backoff (retryable gRPC status codes)
2. circuit breaker on repeated transport-level failures
3. transport metrics logs (`requests/attempts/retries/success/fail/short_circuit`) for observability

Reference Python gRPC bridge (`examples/appfs/bridges/grpc-python/grpc_server.py`) supports optional fault injection knobs for contract testing:

1. `APPFS_BRIDGE_FAIL_NEXT_SUBMIT_ACTION` (int, default `0`)
2. `APPFS_BRIDGE_FAIL_PATH_PREFIX` (only fail matching action paths)
3. `APPFS_BRIDGE_FAIL_GRPC_CODE` (default `UNAVAILABLE`)
4. `APPFS_BRIDGE_FAULT_CONFIG_PATH` (default `/tmp/appfs-bridge-fault-config.json`, hot-reload JSON config written by `CT-017`)
