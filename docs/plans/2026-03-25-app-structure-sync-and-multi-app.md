# AppFS App Structure Sync And Multi-App Design

**Goal:** Let connectors publish and refresh app directory structure dynamically, so AppFS can start from an initial structure, update structure when the active page/scope changes, and eventually mount multiple apps together under one filesystem root.

**Recommendation:** Do **not** start with Windows-native overlay parity. First define and implement a platform-neutral `AppTreeSyncService` plus connector structure-sync contract. Windows-native overlay should remain a follow-up infrastructure track for generic host-base parity, not the first dependency for dynamic app structure.

**Current Constraint:** AppFS `v0.3` treats `/app/<app_id>/_meta/manifest.res.json` as the contract source of truth, but that manifest is still loaded from the filesystem tree itself. This works for static fixtures and demos, but it is too rigid for real apps whose visible directory structure depends on current page, active module, permissions, or server-driven UI state.

---

## 1. Problem Statement

We need to solve three coupled problems:

1. App directory structure must become connector-driven rather than fixture-only.
2. Structure must be refreshable at runtime when the app context changes.
3. One mount should eventually be able to expose multiple apps without duplicating runtime architecture.

Today, the runtime assumes:

1. An app directory already exists under the mounted tree.
2. `_meta/manifest.res.json` is already present and locally readable.
3. Snapshot and live behavior are derived from that local manifest.

That model is sufficient for demo apps, but not for real software where:

1. The initial app surface may need to be fetched from backend state.
2. Entering a page may introduce new folders/resources/actions.
3. Leaving a page may require removing or hiding page-scoped nodes.
4. Multiple apps may need to coexist in the same mounted namespace.

The core architectural question is therefore not "how do we mirror a host directory on Windows?" but "what is the authoritative model for an app tree, and how do we reconcile it into AgentFS safely?"

---

## 2. Key Decision

### 2.1 Source of truth

The connector becomes the source of truth for **connector-owned app structure**.

The mounted filesystem remains the source of truth for:

1. runtime-owned files such as `_stream`, paging state, snapshot expand journal, and materialized cache artifacts;
2. agent/user overlay writes inside mutable paths;
3. recovery metadata and local event history.

This means AppFS needs an explicit ownership model instead of assuming all visible files come from a static base tree.

### 2.2 Priority ordering

The correct implementation order is:

1. Freeze structure-sync contract and ownership rules.
2. Implement platform-neutral reconciliation into AgentFS.
3. Integrate single-app runtime refresh flows.
4. Add multi-app supervisor / namespace.
5. Later, add Windows-native host overlay parity as an infrastructure improvement.

Why this order:

1. Dynamic app structure is primarily a connector/runtime problem, not an overlay problem.
2. If we implement Windows-native overlay first, we still will not know how page-scoped nodes should appear, disappear, or reconcile.
3. Once structure sync is modeled explicitly, it can work on Linux, Windows, and macOS because the reconciler writes into AgentFS itself instead of depending on platform-specific host mirroring.

---

## 3. Target Architecture

### 3.1 New component: `AppTreeSyncService`

Introduce a runtime-owned, platform-neutral service:

`AppTreeSyncService`

Responsibilities:

1. call connector structure APIs;
2. validate returned structure payloads;
3. reconcile connector-owned nodes into AgentFS atomically;
4. maintain per-app structure revision and scope state;
5. preserve runtime-owned and user-owned data;
6. emit evidence/logs for CI and diagnostics.

Non-responsibilities:

1. it does not fetch snapshot records;
2. it does not paginate live data;
3. it does not submit actions;
4. it does not directly mount anything.

This keeps it parallel to existing runtime services:

1. `AppTreeSyncService` owns structure;
2. snapshot cache owns snapshot materialization;
3. paging owns live handle lifecycle;
4. action dispatcher owns submit/event flow.

### 3.2 Reconciler model

