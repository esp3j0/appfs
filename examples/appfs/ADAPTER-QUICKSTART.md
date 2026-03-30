# AppFS Connector Quickstart

This is the default quickstart for AppFS connector shipping path.
Treat Rust in-process `DemoAppConnector` as canonical behavior; HTTP/gRPC demos must match it.

Primary references:

1. `docs/v4/README.md`
2. `docs/v4/APPFS-v0.4-AppStructureSync-ADR.zh-CN.md`
3. `docs/v4/APPFS-v0.4-Connector结构接口.zh-CN.md`

## 1. Choose Connector Path

1. In-process connector (Rust runtime demo path)
2. Out-of-process HTTP connector bridge
3. Out-of-process gRPC connector bridge

## 2. Run Conformance

From this directory:

```bash
cd examples/appfs
sh ./run-conformance.sh inprocess
sh ./run-conformance.sh http-python
sh ./run-conformance.sh grpc-python
```

What it runs:

1. Mount AgentFS live filesystem.
2. Start runtime + selected connector transport path.
3. Execute contract tests via `cli/tests/appfs/run-live-with-adapter.sh`.

## 3. Canonical Demo Parity Checklist

Keep HTTP/gRPC behavior aligned with in-process canonical for:

1. `connector_info` / `health`
2. `prewarm_snapshot_meta`
3. `fetch_snapshot_chunk`
4. `fetch_live_page`
5. `submit_action`

Only transport-specific differences are allowed:

1. `connector_id`
2. `transport`
3. transport envelope details

Key parity fixtures:

1. snapshot start: `rk-001/rk-002`; cursor follow-up (`cursor-2`): `rk-003`
2. snapshot `emitted_bytes`: compact JSON line bytes + newline (`+1`) per record
3. live paging: `handle_id=demo-live-handle-1`, `cursor-1` progression
4. inline submit: `{"ok":true,"path":"...","echo":<payload>}`
5. streaming submit: accepted `{"state":"accepted"}`, progress `{"percent":50}`, terminal `{"ok":true}`

## 4. HTTP / gRPC Starters

1. HTTP: `examples/appfs/http-bridge/python/`
2. gRPC: `examples/appfs/grpc-bridge/python/` (run `./generate_stubs.sh` before server start)

## 5. Legacy Reference (v0.1)

The v0.1 `AppAdapterV1` guides/templates are retained only as legacy reference:

1. `../../docs/v1/APPFS-adapter-developer-guide-v0.1.md`
2. `examples/appfs/adapter-template/rust-minimal`
