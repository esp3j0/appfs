# AppFS HTTP Bridge Example (Python)

This directory is the current Python HTTP connector bridge reference.

It demonstrates the canonical AppFS connector surface over HTTP:

1. `connector_info`
2. `health`
3. `prewarm_snapshot_meta`
4. `fetch_snapshot_chunk`
5. `fetch_live_page`
6. `submit_action`
7. `get_app_structure`
8. `refresh_app_structure`

## Run Unit Tests

```bash
cd examples/appfs/bridges/http-python
uv run python -m unittest discover -s tests -t . -p "test_*.py"
```

## Run the Bridge

```bash
cd examples/appfs/bridges/http-python
uv run python bridge_server.py
```

Supported backend modes:

1. `mock_aiim` (default)
2. `huoyan`

`jsonplaceholder` is retained only as a legacy v1 backend reference and is not part of the current canonical connector path.

## Run Conformance

```bash
cd examples/appfs/bridges/http-python
sh ./run-conformance.sh
```

The script:

1. runs unit tests
2. starts the HTTP bridge
3. runs the live AppFS connector harness

## Recommended Runtime Path

For manual use, prefer:

```bash
agentfs appfs up <id-or-path> <mountpoint>
```

Then register the app through `/_appfs/register_app.act` with:

```json
{"app_id":"aiim","transport":{"kind":"http","endpoint":"http://127.0.0.1:8080","http_timeout_ms":5000,"grpc_timeout_ms":5000,"bridge_max_retries":2,"bridge_initial_backoff_ms":100,"bridge_max_backoff_ms":1000,"bridge_circuit_breaker_failures":5,"bridge_circuit_breaker_cooldown_ms":3000},"client_token":"reg-http-001"}
```

`serve appfs` is still useful for low-level debugging, but it is not the main examples path.

## Bridge Contract

Runtime mainline sends requests to:

1. `POST /connector/info`
2. `POST /connector/health`
3. `POST /connector/snapshot/prewarm`
4. `POST /connector/snapshot/fetch-chunk`
5. `POST /connector/live/fetch-page`
6. `POST /connector/action/submit`
7. `POST /connector/structure/get`
8. `POST /connector/structure/refresh`

Legacy compatibility routes remain internal/reference only and are not the current examples path.
