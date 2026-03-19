# AppFS v0.1 Contract Test Plan

- Version: `0.1-draft-r10`
- Date: `2026-03-17`
- Status: `Draft`
- Depends on: `APPFS-v0.1 (r9)`, `APPFS-adapter-requirements-v0.1`

## 1. Purpose

This plan defines executable contract checks for AppFS v0.1.

Goals:

1. Convert spec MUST clauses into repeatable tests.
2. Provide a stable gate for runtime and adapter changes.
3. Keep tests shell-first to match LLM+bash usage.

## 2. Test Entry

Runner:

```bash
cd cli
APPFS_CONTRACT_TESTS=1 ./tests/test-appfs-contract.sh
```

Static fixture mode (no live runtime):

```bash
cd cli
APPFS_CONTRACT_TESTS=1 APPFS_STATIC_FIXTURE=1 APPFS_ROOT=/mnt/c/Users/esp3j/rep/agentfs/examples/appfs ./tests/test-appfs-contract.sh
```

Optional aggregate runner:

```bash
cd cli
APPFS_CONTRACT_TESTS=1 ./tests/all.sh
```

Linux CI gate (GitHub Actions):

1. Static fixture gate:

```bash
APPFS_CONTRACT_TESTS=1 APPFS_STATIC_FIXTURE=1 APPFS_ROOT=$GITHUB_WORKSPACE/examples/appfs sh ./tests/test-appfs-contract.sh
```

2. Live mount + adapter gate:

```bash
APPFS_CONTRACT_TESTS=1 sh ./tests/appfs/run-live-with-adapter.sh
```

3. Live HTTP bridge gate:

```bash
APPFS_CONTRACT_TESTS=1 \
APPFS_ADAPTER_HTTP_ENDPOINT=http://127.0.0.1:8080 \
APPFS_ADAPTER_BRIDGE_MAX_RETRIES=1 \
APPFS_ADAPTER_BRIDGE_CIRCUIT_BREAKER_FAILURES=2 \
APPFS_ADAPTER_BRIDGE_CIRCUIT_BREAKER_COOLDOWN_MS=1200 \
APPFS_BRIDGE_RESILIENCE_CONTRACT=1 \
sh ./tests/appfs/run-live-with-adapter.sh
```

4. Live gRPC bridge gate:

```bash
APPFS_CONTRACT_TESTS=1 \
APPFS_ADAPTER_GRPC_ENDPOINT=http://127.0.0.1:50051 \
APPFS_ADAPTER_BRIDGE_MAX_RETRIES=1 \
APPFS_ADAPTER_BRIDGE_CIRCUIT_BREAKER_FAILURES=2 \
APPFS_ADAPTER_BRIDGE_CIRCUIT_BREAKER_COOLDOWN_MS=1200 \
APPFS_BRIDGE_RESILIENCE_CONTRACT=1 \
sh ./tests/appfs/run-live-with-adapter.sh
```

## 3. Environment Inputs

