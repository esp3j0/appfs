# AppFS Adapter Layer Requirements v0.1

- Version: `0.1-draft`
- Date: `2026-03-16`
- Status: `Draft`
- Depends on: `APPFS-v0.1 (r7)`

## 1. Decision

Current AppFS v0.1 design is sufficient to start adapter implementation.

Reason:

1. Core interaction loop is closed: `.act` write -> stream events.
2. Action modes are defined: `inline` and `streaming`.
3. Discovery contract exists: `_meta/manifest.res.json` + schemas.
4. Replay baseline exists: `cursor` + `from-seq`.

Known non-blocking gaps remain for later versions (multi-tenant sharing, unified cancel spec, QoS classes).

## 2. Scope

This document defines requirements for the adapter layer only.

In scope:

1. Mapping AppFS nodes to real app operations.
2. Action execution and event emission.
3. Schema and capability publication.
4. Validation and error mapping.

Out of scope:

1. Mount backend implementation (FUSE/WinFsp/NFS).
2. Generic filesystem metadata internals.
3. Cross-app orchestration/transactions.

## 3. Roles and Boundaries

### 3.1 Runtime Responsibilities

1. Path routing and filesystem operation dispatch.
2. Session/principal context injection.
3. Request ID generation (server-side).
4. Stream storage and replay surface (`events`, `cursor`, `from-seq`).
5. Path normalization and unsafe-path precheck before calling adapters.

### 3.2 Adapter Responsibilities

1. Domain/resource/action registration.
2. Resource read realization.
3. Action payload validation and execution.
4. Event production according to AppFS schema.
5. App-specific permission checks and policy enforcement.

## 4. Functional Requirements

### AR-001 Manifest Publication

Adapter MUST provide data required to produce `_meta/manifest.res.json`:

1. Node list and kind (`resource`/`action`).
2. `input_mode`, `execution_mode`, schema references.
3. Action limits (`max_payload_bytes`, optional `rate_limit_hint`).

### AR-002 Resource Read

1. Adapter MUST resolve `*.res.json` nodes to UTF-8 JSON output.
2. Missing resource MUST map to `ENOENT`.
3. Unauthorized resource MUST map to `EACCES`.

### AR-003 Action Submit (`*.act`)

1. Runtime calls adapter on `write+close`.
2. Adapter MUST validate payload according to `input_mode` and declared schema.
3. Validation failure MUST return a deterministic error (`EINVAL`/`EMSGSIZE`) and MUST NOT emit `action.accepted`.
4. Accepted requests MUST produce stream events with runtime-provided `request_id`.

### AR-004 Execution Modes

#### AR-004A Inline Mode

1. Adapter SHOULD complete within `inline_timeout_ms`.
2. Adapter MAY return synchronous success/failure.
3. Adapter SHOULD emit terminal event (`action.completed` or `action.failed`) even when handled synchronously.
4. If timeout exceeded, adapter MAY degrade to async and emit `action.accepted`.

#### AR-004B Streaming Mode

1. Adapter MUST emit `action.accepted` quickly after submission.
2. Adapter SHOULD emit `action.progress` at app-defined cadence.
3. Adapter MUST emit exactly one terminal event (`action.completed` or `action.failed`, optionally `action.canceled`).

### AR-005 Event Contract

Each emitted event line MUST include:

1. `seq` (assigned by runtime stream layer)
2. `ts`
3. `app`
4. `session_id`
5. `request_id`
6. `path`
7. `type`

For `action.failed`, `error.code` and `error.message` MUST be present.

### AR-006 Correlation

1. Adapter MUST support server-generated `request_id`.
2. If payload contains `client_token` (or text-mode `token:` prefix), adapter SHOULD echo it in event payload for correlation.

### AR-007 Replay Support Cooperation

1. Adapter MUST emit events in causal order per request.
2. Adapter MUST tolerate replay readers reconnecting from older `seq` values exposed by runtime.

### AR-008 Search Support

1. Adapter SHOULD provide simple projection resources (`by-name/.../index.res.json`) where applicable.
2. Adapter MAY expose complex search action sinks (`search.act`) with cursorized outputs in events.

### AR-009 Error Mapping

Adapter MUST map app errors to:

1. Filesystem errno class.
2. Structured event error payload (`code`, `message`, optional `retryable`, `details`).

