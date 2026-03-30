# Python HTTP Bridge Mini Backend (uv)

This directory is the AppFS mini backend reference for HTTP bridge mode.

It is split into three layers:

1. Protocol layer: request routing, payload validation, and error mapping.
2. Business layer: in-memory `MockAiimBackend`.
3. Test hooks: fault injector (env vars + config file reload) for CT-017 resilience probes.

## Run unit tests

```bash
cd examples/appfs/http-bridge/python
uv run python -m unittest discover -s tests -t . -p "test_*.py"
```

## Run bridge service

```bash
cd examples/appfs/http-bridge/python
uv run python bridge_server.py
```

Current HTTP connector mainline currently supports only `mock_aiim` backend.
`jsonplaceholder` backend is retained as a legacy v1 reference backend and is not allowed in canonical connector mode.

## Run full live conformance (HTTP bridge)

```bash
cd examples/appfs/http-bridge/python
sh ./run-conformance.sh
```

`run-conformance.sh` enables bridge resilience contract checks by default (`CT-017` included).
It also derives bridge listen host/port from `APPFS_ADAPTER_HTTP_ENDPOINT`.
If default `127.0.0.1:8080` is occupied and no endpoint is specified, it auto-picks a free local port.

Example custom port:

```bash
APPFS_ADAPTER_HTTP_ENDPOINT=http://127.0.0.1:9000 sh ./run-conformance.sh
```

## Run AppFS runtime with bridge mode

```bash
cd cli
cargo run -- serve appfs \
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

## Bridge contract

Mainline runtime sends requests to:

1. `POST /connector/info`
2. `POST /connector/health`
3. `POST /connector/snapshot/prewarm`
4. `POST /connector/snapshot/fetch-chunk`
5. `POST /connector/live/fetch-page`
6. `POST /connector/action/submit`

Response payloads follow canonical AppFS connector shapes:

1. Success: corresponding connector response payload
2. Error: `ConnectorError` (`{code,message,retryable,details?}`)

Legacy compatibility surface (non-mainline):

1. `POST /v1/submit-action`
2. `POST /v1/submit-control-action`