| Variable | Default | Description |
|---|---|---|
| `APPFS_CONTRACT_TESTS` | `0` | Set `1` to enable AppFS contract tests |
| `APPFS_ROOT` | `/app` | Mounted AppFS root |
| `APPFS_APP_ID` | `aiim` | App id under `/app` |
| `APPFS_TEST_ACTION` | `/app/aiim/contacts/zhangsan/send_message.act` | Action sink used by action tests |
| `APPFS_PAGEABLE_RESOURCE` | `/app/aiim/feed/recommendations.res.json` | Live pageable resource used by paging tests |
| `APPFS_EXPIRED_PAGEABLE_RESOURCE` | `/app/aiim/feed/recommendations-expired.res.json` | Expired live pageable resource used by paging error mapping tests |
| `APPFS_LONG_HANDLE_RESOURCE` | `/app/aiim/feed/recommendations-long.res.json` | Live pageable resource with overlong handle id used by portability tests |
| `APPFS_SNAPSHOT_RESOURCE` | `/app/aiim/chats/chat-001/messages.res.jsonl` | Snapshot full-file resource used by grep/rg compatibility tests |
| `APPFS_OVERSIZE_SNAPSHOT_RESOURCE` | `/app/aiim/chats/chat-oversize/messages.res.jsonl` | Snapshot resource used by too-large error mapping tests |
| `APPFS_TIMEOUT_SEC` | `10` | Wait timeout for async assertions |
| `APPFS_STATIC_FIXTURE` | `0` | Set `1` to run only static checks against fixture trees |
| `APPFS_BRIDGE_RESILIENCE_CONTRACT` | `0` | Set `1` in bridge-mode runs to execute `CT-017` (retry/circuit/recovery) |
| `APPFS_BRIDGE_RESILIENCE_CONTACT_PREFIX` | `resilience-` | Contact id prefix used by `CT-017` multi-sink probe |
| `APPFS_BRIDGE_FAULT_CONFIG_PATH` | `/tmp/appfs-bridge-fault-config.json` | Runtime-written bridge fault config for deterministic `CT-017` injection |
| `APPFS_BRIDGE_RESILIENCE_MIN_BREAKER_COOLDOWN_MS` | `4000` | Minimum breaker cooldown floor enforced during `CT-017` to avoid timing races |

## 4. Contract Suite

Note: `cli/tests/appfs/` includes direct scripts for baseline and extended checks (`CT-001`..`CT-015` plus `CT-018` burst append queueing, `CT-020` multiline JSON recovery, `CT-021` snapshot full-file semantics, and `CT-022` snapshot too-large mapping), while `run-live-with-adapter.sh` additionally executes lifecycle probes (`CT-016`), optional bridge resilience probe (`CT-017`), and restart cursor recovery (`CT-019`). Sections below highlight baseline CT-001~CT-005 and the same runner also executes extended live checks (`CT-006` streaming lifecycle, `CT-007` submit-time malformed/invalid JSONL reject behavior, `CT-008` submit ordering, `CT-009` paging error mapping, `CT-010`/`CT-011` submit atomicity/interruption, `CT-012` path safety, `CT-013` duplicate consumption, `CT-014` concurrent submit stress, `CT-015` long-handle normalization, `CT-016` restart reconciliation, `CT-017` bridge retry/circuit/recovery fault tolerance, `CT-018` burst append queueing, `CT-019` restart cursor recovery, `CT-020` shell-expanded multiline JSON recovery, `CT-021` snapshot full-file semantics, and `CT-022` snapshot too-large mapping).

### CT-001 Layout and Required Nodes

Spec refs:

1. `APPFS-v0.1` section 4.
2. `APPFS-v0.1` section 13.

Assertions:

1. Required files exist (`manifest`, `context`, `permissions`, `events`, `cursor`, `from-seq`).
2. If manifest declares live pageable resources, `_paging/fetch_next.act` and `_paging/close.act` are required.
3. `manifest` has `app_id` and `nodes`.

Script:

```text
cli/tests/appfs/test-layout.sh
```

### CT-002 Action Sink Semantics

Spec refs:

1. `APPFS-v0.1` section 7.
2. `APPFS-v0.1` section 8.

Assertions:

1. Append JSONL line (`>>`) to `.act` succeeds.
2. Event stream grows after action submission.
3. `>` overwrite/truncate does not create a committed request.
4. New terminal event contains `request_id` and `type` (when `jq` is available).

Script:

```text
cli/tests/appfs/test-action-basics.sh
```

### CT-003 Stream Replay and Cursor

Spec refs:

1. `APPFS-v0.1` section 8 (replay/resume).

Assertions:

1. `cursor.res.json` has `min_seq`, `max_seq`, `retention_hint_sec`.
2. `from-seq/<seq>.evt.jsonl` returns data for valid sequence.
3. `from-seq/<min_seq-1>.evt.jsonl` fails when older than retained window.

Script:

```text
cli/tests/appfs/test-stream-replay.sh
```

