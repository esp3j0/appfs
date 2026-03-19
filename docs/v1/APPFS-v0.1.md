# AppFS v0.1 (Revised Draft r9)

- Version: `0.1-draft-r9`
- Date: `2026-03-18`
- Status: `Draft`
- Base Runtime: `AgentFS`
- Conformance: `APPFS-conformance-v0.1.md`

## 1. Overview

AppFS defines a filesystem-native app contract for agents that operate through shell commands.

This revision sets a clear priority:

1. Simple mode is Core: append JSONL requests directly to `.act` sinks.
2. Stream-first async is Core: consume result from app event stream.
3. Runtime-generated request IDs are Core: client does not need to generate UUID/ULID.
4. Action execution mode is metadata-driven: `inline` and `streaming` share the same `.act` path model.
5. Long content paging is handle-based: `cat` returns first page and `fetch_next` reads next page.

Target workflow:

```bash
# subscribe stream first
tail -f /app/aiim/_stream/events.evt.jsonl

# then trigger action by append JSONL
printf '{"text":"hi"}\n' >> /app/aiim/contacts/zhangsan/send_message.act
```

## 2. Goals and Non-Goals

### 2.1 Goals

1. Minimal token overhead for LLM+bash agents.
2. One path model for many apps.
3. Self-describing contracts through `_meta`.
4. Event-driven completion without status polling files.

### 2.2 Non-Goals (v0.1)

1. Replacing native app APIs.
2. Full cross-app transaction protocol.
3. Forcing strict idempotency rules at AppFS layer.

Note: idempotency/deduplication policy is app-defined in v0.1.

## 3. Global Namespace

```text
/app/
  .well-known/
    apps.res.json
  <app_id>/
```

`/app/.well-known/apps.res.json` MUST list mounted apps.

Optional scope convention (recommended):

1. Apps that mix account/user/agent/session data SHOULD expose clear subtrees under `/app/<app_id>/` (for example `resources/`, `user/`, `agent/`, `session/`) to keep path intent deterministic for agents.
2. Scope naming is a contract concern (manifest-declared), not an implementation detail.

## 4. Per-App Required Layout

```text
/app/<app_id>/
  _meta/
  _stream/
  _paging/
  _snapshot/
  <domain>/...
```

Required files:

```text
/app/<app_id>/_meta/manifest.res.json
/app/<app_id>/_meta/context.res.json
/app/<app_id>/_meta/permissions.res.json
/app/<app_id>/_meta/schemas/...
/app/<app_id>/_stream/events.evt.jsonl
/app/<app_id>/_stream/cursor.res.json
/app/<app_id>/_stream/from-seq/<seq>.evt.jsonl
/app/<app_id>/_paging/fetch_next.act          # required when app has live pageable resources
/app/<app_id>/_paging/close.act               # required when app has live pageable resources
/app/<app_id>/_snapshot/refresh.act           # optional but recommended for snapshot refresh/materialization checks
```

## 5. Path and Naming Rules

Each path segment that represents IDs (`app_id`, `contact_id`, resource/action IDs) MUST use `[A-Za-z0-9._-]`.

ID segments MUST NOT use Windows reserved names (`CON`, `PRN`, `AUX`, `NUL`, `COM1` ... `COM9`, `LPT1` ... `LPT9`).

If a natural id contains unsupported characters, it MUST be encoded as:

```text
~b64u~<base64url_no_padding>
```

Apps MAY expose friendly aliases (example: `zhangsan`) in addition to encoded ids.

Runtime safety guards (Core):

1. Runtime MUST normalize path separators to `/` before routing.
2. Runtime MUST reject unsafe segments before any adapter side effect:
   - `.` or `..`
   - drive-letter prefixes like `C:`
   - backslash-containing segments
   - NUL bytes or empty reserved segments
3. Unsafe path rejection SHOULD map to `EINVAL` or `EACCES` and MUST happen before backend/app calls.

Segment length and portability:

