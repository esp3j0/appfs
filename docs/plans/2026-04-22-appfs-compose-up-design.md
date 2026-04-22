# AppFS Compose Up Design

**Date:** 2026-04-22  
**Status:** Frozen (CP1, 2026-04-22)  
**Scope:** Declarative startup/orchestration layer above managed AppFS runtime for connector lifecycle, initial app registration, and multi-app bootstrap

## 1. Goal

Add one high-level, declarative startup path so AppFS can be integrated into real software without requiring:

1. manual connector startup in a separate terminal;
2. manual `/_appfs/register_app.act` writes after every boot;
3. duplicated transport flags across shell history, env vars, and ad hoc JSON;
4. one-off glue code in each host application such as `chat-server`.

After this change, the intended happy path is:

```text
agentfs appfs compose up -f appfs-compose.yaml
```

The compose file declares:

1. how to prepare the AgentFS/AppFS runtime;
2. how each connector should be reached or launched;
3. which apps should be registered at startup.

## 2. Why `appfs up` Should Not Absorb This

`agentfs appfs up` already solves one problem well: mount and runtime orchestration for managed AppFS.

It should remain the low-level managed runtime entrypoint, not grow into a giant flag surface for:

1. connector process supervision;
2. external endpoint fallback rules;
3. multi-app desired state;
4. app auto-registration;
5. project-local startup config for host applications.

Trying to solve those with more CLI flags will recreate the same split-brain problem we just removed from `mount + serve appfs`.

Conclusion:

1. `appfs up` stays as the runtime primitive;
2. `appfs compose up` becomes the high-level integration UX.

## 3. Main Decisions

### 3.1 Add a compose-specific orchestration command

The new command is:

```text
agentfs appfs compose up -f appfs-compose.yaml
```

Recommended file names:

1. `appfs-compose.yaml`
2. `appfs-compose.yml`

`-f` is optional when one of those files exists in the current working directory.

V1 scope only includes:

1. `up`

Explicitly out of scope for the first slice:

1. `down`
2. `status`
3. `logs`
4. background daemon mode

### 3.2 Compose is desired configuration, not runtime state

The compose file is **not** a replacement for:

`/_appfs/apps.registry.json`

Instead:

1. compose file = declarative startup intent owned by the operator/project;
2. runtime registry = managed AppFS runtime state stored inside the mounted filesystem.

Compose may seed or replace the initial managed registry before startup, but the runtime registry remains the engine's persisted state model.

### 3.3 Compose owns initial app set at startup

The current managed runtime loads apps from the persisted registry during startup.

That means a compose command cannot simply "start runtime, then maybe register apps later", because undeclared stale registry entries would also boot.

So the frozen compose startup rule is:

1. load and validate compose file;
2. resolve connector endpoints or launch connector commands;
3. synthesize the desired managed registry from compose apps;
4. persist that registry before AppFS runtime starts;
5. start managed AppFS runtime using that registry as the initial app set.

This makes startup deterministic and removes the need for a post-boot manual registration step in the happy path.

### 3.4 Compose manages connectors, not app semantics

Compose can manage connector startup and transport config, but it should not standardize app-specific business semantics such as:

1. Huoyan attached-case bootstrap rules;
2. AIIM demo-specific scope names;
3. app-specific auth or cookie acquisition flows.

Those remain connector-owned concerns, expressed through:

1. connector command args;
2. connector environment variables;
3. connector endpoints and health checks.

Example: Huoyan attached-case bootstrap should stay as connector env such as `APPFS_HUOYAN_BOOTSTRAP_MODE=attached_case`, not become a generic compose-level field.

### 3.5 Compose should support both external and managed connectors

Real integrations need three modes:

1. use an already running connector at a fixed HTTP/gRPC endpoint;
2. launch a connector command locally and supervise it;
3. try an external endpoint first, and if it is unavailable, fall back to launching the connector locally.

So connector mode is frozen as:

1. `external`
2. `command`
3. `external_or_command`

### 3.6 `register_app.act` remains valid after startup

Compose removes the **manual startup requirement** for app registration, but it does not remove the AppFS control plane.

After `compose up` starts:

1. `/_appfs/register_app.act`
2. `/_appfs/unregister_app.act`
3. `/_appfs/list_apps.act`

remain valid for dynamic runtime changes.

Compose just becomes the standard way to define the initial app set.

