# AppFS v0.1-rc1 Release Notes

- Version: `v0.1-rc1`
- Date: `2026-03-17`
- Repository: `esp3j0/appfs`
- Type: `Release Candidate`

## Highlights

1. AppFS v0.1 core protocol draft is stabilized for RC validation:
   - colocated `*.res.json` / `*.act` model
   - stream-first action lifecycle
   - runtime-generated request IDs
   - handle-based pagination (`fetch_next` / `close`)
2. Adapter abstraction is frozen at v0.1 surface (`AppAdapterV1`) for compatibility-oriented integration.
3. Out-of-process bridge parity is covered in both reference docs and CI (HTTP + gRPC).
4. Linux live contract gate is wired into CI and aligned with static fixture checks.
5. Bridge fault-tolerance contract probe is added (`CT-017`) to validate retry/circuit-breaker/cooldown recovery in HTTP and gRPC bridge jobs.

## What Is Included In RC1

1. Runtime AppFS serve loop (`agentfs serve appfs`) with action ingestion, event emission, replay/cursor surfaces, and paging controls.
2. SDK adapter contract/testkit:
   - `sdk/rust/src/appfs_adapter.rs`
   - `sdk/rust/src/appfs_adapter_testkit.rs`
3. Reference adapters:
   - in-process demo adapter
   - HTTP bridge reference mapping
   - gRPC bridge reference mapping
4. Contract tests:
   - `CT-001` to `CT-017`
   - static + live + bridge parity in CI

## CI Baseline For RC1

Main-branch baseline runs (success):

1. Rust CI: `23175318095`
2. Python CI: `23175318076`
3. TypeScript CI: `23175318074`

Manual closure validation (2026-03-17):

1. HTTP bridge live contract run passed (`CT-001`~`CT-017`) on `fsapp:1:test.0`
2. gRPC bridge live contract run passed (`CT-001`~`CT-017`) on `fsapp:1:test.0`
3. Closure record: `APPFS-rc-closure-v0.1.md`

## Known Gaps (Deferred)

1. Unified cancel semantics across apps.
2. Standardized idempotency behavior in spec (currently app-defined).
3. Stream QoS/backpressure classes.
4. Multi-tenant sharing/user isolation model.

## Compatibility Notes For Adapter Authors

1. Any implementation language is allowed.
2. Compatibility is behavior-based (conformance + contract tests), not runtime-language based.
3. v0.1 adapter surface is additive-only in `0.1.x`; breaking changes are deferred to `v0.2`.
