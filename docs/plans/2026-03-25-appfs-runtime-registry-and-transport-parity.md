# AppFS Runtime Registry And Transport Parity Design

**Date:** 2026-03-25  
**Status:** Implemented  
**Scope:** AppFS v0.4 follow-up after A3-A7  
**Outcome:** Implemented via `B1-B4` (`#106`-`#109`) and later folded into the managed-first closure pass documented in `2026-03-26-appfs-runtime-closure-design.md`

## 1. Problem Summary

Current AppFS state is internally inconsistent in three places:

1. `AppConnectorV3` structure sync exists only for in-process demo connectors.
2. `mount` and `serve appfs` both require app/backend/session configuration because each process owns its own connector configuration surface.
3. Multi-app support is startup-static. The runtime can start with multiple apps, but it cannot add or remove apps dynamically after startup.

This produces four concrete user-facing defects:

1. Real HTTP/gRPC connectors cannot bootstrap or refresh app tree structure.
2. `cargo run -- mount` must still be told which app(s) exist for snapshot read-through routing.
3. `cargo run -- serve appfs` must be told the same app/backend configuration again for action/event/tree-sync runtime work.
4. Dynamic app lifecycle is missing. A newly opened app cannot be registered into a running system without restarting both sides.

The immediate design goal is therefore:

1. make `AppConnectorV3` transport-complete;
2. eliminate duplicated app/backend/session configuration as the operational source of truth;
3. allow `serve appfs` to manage app structure and app lifecycle explicitly;
4. keep mount-side snapshot read-through working without turning `mount` into the app lifecycle owner.

## 2. Design Goals

### Functional goals

1. HTTP bridge implements `get_app_structure` and `refresh_app_structure`.
2. gRPC bridge implements `get_app_structure` and `refresh_app_structure`.
3. `serve appfs` can initialize an app tree using structure sync without relying on pre-existing fixture layout.
4. `serve appfs` can explicitly add, refresh, and remove apps at runtime.
5. `mount` can route snapshot read-through for all registered apps without needing duplicated per-app CLI config.

### Non-goals

1. No mount-to-serve local IPC in this phase.
2. No automatic connector discovery from arbitrary external processes.
3. No attempt to make `mount` itself own app structure sync or app registry mutations.
4. No change to snapshot read-through semantics established in the previous phase.

## 3. Current Architecture Defects

### 3.1 Transport asymmetry

`build_structure_connector(...)` currently returns `None` whenever HTTP or gRPC endpoints are configured. This means:

1. `ensure_app_structure_initialized(...)` only works when the runtime can construct the in-process demo connector.
2. real bridge-backed connectors cannot bootstrap tree structure;
3. explicit structure actions exist, but only the in-process path can satisfy them.

### 3.2 Split configuration authority

Today:

1. `mount` owns app IDs and bridge config for snapshot read-through;
2. `serve appfs` owns app IDs and bridge config for action dispatch, stream emission, and structure sync.

This creates operational drift:

1. both processes must be told the same app list;
2. both processes must be told the same transport endpoint;
3. a mismatch is possible and difficult to diagnose;
4. the system has no single persisted app registry.

### 3.3 Static multi-app lifecycle

Current multi-app behavior is “start with N apps”. It is not “running system accepts app registration changes”.

That is insufficient for the intended workflow:

1. user opens a new app;
2. connector publishes new structure;
3. runtime should register it;
4. mount should expose it;
5. user should not need a full restart.

## 4. Key Architectural Decision

The correct next-step source of truth is a **shared persisted app registry** under the AgentFS root, owned by `serve appfs`.

The registry should become the operational authority for:

1. registered app IDs;
2. per-app connector transport configuration;
3. per-app session identity;
4. optional per-app active transport mode;
5. per-app structure sync revision/scope metadata pointers.

`serve appfs` should own mutations to this registry.  
`mount` should only consume it.

This keeps responsibilities clean:

1. `serve appfs` owns app lifecycle and structure sync.
2. `mount` owns filesystem exposure and snapshot read-through.
3. connector transport is configured once, then persisted.

## 5. Proposed Architecture

## 5.1 New shared document: app registry

Introduce a persisted registry file under the mounted root:

`/_appfs/apps.registry.json`

Suggested shape:

```json
{
  "version": 1,
  "apps": [
    {
      "app_id": "aiim",
      "transport": {
        "kind": "http",
        "endpoint": "http://127.0.0.1:8080",
        "http_timeout_ms": 5000,
        "grpc_timeout_ms": 5000,
        "bridge_max_retries": 2,
        "bridge_initial_backoff_ms": 100,
        "bridge_max_backoff_ms": 1000,
        "bridge_circuit_breaker_failures": 5,
        "bridge_circuit_breaker_cooldown_ms": 3000
      },
      "session_id": "sess-aiim-001",
      "registered_at": "2026-03-25T10:00:00Z",
      "active_scope": "chat-001"
    }
  ]
}
```

Rules:

1. this file is runtime-owned, not connector-owned;
2. `serve appfs` writes it atomically;
3. `mount` reads it lazily and refreshes its in-memory routing table when the file changes.

## 5.2 Transport parity layer

We need full V3 transport parity:

1. HTTP bridge V3 endpoints
2. gRPC bridge V3 endpoints
3. Rust bridge client adapters implementing `AppConnectorV3`

These should remain thin transport shims:

1. request validation
2. error envelope normalization
3. exact mapping of V3 structure request/response types

They must not own reconciliation policy.

## 5.3 Runtime lifecycle supervisor

Extend the current multi-app supervisor into a real app lifecycle manager.

Add explicit runtime control actions:

1. `/_appfs/register_app.act`
2. `/_appfs/unregister_app.act`
3. `/_appfs/list_apps.act`
4. keep per-app `/_app/enter_scope.act`
5. keep per-app `/_app/refresh_structure.act`