1. Each segment MUST be <= 255 bytes in UTF-8.
2. If a runtime/adapter derives a segment from natural text and it exceeds limit, it MUST shorten deterministically and append a short hash suffix (for example `_` + first 8 hex chars of SHA-256).
3. The shortening rule MUST preserve cross-platform safety (including Windows filename rules above).

## 6. Node Types (Colocated Model)

Within the same domain tree:

1. `*.res.json` => live/pageable resource (read-oriented, page envelope)
2. `*.res.jsonl` => snapshot full-file resource (read-oriented, one JSON item per line)
3. `*.act` => action sink (write-oriented)
4. `*.cfg.json` => config document (read/write)
5. `*.evt.jsonl` => event stream

Example:

```text
/app/aiim/contacts/zhangsan/profile.res.json
/app/aiim/chats/chat-001/messages.res.jsonl
/app/aiim/contacts/zhangsan/send_message.act
```

## 7. Action Sink Semantics (`*.act`)

`*.act` is a command sink, not a normal persistent file.

Core behavior:

1. Client appends one JSON line per request (`append + '\n'`).
2. Runtime treats each complete JSONL line as one action submission.
3. Runtime immediately generates `request_id` (server-side).
4. Runtime/app emits result lifecycle events to `_stream/events.evt.jsonl`.

Execution mode:

Each action MUST declare `execution_mode` in `manifest.res.json`:

1. `inline`: runtime SHOULD try to complete within `inline_timeout_ms`.
2. `streaming`: runtime accepts quickly and reports progress/result via stream events.

Submission timing rules:

1. If submit-time basic checks fail (malformed payload, size limit), runtime MUST reject and MUST NOT emit `action.accepted`.
2. For `streaming` actions, if close succeeds, runtime MUST emit `action.accepted` within a bounded time (recommended <= 1s).
3. For `inline` actions:
   - runtime MAY finish inside close and return success/failure synchronously.
   - runtime SHOULD still emit a terminal event for audit (`action.completed` or `action.failed`).
   - if not completed within `inline_timeout_ms`, runtime MAY degrade to async handling, MUST emit `action.accepted`, and then terminal event later.
4. For any accepted request, terminal event MUST be exactly one of `action.completed`, `action.failed`, or `action.canceled` (if cancellation is supported by app policy).
5. Runtime MUST treat newline-terminated JSONL records as commit boundaries. Interrupted/partial writes (missing trailing newline) MUST NOT create requests or side effects.

Input format:

1. `*.act` payload is JSON-only in v0.1 runtime profile.
2. Each submission MUST be one JSON object line (`JSONL`).
3. Preferred client format is single-line JSON with escaped newlines in strings (for example `\\n`).
4. Runtime MAY apply compatibility recovery for shell-expanded multiline JSON fragments by joining adjacent committed lines until one valid JSON object is formed within recovery limits.
5. Runtime recovery limits in the reference implementation are `MAX_RECOVERY_LINES=32` and `MAX_RECOVERY_BYTES=65536` (effective bytes limit is `min(MAX_RECOVERY_BYTES, action.max_payload_bytes)` when action-level limit exists).

Examples:

```bash
printf '{"text":"hi"}\n' >> /app/aiim/contacts/zhangsan/send_message.act
printf '{"text":"hi","priority":"high"}\n' >> /app/aiim/contacts/zhangsan/send_message.act
```

Operation matrix for `.act`:

1. `append` (`>>`, `O_APPEND`) with complete JSON lines: MUST submit in observed line order.
2. Partial trailing line (no newline): MUST stay pending and MUST NOT submit until line is complete.
3. Runtime MUST process committed lines with `at-least-once` delivery semantics.
4. `read` (`cat`): MUST fail with `EACCES` (command sink is write-only).
5. `>` overwrite/truncate on `.act`: treated as illegal mutation and MUST NOT submit overwritten bytes.
6. `rename`/`move` of `.act`: MUST fail with `EOPNOTSUPP`.
7. `delete` of `.act`: MUST fail with `EOPNOTSUPP`.

