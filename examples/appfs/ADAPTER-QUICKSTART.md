# AppFS Adapter Quickstart (MVP)

This guide targets adapter authors who want to pass AppFS v0.1 conformance with minimum setup.

For full implementation and troubleshooting details, use:

1. `doc/APPFS-adapter-developer-guide-v0.1.md`
2. `doc/APPFS-adapter-structure-mapping-v0.1.md`

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
3. Execute `CT-001` to `CT-017` via `cli/tests/appfs/run-live-with-adapter.sh`.

## 3. Define Structure Before Writing Handlers

Before coding bridge handlers, define:

1. Node templates in `manifest.res.json` (`*.res.json`, `*.act`).
2. Real sink/resource files under `/app/<app_id>/...`.
3. A node-to-handler mapping table.

Reference:

1. `../../doc/APPFS-adapter-structure-mapping-v0.1.md`

## 4. Minimal Rust Adapter Template

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

## 5. HTTP Bridge Starter

Starter location:

1. `examples/appfs/http-bridge/python/bridge_server.py`
2. `examples/appfs/http-bridge/python/run-conformance.sh`

Manual run:

```bash
cd examples/appfs/http-bridge/python
uv run python bridge_server.py
```

## 6. gRPC Bridge Starter

Starter location:

1. `examples/appfs/grpc-bridge/python/grpc_server.py`
2. `examples/appfs/grpc-bridge/python/run-conformance.sh`

Before running gRPC quickstart:

1. Install dependencies in `examples/appfs/grpc-bridge/python/requirements.txt`.
2. Generate stubs via `./generate_stubs.sh`.

## 7. Compatibility Checklist (Minimum)

Before claiming compatibility, verify:

1. `.act` write+close submit semantics.
2. Stream lifecycle and replay surfaces.
3. Paging handle error mapping (`fetch_next`, `close`).
4. `AppAdapterV1` contract compliance.
5. CI/static/live conformance evidence.
6. Declared node templates and bridge handlers are fully mapped 1:1.

Reference docs:

1. `../../doc/APPFS-v0.1.md`
2. `../../doc/APPFS-adapter-requirements-v0.1.md`
3. `../../doc/APPFS-compatibility-matrix-v0.1.md`
4. `../../doc/APPFS-conformance-v0.1.md`
5. `../../doc/APPFS-contract-tests-v0.1.md`
6. `../../doc/APPFS-adapter-developer-guide-v0.1.md`
7. `../../doc/APPFS-adapter-structure-mapping-v0.1.md`

## 8. Troubleshooting Entry

If you hit runtime/bridge test failures (port conflicts, `uv` issues, gRPC deps, CT-017 failures), start from:

1. `../../doc/APPFS-adapter-developer-guide-v0.1.md#8-troubleshooting-handbook`

## 9. Scaffold New Adapter

Generate a new Python HTTP bridge scaffold:

```bash
sh ./new-adapter.sh myapp
```

Generated path:

1. `./adapters/myapp/python`

For custom app fixtures during live conformance, override:

1. `APPFS_FIXTURE_DIR`
2. `APPFS_APP_ID`
