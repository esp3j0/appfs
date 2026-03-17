# AppFS Example Tree

This directory contains AppFS v0.1 reference fixtures and adapter examples aligned with `APPFS-v0.1 (r8)`.

## Layout

1. `.well-known/apps.res.json` for app discovery.
2. `aiim/_meta/*` for manifest/context/permissions/schema metadata.
3. `aiim/_stream/*` sample event stream + replay snapshots.
4. `aiim/_paging/*` action sinks for paging protocol.
5. Resource/action sample paths under `contacts/`, `files/`, `chats/`.
6. `http-bridge/python/` and `grpc-bridge/python/` out-of-process adapter bridge examples.
7. `adapter-template/rust-minimal/` minimal Rust adapter template using frozen `AppAdapterV1`.

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

See `ADAPTER-QUICKSTART.md` for adapter author workflow.
