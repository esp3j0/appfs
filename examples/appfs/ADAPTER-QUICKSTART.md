# AppFS Connector Quickstart

This quickstart is for the current AppFS connector path.

Design against the canonical `AppConnector` surface and the managed AppFS runtime.
Treat the in-process demo connector as the behavior reference; HTTP and gRPC bridges should match it.

Primary references:

1. `../../docs/v4/README.md`
2. `../../docs/v4/APPFS-v0.4-AppStructureSync-ADR.zh-CN.md`
3. `../../docs/v4/APPFS-v0.4-Connector结构接口.zh-CN.md`

## 1. Choose a Connector Path

1. in-process connector
2. out-of-process HTTP bridge
3. out-of-process gRPC bridge

## 2. Start from the Current Scaffold

Generate a Python HTTP connector scaffold:

```bash
sh ./new-connector.sh my-app
```

This creates:

```text
examples/appfs/connectors/my-app/http-python
```

The scaffold is based on the current `AppConnector` surface and current bridge contract.

## 3. Implement the Connector Surface

A complete connector should define:

1. `connector_info`
2. `health`
3. `prewarm_snapshot_meta`
4. `fetch_snapshot_chunk`
5. `fetch_live_page`
6. `submit_action`
7. `get_app_structure`
8. `refresh_app_structure`

Structure and fixture design should start from:

1. the connector-owned tree
2. scope transitions
3. snapshot resources
4. live pageable resources
5. action sinks

## 4. Verify Against the Live Harness

From this directory:

```bash
sh ./run-conformance.sh inprocess
sh ./run-conformance.sh http-python
sh ./run-conformance.sh grpc-python
```

For a generated connector:

```bash
cd connectors/my-app/http-python
uv run python -m unittest discover -s tests -t . -p "test_*.py"
APPFS_ADAPTER_HTTP_ENDPOINT=http://127.0.0.1:8080 sh ./run-conformance.sh
```

## 5. Runtime Model

The recommended user-facing runtime path is:

```bash
agentfs appfs up <id-or-path> <mountpoint>
```

Then register the app through `/_appfs/register_app.act`.

Do not design around:

1. `AppAdapterV1`
2. `/v1/submit-action` as the main integration surface
3. `/_snapshot/refresh.act` as the normal snapshot read path
4. `mount + serve appfs` as the main examples flow

## 6. Legacy Reference

Historical v0.1 materials are kept under:

1. `legacy/v1/`
2. `../../docs/v1/`
