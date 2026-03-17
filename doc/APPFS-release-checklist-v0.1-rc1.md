# AppFS v0.1-rc1 Release Checklist

- Target: `v0.1-rc1`
- Date: `2026-03-17`
- Status: `Closed (Ready to tag)`
- Freeze commit (implementation baseline): `e521262`

## 1. Release Scope

This RC validates AppFS v0.1 Core semantics and adapter portability path on top of AgentFS runtime.

In scope:

1. AppFS core contract (`.act` sink semantics, event stream, replay, paging cursor/handle flow).
2. Adapter SDK v0.1 frozen surface (`AppAdapterV1`).
3. In-process adapter path and out-of-process bridge parity (HTTP and gRPC).
4. CI gate for static + live contract suites.

Out of scope (explicitly deferred to v0.2+):

1. Unified cancel endpoint across all apps.
2. Standardized idempotency policy at AppFS spec layer.
3. Stream QoS/backpressure classes.
4. Multi-tenant sharing model.

## 2. Frozen Inputs

Core docs frozen for this RC:

1. `APPFS-v0.1.md` (`0.1-draft-r8`)
2. `APPFS-adapter-requirements-v0.1.md` (`0.1-draft-r5`)
3. `APPFS-conformance-v0.1.md` (`0.1`)
4. `APPFS-contract-tests-v0.1.md` (`0.1-draft-r10`)
5. `APPFS-adapter-http-bridge-v0.1.md`
6. `APPFS-adapter-grpc-bridge-v0.1.md`

Implementation surfaces frozen for this RC:

1. `cli/src/cmd/appfs.rs`
2. `sdk/rust/src/appfs_adapter.rs`
3. `sdk/rust/src/appfs_adapter_testkit.rs`
4. `sdk/rust/src/appfs_demo_adapter.rs`
5. `cli/tests/appfs/*`
6. `.github/workflows/rust.yml`

## 3. Go/No-Go Gates

Required before tagging:

1. `main` Rust CI green, including:
   - `AppFS Contract Gate (linux)`
   - `AppFS Contract Gate (linux, http bridge)`
   - `AppFS Contract Gate (linux, grpc bridge)`
   - `Test (ubuntu-latest, cli)`
   - `Test (ubuntu-latest, sdk/rust)`
   - Bridge gate runs include resilience probe `CT-017` (retry/circuit/cooldown)
2. `main` Python CI green.
3. `main` TypeScript CI green.
4. Branch protection for `main` enforces PR-only and required checks.
5. No open blocker in AppFS v0.1 Core checklist.

Evidence snapshot:

1. Rust CI (push/main): `23175318095`
2. Python CI (push/main): `23175318076`
3. TypeScript CI (push/main): `23175318074`
4. Remote live validation (HTTP bridge): `/tmp/appfs-http-live-run.log` (`CT-017 done`, `LIVE AppFS contract tests passed.`)
5. Remote live validation (gRPC bridge): `/tmp/appfs-grpc-live-run.log` (`CT-017 done`, `LIVE AppFS contract tests passed.`)
6. RC closure report: `APPFS-rc-closure-v0.1.md`

## 4. Release Commands (Tag + Release)

```bash
git fetch origin
git checkout main
git pull --ff-only origin main

# choose exact commit if needed
git tag -a appfs-v0.1-rc1 -m "AppFS v0.1-rc1"
git push origin appfs-v0.1-rc1

# optional GitHub release draft
gh release create appfs-v0.1-rc1 \
  --repo esp3j0/appfs \
  --title "AppFS v0.1-rc1" \
  --notes-file APPFS-release-notes-v0.1-rc1.md \
  --draft
```

## 5. Post-Tag Checks

1. Verify release workflow sees the new tag and runs successfully.
2. Verify release artifacts/announcement behavior matches repo policy.
3. Freeze `APPFS-v0.1` wording from `Draft` to `RC` only after RC sign-off.
