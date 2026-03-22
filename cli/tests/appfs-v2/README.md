# AppFS v2 Contract Tests (Phase D)

This directory hosts the executable AppFS v2 CT2 contract suite.

Current Linux required set:

1. `CT2-001` startup prewarm
2. `CT2-002` snapshot read hit path
3. `CT2-003` read miss expand
4. `CT2-004` concurrent dedupe
5. `CT2-005` snapshot too-large mapping
6. `CT2-006` recovery incomplete expand
7. `CT2-007` ActionLineV2 JSONL parsing
8. `CT2-008` submit-time rejection rules
9. `CT2-009` snapshot/live dual semantics

Extended coverage:

1. `CT2-028` timeout `return_stale` fallback + stale structural validation

Informational matrix:

1. `CT2-010` minimal cross-platform consistency (`test-ct2-010-cross-platform-minimal.sh`)

Run:

```bash
cd cli
APPFS_V2_CONTRACT_TESTS=1 ./tests/test-appfs-v2-contract.sh
```

Strict mode (treat any pending extended case as failure):

```bash
cd cli
APPFS_V2_CONTRACT_TESTS=1 APPFS_V2_STRICT=1 ./tests/test-appfs-v2-contract.sh
```

Run CT2-010 informational matrix:

```bash
cd cli
sh tests/appfs-v2/test-ct2-010-cross-platform-minimal.sh
```

Optional Linux-reference comparison:

```bash
cd cli
APPFS_V2_CT2_010_REFERENCE=/path/to/linux-summary.json \
sh tests/appfs-v2/test-ct2-010-cross-platform-minimal.sh
```

Notes:

1. Required set (`CT2-001..009`) must not return pending.
2. `CT2-001` builds `agentfs` before running by default to avoid stale local binaries. Set `APPFS_V2_BUILD_BEFORE_RUN=0` to skip this rebuild.
3. In CI gate narrative, v0.1 baseline smoke remains in the AppFS v0.1 contract suites (`test-appfs-contract.sh` and `run-live-with-adapter.sh`), while this v2 suite validates the CT2 contract set.
4. `CT2-010` is informational in Phase E and is intentionally not part of the required gate.
