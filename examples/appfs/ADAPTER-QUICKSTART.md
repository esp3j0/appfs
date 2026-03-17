# AppFS Adapter Quickstart (MVP)

This guide targets adapter authors who want to pass AppFS v0.1 conformance with minimum setup.

## 1. Choose Adapter Path

1. In-process (Rust runtime demo path):
   - Fastest way to run the full live suite.
2. Out-of-process HTTP bridge:
   - Easy polyglot integration.
3. Out-of-process gRPC bridge:
   - Better typed transport contract for multi-language implementations.

## 2. One-Command Conformance

From this directory:

```bash
cd examples/appfs
sh ./run-conformance.sh inprocess
sh ./run-conformance.sh http-python
sh ./run-conformance.sh grpc-python
```

What it runs:

1. Mount AgentFS live filesystem.
2. Start adapter runtime (or runtime + bridge endpoint).
3. Execute `CT-001` to `CT-016` via `cli/tests/appfs/run-live-with-adapter.sh`.

## 3. Minimal Rust Adapter Template

Template location:

1. `examples/appfs/adapter-template/rust-minimal`

Template commands:

```bash
cd examples/appfs/adapter-template/rust-minimal
cargo test
```

Template tests use frozen SDK matrix runners:

1. `run_required_case_matrix_v1`
2. `run_error_case_matrix_v1`

## 4. HTTP Bridge Starter

Starter location:

1. `examples/appfs/http-bridge/python/bridge_server.py`
2. `examples/appfs/http-bridge/python/run-conformance.sh`

Manual run:

```bash
cd examples/appfs/http-bridge/python
python3 bridge_server.py
```

## 5. gRPC Bridge Starter

Starter location:

1. `examples/appfs/grpc-bridge/python/grpc_server.py`
2. `examples/appfs/grpc-bridge/python/run-conformance.sh`

Before running gRPC quickstart:

1. Install dependencies in `examples/appfs/grpc-bridge/python/requirements.txt`.
2. Generate stubs via `./generate_stubs.sh`.

## 6. Compatibility Checklist (Minimum)

Before claiming compatibility, verify:

1. `.act` write+close submit semantics.
2. Stream lifecycle and replay surfaces.
3. Paging handle error mapping (`fetch_next`, `close`).
4. `AppAdapterV1` contract compliance.
5. CI/static/live conformance evidence.

Reference docs:

1. `APPFS-v0.1.md`
2. `APPFS-adapter-requirements-v0.1.md`
3. `APPFS-conformance-v0.1.md`
4. `APPFS-contract-tests-v0.1.md`