### CT-004 Paging Handle Protocol

Spec refs:

1. `APPFS-v0.1` section 11.

Assertions:

1. Pageable `cat` returns `{items, page}`.
2. `page.handle_id` is present.
3. `fetch_next.act` accepts `handle_id`.
4. Stream contains completion event for paging action.
5. `close.act` accepts `handle_id`.

Script:

```text
cli/tests/appfs/test-paging.sh
```

### CT-005 Manifest Policy Checks

Spec refs:

1. `APPFS-v0.1` section 5.
2. `APPFS-v0.1` section 13.

Assertions:

1. Node names do not contain forbidden path patterns (`..`, backslash, drive letters).
2. Action nodes define expected fields (`input_mode`, `execution_mode`).
3. Snapshot resources (`output_mode=jsonl`) define `snapshot.max_materialized_bytes` and do not enable paging.
4. Pageable resources define `paging` metadata with `paging.mode=live`.

Script:

```text
cli/tests/appfs/test-manifest-policy.sh
```

### CT-017 Bridge Fault Tolerance (Retry/Circuit/Recovery)

Spec refs:

1. `APPFS-adapter-requirements-v0.1` (`AR-019`, resilience baseline).
2. Bridge runtime resilience options in `agentfs serve appfs`.

Assertions:

1. Retryable transport failures trigger bounded retry attempts (observed in adapter log).
2. After consecutive retryable failures hit threshold, circuit breaker short-circuits new requests.
3. During circuit-open window, request still receives deterministic terminal failure event.
4. After cooldown, a healthy request succeeds without runtime restart.

Entry:

```text
cli/tests/appfs/run-live-with-adapter.sh (enabled via APPFS_BRIDGE_RESILIENCE_CONTRACT=1)
```

### CT-020 Shell-Expanded Multiline JSON Recovery

Spec refs:

1. `APPFS-v0.1` section 7 (JSONL submission boundaries + runtime compatibility recovery).
2. `APPFS-adapter-requirements-v0.1` (`AR-016`, submit-time validation and ordering).

Assertions:

1. Runtime recovers one request from shell-expanded multiline JSON fragments on a single `.act` sink.
2. Recovered request emits deterministic terminal event (`action.completed`) with token correlation.
3. Consecutive multiline submissions on the same sink are both processed and stream sequence order is preserved.

Script:

```text
cli/tests/appfs/test-submit-multiline-recovery.sh
```

### CT-021 Snapshot Full-File Semantics

Spec refs:

1. `APPFS-v0.1` section 6 (resource suffix semantics).
2. `APPFS-v0.1` section 11 (snapshot vs live split).

Assertions:

1. Snapshot resource is exposed as `*.res.jsonl` full file.
2. Snapshot lines are JSON message items (no `{items,page}` envelope).
3. Shell text tools (`rg`/`grep`) can query snapshot content directly.

Script:

```text
cli/tests/appfs/test-snapshot-full-file.sh
```

### CT-022 Snapshot Too-Large Error Mapping

Spec refs:

1. `APPFS-v0.1` section 13 (snapshot limits).
2. `APPFS-adapter-requirements-v0.1` (deterministic error mapping).

Assertions:

1. Submitting `/_snapshot/refresh.act` for an over-limit snapshot emits `action.failed`.
2. `error.code` is `SNAPSHOT_TOO_LARGE`.

Script:

```text
cli/tests/appfs/test-snapshot-too-large.sh
```

## 5. Gaps and Follow-up (v0.2 Candidate)

Not fully covered by shell black-box tests today:

1. Runtime pre-router unsafe segment rejection before backend side effects (needs lower-layer API test hooks).
2. `at-least-once` duplicate delivery behavior under crash/retry simulation.
3. Segment shortening hash determinism for generated IDs across adapters.

Recommended follow-up:

1. Add SDK-level and unit-level tests in runtime crate for path normalization and guard ordering.
2. Add fault-injection test harness for stream durability and replay.