Responsibilities of `register_app`:

1. validate app config payload;
2. persist or update app registry entry;
3. construct connector(s);
4. bootstrap app structure;
5. start action/event polling for the app;
6. emit success/failure event.

Responsibilities of `unregister_app`:

1. stop polling the app;
2. persist registry removal;
3. optionally leave app tree on disk in a detached state for post-mortem;
4. do not delete runtime-owned stream history by default.

## 5.4 Mount-side registry consumer

`mount` should stop requiring per-app transport configuration once a registry exists.

Proposed mount behavior:

1. if explicit `--appfs-app-id` / endpoint flags are given, treat them as bootstrap or override mode;
2. otherwise, load registry from `/_appfs/apps.registry.json`;
3. create read-through runtimes for all registered apps;
4. refresh routing table on registry change detection.

That gives us two operational modes:

1. bootstrap mode: explicit CLI config
2. managed mode: registry-driven

The target steady state is managed mode.

## 6. Runtime Flow Changes

### 6.1 Startup

Recommended startup flow:

1. user starts `serve appfs` with either:
   - explicit bootstrap app config, or
   - `--managed` and an existing registry;
2. runtime loads registry;
3. runtime creates/refreshes app adapters for all registered apps;
4. runtime bootstraps structure via V3 connector;
5. runtime persists updated registry state if sessions/scopes changed.

### 6.2 Registering a new app at runtime

1. caller writes to `/_appfs/register_app.act`;
2. runtime validates payload;
3. runtime creates registry entry;
4. runtime executes `get_app_structure`;
5. runtime materializes connector-owned tree;
6. mount notices registry change and begins routing snapshot read-through for that app.

No restart is required.

### 6.3 Entering a new scope

1. caller writes to `/app/<app_id>/_app/enter_scope.act`;
2. runtime invokes `refresh_app_structure`;
3. runtime reconciles new tree;
4. registry active scope is updated if changed.

### 6.4 Mount read-through

Mount-side read-through remains path-driven:

1. split app ID from filesystem path;
2. resolve app transport config from in-memory registry snapshot;
3. perform V2 snapshot calls for that app;
4. do not own tree sync.

This preserves the separation:

1. tree sync in `serve`
2. snapshot expansion in `mount`

## 7. CLI Shape Changes

Current CLI is operational but redundant. Proposed evolution:

### Phase 1

Keep existing flags for compatibility:

1. `serve appfs --app-id/--app ... --adapter-http-endpoint/--adapter-grpc-endpoint`
2. `mount --appfs-app-id/--appfs-app ... --adapter-http-endpoint/--adapter-grpc-endpoint`

Add:

1. `serve appfs --managed`
2. `mount --managed-appfs`

Behavior:

1. explicit flags bootstrap registry entries;
2. managed mode consumes registry.

### Phase 2

Prefer:

1. `serve appfs --register-app ...`
2. `serve appfs --managed`
3. `mount --managed-appfs`

The duplicated app/backend flags can later be demoted to bootstrap-only or debug-only use.

## 8. Issue Breakdown

### B1. HTTP bridge V3 structure parity

Deliver:

1. HTTP bridge protocol for `get_app_structure`
2. HTTP bridge protocol for `refresh_app_structure`
3. Rust `HttpBridgeConnectorV3`
4. tests for envelope, validation, malformed payload, and revision handling

Acceptance:

1. `serve appfs` with `--adapter-http-endpoint` can bootstrap structure
2. `/_app/enter_scope.act` works over HTTP
3. `/_app/refresh_structure.act` works over HTTP

### B2. gRPC bridge V3 structure parity

Deliver:

1. proto additions for V3 structure APIs
2. Python reference server support
3. Rust `GrpcBridgeConnectorV3`
4. tests for enum handling, malformed payload rejection, and revision handling

Acceptance:

1. `serve appfs` with `--adapter-grpc-endpoint` can bootstrap structure
2. `/_app/enter_scope.act` works over gRPC
3. `/_app/refresh_structure.act` works over gRPC

### B3. Shared app registry

Deliver:

1. runtime-owned persisted registry format
2. atomic registry read/write helpers
3. mount-side registry consumer
4. bootstrap-vs-managed CLI rules

Acceptance:

1. one persisted registry becomes the source of truth for app/backend/session config
2. `mount --managed-appfs` no longer needs duplicated per-app endpoint flags
3. registry corruption and partial writes have deterministic failure handling

### B4. Runtime dynamic app lifecycle

Deliver:

1. `/_appfs/register_app.act`
2. `/_appfs/unregister_app.act`
3. `/_appfs/list_apps.act`
4. supervisor add/remove runtime paths
5. event evidence and regression coverage

Acceptance:

1. a new app can be registered into a running `serve appfs`
2. structure bootstrap occurs immediately
3. mount can serve snapshot read-through for the newly registered app in managed mode
4. unregister cleanly stops runtime ownership without deleting unrelated state

## 9. Risks

1. registry update races between runtime instances
2. mount-side cache staleness after registry changes
3. partial app registration leaving orphaned trees
4. connector transport mismatch between stored registry and active bridge state

Mitigations:

1. single-writer policy for registry mutations
2. atomic write + generation counter
3. explicit registration state machine: `pending`, `active`, `failed`
4. startup validation and clear runtime diagnostics

## 10. Recommendation

Implement in this order:

1. B1 HTTP V3 parity
2. B2 gRPC V3 parity
3. B3 shared app registry
4. B4 runtime dynamic app lifecycle

Do not start with more CLI flag layering. That would increase complexity without removing the underlying split-brain. The split only goes away once transport parity exists and both `mount` and `serve` can rely on the same persisted registry.
