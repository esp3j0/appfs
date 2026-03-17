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

## Run full live conformance (HTTP bridge)

```bash
cd examples/appfs/http-bridge/python
sh ./run-conformance.sh
```

`run-conformance.sh` enables bridge resilience contract checks by default (`CT-017` included).

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

Runtime sends requests to:

1. `POST /v1/submit-action`
2. `POST /v1/submit-control-action`

Response payloads should match AppFS adapter SDK result shapes:

1. `AdapterSubmitOutcomeV1`
2. `AdapterControlOutcomeV1`
3. `AdapterErrorV1` (or `{code,message,retryable}` fallback)