## 4. Target UX

### 4.1 Minimal example

```yaml
version: 1

runtime:
  db: .agentfs/huoyan-local.db
  mountpoint: C:\mnt\appfs-huoyan
  backend: winfsp
  init: if_missing
  reset: false

connectors:
  huoyan-http:
    mode: external_or_command
    transport: http
    endpoint: http://127.0.0.1:8080
    healthcheck:
      kind: connector
      interval_ms: 1000
      timeout_ms: 3000
      max_attempts: 20
    command:
      cwd: ./examples/appfs/bridges/http-python
      program: uv
      args: ["run", "python", "bridge_server.py"]
      env:
        APPFS_HTTP_BRIDGE_BACKEND: huoyan
        APPFS_HUOYAN_HOST: http://127.0.0.1:7777
        APPFS_HUOYAN_CASE_ID: "1"
        APPFS_HUOYAN_BOOTSTRAP_MODE: attached_case

apps:
  huoyan:
    connector: huoyan-http
    transport:
      http_timeout_ms: 5000
      grpc_timeout_ms: 5000
      bridge_max_retries: 2
      bridge_initial_backoff_ms: 100
      bridge_max_backoff_ms: 1000
      bridge_circuit_breaker_failures: 5
      bridge_circuit_breaker_cooldown_ms: 3000
```

Then the operator runs:

```text
agentfs appfs compose up -f appfs-compose.yaml
```

Result:

1. connector endpoint is resolved or launched;
2. AgentFS DB is created if needed;
3. managed AppFS runtime starts;
4. `huoyan` is already present in the initial registry;
5. the mounted tree is ready without a manual register step.

### 4.2 Chat-server integration shape

For `chat-server`, the intended pattern is:

1. keep one checked-in `appfs-compose.yaml` near the project;
2. let compose own connector env, connector launch command, DB path, and mountpoint;
3. let `chat-server` invoke one command instead of reproducing AppFS boot logic itself.

That reduces integration code and makes the runtime topology reviewable in one file.

## 5. Compose Schema

### 5.1 Top-level layout

```yaml
version: 1
name: optional-project-name
runtime: ...
connectors: ...
apps: ...
```

### 5.2 `runtime`

`runtime` declares how managed AppFS should start.

Fields:

1. `db`: AgentFS database path
2. `mountpoint`: mount path
3. `backend`: `fuse | nfs | winfsp`
4. `init`: `if_missing | always | never`
5. `reset`: boolean, default `false`
6. `auto_unmount`: boolean, default `true`
7. `allow_root`: boolean, optional
8. `system`: boolean, optional
9. `uid`: optional Linux/macOS override
10. `gid`: optional Linux/macOS override
11. `poll_ms`: optional runtime fallback poll interval

Frozen behavior:

1. relative paths are resolved relative to the compose file directory;
2. `reset: true` means remove or recreate the AgentFS DB before startup;
3. `init: if_missing` is the default.

### 5.3 `connectors`

`connectors` is a map keyed by connector name.

Each entry contains:

1. `mode`: `external | command | external_or_command`
2. `transport`: `http | grpc`
3. `endpoint`: required for V1 bridge transports, including `command`, because compose does not yet auto-discover connector endpoints
4. `healthcheck`: optional object
5. `command`: required for `command` and `external_or_command`

`command` fields:

1. `cwd`
2. `program`
3. `args`
4. `env`

`healthcheck` fields:

1. `kind`: for V1 only `connector`
2. `timeout_ms`
3. `interval_ms`
4. `max_attempts`

Frozen V1 rule:

1. health means "the connector transport can answer enough to be safely registered";
2. V1 does not define per-app semantic readiness beyond connector health.

### 5.4 `apps`

`apps` is a map keyed by `app_id`.

Each app entry contains:

1. `connector`: reference into `connectors`
2. `session_id`: optional
3. `transport`: optional transport overrides

`transport` fields mirror current AppFS registration payload:

1. `http_timeout_ms`
2. `grpc_timeout_ms`
3. `bridge_max_retries`
4. `bridge_initial_backoff_ms`
5. `bridge_max_backoff_ms`
6. `bridge_circuit_breaker_failures`
7. `bridge_circuit_breaker_cooldown_ms`

Compose does **not** duplicate app-specific structure config here.

If an app needs startup specialization, that belongs in the referenced connector definition.

