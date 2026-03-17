# AppFS Contract Test Skeleton

This directory contains shell-first contract tests for AppFS v0.1.

## Run

```bash
cd cli
APPFS_CONTRACT_TESTS=1 ./tests/test-appfs-contract.sh
```

For static fixture validation (without mounted runtime):

```bash
cd cli
APPFS_CONTRACT_TESTS=1 APPFS_STATIC_FIXTURE=1 APPFS_ROOT=/mnt/c/Users/esp3j/rep/agentfs/examples/appfs ./tests/test-appfs-contract.sh
```

To run through the existing aggregate test entry:

```bash
cd cli
APPFS_CONTRACT_TESTS=1 ./tests/all.sh
```

For a full Linux live run (mount + fixture + `serve appfs` + contract tests):

```bash
cd cli
./tests/appfs/run-live-with-adapter.sh
```

For bridge-mode runs (runtime -> external HTTP adapter), start bridge service first and export:

```bash
export APPFS_ADAPTER_HTTP_ENDPOINT=http://127.0.0.1:8080
```

For gRPC bridge example, use:

1. `examples/appfs/grpc-bridge/python/grpc_server.py`
2. `examples/appfs/grpc-bridge/python/http_gateway.py`

For runtime native gRPC bridge mode, export:

```bash
export APPFS_ADAPTER_GRPC_ENDPOINT=http://127.0.0.1:50051
```

## Environment

| Variable | Default |
|---|---|
| `APPFS_ROOT` | `/app` |
| `APPFS_APP_ID` | `aiim` |
| `APPFS_TEST_ACTION` | `/app/aiim/contacts/zhangsan/send_message.act` |
| `APPFS_STREAMING_ACTION` | `/app/aiim/files/file-001/download.act` |
| `APPFS_PAGEABLE_RESOURCE` | `/app/aiim/chats/chat-001/messages.res.json` |
| `APPFS_EXPIRED_PAGEABLE_RESOURCE` | `/app/aiim/chats/chat-expired/messages.res.json` |
| `APPFS_LONG_HANDLE_RESOURCE` | `/app/aiim/chats/chat-long/messages.res.json` |
| `APPFS_TIMEOUT_SEC` | `10` |
| `APPFS_STATIC_FIXTURE` | `0` |

## Live Harness Environment (`run-live-with-adapter.sh`)

| Variable | Default |
|---|---|
| `APPFS_FIXTURE_DIR` | `../examples/appfs` (from repo root) |
| `APPFS_LIVE_AGENT_ID` | `appfs-live-$$` |
| `APPFS_LIVE_MOUNTPOINT` | `/tmp/agentfs-appfs-live-$$` |
| `APPFS_APP_ID` | `aiim` |
| `APPFS_ADAPTER_POLL_MS` | `100` |
| `APPFS_ADAPTER_RECONCILE_POLL_MS` | `1000` |
| `APPFS_ADAPTER_HTTP_ENDPOINT` | _empty_ (uses in-process demo adapter) |
| `APPFS_ADAPTER_HTTP_TIMEOUT_MS` | `5000` |
| `APPFS_ADAPTER_GRPC_ENDPOINT` | _empty_ (mutually exclusive with HTTP endpoint) |
| `APPFS_ADAPTER_GRPC_TIMEOUT_MS` | `5000` |
| `APPFS_ADAPTER_BRIDGE_MAX_RETRIES` | `2` |
| `APPFS_ADAPTER_BRIDGE_INITIAL_BACKOFF_MS` | `100` |
| `APPFS_ADAPTER_BRIDGE_MAX_BACKOFF_MS` | `1000` |
| `APPFS_ADAPTER_BRIDGE_CIRCUIT_BREAKER_FAILURES` | `5` |
| `APPFS_ADAPTER_BRIDGE_CIRCUIT_BREAKER_COOLDOWN_MS` | `3000` |
| `APPFS_BRIDGE_RESILIENCE_CONTRACT` | `0` |
| `APPFS_BRIDGE_RESILIENCE_COOLDOWN_WAIT_SEC` | `4` |
| `APPFS_TIMEOUT_SEC` | `20` |
| `APPFS_MOUNT_WAIT_SEC` | `20` |
| `APPFS_MOUNT_LOG` | `cli/appfs-mount-live.log` |
| `APPFS_ADAPTER_LOG` | `cli/appfs-adapter-live.log` |

`run-live-with-adapter.sh` uses fixed in-fixture paths for:

1. `APPFS_TEST_ACTION`
2. `APPFS_STREAMING_ACTION`
3. `APPFS_PAGEABLE_RESOURCE`
4. `APPFS_EXPIRED_PAGEABLE_RESOURCE`
5. `APPFS_LONG_HANDLE_RESOURCE`

to avoid inheriting stale shell environment overrides.

## Notes

1. Linux CI now runs AppFS contract gates in `.github/workflows/rust.yml`: `appfs-contract-gate` (in-process), `appfs-contract-gate-http-bridge`, and `appfs-contract-gate-grpc-bridge`.
2. Some checks require `jq`; if missing, JSON field-level assertions are skipped.
3. `APPFS_STATIC_FIXTURE=1` runs only static contract checks (layout/replay/manifest policy).
4. `run-live-with-adapter.sh` is Linux/FUSE oriented and expects `fusermount` + `mountpoint`.
5. Live suite validates paging error mapping (`CT-009`), streaming lifecycle (`CT-006`), malformed submit rejection (`CT-007`), ordered multi-submit behavior (`CT-008`), in-progress write atomicity (`CT-010`), interrupted-write no-commit behavior (`CT-011`), unsafe-path no-side-effect guard (`CT-012`), duplicate-consumption semantics (`CT-013`), concurrent same-action stress (`CT-014`), and long-handle normalization compatibility (`CT-015`).
6. `run-live-with-adapter.sh` additionally runs lifecycle restart probes, including accepted-but-not-terminal reconciliation for streaming requests (`CT-016`).
7. When `APPFS_BRIDGE_RESILIENCE_CONTRACT=1` and bridge mode is enabled, `run-live-with-adapter.sh` also runs `CT-017` (retry + circuit-breaker + cooldown recovery probe) and checks adapter logs for retry/short-circuit observations.
8. This is a skeleton focused on protocol gates, not full adapter business behavior.
9. Bridge mode now performs endpoint readiness precheck (`host:port` connect) before starting runtime; unreachable bridge endpoints fail fast.
