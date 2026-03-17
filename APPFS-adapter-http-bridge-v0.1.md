# AppFS Adapter HTTP Bridge v0.1 (Reference)

- Version: `0.1-draft`
- Date: `2026-03-17`
- Status: `Draft`
- Scope: Optional language bridge for `AppAdapterV1`

## 1. Purpose

This document defines a minimal HTTP mapping so non-Rust adapters can integrate with `agentfs serve appfs` while preserving AppFS Core semantics.

## 2. Runtime Switch

When starting runtime:

```bash
agentfs serve appfs \
  --root /app \
  --app-id aiim \
  --adapter-http-endpoint http://127.0.0.1:8080 \
  --adapter-http-timeout-ms 5000 \
  --adapter-bridge-max-retries 2 \
  --adapter-bridge-initial-backoff-ms 100 \
  --adapter-bridge-max-backoff-ms 1000 \
  --adapter-bridge-circuit-breaker-failures 5 \
  --adapter-bridge-circuit-breaker-cooldown-ms 3000
```

Equivalent env vars:

1. `APPFS_ADAPTER_HTTP_ENDPOINT`
2. `APPFS_ADAPTER_HTTP_TIMEOUT_MS`
3. `APPFS_ADAPTER_BRIDGE_MAX_RETRIES`
4. `APPFS_ADAPTER_BRIDGE_INITIAL_BACKOFF_MS`
5. `APPFS_ADAPTER_BRIDGE_MAX_BACKOFF_MS`
6. `APPFS_ADAPTER_BRIDGE_CIRCUIT_BREAKER_FAILURES`
7. `APPFS_ADAPTER_BRIDGE_CIRCUIT_BREAKER_COOLDOWN_MS`

If endpoint is omitted, runtime uses in-process `DemoAppAdapterV1`.

## 3. Endpoints

HTTP `POST` JSON endpoints expected by runtime:

1. `/v1/submit-action`
2. `/v1/submit-control-action`

## 4. Request/Response Shapes

### 4.1 `POST /v1/submit-action`

Request:

```json
{
  "app_id": "aiim",
  "path": "/contacts/zhangsan/send_message.act",
  "payload": "hello\n",
  "input_mode": "text",
  "execution_mode": "inline",
  "context": {
    "app_id": "aiim",
    "session_id": "sess-1234",
    "request_id": "req-5678",
    "client_token": "tok-1"
  }
}
```

Success response MUST be `AdapterSubmitOutcomeV1` JSON:

```json
{
  "kind": "completed",
  "content": "send success"
}
```

or:

```json
{
  "kind": "streaming",
  "plan": {
    "accepted_content": "accepted",
    "progress_content": { "percent": 50 },
    "terminal_content": { "ok": true }
  }
}
```

### 4.2 `POST /v1/submit-control-action`

Request:

```json
{
  "app_id": "aiim",
  "path": "/_paging/fetch_next.act",
  "action": {
    "kind": "paging_fetch_next",
    "handle_id": "ph_abc",
    "page_no": 1,
    "has_more": true
  },
  "context": {
    "app_id": "aiim",
    "session_id": "sess-1234",
    "request_id": "req-5678",
    "client_token": "tok-1"
  }
}
```

Success response MUST be `AdapterControlOutcomeV1` JSON:

```json
{
  "kind": "completed",
  "content": {
    "closed": true
  }
}
```

## 5. Error Mapping

Bridge service SHOULD return one of:

1. `AdapterErrorV1` JSON (preferred)
2. Simple error JSON:

```json
{
  "code": "PERMISSION_DENIED",
  "message": "forbidden",
  "retryable": false
}
```

Runtime behavior:

1. First parse full `AdapterErrorV1`.
2. Fallback parse simple `{code,message,retryable}`.
3. Otherwise map to `AdapterErrorV1::Internal` with HTTP status/body.

## 6. Compatibility Note

This bridge only changes transport. AppFS protocol guarantees still stay in runtime:

1. write+close submit boundary
2. event persistence/order
3. cursor/replay atomicity
4. paging close-time error semantics

## 7. Bridge Resilience and Metrics

Runtime-side HTTP bridge dispatch includes:

1. bounded retry with exponential backoff (transport + retryable status)
2. circuit breaker on repeated transport-level failures
3. transport metrics logs (`requests/attempts/retries/success/fail/short_circuit`) for observability

Reference Python bridge (`examples/appfs/http-bridge/python/bridge_server.py`) supports optional fault injection knobs for contract testing:

1. `APPFS_BRIDGE_FAIL_NEXT_SUBMIT_ACTION` (int, default `0`)
2. `APPFS_BRIDGE_FAIL_PATH_PREFIX` (only fail matching action paths)
3. `APPFS_BRIDGE_FAIL_HTTP_STATUS` (default `503`)
4. `APPFS_BRIDGE_FAULT_CONFIG_PATH` (default `/tmp/appfs-bridge-fault-config.json`, hot-reload JSON config written by `CT-017`)