## 6. Lifecycle

### 6.1 Startup sequence

`compose up` should execute in this order:

1. locate and parse compose YAML;
2. normalize paths relative to the compose file;
3. validate the runtime/app/connector graph;
4. initialize or reset the AgentFS database if configured;
5. prepare the connector supervisors;
6. for each connector:
   1. if `external`, wait for endpoint health;
   2. if `command`, launch child process and wait for health;
   3. if `external_or_command`, probe endpoint first and launch only on failure;
7. synthesize a managed registry document from `apps`;
8. persist that registry before runtime startup;
9. start the shared managed AppFS orchestrator;
10. keep running in the foreground and stream logs until shutdown.

### 6.2 Shutdown sequence

On Ctrl+C or fatal error:

1. stop AppFS runtime first;
2. unmount AppFS;
3. terminate connector child processes started by compose;
4. leave externally managed connectors alone.

### 6.3 Runtime mutations after startup

After compose startup, the operator may still:

1. register new apps dynamically;
2. unregister apps dynamically;
3. refresh structure;
4. enter scope;
5. trigger actions through ordinary AppFS paths.

Frozen rule:

1. the next `compose up` re-seeds the initial registry from the compose file again;
2. compose is authoritative for startup state, not for every runtime mutation after boot.

## 7. Internal Architecture

### 7.1 Reuse the managed AppFS orchestrator

`compose up` should not shell out to a second full CLI process as its long-term architecture.

Instead, extract a shared internal orchestration layer used by both:

1. `agentfs appfs up`
2. `agentfs appfs compose up`

Suggested boundary:

1. a reusable managed-runtime launcher that starts mount + runtime and owns shutdown order;
2. compose-specific preparation layered above it.

### 7.2 Add a compose domain model

Recommended modules:

1. `cli/src/cmd/appfs_compose.rs`
2. `cli/src/cmd/appfs_compose/schema.rs`
3. `cli/src/cmd/appfs_compose/loader.rs`
4. `cli/src/cmd/appfs_compose/connector_supervisor.rs`
5. `cli/src/cmd/appfs_compose/reconcile.rs`

Responsibilities:

1. schema/validation
2. path normalization
3. connector process management
4. compose-to-registry synthesis
5. orchestration entrypoint

### 7.3 Registry synthesis should use existing runtime config types

Do not invent a second registration model.

Compose should normalize into the same internal structures already used for:

1. managed registry documents
2. runtime transport config
3. connector builders

That keeps compose as a UX layer rather than a second runtime stack.

## 8. Non-Goals

V1 does not include:

1. background daemonization;
2. persistent supervisor state outside the AgentFS DB;
3. compose-driven hot reload when `appfs-compose.yaml` changes;
4. standardized app-specific bootstrap semantics;
5. full `docker compose` parity;
6. ownership-aware pruning of runtime-only apps after startup.

## 9. Why This Helps Real App Pilots

This directly improves real-app onboarding:

1. one checked-in file documents how the app is mounted, how the connector is reached, and which app IDs exist;
2. host applications such as `chat-server` can call one startup command instead of recreating AppFS boot logic;
3. env-heavy connectors such as Huoyan become reproducible and reviewable;
4. multi-app startup no longer depends on shell snippets that write JSON into `register_app.act`.

## 10. Implementation Order

### CP1. Freeze the design

1. add this design doc;
2. link it from `docs/v4/README.md`.

### CP2. Compose schema and loader

1. add YAML schema structs;
2. add file discovery and path normalization;
3. add validation and tests.

### CP3. Connector supervision

1. implement `external`, `command`, and `external_or_command`;
2. add connector health wait logic;
3. add process shutdown handling.

### CP4. Runtime bootstrap from compose

1. synthesize managed registry from compose apps;
2. persist it before runtime startup;
3. extract shared AppFS managed orchestrator used by both `appfs up` and `compose up`.

### CP5. CLI and docs

1. add `agentfs appfs compose up`;
2. add examples for Windows and Linux;
3. document the `chat-server` integration path.

## 11. Acceptance Criteria

This design is complete when:

1. `agentfs appfs compose up` can start AppFS from one YAML file;
2. compose can use both external endpoints and locally launched connectors;
3. declared apps appear at startup without manual `register_app.act`;
4. one compose file can bootstrap more than one app;
5. `appfs up` still works as a lower-level primitive for debugging.