## 8. Stream-First Async Contract (Core)

Every app MUST expose:

```text
/app/<app_id>/_stream/events.evt.jsonl
```

Minimum event types:

1. For `inline`: `action.completed`, `action.failed`
2. For `streaming`: `action.accepted`, `action.completed`, `action.failed`

Recommended additional types:

1. `action.progress`
2. `action.output`
3. `resource.updated`
4. `action.awaiting_approval`
5. `action.canceled`

Event envelope (minimum fields):

```json
{
  "seq": 1201,
  "event_id": "evt-1201",
  "ts": "2026-03-15T10:00:00Z",
  "app": "aiim",
  "session_id": "sess-7f2c",
  "request_id": "req-9a84d2",
  "path": "/contacts/zhangsan/send_message.act",
  "type": "action.completed",
  "content": "send success"
}
```

Rules:

1. `seq` MUST be monotonically increasing per app stream.
2. Stream readers SHOULD block and wait for new lines (tail-friendly).
3. `request_id` MUST be generated by runtime/app and returned in stream events.

Delivery semantics and dedup:

1. AppFS stream delivery is `at-least-once`.
2. Runtime MUST provide stable `event_id` per emitted event.
3. Consumers MUST tolerate duplicate deliveries (especially when combining `tail` with `from-seq` replay).
4. Recommended consumer dedup key is `(app, session_id, event_id)` when `event_id` exists, otherwise `(app, session_id, seq)`.

Replay/resume (Core):

1. Runtime MUST expose `/app/<app_id>/_stream/cursor.res.json` with:
   - `min_seq`: oldest retained event sequence
   - `max_seq`: latest committed sequence
   - `retention_hint_sec`: retention hint
2. Runtime MUST expose `/app/<app_id>/_stream/from-seq/<seq>.evt.jsonl` and return events with `seq >= <seq>`.
3. If requested `<seq>` is older than `min_seq`, runtime MUST fail with `ERANGE`.
4. Agents SHOULD persist last seen `seq` and resume via `from-seq`.
5. For `streaming` actions with progress, app SHOULD emit `action.progress` at app-defined frequency.
6. For `streaming` actions, app SHOULD avoid silent periods longer than `progress_policy.max_silence_ms` when such policy is declared.

## 9. Correlation and Optional Client Token

Because request IDs are server-generated, clients MAY provide an optional token in payload:

```json
{"text":"hi","client_token":"msg-001"}
```

If present, app SHOULD echo `client_token` in events for easier correlation.

No UUID generation is required from the LLM client.

## 10. Polling Policy

Core spec does not require polling status files.

Primary completion signal is stream event:

1. success => `action.completed`
2. failure => `action.failed`

Optional materialized result files MAY exist, but are not required for v0.1 core.

## 11. Search and Pagination

Simple filtering SHOULD be represented as resource projections:

```text
/app/<app_id>/contacts/by-name/zhangsan/index.res.json
```

Complex search MAY use action sink near resource:

```text
/app/<app_id>/contacts/search.act
```

For long content resources, AppFS splits semantics by resource type:

1. `snapshot` resources are exposed as full files (`*.res.jsonl`) and MUST NOT require page handles.
2. `live` resources use the Page Handle protocol (`*.res.json` + `/_paging/*`).

### 11.1 Default First Page via `cat`

Snapshot default (`*.res.jsonl`):

1. `cat <resource>.res.jsonl` returns full file content.
2. Each line MUST be one JSON object item.
3. Snapshot resources MUST NOT include `{items,page}` envelopes.
4. Snapshot resources MUST NOT expose or require `handle_id`.

Live default (`*.res.json`):

When a live resource is declared pageable, `cat <resource>.res.json` MUST return page 0 using this envelope:

```json
{
  "items": [],
  "page": {
    "handle_id": "ph_live_7f2c",
    "page_no": 0,
    "has_more": true,
    "mode": "live",
    "expires_at": "2026-03-16T12:30:00Z"
  }
}
```

`mode` in page envelopes MUST be `live`.

