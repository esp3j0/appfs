# AppFS Example Tree

This directory contains a minimal AppFS example app (`aiim`) aligned with `APPFS-v0.1 (r7)`.

Contents:

1. `.well-known/apps.res.json` for app discovery.
2. `aiim/_meta/*` for manifest/context/permissions/schema metadata.
3. `aiim/_stream/*` sample event stream + replay snapshots.
4. `aiim/_paging/*` action sinks for paging protocol.
5. Resource/action sample paths under `contacts/`, `files/`, `chats/`.

Use with static contract checks:

```bash
cd cli
APPFS_CONTRACT_TESTS=1 APPFS_STATIC_FIXTURE=1 APPFS_ROOT=/mnt/c/Users/esp3j/rep/agentfs/examples/appfs ./tests/test-appfs-contract.sh
```
