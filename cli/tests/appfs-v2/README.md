# AppFS v2 Contract Skeleton

This directory hosts the Phase B minimal executable CT2 skeleton.

Current scope:

1. `CT2-002` snapshot read hit path
2. `CT2-007` ActionLineV2 JSONL parsing
3. `CT2-008` submit-time rejection rules
4. `CT2-009` snapshot/live dual semantics

Run:

```bash
cd cli
APPFS_V2_CONTRACT_TESTS=1 ./tests/test-appfs-v2-contract.sh
```

Strict mode (treat pending skeleton as failure):

```bash
cd cli
APPFS_V2_CONTRACT_TESTS=1 APPFS_V2_STRICT=1 ./tests/test-appfs-v2-contract.sh
```

Notes:

1. Exit code `2` means skeleton pending (`XFAIL` in non-strict mode).
2. This suite is additive and does not change v0.1 contract gates.