### 11.2 Fetch Next Page

Next page is fetched by action sink:

```text
/app/<app_id>/_paging/fetch_next.act
```

Payload format:

1. JSON-only: `{ "handle_id": "..." }`.
2. Optional extension fields MAY be included (for example `max_items`).

Example:

```bash
printf '{"handle_id":"ph_live_7f2c"}\n' >> /app/aiim/_paging/fetch_next.act
```

Result delivery:

1. `fetch_next` result SHOULD be emitted in `action.completed` payload.
2. Payload SHOULD reuse the same `{items, page}` envelope.
3. `page.page_no` MUST increment by 1 when advancing.

### 11.3 Close Page Handle

Handle cleanup action:

```text
/app/<app_id>/_paging/close.act
```

Payload:

```json
{"handle_id":"ph_live_7f2c"}
```

If client does not close explicitly, runtime/app MAY expire handles by TTL.

### 11.4 Handle Rules

1. `handle_id` MUST be session-scoped.
2. Accessing a handle from another session MUST fail with permission-denied semantics (`EACCES` and/or `action.failed` with `PERMISSION_DENIED`).
3. Expired handles MUST fail with `action.failed` (`error.code = "PAGER_HANDLE_EXPIRED"`).
4. Invalid handle format MUST fail with `EINVAL`.
5. Unknown handle MUST fail with `action.failed` (`error.code = "PAGER_HANDLE_NOT_FOUND"`).
6. Already-closed handle MUST fail with `action.failed` (`error.code = "PAGER_HANDLE_CLOSED"`).

### 11.5 Paging Action Error Mapping

For both `/_paging/fetch_next.act` and `/_paging/close.act`:

1. Malformed `handle_id` format: submit-time filesystem error `EINVAL`, and MUST NOT emit `action.accepted`.
2. Unknown handle: terminal `action.failed` with `error.code = "PAGER_HANDLE_NOT_FOUND"`.
3. Expired handle: terminal `action.failed` with `error.code = "PAGER_HANDLE_EXPIRED"`.
4. Already-closed handle: terminal `action.failed` with `error.code = "PAGER_HANDLE_CLOSED"`.
5. Cross-session handle access: terminal `action.failed` with `error.code = "PERMISSION_DENIED"` (app-specific detail MAY be appended).

### 11.6 Live Feed Behavior

For `mode = live`:

1. `has_more` MAY remain `true` indefinitely.
2. If currently no new content, app MAY return empty `items` with:

```json
{
  "items": [],
  "page": {
    "handle_id": "ph_live_1",
    "page_no": 42,
    "has_more": true,
    "mode": "live",
    "retry_after_ms": 1500
  }
}
```

## 12. Error Model

Two layers:

1. Filesystem error (`ENOENT`, `EACCES`, `EINVAL`, ...)
2. Structured event payload for action failures

Standard error code set (recommended):

1. `INVALID_ARGUMENT`
2. `INVALID_PATH`
3. `NOT_FOUND`
4. `ALREADY_EXISTS`
5. `PERMISSION_DENIED`
6. `RESOURCE_EXHAUSTED`
7. `FAILED_PRECONDITION`
8. `TIMEOUT`
9. `UNAVAILABLE`
10. `INTERNAL`
11. `UNIMPLEMENTED`

Apps MAY emit app-specific codes (for example `AIIM_CONTACT_NOT_FOUND`) and SHOULD keep a stable mapping to the standard set above.

Failure example:

```json
{
  "type": "action.failed",
  "request_id": "req-9a84d2",
  "error": {
    "code": "AIIM_CONTACT_NOT_FOUND",
    "message": "contact does not exist",
    "retryable": false
  }
}
```

## 13. Self-Describing Metadata

`/app/<app_id>/_meta/manifest.res.json` MUST declare:

1. available nodes
2. input/output/event schema refs
3. payload mode (`json`) for each `.act`
4. limits for each action (`max_payload_bytes`, `rate_limit_hint`)
5. execution mode for each action (`execution_mode`) and mode-specific hints (`inline_timeout_ms` or `progress_policy`)
6. paging metadata for live pageable resources (`paging.enabled`, page sizes, mode=`live`, handle TTL)
7. snapshot limits for snapshot full-file resources (`snapshot.max_materialized_bytes`)

Example:

```json
{
  "app_id": "aiim",
  "contract_version": "0.1",
  "conformance": {
    "appfs_version": "0.1",
    "profiles": ["core"],
    "recommended": ["observer", "progress_policy"],
    "extensions": [],
    "implementation": {
      "name": "aiim-adapter",
      "version": "0.1.0",
      "language": "rust"
    }
  },
  "nodes": {
    "chats/{chat_id}/messages.res.jsonl": {
      "kind": "resource",
      "output_mode": "jsonl",
      "output_schema": "_meta/schemas/chat.message.item.output.schema.json",
      "snapshot": {
        "max_materialized_bytes": 10485760
      }
    },
    "feed/recommendations.res.json": {
      "kind": "resource",
      "output_mode": "json",
      "output_schema": "_meta/schemas/feed.recommendations.page.schema.json",
      "paging": {
        "enabled": true,
        "mode": "live",
        "default_page_size": 20,
        "max_page_size": 50,
        "handle_ttl_sec": 600
      }
    },
    "contacts/{contact_id}/profile.res.json": {
      "kind": "resource",
      "output_schema": "_meta/schemas/contact.profile.output.schema.json"
    },
    "contacts/{contact_id}/send_message.act": {
      "kind": "action",
      "input_mode": "json",
      "execution_mode": "inline",
      "inline_timeout_ms": 2000,
      "input_schema": "_meta/schemas/send_message.input.schema.json",
      "event_schema": "_meta/schemas/events.evt.schema.json",
      "max_payload_bytes": 8192
    },
    "files/{file_id}/download.act": {
      "kind": "action",
      "input_mode": "json",
      "execution_mode": "streaming",
      "input_schema": "_meta/schemas/download.input.schema.json",
      "event_schema": "_meta/schemas/events.evt.schema.json",
      "progress_policy": {
        "mode": "app_defined",
        "max_silence_ms": 10000
      }
    },
    "_paging/fetch_next.act": {
      "kind": "action",
      "input_mode": "json",
      "execution_mode": "inline",
      "inline_timeout_ms": 500,
      "input_schema": "_meta/schemas/paging.fetch_next.input.schema.json",
      "event_schema": "_meta/schemas/events.evt.schema.json"
    },
    "_paging/close.act": {
      "kind": "action",
      "input_mode": "json",
      "execution_mode": "inline",
      "inline_timeout_ms": 500,
      "input_schema": "_meta/schemas/paging.close.input.schema.json",
      "event_schema": "_meta/schemas/events.evt.schema.json"
    },
    "_snapshot/refresh.act": {
      "kind": "action",
      "input_mode": "json",
      "execution_mode": "inline",
      "inline_timeout_ms": 1000,
      "input_schema": "_meta/schemas/snapshot.refresh.input.schema.json",
      "event_schema": "_meta/schemas/events.evt.schema.json"
    }
  }
}
```

## 14. How to Claim Compatibility

To claim **AppFS v0.1 Core compatibility**, an implementation SHOULD:

1. Publish `contract_version` and `conformance` in `manifest.res.json`.
2. Run contract test gates:
   - static: `CT-001`, `CT-003`, `CT-005`
   - live: `CT-002`, `CT-004`
3. Publish acceptance checklist status (pass/fail) against adapter requirements.
4. Avoid claiming `core` if any Core MUST requirement is currently failing.

Minimal conformance claim structure:

```json
{
  "conformance": {
    "appfs_version": "0.1",
    "profiles": ["core"],
    "recommended": ["observer"],
    "extensions": [],
    "implementation": {
      "name": "example-adapter",
      "version": "0.1.0",
      "language": "rust"
    }
  }
}
```

## 15. Security and Context

