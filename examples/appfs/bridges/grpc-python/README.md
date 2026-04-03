# AppFS gRPC Bridge Example (Python)

This directory is the current Python gRPC connector bridge reference.

It exposes the canonical AppFS connector surface over gRPC:

1. `AppfsConnector` for runtime traffic
2. `AppfsStructureConnector` for structure bootstrap and refresh

## Install Dependencies

```bash
cd examples/appfs/bridges/grpc-python
python3 -m venv .venv
source .venv/bin/activate
pip install -r requirements.txt
```

## Generate Stubs

```bash
cd examples/appfs/bridges/grpc-python
./generate_stubs.sh
```

Current canonical stubs are:

1. `appfs_connector_pb2.py`
2. `appfs_connector_pb2_grpc.py`
3. `appfs_structure_pb2.py`
4. `appfs_structure_pb2_grpc.py`

The script also generates a legacy adapter v1 stub for reference assets kept under `examples/appfs/legacy/v1/`.

## Start gRPC Server

```bash
cd examples/appfs/bridges/grpc-python
python3 grpc_server.py
```

Default listen endpoint: `127.0.0.1:50051`.

## Run Conformance

```bash
cd examples/appfs/bridges/grpc-python
./generate_stubs.sh
sh ./run-conformance.sh
```

## Recommended Runtime Path

For manual use, prefer:

```bash
agentfs appfs up <id-or-path> <mountpoint>
```

Then register an app through `/_appfs/register_app.act` using a gRPC transport payload.

The legacy HTTP gateway and adapter-v1 proto are retained only under `examples/appfs/legacy/v1/` and are not part of the current examples main path.
