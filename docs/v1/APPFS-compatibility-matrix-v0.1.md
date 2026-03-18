# AppFS Compatibility Matrix v0.1

- Version: `0.1`
- Date: `2026-03-17`
- Status: `Draft`
- Scope: Adapter implementation language x transport x capability level

## 1. Reading Rules

1. `Core` means AppFS v0.1 required compatibility claim.
2. `Recommended` means Core + recommended profile checks (observer/progress-policy surfaces if declared).
3. `Extension` means Core + app/vendor extension validation.
4. Minimum acceptance commands are intentionally shell-first and CI-aligned.

## 2. Matrix

| Language | Transport | Core (minimum acceptance command) | Recommended (minimum acceptance command) | Extension (minimum acceptance command) |
|---|---|---|---|---|
| Rust | in-process | `cd examples/appfs && sh ./run-conformance.sh inprocess` | Core command + `cat /app/<app_id>/_meta/manifest.res.json` and verify `conformance.recommended` matches implementation | Recommended command + run extension-specific contract script (example: `sh ./tests/appfs/test-<extension>.sh`) |
| Rust | HTTP bridge | `cd cli && APPFS_CONTRACT_TESTS=1 APPFS_ADAPTER_HTTP_ENDPOINT=http://127.0.0.1:8080 APPFS_BRIDGE_RESILIENCE_CONTRACT=1 sh ./tests/appfs/run-live-with-adapter.sh` | Core command + observer/progress-policy metadata verification | Recommended command + extension-specific contract script |
| Rust | gRPC bridge | `cd cli && APPFS_CONTRACT_TESTS=1 APPFS_ADAPTER_GRPC_ENDPOINT=http://127.0.0.1:50051 APPFS_BRIDGE_RESILIENCE_CONTRACT=1 sh ./tests/appfs/run-live-with-adapter.sh` | Core command + observer/progress-policy metadata verification | Recommended command + extension-specific contract script |
| Python | in-process | `N/A` (runtime in-process adapter surface is Rust-only in v0.1) | `N/A` | `N/A` |
| Python | HTTP bridge | `cd examples/appfs && sh ./run-conformance.sh http-python` | Core command + `cat /app/<app_id>/_meta/manifest.res.json` and verify recommended profile keys | Recommended command + extension-specific contract script |
| Python | gRPC bridge | `cd examples/appfs && sh ./run-conformance.sh grpc-python` | Core command + observer/progress-policy metadata verification | Recommended command + extension-specific contract script |
| Go | in-process | `N/A` (runtime in-process adapter surface is Rust-only in v0.1) | `N/A` | `N/A` |
| Go | HTTP bridge | `cd cli && APPFS_CONTRACT_TESTS=1 APPFS_ADAPTER_HTTP_ENDPOINT=http://127.0.0.1:8080 APPFS_BRIDGE_RESILIENCE_CONTRACT=1 sh ./tests/appfs/run-live-with-adapter.sh` | Core command + manifest recommended-profile verification | Recommended command + extension-specific contract script |
| Go | gRPC bridge | `cd cli && APPFS_CONTRACT_TESTS=1 APPFS_ADAPTER_GRPC_ENDPOINT=http://127.0.0.1:50051 APPFS_BRIDGE_RESILIENCE_CONTRACT=1 sh ./tests/appfs/run-live-with-adapter.sh` | Core command + manifest recommended-profile verification | Recommended command + extension-specific contract script |
| TypeScript | in-process | `N/A` (runtime in-process adapter surface is Rust-only in v0.1) | `N/A` | `N/A` |
| TypeScript | HTTP bridge | `cd cli && APPFS_CONTRACT_TESTS=1 APPFS_ADAPTER_HTTP_ENDPOINT=http://127.0.0.1:8080 APPFS_BRIDGE_RESILIENCE_CONTRACT=1 sh ./tests/appfs/run-live-with-adapter.sh` | Core command + manifest recommended-profile verification | Recommended command + extension-specific contract script |
| TypeScript | gRPC bridge | `cd cli && APPFS_CONTRACT_TESTS=1 APPFS_ADAPTER_GRPC_ENDPOINT=http://127.0.0.1:50051 APPFS_BRIDGE_RESILIENCE_CONTRACT=1 sh ./tests/appfs/run-live-with-adapter.sh` | Core command + manifest recommended-profile verification | Recommended command + extension-specific contract script |

## 3. CI Tier Mapping (Required vs Informational)

Required CI gates:

1. static contract + live in-process (`appfs-contract-gate`)
2. live HTTP bridge (`appfs-contract-gate-http-bridge`)

Informational CI gates:

1. live gRPC bridge (`appfs-contract-gate-grpc-bridge`, allowed to fail but must report signal)

## 4. Minimal Evidence for Compatibility Claim

For any matrix cell claiming Core:

1. command output for minimum acceptance command
2. contract suite summary (`CT-001` to `CT-019`, with `CT-017` expected when bridge resilience probe is enabled)
3. manifest conformance block snapshot

For Recommended/Extension:

1. Core evidence
2. declared recommended/extensions list in manifest
3. additional script/log evidence for each claimed item