Connector structure sync should not directly write arbitrary files. Instead, it returns a declarative tree model which runtime reconciles into AgentFS.

Reconciler properties:

1. per-app single-flight;
2. atomic publish using journal + staging area;
3. revision-aware no-op when structure revision is unchanged;
4. scope-aware prune of connector-owned nodes only;
5. no deletion of runtime-owned internal files;
6. no overwrite of user-owned mutable outputs unless explicitly declared safe.

### 3.3 App runtime layering

Per app:

1. `AppRuntime`
2. `AppTreeSyncService`
3. `SnapshotCacheService`
4. `PagingService`
5. `ActionService`

For multi-app:

1. `AppRuntimeSupervisor`
2. one `AppRuntime` per `app_id`
3. shared mount root
4. shared bridge transport pool where safe

---

## 4. Connector Contract Extension

The existing `AppConnectorV2` is missing a structure surface. We should extend it rather than inventing a side protocol.

### 4.1 New methods

Recommended additions:

1. `get_app_structure(request, ctx) -> GetAppStructureResponseV3`
2. `refresh_app_structure(request, ctx) -> RefreshAppStructureResponseV3`

Rationale:

1. `get_app_structure` covers initialization and cold start.
2. `refresh_app_structure` covers context changes, page entry, permission changes, or explicit refresh.

We should keep both even if they share the same backend implementation, because their intent differs:

1. initialize a canonical starting tree;
2. re-sync after state transition.

### 4.2 Request shapes

Recommended request model:

```rust
pub enum AppStructureSyncReasonV3 {
    Initialize,
    EnterScope,
    Refresh,
    Recover,
}

pub struct GetAppStructureRequestV3 {
    pub app_id: String,
    pub known_revision: Option<String>,
}

pub struct RefreshAppStructureRequestV3 {
    pub app_id: String,
    pub known_revision: Option<String>,
    pub reason: AppStructureSyncReasonV3,
    pub target_scope: Option<String>,
    pub trigger_action_path: Option<String>,
}
```

Notes:

1. `target_scope` is the connector-defined page/module/workspace identifier.
2. `trigger_action_path` is optional diagnostic context so connector can tailor refresh after navigation-type actions.

### 4.3 Response shape

The response should be declarative and revisioned.

```rust
pub struct AppStructureSnapshotV3 {
    pub app_id: String,
    pub revision: String,
    pub active_scope: Option<String>,
    pub ownership_prefixes: Vec<String>,
    pub nodes: Vec<AppStructureNodeV3>,
}

pub enum AppStructureNodeKindV3 {
    Directory,
    ActionFile,
    SnapshotResource,
    LiveResource,
    StaticJsonResource,
}

pub struct AppStructureNodeV3 {
    pub path: String,
    pub kind: AppStructureNodeKindV3,
    pub manifest_entry: Option<serde_json::Value>,
    pub seed_content: Option<serde_json::Value>,
    pub mutable: bool,
    pub scope: Option<String>,
}
```

Meaning:

1. `nodes` declares the connector-owned visible tree.
2. `manifest_entry` is the normalized node contract needed by runtime.
3. `seed_content` is for small connector-owned bootstrap files only, not snapshot bodies.
4. `ownership_prefixes` defines the prune boundary for reconciliation.

### 4.4 Why not return raw files

Returning raw files would blur responsibilities and create transport-specific drift. A declarative structure model is better because:

1. runtime stays in control of path validation;
2. manifest and visible tree stay internally consistent;
3. reconciliation can be atomic and testable;
4. transport adapters remain protocol shims.

---

## 5. Ownership And Reconciliation Rules

This is the most important part of the design.

### 5.1 Ownership classes

Every path in `/app/<app_id>` belongs to exactly one class:

1. `connector-owned`
2. `runtime-owned`
3. `agent-owned`

Examples:

Connector-owned:

1. `_meta/manifest.res.json`
2. declared action sinks
3. declared snapshot resource placeholders
4. declared live resource placeholders
5. page/module directories created by connector structure sync