### AR-010 Path Safety Guard

1. Runtime+adapter chain MUST reject traversal-style or unsafe paths before side effects.
2. Rejected cases include at least: `.`/`..` segments, drive-letter injection (`C:`), backslash-separated traversal, and NUL bytes.
3. On unsafe input, adapter-facing business handlers MUST NOT run (no app/backend side effect).

### AR-011 Filename/ID Portability Guard

1. Adapter MUST enforce AppFS segment character policy and reserved-name policy.
2. For runtime-generated segments exceeding 255 UTF-8 bytes, adapter MUST apply deterministic shortening with hash suffix.
3. The same input MUST produce the same shortened output.

### AR-012 Stream Delivery Semantics

1. Event delivery contract is `at-least-once`.
2. Adapter MUST assume replay and duplicate-consumption scenarios are normal.
3. Adapter-emitted payload SHOULD include stable correlation hints (`request_id`, optional `client_token`, optional `event_id`).

### AR-013 Observer Publication

Adapter SHOULD expose or feed data for `/app/<app_id>/_meta/observer.res.json`:

1. action counters (`accepted_total`, `completed_total`, `failed_total`)
2. latency aggregates (`p95_accept_ms`, `p95_end_to_end_ms`)
3. stream pressure (`stream_backlog`)
4. last error timestamp (`last_error_ts`)

## 5. Non-Functional Requirements

### ANR-001 Latency

1. `inline` actions target: P95 <= 2s (app-dependent).
2. `streaming` acceptance target: P95 <= 1s to `action.accepted`.

### ANR-002 Reliability

1. After `action.accepted`, terminal event MUST eventually appear unless process crash occurs.
2. Adapter SHOULD be crash-safe by delegating durable stream persistence to runtime.
3. Recovery path MUST preserve event ordering per request.

### ANR-003 Observability

Adapter MUST expose structured logs including:

1. `request_id`
2. action path
3. execution mode
4. latency
5. result status
6. normalized error code (when failed)

## 6. Suggested Adapter Interface (Rust-Oriented)

This is a requirements-oriented shape, not strict API freeze.

```rust
pub trait AppAdapter {
    fn app_id(&self) -> &str;
    fn manifest(&self) -> ManifestDescriptor;

    fn read_resource(&self, path: &str, ctx: &RequestContext) -> Result<Vec<u8>, AdapterError>;

    fn submit_action(
        &self,
        path: &str,
        payload: &[u8],
        request_id: &str,
        ctx: &RequestContext,
        emitter: &dyn EventEmitter,
    ) -> Result<SubmitResult, AdapterError>;
}
```

Where:

1. `SubmitResult` indicates sync completion vs async accepted.
2. `EventEmitter` abstracts writing JSONL events to runtime stream.

## 7. Security Requirements

1. Adapter MUST consume principal/session context from runtime (from `_meta/context` model).
2. Adapter MUST enforce app-level scopes before side effects.
3. For approval-required actions, adapter MUST emit `action.awaiting_approval` and defer terminal result.

## 8. Validation and Acceptance Checklist

Adapter implementation is accepted when all checks pass:

1. Manifest completeness: node/action/schema fields present.
2. `.act` validation path: malformed payload returns sync error and no `action.accepted`.
3. Inline action path: sync result works; terminal event emitted.
4. Streaming action path: accepted -> progress(optional) -> terminal flow works.
5. Failed action path: `action.failed` contains structured `error`.
6. Correlation: `request_id` always present; `client_token` echoed when provided.
7. Replay compatibility: events can be consumed via `from-seq`.
8. Unsafe path guard: traversal/drive-injection/backslash payloads are rejected before side effects.
9. Segment portability: overlong generated names are deterministically shortened with hash and remain <= 255 bytes.
10. Delivery semantics: consumer-side duplicate handling is validated in integration tests.

## 9. Delivery Plan

### Phase 1 (Core Adapter Skeleton)

1. Manifest generation.
2. Resource read handlers.
3. Action submit pipeline with validation.

### Phase 2 (Mode Semantics)

1. Inline mode behavior and timeout fallback.
2. Streaming mode progress and terminal guarantees.

### Phase 3 (Hardening)

1. Error mapping consistency.
2. Permission checks and approval flow.
3. Contract tests and performance baselines.
