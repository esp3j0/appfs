# AppFS v3 Contract Tests (runner surface path remains `test-appfs-v2-contract.sh`)

This directory hosts the executable AppFS v0.3 (V3 semantics) CT2 contract suite.

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
APPFS_V3_CONTRACT_TESTS=1 ./tests/test-appfs-v2-contract.sh
```

Strict mode (treat any pending extended case as failure):

```bash
cd cli
APPFS_V3_CONTRACT_TESTS=1 APPFS_V3_STRICT=1 ./tests/test-appfs-v2-contract.sh
```

Subset selection (for bridge-specific CI/local runs):

```bash
cd cli
APPFS_V3_CONTRACT_TESTS=1 \
APPFS_V3_REQUIRED_CASES='ct2-002,ct2-007,ct2-008,ct2-009' \
APPFS_V3_EXTENDED_CASES='none' \
./tests/test-appfs-v2-contract.sh
```

```bash
cd cli
APPFS_V3_CONTRACT_TESTS=1 \
APPFS_V3_REQUIRED_CASES='test-ct2-002-snapshot-hit.sh test-ct2-009-dual-shape.sh' \
APPFS_V3_EXTENDED_CASES='ct2-028' \
./tests/test-appfs-v2-contract.sh
```

Preferred v0.3/V3 env surface (legacy V2 aliases still accepted during migration window):

```bash
cd cli
APPFS_V3_CONTRACT_TESTS=1 \
APPFS_V3_REQUIRED_CASES='ct2-002,ct2-007,ct2-008,ct2-009' \
APPFS_V3_EXTENDED_CASES='none' \
./tests/test-appfs-v2-contract.sh
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

Bridge gate tiers (CI steady-state):

1. v0.3 HTTP bridge signal (informational): stable CT2 subset (`CT2-002/003/004/005/007/008/009`).
2. v0.3 HTTP bridge high-risk signal (informational): `CT2-006` (required tier inside signal job) + `CT2-028` (extended tier).
3. v0.3 gRPC bridge signal (informational): CT2 subset (`CT2-002/007/008/009`).
4. v0.1 live bridge suites: legacy baseline smoke (`tests/appfs/run-live-with-adapter.sh`) kept for bridge-path regression signal.

Notes:

1. Required set (`CT2-001..009`) must not return pending.
2. `CT2-001` builds `agentfs` before running by default to avoid stale local binaries. Set `APPFS_V3_BUILD_BEFORE_RUN=0` (or legacy alias `APPFS_V2_BUILD_BEFORE_RUN=0`) to skip this rebuild.
3. Gate tiering above is CI positioning only; this runner's required/extended semantics are unchanged.
4. `CT2-010` is informational in Phase E and is intentionally not part of the required gate.
5. Subset selectors:
   - `APPFS_V3_REQUIRED_CASES`: 逗号或空格分隔，支持 `CT2-ID` 或包含 `ct2-xxx` 的脚本名/路径（legacy alias: `APPFS_V2_REQUIRED_CASES`）。
   - `APPFS_V3_EXTENDED_CASES`: 同上；可设为 `none`/`off` 跳过 extended tier（legacy alias: `APPFS_V2_EXTENDED_CASES`）。
   - 非法或未知 selector 会显式失败，不会静默忽略。

Migration window (`APPFS_V2_*` -> `APPFS_V3_*`):

1. The runner now treats `APPFS_V3_*` as canonical and maps legacy `APPFS_V2_*` as aliases.
2. If both are set for the same knob, `APPFS_V3_*` wins.
3. Workflow/CI should use `APPFS_V3_*` going forward.
4. Compatibility aliases are temporary and will be removed in a later v0.3 closure issue after branch-protection/check-run migration completes.
5. The required CI check-run name is intentionally kept stable during this window (`AppFS Contract Gate (required, linux, inprocess v2)`) even though runner env semantics are now V3-first.