Runtime-owned:

1. `_stream/*`
2. `_paging/*`
3. snapshot materialized JSONL bodies
4. snapshot journal/temp artifacts
5. cursor/event/control state

Agent-owned:

1. user-created scratch files under app-specific writable areas
2. agent notes, caches, or app-local derived artifacts in explicitly allowed prefixes

### 5.2 Reconciliation policy

Reconciliation updates only connector-owned nodes.

Allowed:

1. add newly declared connector-owned nodes;
2. update connector-owned manifest metadata;
3. prune connector-owned nodes that are absent in the new structure snapshot;
4. preserve runtime-owned descendants under known internal prefixes.

Forbidden:

1. deleting runtime-owned files because a connector scope changed;
2. deleting agent-owned files silently;
3. replacing materialized snapshot data with connector seed content;
4. changing app ownership class for an existing path without explicit migration logic.

### 5.3 Atomicity

Structure sync must use journal + staging, similar to snapshot publish semantics:

1. fetch structure snapshot;
2. validate node graph;
3. compute diff;
4. stage updates under runtime temp area;
5. publish tree metadata atomically;
6. finalize revision marker.

On recovery:

1. incomplete structure sync is rolled back or replayed safely;
2. no partially published app tree should remain visible as success.

---

## 6. Runtime Trigger Model

### 6.1 Initial sync

At initialization or first app activation:

1. runtime creates `/app/<app_id>` root if absent;
2. runtime calls `get_app_structure`;
3. runtime reconciles connector-owned tree;
4. runtime starts normal action/live/snapshot surfaces.

This replaces the current assumption that app layout already exists in `--base`.

### 6.2 Page/scope refresh

When page context changes:

1. runtime calls `refresh_app_structure`;
2. connector returns a new snapshot with `revision` and `active_scope`;
3. reconciler updates only connector-owned scope-controlled nodes.

Recommended triggers:

1. explicit control action such as `/_app/enter_scope.act`;
2. selected successful actions marked as `refresh_structure_after_success`;
3. manual force refresh action such as `/_app/refresh_structure.act`;
4. optional periodic revalidate if connector declares dynamic structure TTL.

### 6.3 Why explicit page entry is better first

For the first version, page entry should be explicit instead of inferred from arbitrary reads because:

1. navigation semantics are app-specific;
2. action success does not always imply page transition;
3. explicit triggers are easier to test and reason about.

So the first shipping model should be:

1. explicit initialize;
2. explicit enter-scope;
3. explicit refresh;
4. optional action-driven refresh hints later.

---

## 7. Multi-App Mount Model

### 7.1 Namespace

The mounted root should expose multiple apps as sibling directories:

```text
/aiim
/notion
/slack
```

We should not add another `/app` layer inside the mounted root because current AppFS examples and runtime assumptions already treat the mounted root itself as the app namespace container.

### 7.2 Supervisor

Introduce `AppRuntimeSupervisor`:

1. accepts a configured set of apps;
2. owns one `AppRuntime` per `app_id`;
3. routes app-local actions, snapshot sync, and live paging by path prefix;
4. keeps per-app connector/session/revision state isolated.

### 7.3 CLI evolution

Recommended evolution:

Current:

1. `serve appfs --app-id aiim`
2. `mount --appfs-app-id aiim`

Future:

1. `serve appfs --app-id aiim` remains as single-app convenience mode
2. add `serve appfs --app aiim --app notion`
3. add `mount --appfs-app aiim --appfs-app notion`
4. optional config-file mode later for large app sets

### 7.4 Isolation

Per app isolation requirements:

1. separate structure revision and active scope;
2. separate connector session if needed;
3. separate runtime journals and caches;
4. separate evidence/log streams tagged by `app_id`.

---

## 8. Windows-Native Overlay Positioning

### 8.1 Why it is not first

Windows-native overlay parity is useful, but it is not the first dependency for this project goal because:

1. structure sync writes directly into AgentFS and therefore does not require a host base mirror;
2. dynamic structure refresh semantics must be solved before host overlay parity matters;
3. multi-app mount needs runtime supervision and reconciliation more than host passthrough.

### 8.2 Why it still matters

We should still do Windows-native overlay later because it improves:

1. generic `--base` parity with Linux/macOS;
2. local development ergonomics;
3. non-AppFS overlay use cases;
4. consistency of host-backed testing behavior.

So Windows-native overlay is a **follow-up infrastructure milestone**, not the entry point for dynamic structure sync.

---

## 9. Recommended Implementation Sequence

### Phase A: Freeze semantics

1. ADR for app-structure sync and multi-app namespace.
2. Freeze ownership model and reconciliation rules.
3. Freeze connector V3 structure-sync request/response types.

### Phase B: Single-app structure sync foundation

1. Add connector structure methods and types.
2. Implement `AppTreeSyncService`.
3. Reconcile connector-owned nodes into AgentFS.
4. Keep single-app runtime mode only.

### Phase C: Explicit page/scope refresh

1. Add control actions for enter-scope and refresh.
2. Add connector-driven scope revisions.
3. Add recovery/journal/evidence coverage.

### Phase D: Multi-app runtime

1. Add `AppRuntimeSupervisor`.
2. Add CLI for multiple apps.
3. Extend tests and CI to multi-app paths.

### Phase E: Windows-native host overlay parity

1. Add Windows-native `HostFS` equivalent.
2. Reuse generic overlay semantics where applicable.
3. Remove hydration workaround for generic overlay mode.

---

## 10. Issue Breakdown

### Issue A1: ADR Freeze For Structure Sync

Acceptance:

1. source-of-truth and ownership model documented;
2. explicit answer that Windows-native overlay is not phase 1;
3. multi-app namespace frozen.

### Issue A2: SDK Connector Structure Types

Acceptance:

1. add structure-sync request/response types;
2. extend `AppConnectorV2` or introduce `AppConnectorV3` cleanly;
3. bridge transports gain matching protocol types.

### Issue A3: `AppTreeSyncService` + Reconciler

Acceptance:

1. single-app initial sync works;
2. revision no-op works;
3. connector-owned prune works;
4. runtime-owned files are preserved.

### Issue A4: Explicit Scope Refresh Actions

Acceptance:

1. `enter_scope` and `refresh_structure` control surfaces work;
2. connector can return new scope tree;
3. page-scoped nodes update correctly.

### Issue A5: Single-App CI/Gate Coverage

Acceptance:

1. evidence proves structure calls happened;
2. refresh failure/recovery covered;
3. snapshot/live/action regressions remain green.

### Issue A6: Multi-App Supervisor

Acceptance:

1. multiple apps mounted under one root;
2. app-local routing is isolated;
3. per-app revisions/journals do not collide.

### Issue A7: Windows-Native Overlay Parity

Acceptance:

1. generic host-base overlay no longer requires hydration workaround;
2. semantics align with Linux/macOS overlay paths;
3. no regression to AppTreeSyncService.

---

## 11. Risks

1. **Connector returns unstable structure revisions**
   - Mitigation: require deterministic revision and no-op semantics.
2. **Refresh deletes runtime-generated files**
   - Mitigation: explicit ownership classes and protected prefixes.
3. **Multi-app mode leaks session or cursor state across apps**
   - Mitigation: supervisor-owned per-app isolation.
4. **Protocol grows too large too early**
   - Mitigation: first ship initialize + explicit scope refresh only.

---

## 12. Recommendation

Start with:

1. `ADR + connector structure contract`
2. `AppTreeSyncService`
3. `single-app explicit scope refresh`

Do **not** start with Windows-native overlay parity.

That work is still worth doing, but it should follow structure-sync foundation, because the real blocker for your goal is missing app-tree semantics, not missing Windows host overlay symmetry.
