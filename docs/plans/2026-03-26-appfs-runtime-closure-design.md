# AppFS Runtime Closure Design

**Date:** 2026-03-26  
**Status:** Frozen (C1, 2026-03-26)  
**Scope:** AppFS runtime architecture cleanup after `v0.3` connectorization and `v0.4` managed lifecycle / structure sync

## 1. Goal

Do one deliberate architecture cleanup pass so AppFS stops feeling like "old static fixture path plus new managed path layered on top". After this cleanup, the runtime model should be:

1. connector-driven app structure is the primary model;
2. managed registry is the primary operational model;
3. startup flow is one obvious happy path;
4. low-level debug commands still exist, but no longer define the default product shape.

This is intentionally allowed to be breaking. We do not optimize for backward compatibility in this phase.

## 2. Current Problems

The codebase now works, but it still carries three generations of operational assumptions at once:

1. legacy static fixture/bootstrap assumptions (`--base`, pre-existing app tree, local manifest as bootstrap truth);
2. `v0.3` split runtime assumptions (`mount` owns read-through, `serve appfs` owns action/event/control);
3. `v0.4` managed lifecycle assumptions (registry-driven apps, dynamic structure sync, runtime app registration).

That creates concrete complexity:

1. AppFS still looks bootstrap-first in CLI shape even though structure is now connector-driven.
2. `mount` and `serve appfs` remain conceptually separate engine pieces, but the user experience still feels like "start two daemons and keep flags in sync".
3. `--base` remains prominent in docs and flow design even though the managed path can now create app structure without it.
4. code ownership is smeared across `mount.rs`, `appfs.rs`, `tree_sync.rs`, `supervisor_control.rs`, and mount-side runtime glue without a clearly named top-level runtime package.

## 3. Main Decisions

### 3.1 `--base` should stop being part of the AppFS happy path

Recommendation:

1. remove `--base` from the **AppFS runtime story**;
2. keep `--base` as an **AgentFS core overlay capability**.

This distinction matters.

We should **not** remove `agentfs init --base` from AgentFS globally, because:

1. it is a generic filesystem feature, not just an AppFS bootstrap hack;
2. it remains useful for non-AppFS overlay workflows and low-level tests;
3. removing it would mix AppFS product cleanup with unrelated AgentFS storage semantics.

But we **should** remove it from AppFS default startup, examples, and design assumptions because:

1. `AppConnectorV3 + AppTreeSyncService` can now bootstrap connector-owned structure;
2. real app onboarding should not depend on pre-baked fixture trees;
3. managed runtime already has enough control surface to create and refresh app roots dynamically.

Conclusion:

1. `--base` stays in AgentFS;
2. AppFS docs, examples, and default flows stop depending on it;
3. fixture-based bootstrap becomes test/demo-only, not the primary path.

### 3.2 Do not make `mount` secretly become `serve`

Recommendation:

1. keep `mount` and runtime as separate internal subsystems;
2. add a **new orchestration command** as the primary UX entrypoint.

Do **not** overload `agentfs mount` to silently spawn `serve appfs`.

Why:

1. `mount` is fundamentally a filesystem primitive;
2. `serve appfs` is an event loop plus connector runtime plus app supervisor;
3. silently turning `mount` into a multi-process orchestrator will make debugging, lifecycle control, and error reporting harder.

Instead, introduce a first-class high-level command, for example:

```text
agentfs appfs up <id-or-path> <mountpoint>
```

Responsibilities of `appfs up`:

1. open the AgentFS database;
2. start mount backend;
3. start the AppFS runtime supervisor;
4. manage shutdown order;
5. surface logs in one place;
6. default to managed registry mode.

Low-level commands remain:

1. `agentfs mount ...` for filesystem-only debugging;
2. `agentfs serve appfs ...` for runtime-only debugging.

But the product default becomes `agentfs appfs up`.

### 3.3 Managed registry should become mandatory for AppFS default mode

Recommendation:

1. managed registry becomes the only default AppFS routing/config source;
2. explicit `--appfs-app-id`, `--appfs-app`, `--app-id`, `--app`, transport endpoint bootstrap flags become debug/bootstrap-only;
3. later, those explicit flags can be hidden or removed.

Why:

1. we already have `/_appfs/apps.registry.json`;
2. runtime dynamic registration already writes it;
3. mount-side read-through already consumes it in managed mode;
4. continuing to support equal-status duplicated CLI config guarantees more split-brain complexity.

Target steady-state:

1. startup does not ask for app IDs;
2. startup does not ask for connector endpoint flags;
3. app registration happens through control plane or a small helper CLI that writes the same registry/control actions.

### 3.4 Runtime-facing connector surface must be unified

Recommendation:

1. `AppConnectorV2` and `AppConnectorV3` stop being the runtime's primary abstraction;
2. Rust SDK / runtime adopt a single canonical trait: `AppConnector`;
3. HTTP / gRPC bridge continue speaking current `v2` and `v3` wire contracts internally, but adapters map them into the unified trait.

Why:

1. `V3` is effectively `V2 + structure sync`, not a truly separate runtime concept;
2. dual builder flows (`business connector` + `structure connector`) force version thinking into runtime control flow;
3. future lifecycle and registry work will become harder if runtime keeps carrying two connector objects.

Conclusion:

1. runtime and mount-side read-through each hold one `AppConnector`;
2. structure sync methods are optional-capability methods on the unified trait with `NOT_SUPPORTED` defaults;
3. transport versioning stays at the adapter boundary.

## 4. Target Runtime Shape

The cleaned-up runtime should have three clear layers.

### 4.1 Layer A: Core AgentFS

This layer stays generic:

1. SQLite-backed filesystem
2. overlay support
3. mount backends (FUSE / NFS / WinFsp)
4. non-AppFS filesystem commands

No AppFS-specific lifecycle or app routing assumptions live here.

### 4.2 Layer B: AppFS Engine

This becomes a named package boundary, conceptually:

1. runtime supervisor
2. app registry manager
3. tree sync service
4. snapshot read-through service
5. action/event/control services
6. connector transport adapters

This layer owns all AppFS semantics.

### 4.3 Layer C: AppFS Orchestration UX

This layer owns user-facing workflow:

1. `agentfs appfs up`
2. future `agentfs appfs down/status`
3. optional helper commands can come later, but current lifecycle control remains `/_appfs/*`

This layer should call into Layer B, not re-implement any business logic.

## 5. Code Cleanup Recommendations

### 5.1 Split `appfs.rs`

`cli/src/cmd/appfs.rs` is currently carrying too much mixed responsibility.

It should be split into clearer units:

1. `runtime_supervisor.rs`
   - current `AppfsRuntimeSupervisor`
   - runtime lifecycle add/remove/list
   - high-level poll loop coordination
2. `runtime_config.rs`
   - CLI/runtime arg normalization
   - session/app normalization
   - managed-vs-explicit config resolution
3. `runtime_entry.rs`
   - per-app runtime entry construction
   - active scope reads
   - transport summary helpers

The current `appfs.rs` can become a small façade that wires the modules together.

### 5.2 Separate registry ownership from supervisor control

Today the registry logic is partly in:

1. `registry.rs`
2. `supervisor_control.rs`
3. `appfs.rs`

That should be tightened:

1. `registry.rs` owns the document schema and atomic persistence only;
2. a new `registry_manager.rs` owns mutation workflows and state transitions;
3. `supervisor_control.rs` only parses/dispatches control actions.

This will make runtime lifecycle errors much easier to reason about.

### 5.3 Move mount-side AppFS into a dedicated runtime-facing boundary

`mount_runtime.rs` should be the named home for mount-side AppFS behavior instead of an ad hoc read-through helper.

Recommended cleanup:

1. keep the logic in AppFS domain space;
2. rename or reorganize so it is obviously "mount-side AppFS runtime service", not an ad hoc utility;
3. isolate platform-specific open/delete quirks behind narrower helpers.

Suggested shape:

1. `cmd/appfs/mount_runtime.rs`
2. optional narrower platform helpers when Windows quirks justify them

This matters because Windows-specific WinFsp behavior has already started leaking into structure-sync and read-through debugging.

### 5.4 Make manifest generation explicitly derived

We should stop thinking of `_meta/manifest.res.json` as primary storage and instead treat it as a generated AppFS view.

Recommendation:

1. connector-owned node structure becomes the real source;
2. runtime generates `_meta/manifest.res.json` deterministically from structure sync snapshot;
3. mount/read-through consumes generated manifest view, not fixture bootstrap assumptions.

This will reduce a lot of conceptual confusion around "is the manifest primary, or is the connector structure primary?"

### 5.5 Collapse V2/V3 usage into canonical `AppConnector`

Runtime code should not keep branching on `AppConnectorV2` and `AppConnectorV3`.

Frozen direction:

1. Rust SDK exports canonical `AppConnector` and unversioned connector types;
2. runtime builder becomes `build_app_connector(...)`;
3. adapters remain free to bridge current wire contracts behind that interface.

## 6. Startup Flow Optimization

### 6.1 Recommended target flow

Target happy path:

```text
agentfs init my-runtime
agentfs appfs up .agentfs/my-runtime.db C:\mnt\appfs
```

After startup:

1. root-level `/_appfs` control plane is available immediately;
2. app registration happens through `register_app.act` or helper CLI;
3. runtime bootstraps structure;
4. mount picks up registry automatically;
5. snapshot read-through works without extra app/backend flags.

### 6.2 Optional helper CLI

If we want to make first app registration less shell-heavy, add:

```text
agentfs appfs register --root <mountpoint> --app-id aiim --transport http --endpoint http://127.0.0.1:8080
```

This should not create a second code path. It should simply write the same control action or call the same internal supervisor handler.

### 6.3 Commands to demote

These should become advanced/debug-only over time:

1. `mount --appfs-app-id ...`
2. `mount --appfs-app ...`
3. `mount --managed-appfs` as an exposed primary concept
4. `serve appfs --app-id ...`
5. `serve appfs --managed` as an exposed primary concept

The product should expose orchestration, not internal plumbing.

## 7. Recommended Breaking Changes

If we are willing to do a real cleanup, I recommend these explicit breaks:

1. AppFS docs/examples stop using `--base`.
2. AppFS primary startup switches to `agentfs appfs up`.
3. Managed registry becomes mandatory for the AppFS happy path.
4. Explicit app/backend bootstrap flags are marked internal/debug-only.
5. `/_appfs` becomes the formal root control plane for runtime lifecycle.

I do **not** recommend these breaks yet:

1. deleting `agentfs init --base` from AgentFS globally;
2. folding mount and runtime into one inseparable codepath internally;
3. removing low-level `mount` / `serve appfs` commands entirely.

## 8. Execution Order

### C1. Freeze the closure architecture

Write ADR for:

1. AppFS defaults to managed mode
2. `--base` removed from AppFS happy path
3. new `agentfs appfs up` orchestration command
4. canonical `AppConnector` replaces runtime dual-connector usage
5. low-level commands demoted to debug surfaces

### C2. Refactor code boundaries before new UX

Do the internal split first:

1. canonical `AppConnector` adoption
2. `appfs.rs` breakup
3. registry manager extraction
4. mount runtime cleanup
5. manifest-generation ownership cleanup

This reduces the risk that a new orchestration command gets built on top of messy boundaries.

### C3. Add orchestration command

Add:

1. `agentfs appfs up`
2. maybe `agentfs appfs down`
3. maybe `agentfs appfs register`

### C4. Rewrite docs/examples around managed-first flow

Update:

1. README
2. Windows/Linux quick start
3. validation scripts
4. demo instructions

### C5. Remove or hide explicit AppFS bootstrap flags

Once orchestration is stable:

1. hide low-level AppFS flags from primary docs
2. optionally remove them in a follow-up breaking cleanup

## 9. Recommendation

My recommendation is:

1. **Do not globally remove `--base`**; remove it from AppFS’s default workflow only.
2. **Do not make `mount` secretly auto-run runtime**; add a new explicit orchestration command instead.
3. **Make managed registry the default AppFS architecture** and demote explicit app/backend CLI bootstrap.
4. **Refactor `appfs.rs` / registry / mount-readthrough boundaries before adding more user-facing commands**.

If we do those four things, the codebase becomes much easier to reason about, and the startup flow goes from:

1. init
2. bridge
3. mount
4. serve
5. register

to:

1. init
2. appfs up
3. register

That is the cleanest end-state for the architecture we already built.
