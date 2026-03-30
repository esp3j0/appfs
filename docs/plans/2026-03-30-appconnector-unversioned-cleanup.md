# AppConnector Unversioned Cleanup Design

**Date:** 2026-03-30  
**Status:** Frozen (U1, 2026-03-30)  
**Scope:** Remove `V2` / `V3` naming and compatibility layers from the AppFS connector/runtime surface

## 1. Goal

Do one explicit cleanup pass so the AppFS codebase stops exposing three different conceptual layers at once:

1. old business connector naming (`V2`)
2. structure-sync extension naming (`V3`)
3. unified runtime-facing connector naming (`AppConnector`)

After this cleanup, the codebase should read as if AppFS has only one current connector model:

1. a single `AppConnector` trait;
2. a single unversioned request/response type family;
3. unversioned bridge routes / services;
4. unversioned tests, docs, and examples.

This cleanup is intentionally breaking. We are not preserving backward compatibility in this phase.

## 2. Why This Cleanup Is Worth Doing

The runtime architecture has already been conceptually unified, but the code still makes readers carry historical context that is no longer useful.

Current problems:

1. `sdk/rust/src/appfs_connector_v2.rs`, `sdk/rust/src/appfs_connector_v3.rs`, and `sdk/rust/src/appfs_connector.rs` split one conceptual model across three files.
2. SDK exports still advertise `AppConnectorV2`, `AppConnectorV3`, and many `*V2` / `*V3` data types even though runtime code is supposed to think in terms of `AppConnector`.
3. adapters such as HTTP and gRPC bridges keep names like `HttpBridgeConnectorV2` and `GrpcBridgeConnectorV2`, which implies the version split is still important.
4. tests, docs, and helper scripts still encode the old version names, which makes new contributors infer that both surfaces are still first-class.
5. keeping the old version names in wire contracts and file/module names creates accidental coupling between "historical rollout order" and "current product model".

## 3. Main Decisions

### 3.1 `AppConnector` becomes the only public connector model

The canonical AppFS connector surface is:

`AppConnector`

It owns all connector capabilities:

1. connector identity / health
2. snapshot prewarm / fetch
3. live paging
4. action submission
5. structure bootstrap / refresh

The following traits stop existing as public concepts:

1. `AppConnectorV2`
2. `AppConnectorV3`

### 3.2 All public connector types become unversioned

The public SDK should expose a single unversioned family:

1. `ConnectorInfo`
2. `ConnectorContext`
3. `ConnectorError`
4. `FetchSnapshotChunkRequest` / `Response`
5. `FetchLivePageRequest` / `Response`
6. `SubmitActionRequest` / `Response`
7. `GetAppStructureRequest` / `Response`
8. `RefreshAppStructureRequest` / `Response`
9. `AppStructureSnapshot`
10. `AppStructureNode`
11. `AppStructureSyncReason`
12. `AppStructureSyncResult`

The following version-suffixed type families should be removed instead of merely re-exported:

1. `*V2`
2. `*V3`

### 3.3 Compatibility shims are removed, not hidden

We are explicitly choosing not to carry a compatibility window.

That means:

1. remove `sdk/rust/src/appfs_connector_v2.rs`
2. remove `sdk/rust/src/appfs_connector_v3.rs`
3. remove re-exports of versioned types from `sdk/rust/src/lib.rs`
4. rename implementations and adapters to unversioned names

This is cleaner than preserving wrapper aliases that keep leaking history into the code.

### 3.4 Wire contracts also lose version suffixes

Because we are not preserving backward compatibility, transport surfaces should also be cleaned up.

HTTP:

1. `/v2/connector/*` and `/v3/connector/structure/*` are replaced by canonical `/connector/*` endpoints

gRPC:

1. `appfs.connector.v2` and `appfs.connector.v3` are replaced by a single canonical `appfs.connector`
2. request/response messages and service names lose `V2` / `V3` suffixes

The bridges may keep migration helpers only inside tests while the refactor lands, but shipping code should not present versioned wire shapes.

### 3.5 Tests and docs stop encoding rollout-era version names

This cleanup is not complete until the naming is gone from:

1. contract tests
2. Windows regression scripts
3. README and architecture docs
4. examples and bridge READMEs
5. release notes / changelog entries that describe current architecture

Historical docs may still mention `v0.3` / `v0.4` as milestones, but those are architecture milestones, not active connector API names.

## 4. Scope Boundaries

### In Scope

1. Rust SDK connector modules and exports
2. demo connector
3. runtime and mount-side read-through code
4. HTTP bridge routes and payload types
5. gRPC proto/service names and generated bindings
6. tests, scripts, docs, and examples tied to connector naming

### Out of Scope

1. `AppAdapterV1`
2. non-AppFS AgentFS core APIs
3. external package rename of the `agentfs` CLI itself
4. deeper protocol redesign beyond removing version suffixes

## 5. Implementation Order

### U1. Freeze the cleanup

1. freeze this plan
2. update ADR language so `AppConnector` is the only current connector concept

### U2. SDK cleanup

1. move unversioned type definitions into `sdk/rust/src/appfs_connector.rs`
2. remove `appfs_connector_v2.rs` and `appfs_connector_v3.rs`
3. update `sdk/rust/src/lib.rs` exports
4. rename demo connector types and tests

### U3. Runtime and mount cleanup

1. update runtime, tree sync, paging, snapshot cache, mount runtime, and tests to use unversioned types
2. rename `LegacyAdapterConnectorV2`, `HttpBridgeConnectorV2`, `GrpcBridgeConnectorV2`, `DemoAppConnectorV2`

### U4. Transport cleanup

1. rename HTTP routes to canonical `/connector/*`
2. rename gRPC proto package/service/message names to canonical forms
3. update Python bridge servers and generated bindings

### U5. Test and documentation cleanup

1. remove `v2` / `v3` naming from current contract scripts where they refer to connector surface
2. update docs and examples
3. keep milestone docs intact where they describe past rollout phases

## 6. Acceptance Criteria

The cleanup is complete when:

1. there is no shipping Rust trait named `AppConnectorV2` or `AppConnectorV3`
2. there are no shipping connector request/response types with `V2` / `V3` suffixes
3. runtime and mount code compile against only unversioned connector types
4. HTTP and gRPC bridges expose canonical unversioned connector routes / services
5. tests pass without referring to current connector behavior as `v2` or `v3`

## 7. Risks

1. gRPC codegen churn will touch many files at once.
2. transport renaming can temporarily break tests and example scripts.
3. some historical docs should remain as historical records; the cleanup should target active docs and code, not erase rollout history.

These risks are acceptable because the codebase is still pre-stable and we explicitly do not require compatibility for this pass.