`/app/<app_id>/_meta/context.res.json` MUST declare principal/session context.

`/app/<app_id>/_meta/permissions.res.json` MUST declare granted and denied scopes.

If human approval is required:

1. app/runtime emits `action.awaiting_approval`
2. completion/failure event is emitted after decision

## 16. Runtime Observability (Recommended)

Runtime SHOULD expose per-app observer status:

```text
/app/<app_id>/_meta/observer.res.json
```

Recommended fields:

1. `accepted_total`
2. `completed_total`
3. `failed_total`
4. `stream_backlog`
5. `p95_accept_ms`
6. `p95_end_to_end_ms`
7. `last_error_ts`

Purpose:

1. Give agents and operators a low-token health surface.
2. Speed up diagnosis of adapter/runtime regressions.

## 17. Optional Advanced Mode (Non-Core)

For clients that need explicit request file lifecycles, apps MAY also expose:

```text
<action>.act/
```

with explicit request documents. This is optional and must not be required for basic LLM+bash usage.

## 18. AIIM Example

Directory:

```text
/app/aiim/
  _meta/manifest.res.json
  _stream/events.evt.jsonl
  _stream/cursor.res.json
  _paging/fetch_next.act
  _paging/close.act
  _snapshot/refresh.act
  chats/chat-001/messages.res.jsonl
  feed/recommendations.res.json
  contacts/zhangsan/profile.res.json
  contacts/zhangsan/send_message.act
  files/file-001/download.act
```

Usage:

```bash
# terminal 1
tail -f /app/aiim/_stream/events.evt.jsonl

# terminal 2
printf '{"text":"hi"}\n' >> /app/aiim/contacts/zhangsan/send_message.act

# terminal 3
printf '{"target":"C:/tmp/file-001.bin"}\n' >> /app/aiim/files/file-001/download.act

# terminal 4: snapshot is a full file
cat /app/aiim/chats/chat-001/messages.res.jsonl | rg "hi"

# terminal 5: live resource paging
cat /app/aiim/feed/recommendations.res.json
printf '{"handle_id":"ph_live_7f2c"}\n' >> /app/aiim/_paging/fetch_next.act

# terminal 6: optional snapshot refresh/materialization checkpoint
printf '{"resource_path":"/chats/chat-001/messages.res.jsonl"}\n' >> /app/aiim/_snapshot/refresh.act
```

Expected events:

```json
{"seq":1201,"ts":"2026-03-15T10:00:00Z","app":"aiim","session_id":"sess-7f2c","request_id":"req-9a84d2","path":"/contacts/zhangsan/send_message.act","type":"action.completed","content":"send success"}
{"seq":1202,"ts":"2026-03-15T10:00:01Z","app":"aiim","session_id":"sess-7f2c","request_id":"req-b7d120","path":"/files/file-001/download.act","type":"action.accepted","content":"download started"}
{"seq":1203,"ts":"2026-03-15T10:00:02Z","app":"aiim","session_id":"sess-7f2c","request_id":"req-b7d120","path":"/files/file-001/download.act","type":"action.progress","content":{"percent":25}}
{"seq":1204,"ts":"2026-03-15T10:00:06Z","app":"aiim","session_id":"sess-7f2c","request_id":"req-b7d120","path":"/files/file-001/download.act","type":"action.completed","content":{"saved_to":"C:/tmp/file-001.bin"}}
{"seq":1205,"ts":"2026-03-16T09:20:00Z","app":"aiim","session_id":"sess-7f2c","request_id":"req-c115aa","path":"/_paging/fetch_next.act","type":"action.completed","content":{"items":[{"id":"rec-3","text":"..."}],"page":{"handle_id":"ph_live_7f2c","page_no":1,"has_more":true,"mode":"live"}}}
```

## 19. Open Items for v0.2

1. Unified cancellation endpoint semantics for all apps.
2. Optional standard idempotency key behavior (if promoted from app-defined to spec-defined).
3. Backpressure and QoS classes for heavy event streams.
4. Promote observer contract from recommended to core.
