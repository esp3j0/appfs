# AppFS Example Tree

This directory contains AppFS fixtures and bridge examples used by v0.1 baseline and v0.3 connectorization flows.

## Layout

1. `.well-known/apps.res.json` for app discovery.
2. `aiim/_meta/*` for manifest/context/permissions/schema metadata.
3. `aiim/_stream/*` sample event stream + replay snapshots.
4. `aiim/_paging/*` action sinks for live resource paging protocol.
5. `aiim/_snapshot/refresh.act` for explicit snapshot materialization checks.
6. Resource/action sample paths under `contacts/`, `files/`, `chats/`, `feed/`.
7. `http-bridge/python/` and `grpc-bridge/python/` out-of-process bridge examples.
8. `adapter-template/rust-minimal/` minimal Rust adapter template using frozen `AppAdapterV1`.
9. `new-adapter.sh` one-command scaffold for new Python HTTP bridge adapters.

## Contract Checks

Static fixture check:

```bash
cd cli
APPFS_CONTRACT_TESTS=1 APPFS_STATIC_FIXTURE=1 APPFS_ROOT="$PWD/../examples/appfs" sh ./tests/test-appfs-contract.sh
```

Live conformance (one command):

```bash
cd examples/appfs
sh ./run-conformance.sh inprocess
sh ./run-conformance.sh http-python
sh ./run-conformance.sh grpc-python
```

## V0.3 Demo Connector Parity

`sdk/rust/src/appfs_demo_adapter.rs` (`DemoAppConnectorV2`) is the canonical demo behavior surface.
In-process / HTTP / gRPC should match on business semantics for:

1. `connector_info` / `health`
2. `prewarm_snapshot_meta`
3. `fetch_snapshot_chunk`
4. `fetch_live_page`
5. `submit_action`

Only transport-specific differences should remain:

1. `connector_id`
2. `transport`
3. bridge wrapper details (serialization / endpoint envelope)

Parity fixture highlights:

1. snapshot start records: `rk-001/rk-002` with `{"id":"m-1","text":"hello"}` and `{"id":"m-2","text":"world"}`
2. snapshot cursor follow-up (`cursor-2`): `rk-003` with `{"id":"m-3","text":"done"}`
3. snapshot `emitted_bytes`: compact JSON line bytes + newline (`+1`) per record
4. live page: `handle_id=demo-live-handle-1`, page 1 -> `next_cursor=cursor-1`, page 2 -> no next cursor
5. live items: `{"id":"item-{page_no}","resource":"<resource_path>"}`
6. inline submit outcome: `{"ok":true,"path":"...","echo":<payload>}`
7. streaming submit plan: accepted `{"state":"accepted"}`, progress `{"percent":50}`, terminal `{"ok":true}`

See `ADAPTER-QUICKSTART.md` for adapter author workflow.
For full workflow + troubleshooting, see `../../docs/v1/APPFS-adapter-developer-guide-v0.1.md`.

