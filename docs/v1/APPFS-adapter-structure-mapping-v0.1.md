# AppFS Adapter Structure and Bridge Mapping Guide v0.1

- Version: `0.1`
- Date: `2026-03-18`
- Status: `Draft`
- Audience: Adapter authors integrating a real app backend

## 1. Why This Guide Exists

Two recurring questions are valid:

1. "How do I define my AppFS tree (home/settings/pages/resources)?"
2. "After scaffold generation, how do files map to bridge handlers?"

This guide makes that mapping explicit.

## 2. Source of Truth Model

For a real app adapter, use three artifacts with clear ownership:

1. `manifest.res.json` (contract source of truth)
2. AppFS file tree (runtime-visible surfaces)
3. Bridge route map (implementation mapping)

Order matters:

1. Design node templates in manifest.
2. Materialize required files/sinks in the filesystem tree.
3. Implement bridge handlers for each declared action/control route.

## 3. How to Define AppFS Structure

## 3.1 "Page" vs "Path"

AppFS is capability-oriented. UI pages are translated into resources/actions.

Example:

1. Home page feed -> `home/feed.res.json`
2. Settings profile save -> `settings/profile/save.act`
3. Settings notification toggle -> `settings/notifications/toggle.act`

Do not model a page as one opaque file. Model what the agent can read/write.

## 3.2 Node Template Rules

Define paths as templates in `nodes`:

1. Live resource: `*.res.json`
2. Snapshot full-file resource: `*.res.jsonl`
3. Action sink: `*.act`
4. Include placeholders when entity IDs are dynamic (for example `{user_id}`, `{chat_id}`).

Example:

```json
{
  "nodes": {
    "home/feed.res.json": { "kind": "resource", "output_mode": "json" },
    "chats/{chat_id}/messages.res.jsonl": {
      "kind": "resource",
      "output_mode": "jsonl",
      "snapshot": { "max_materialized_bytes": 10485760 }
    },
    "settings/profile/save.act": {
      "kind": "action",
      "input_mode": "json",
      "execution_mode": "inline",
      "input_schema": "_meta/schemas/settings.profile.save.input.schema.json"
    }
  }
}
```

## 3.3 Runtime Behavior You Must Know

Current runtime behavior (important for adapter authors):

1. Runtime loads action specs from `_meta/manifest.res.json`.
2. Runtime treats `*.act` as append-only JSONL sinks under `/app/<app_id>/...` and tracks per-sink cursor offsets.
3. Runtime submits each complete newline-terminated JSON line in observed order.
4. Runtime defers incomplete tail lines (no trailing `\n`) until completion.
5. An undeclared `.act` path is ignored (no side effect).

Practical implication:

1. If a path is declared in manifest but no `.act` sink exists, nothing can trigger there.
2. If a `.act` file exists but manifest has no matching template, runtime ignores it.

## 4. Scaffold-to-File Association

After `new-adapter.sh`, think in a 1:1 mapping table:

1. One declared action template -> one bridge handler branch
2. One control action kind (`paging_fetch_next`, `paging_close`) -> one control handler (only for live paging)
3. One snapshot refresh action (`_snapshot/refresh.act`) -> one snapshot materialization handler
4. One resource template -> one resource producer path (adapter/backend side)

Recommended mapping table format:

| Node template | Kind | Execution mode | Bridge route | Backend handler |
|---|---|---|---|---|
| `contacts/{contact_id}/send_message.act` | action | inline | `/v1/submit-action` | `handle_send_message` |
| `files/{file_id}/download.act` | action | streaming | `/v1/submit-action` | `handle_download` |
| `_snapshot/refresh.act` | action | inline | `/v1/submit-action` | `handle_snapshot_refresh` |
| `_paging/fetch_next.act` | control | inline | `/v1/submit-control-action` | `handle_paging_fetch_next` |
| `_paging/close.act` | control | inline | `/v1/submit-control-action` | `handle_paging_close` |

## 5. Bridge Implementation Pattern

Use this split:

1. `protocol.py`: validation + dispatch + error mapping
2. `mock_aiim.py` or real backend connector: business logic
3. optional route contract file: template matcher + handler registry

For each declared action template:

1. Validate payload against declared `input_mode` and schema intent.
2. Enforce `execution_mode` (`inline` vs `streaming`) consistency.
3. Return AppAdapterV1-compatible outcome (`completed` or `streaming plan`).

## 6. Minimal Build Sequence for a Real App Connector

1. Draft app capability list from product flows (not UI labels).
2. Encode node templates and schemas in `manifest.res.json`.
3. Create sink/resource files in AppFS tree.
4. Fill node-to-handler mapping table.
5. Implement handlers in bridge backend.
6. Run:
   - unit tests for protocol/backend
   - `CT-001 ~ CT-022` live conformance (`CT-017` when bridge resilience probe is enabled)

## 7. Common Failure Modes

1. Declared template does not match real sink path -> action ignored.
2. `input_mode` says `json` but handler treats payload as text -> submit-time reject or backend failure.
3. `execution_mode` mismatch (`send_message` should be inline but implemented as streaming) -> contract failure.
4. Missing `_paging/*` control actions while claiming live pageable resources -> conformance failure.
5. Declaring `output_mode=jsonl` without `snapshot.max_materialized_bytes` -> manifest policy failure.

## 8. Decision: Docs or Code?

For this confusion, both are needed:

1. Docs: mandatory (define contract-to-handler process explicitly).
2. Code: recommended (scaffold should generate a mapping template so developers do not infer architecture).

This repository now treats docs as the first step. Scaffold improvements should follow the same mapping model.
