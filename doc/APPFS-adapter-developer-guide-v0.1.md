# AppFS Adapter Developer Guide v0.1

- Version: `0.1`
- Date: `2026-03-17`
- Status: `Draft`
- Audience: Adapter implementers (Rust/Python/Go/TS), runtime integrators

## 1. Target Outcome

This guide is the single onboarding entry for adapter developers.

Success means:

1. You can run `init -> submit -> stream -> paging` locally.
2. You can pass `CT-001 ~ CT-017`.
3. You know where to debug failures and how to claim compatibility.

## 2. Read Order (Do This Sequence)

1. Protocol baseline: `doc/APPFS-v0.1.md`
2. Adapter requirements: `doc/APPFS-adapter-requirements-v0.1.md`
3. This guide for implementation workflow.
4. Contract and conformance definitions:
   - `doc/APPFS-conformance-v0.1.md`
   - `doc/APPFS-contract-tests-v0.1.md`
5. Compatibility matrix:
   - `doc/APPFS-compatibility-matrix-v0.1.md`

## 3. 30-Minute Minimum Loop

## 3.1 Quick Commands

```bash
# 1) static fixture checks
cd cli
APPFS_CONTRACT_TESTS=1 APPFS_STATIC_FIXTURE=1 APPFS_ROOT="$PWD/../examples/appfs" sh ./tests/test-appfs-contract.sh

# 2) live in-process checks
cd ../examples/appfs
sh ./run-conformance.sh inprocess

# 3) live HTTP bridge checks (uv + CT-017 included)
sh ./run-conformance.sh http-python
```

## 3.2 What Each Run Validates

1. Static mode validates fixture layout/schema/policy (`CT-001`, `CT-003`, `CT-005`).
2. Live mode validates action and stream behavior, paging, path-safety, resilience (`CT-002` to `CT-017`).
3. HTTP/gRPC bridge modes validate transport parity against the same runtime contract.

## 4. Choose Implementation Path

## 4.1 In-Process Rust Adapter

Use when:

1. You are extending the runtime directly.
2. You want simplest debug cycle with one process.

References:

1. `sdk/rust/src/appfs_adapter.rs`
2. `sdk/rust/src/appfs_demo_adapter.rs`
3. `examples/appfs/adapter-template/rust-minimal/`

## 4.2 Out-of-Process HTTP Bridge

Use when:

1. You want polyglot implementation.
2. You need independent deploy/restart lifecycle.

Reference:

1. `examples/appfs/http-bridge/python/`
2. `doc/APPFS-adapter-http-bridge-v0.1.md`

## 4.3 Out-of-Process gRPC Bridge

Use when:

1. You want stronger typed contract over transport.
2. You have multi-language teams sharing proto contracts.

Reference:

1. `examples/appfs/grpc-bridge/python/`
2. `doc/APPFS-adapter-grpc-bridge-v0.1.md`

## 5. Adapter Contract Essentials

You must implement behavior, not just endpoints.

## 5.1 Action Submit

1. Trigger on `.act` write+close.
2. Reject malformed payload at close-time without emitting `action.accepted`.
3. Emit exactly one terminal event for accepted requests.

## 5.2 Streaming and Replay

1. Keep stream append order stable per request.
2. Preserve `event_id` and `request_id`.
3. Support replay surfaces (`cursor`, `from-seq`).

## 5.3 Paging

1. `cat` pageable resource returns first page + handle.
2. `/_paging/fetch_next.act` returns next page envelope.
3. `/_paging/close.act` is idempotent and deterministic.

## 5.4 Safety and Portability

1. Reject unsafe paths before side effects.
2. Keep segment naming cross-platform safe.
3. Keep overlong runtime handles deterministically normalized.

## 6. Bridge Runtime and Resilience Knobs

Runtime knobs (for HTTP/gRPC bridge mode):

1. `APPFS_ADAPTER_BRIDGE_MAX_RETRIES`
2. `APPFS_ADAPTER_BRIDGE_INITIAL_BACKOFF_MS`
3. `APPFS_ADAPTER_BRIDGE_MAX_BACKOFF_MS`
4. `APPFS_ADAPTER_BRIDGE_CIRCUIT_BREAKER_FAILURES`
5. `APPFS_ADAPTER_BRIDGE_CIRCUIT_BREAKER_COOLDOWN_MS`

CT-017 uses:

1. `APPFS_BRIDGE_RESILIENCE_CONTRACT=1`
2. `APPFS_BRIDGE_FAULT_CONFIG_PATH` (fault injector hot-reload file)

## 7. CI Integration Baseline

Minimum gate:

1. Linux static AppFS contract suite.
2. Linux live AppFS suite.
3. HTTP bridge live suite.

Recommended:

1. gRPC bridge suite as informational gate.
2. Bridge-side unit tests (validation/fault injection/server dispatch).

Reference CI tiering in this repo:

1. Required: `appfs-contract-gate`, `appfs-contract-gate-http-bridge`
2. Informational: `appfs-contract-gate-grpc-bridge` (`continue-on-error`)

## 8. Troubleshooting Handbook

## 8.1 `Address already in use`

Symptoms:

1. Bridge fails to bind and exits early.

Actions:

1. Check listener:
   - `ss -ltnp | grep ':8080'`
2. Switch port in one place:
   - `APPFS_ADAPTER_HTTP_ENDPOINT=http://127.0.0.1:9000 sh ./run-conformance.sh`
3. Ensure bridge startup script derives `APPFS_BRIDGE_HOST/PORT` from endpoint.

## 8.2 `Start directory is not importable: tests` (Python 3.12+)

Actions:

1. Add `tests/__init__.py`.
2. Run discover with top-level:
   - `uv run python -m unittest discover -s tests -t . -p "test_*.py"`

## 8.3 `ModuleNotFoundError: grpc`

Actions:

1. Install bridge deps:
   - `python3 -m pip install -r requirements.txt`
2. Regenerate stubs before run:
   - `./generate_stubs.sh`

## 8.4 CT-017 failures (`missing action.failed`, no circuit open)

Actions:

1. Verify resilience env vars are set.
2. Verify fault path prefix matches runtime probe path.
3. Check adapter log for `retry`, `circuit opened`, `short-circuit`.
4. Ensure breaker cooldown >= probe minimum (default `4000ms`).

## 8.5 Live mount fails or hangs

Actions:

1. Validate FUSE dependencies on Linux.
2. Clear stale mountpoint and old mount process.
3. Tail logs:
   - `cli/appfs-mount-live.log`
   - `cli/appfs-adapter-live.log`

## 9. Compatibility Claim Checklist

Before claiming `AppFS v0.1 Core`:

1. `CT-001 ~ CT-017` pass on your target path.
2. Adapter requirements checklist has evidence links.
3. Manifest includes conformance block.
4. CI gate is green for required suites.

## 10. Suggested Next Build Order (After v0.1)

1. Add one real app connector (not only mock backend).
2. Add adapter bootstrap template (`new-adapter` scaffold).
3. Extend compatibility matrix (language x transport x capability).

Current reference implementations:

1. Real-upstream backend mode:
   - `examples/appfs/http-bridge/python/appfs_http_bridge/jsonplaceholder_backend.py`
   - Enable by `APPFS_HTTP_BRIDGE_BACKEND=jsonplaceholder`
2. Scaffold generator:
   - `examples/appfs/new-adapter.sh <adapter_id>`
   - Generates `examples/appfs/adapters/<adapter_id>/python`
