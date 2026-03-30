# AppFS Connector Contract Tests

This directory hosts the executable connector contract suite for AppFS.

Current Linux required set:

1. `CT2-001` startup prewarm
2. `CT2-002` snapshot read hit path
3. `CT2-003` read miss expand
4. `CT2-004` concurrent dedupe
5. `CT2-005` snapshot too-large mapping
6. `CT2-006` recovery incomplete expand
7. `CT2-007` ActionLine JSONL parsing
8. `CT2-008` submit-time rejection rules
9. `CT2-009` snapshot/live dual semantics

Extended coverage:

1. `CT2-028` timeout `return_stale` fallback + stale structural validation

Informational matrix:

1. `CT2-010` minimal cross-platform consistency (`test-ct2-010-cross-platform-minimal.sh`)

Run:

```bash
cd cli
APPFS_CONNECTOR_CONTRACT_TESTS=1 ./tests/test-appfs-connector-contract.sh
```

Strict mode (treat any pending extended case as failure):

```bash
cd cli
APPFS_CONNECTOR_CONTRACT_TESTS=1 APPFS_CONNECTOR_STRICT=1 ./tests/test-appfs-connector-contract.sh
```

Subset selection (for bridge-specific CI/local runs):

```bash
cd cli
APPFS_CONNECTOR_CONTRACT_TESTS=1 \
APPFS_CONNECTOR_REQUIRED_CASES='ct2-002,ct2-007,ct2-008,ct2-009' \
APPFS_CONNECTOR_EXTENDED_CASES='none' \
./tests/test-appfs-connector-contract.sh
```

```bash
cd cli
APPFS_CONNECTOR_CONTRACT_TESTS=1 \
APPFS_CONNECTOR_REQUIRED_CASES='test-ct2-002-snapshot-hit.sh test-ct2-009-dual-shape.sh' \
APPFS_CONNECTOR_EXTENDED_CASES='ct2-028' \
./tests/test-appfs-connector-contract.sh
```

Run CT2-010 informational matrix:

```bash
cd cli
sh tests/appfs-connector/test-ct2-010-cross-platform-minimal.sh
```

Optional Linux-reference comparison:

```bash
cd cli
APPFS_CT2_010_REFERENCE=/path/to/linux-summary.json \
sh tests/appfs-connector/test-ct2-010-cross-platform-minimal.sh
```

Bridge gate tiers (CI steady-state):

1. HTTP bridge signal (informational): stable CT2 subset (`CT2-002/003/004/005/007/008/009`).
2. HTTP bridge high-risk signal (informational): `CT2-006` (required tier inside signal job) + `CT2-028` (extended tier).
3. gRPC bridge signal (informational): CT2 subset (`CT2-002/007/008/009`).
4. v0.1 live bridge suites: legacy baseline smoke (`tests/appfs/run-live-with-adapter.sh`) kept for bridge-path regression signal.

Notes:

1. Required set (`CT2-001..009`) must not return pending.
2. `CT2-001` builds `agentfs` before running by default to avoid stale local binaries. Set `APPFS_BUILD_BEFORE_RUN=0` to skip this rebuild.
3. Gate tiering above is CI positioning only; this runner's required/extended semantics are unchanged.
4. `CT2-010` is informational and is intentionally not part of the required gate.
5. Subset selectors:
   - `APPFS_CONNECTOR_REQUIRED_CASES`: comma- or space-separated; supports `CT2-ID` or any script/path containing `ct2-xxx`.
   - `APPFS_CONNECTOR_EXTENDED_CASES`: same format; use `none`/`off` to skip the extended tier.
   - Invalid or unknown selectors fail fast.
