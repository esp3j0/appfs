#!/usr/bin/env python3
import json
import os
import threading
import time
from concurrent import futures
from datetime import datetime, timezone

import grpc

import appfs_adapter_v1_pb2 as pb1
import appfs_adapter_v1_pb2_grpc as pb1_grpc
import appfs_connector_pb2 as connector_pb
import appfs_connector_pb2_grpc as connector_pb_grpc
import appfs_structure_pb2 as structure_pb
import appfs_structure_pb2_grpc as structure_pb_grpc


def _env_int(name: str, default: int) -> int:
    raw = os.getenv(name, "").strip()
    if raw == "":
        return default
    try:
        return int(raw)
    except ValueError:
        return default


def _now_iso() -> str:
    return datetime.now(timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z")


def _json_compact(value: object) -> str:
    return json.dumps(value, separators=(",", ":"))


def _fixed_checked_at() -> str:
    return "2026-03-24T00:00:00Z"


def _fixed_live_expires_at() -> str:
    return "2026-03-24T01:00:00Z"


def _env_delay_ms(name: str) -> int:
    raw = os.getenv(name, "").strip()
    if raw == "":
        return 0
    try:
        return max(0, int(raw))
    except ValueError:
        return 0


def _validate_connector_context(message: object) -> connector_pb.ConnectorError | None:
    if not hasattr(message, "HasField") or not message.HasField("context"):
        return connector_pb.ConnectorError(
            code="INVALID_ARGUMENT",
            message="context object is required",
            retryable=False,
        )
    ctx = message.context
    required = (
        ("app_id", ctx.app_id),
        ("session_id", ctx.session_id),
        ("request_id", ctx.request_id),
    )
    for field, value in required:
        if not isinstance(value, str) or value.strip() == "":
            return connector_pb.ConnectorError(
                code="INVALID_ARGUMENT",
                message=f"context.{field} must be non-empty string",
                retryable=False,
            )
    return None


def _validate_structure_context(message: object) -> structure_pb.ConnectorError | None:
    if not hasattr(message, "HasField") or not message.HasField("context"):
        return structure_pb.ConnectorError(
            code="INVALID_ARGUMENT",
            message="context object is required",
            retryable=False,
        )
    ctx = message.context
    required = (
        ("app_id", ctx.app_id),
        ("session_id", ctx.session_id),
        ("request_id", ctx.request_id),
    )
    for field, value in required:
        if not isinstance(value, str) or value.strip() == "":
            return structure_pb.ConnectorError(
                code="INVALID_ARGUMENT",
                message=f"context.{field} must be non-empty string",
                retryable=False,
            )
    return None


def _action_manifest(template: str, execution_mode: str = "inline") -> dict[str, object]:
    return {
        "template": template,
        "kind": "action",
        "input_mode": "json",
        "execution_mode": execution_mode,
    }


def _snapshot_manifest(template: str, max_bytes: int) -> dict[str, object]:
    return {
        "template": template,
        "kind": "resource",
        "output_mode": "jsonl",
        "snapshot": {
            "max_materialized_bytes": max_bytes,
            "prewarm": True,
            "prewarm_timeout_ms": 5000,
            "read_through_timeout_ms": 10000,
            "on_timeout": "return_stale",
        },
    }


def _live_manifest(template: str) -> dict[str, object]:
    return {
        "template": template,
        "kind": "resource",
        "output_mode": "json",
        "paging": {
            "enabled": True,
            "mode": "live",
        },
    }


def _structure_nodes(active_scope: str) -> list[dict[str, object]]:
    nodes: list[dict[str, object]] = [
        {"path": "contacts", "kind": "directory", "mutable": False},
        {"path": "contacts/zhangsan", "kind": "directory", "mutable": False},
        {
            "path": "contacts/zhangsan/send_message.act",
            "kind": "action_file",
            "manifest_entry": _action_manifest("contacts/{contact_id}/send_message.act"),
            "mutable": True,
        },
        {"path": "feed", "kind": "directory", "mutable": False},
        {
            "path": "feed/recommendations.res.json",
            "kind": "live_resource",
            "manifest_entry": _live_manifest("feed/recommendations.res.json"),
            "seed_content": {
                "items": [],
                "page": {"handle_id": "", "page_no": 0, "has_more": True, "mode": "live"},
            },
            "mutable": False,
        },
        {"path": "_paging", "kind": "directory", "mutable": False},
        {
            "path": "_paging/fetch_next.act",
            "kind": "action_file",
            "manifest_entry": _action_manifest("_paging/fetch_next.act"),
            "mutable": True,
        },
        {
            "path": "_paging/close.act",
            "kind": "action_file",
            "manifest_entry": _action_manifest("_paging/close.act"),
            "mutable": True,
        },
        {"path": "_app", "kind": "directory", "mutable": False},
        {
            "path": "_app/enter_scope.act",
            "kind": "action_file",
            "manifest_entry": _action_manifest("_app/enter_scope.act"),
            "mutable": True,
        },
        {
            "path": "_app/refresh_structure.act",
            "kind": "action_file",
            "manifest_entry": _action_manifest("_app/refresh_structure.act"),
            "mutable": True,
        },
    ]

    if active_scope == "chat-long":
        nodes.extend(
            [
                {"path": "chats", "kind": "directory", "mutable": False, "scope": "chat-long"},
                {
                    "path": "chats/chat-long",
                    "kind": "directory",
                    "mutable": False,
                    "scope": "chat-long",
                },
                {
                    "path": "chats/chat-long/messages.res.jsonl",
                    "kind": "snapshot_resource",
                    "manifest_entry": _snapshot_manifest(
                        "chats/chat-long/messages.res.jsonl", 1024
                    ),
                    "mutable": False,
                    "scope": "chat-long",
                },
            ]
        )
    else:
        nodes.extend(
            [
                {"path": "chats", "kind": "directory", "mutable": False, "scope": "chat-001"},
                {
                    "path": "chats/chat-001",
                    "kind": "directory",
                    "mutable": False,
                    "scope": "chat-001",
                },
                {
                    "path": "chats/chat-001/messages.res.jsonl",
                    "kind": "snapshot_resource",
                    "manifest_entry": _snapshot_manifest(
                        "chats/chat-001/messages.res.jsonl", 10 * 1024 * 1024
                    ),
                    "mutable": False,
                    "scope": "chat-001",
                },
            ]
        )
    return nodes


def _structure_snapshot(scope: str | None) -> dict[str, object]:
    if scope in (None, "chat-001"):
        active_scope = "chat-001"
    elif scope == "chat-long":
        active_scope = "chat-long"
    else:
        raise ValueError(f"unknown structure scope: {scope}")
    return {
        "app_id": "aiim",
        "revision": f"demo-structure-{active_scope}",
        "active_scope": active_scope,
        "ownership_prefixes": ["_meta", "contacts", "feed", "chats", "_paging", "_app"],
        "nodes": _structure_nodes(active_scope),
    }


def _structure_node_kind(kind: str) -> int:
    mapping = {
        "directory": structure_pb.APP_STRUCTURE_NODE_KIND_DIRECTORY,
        "action_file": structure_pb.APP_STRUCTURE_NODE_KIND_ACTION_FILE,
        "snapshot_resource": structure_pb.APP_STRUCTURE_NODE_KIND_SNAPSHOT_RESOURCE,
        "live_resource": structure_pb.APP_STRUCTURE_NODE_KIND_LIVE_RESOURCE,
        "static_json_resource": structure_pb.APP_STRUCTURE_NODE_KIND_STATIC_JSON_RESOURCE,
    }
    return mapping.get(kind, structure_pb.APP_STRUCTURE_NODE_KIND_UNSPECIFIED)


def _structure_snapshot_message(snapshot: dict[str, object]) -> structure_pb.AppStructureSnapshot:
    return structure_pb.AppStructureSnapshot(
        app_id=str(snapshot["app_id"]),
        revision=str(snapshot["revision"]),
        active_scope=snapshot.get("active_scope"),
        ownership_prefixes=[str(value) for value in snapshot.get("ownership_prefixes", [])],
        nodes=[
            structure_pb.AppStructureNode(
                path=str(node["path"]),
                kind=_structure_node_kind(str(node["kind"])),
                manifest_entry_json=(
                    _json_compact(node["manifest_entry"])
                    if node.get("manifest_entry") is not None
                    else None
                ),
                seed_content_json=(
                    _json_compact(node["seed_content"])
                    if node.get("seed_content") is not None
                    else None
                ),
                mutable=bool(node.get("mutable", False)),
                scope=node.get("scope"),
            )
            for node in snapshot.get("nodes", [])
        ],
    )


def _parse_fail_status_code(raw: str) -> grpc.StatusCode:
    normalized = (raw or "").strip().upper()
    if normalized == "":
        return grpc.StatusCode.UNAVAILABLE
    code = grpc.StatusCode.__members__.get(normalized)
    if code is None:
        return grpc.StatusCode.UNAVAILABLE
    return code


class FaultInjector:
    def __init__(self) -> None:
        self._lock = threading.Lock()
        self.fail_next_submit_action = max(0, _env_int("APPFS_BRIDGE_FAIL_NEXT_SUBMIT_ACTION", 0))
        self.fail_path_prefix = os.getenv("APPFS_BRIDGE_FAIL_PATH_PREFIX", "").strip()
        self.fail_status_code = _parse_fail_status_code(os.getenv("APPFS_BRIDGE_FAIL_GRPC_CODE", ""))
        self.config_path = os.getenv(
            "APPFS_BRIDGE_FAULT_CONFIG_PATH", "/tmp/appfs-bridge-fault-config.json"
        ).strip()
        self._last_config_mtime = None

    def _reload_config_from_file(self) -> None:
        if self.config_path == "":
            return
        try:
            stat = os.stat(self.config_path)
        except OSError:
            return
        mtime = stat.st_mtime
        if self._last_config_mtime == mtime:
            return
        try:
            with open(self.config_path, "r", encoding="utf-8") as f:
                data = json.load(f)
        except Exception:
            return
        try:
            self.fail_next_submit_action = max(0, int(data.get("fail_next_submit_action", 0)))
            self.fail_path_prefix = str(data.get("fail_path_prefix", self.fail_path_prefix)).strip()
            self.fail_status_code = _parse_fail_status_code(
                str(data.get("fail_grpc_code", self.fail_status_code.name))
            )
        except Exception:
            return
        self._last_config_mtime = mtime

    def maybe_fail_submit_action(self, path: str) -> tuple[bool, int]:
        with self._lock:
            self._reload_config_from_file()
            if self.fail_next_submit_action <= 0:
                return (False, 0)
            if self.fail_path_prefix and not path.startswith(self.fail_path_prefix):
                return (False, self.fail_next_submit_action)
            self.fail_next_submit_action -= 1
            return (True, self.fail_next_submit_action)


FAULT_INJECTOR = FaultInjector()


class BridgeServiceV1(pb1_grpc.AppfsAdapterBridgeServicer):
    def SubmitAction(self, request: pb1.SubmitActionRequest, context: grpc.ServicerContext):
        path = request.path
        execution_mode = request.execution_mode
        should_fail, remaining = FAULT_INJECTOR.maybe_fail_submit_action(path)
        if should_fail:
            context.abort(
                FAULT_INJECTOR.fail_status_code,
                f"fault injected for path={path}, remaining={remaining}",
            )

        if execution_mode == pb1.EXECUTION_MODE_INLINE:
            if path.endswith("/send_message.act"):
                return pb1.SubmitActionResponse(
                    completed=pb1.CompletedOutcome(content_json=json.dumps("send success"))
                )
            return pb1.SubmitActionResponse(
                completed=pb1.CompletedOutcome(content_json=json.dumps("action completed"))
            )

        terminal = {"ok": True}
        if path.endswith("/download.act"):
            try:
                payload = json.loads(request.payload)
                terminal = {"saved_to": payload.get("target", "unknown")}
            except Exception:
                terminal = {"saved_to": "unknown"}

        return pb1.SubmitActionResponse(
            streaming=pb1.StreamingOutcome(
                accepted_content_json=json.dumps("accepted"),
                progress_content_json=json.dumps({"percent": 50}),
                terminal_content_json=json.dumps(terminal),
                has_accepted_content=True,
                has_progress_content=True,
            )
        )

    def SubmitControlAction(
        self, request: pb1.SubmitControlActionRequest, context: grpc.ServicerContext
    ):
        which = request.WhichOneof("action")

        if which == "paging_fetch_next":
            action = request.paging_fetch_next
            content = {
                "items": [{"id": f"m-{action.page_no}", "text": "generated by grpc bridge"}],
                "page": {
                    "handle_id": action.handle_id,
                    "page_no": action.page_no,
                    "has_more": action.has_more,
                    "mode": "live",
                },
            }
            return pb1.SubmitControlActionResponse(
                completed=pb1.ControlCompletedOutcome(content_json=json.dumps(content))
            )

        if which == "paging_close":
            action = request.paging_close
            content = {"closed": True, "handle_id": action.handle_id}
            return pb1.SubmitControlActionResponse(
                completed=pb1.ControlCompletedOutcome(content_json=json.dumps(content))
            )

        return pb1.SubmitControlActionResponse(
            error=pb1.BridgeError(
                code="NOT_SUPPORTED",
                message=f"unsupported control action: {which}",
                retryable=False,
            )
        )


class BridgeConnectorService(connector_pb_grpc.AppfsConnectorServicer):
    def GetConnectorInfo(self, request: connector_pb.GetConnectorInfoRequest, context: grpc.ServicerContext):
        _ = request
        return connector_pb.GetConnectorInfoResponse(
            info=connector_pb.ConnectorInfo(
                connector_id="mock-grpc",
                version="0.3.0-demo",
                app_id="aiim",
                transport=connector_pb.CONNECTOR_TRANSPORT_GRPC_BRIDGE,
                supports_snapshot=True,
                supports_live=True,
                supports_action=True,
                optional_features=["demo_mode"],
            )
        )

    def Health(self, request: connector_pb.HealthRequest, context: grpc.ServicerContext):
        _ = context
        context_error = _validate_connector_context(request)
        if context_error is not None:
            return connector_pb.HealthResponse(error=context_error)
        trace_id = request.context.trace_id
        if trace_id == "force-upstream-unavailable":
            return connector_pb.HealthResponse(
                error=connector_pb.ConnectorError(
                    code="UPSTREAM_UNAVAILABLE",
                    message="upstream endpoint is unavailable",
                    retryable=True,
                )
            )
        auth_status = connector_pb.AUTH_STATUS_EXPIRED if trace_id == "force-auth-expired" else connector_pb.AUTH_STATUS_VALID
        healthy = auth_status == connector_pb.AUTH_STATUS_VALID
        return connector_pb.HealthResponse(
            status=connector_pb.HealthStatus(
                healthy=healthy,
                auth_status=auth_status,
                message="demo connector healthy",
                checked_at=_fixed_checked_at(),
            )
        )

    def PrewarmSnapshotMeta(self, request: connector_pb.PrewarmSnapshotMetaRequest, context: grpc.ServicerContext):
        _ = context
        context_error = _validate_connector_context(request)
        if context_error is not None:
            return connector_pb.PrewarmSnapshotMetaResponse(error=context_error)
        if "/forbidden/" in request.resource_path:
            return connector_pb.PrewarmSnapshotMetaResponse(
                error=connector_pb.ConnectorError(
                    code="PERMISSION_DENIED",
                    message="resource is forbidden",
                    retryable=False,
                )
            )
        delay_ms = _env_delay_ms("APPFS_PREWARM_DELAY_MS")
        timeout_ms = max(1, request.timeout_ms)
        if delay_ms > timeout_ms:
            time.sleep(timeout_ms / 1000.0)
            return connector_pb.PrewarmSnapshotMetaResponse(
                error=connector_pb.ConnectorError(
                    code="TIMEOUT",
                    message=f"prewarm timeout resource={request.resource_path} delay_ms={delay_ms} timeout_ms={timeout_ms}",
                    retryable=True,
                )
            )
        if delay_ms > 0:
            time.sleep(delay_ms / 1000.0)
        return connector_pb.PrewarmSnapshotMetaResponse(
            meta=connector_pb.SnapshotMeta(
                size_bytes=5000,
                revision="demo-connector",
                last_modified=_fixed_checked_at(),
                item_count=2,
            )
        )

    def FetchSnapshotChunk(self, request: connector_pb.FetchSnapshotChunkRequest, context: grpc.ServicerContext):
        _ = context
        context_error = _validate_connector_context(request)
        if context_error is not None:
            return connector_pb.FetchSnapshotChunkResponse(error=context_error)
        if not request.HasField("request"):
            return connector_pb.FetchSnapshotChunkResponse(
                error=connector_pb.ConnectorError(
                    code="INVALID_ARGUMENT",
                    message="missing request payload",
                    retryable=False,
                )
            )
        req = request.request
        if req.budget_bytes <= 0:
            return connector_pb.FetchSnapshotChunkResponse(
                error=connector_pb.ConnectorError(
                    code="INVALID_ARGUMENT",
                    message="budget_bytes must be > 0",
                    retryable=False,
                )
            )
        if "too_large" in req.resource_path:
            return connector_pb.FetchSnapshotChunkResponse(
                error=connector_pb.ConnectorError(
                    code="SNAPSHOT_TOO_LARGE",
                    message="snapshot exceeds configured limit",
                    retryable=False,
                )
            )
        resume_kind = req.resume.WhichOneof("kind") if req.resume else None
        if resume_kind == "start":
            records = [
                connector_pb.SnapshotRecord(
                    record_key="rk-001",
                    ordering_key="ok-001",
                    line_json=_json_compact({"id": "m-1", "text": "hello"}),
                ),
                connector_pb.SnapshotRecord(
                    record_key="rk-002",
                    ordering_key="ok-002",
                    line_json=_json_compact({"id": "m-2", "text": "world"}),
                ),
            ]
            emitted_bytes = sum((len(r.line_json.encode("utf-8")) + 1) for r in records)
            return connector_pb.FetchSnapshotChunkResponse(
                response=connector_pb.SnapshotChunkResponse(
                    records=records,
                    emitted_bytes=emitted_bytes,
                    next_cursor="cursor-2",
                    has_more=True,
                    revision="demo-connector",
                )
            )
        if resume_kind == "cursor":
            if req.resume.cursor == "cursor-invalid":
                return connector_pb.FetchSnapshotChunkResponse(
                    error=connector_pb.ConnectorError(
                        code="INVALID_ARGUMENT",
                        message="resume cursor is invalid",
                        retryable=False,
                    )
                )
            if req.resume.cursor != "cursor-2":
                return connector_pb.FetchSnapshotChunkResponse(
                    error=connector_pb.ConnectorError(
                        code="INVALID_ARGUMENT",
                        message="resume cursor is unknown",
                        retryable=False,
                    )
                )
            records = [
                connector_pb.SnapshotRecord(
                    record_key="rk-003",
                    ordering_key="ok-003",
                    line_json=_json_compact({"id": "m-3", "text": "done"}),
                )
            ]
            return connector_pb.FetchSnapshotChunkResponse(
                response=connector_pb.SnapshotChunkResponse(
                    records=records,
                    emitted_bytes=sum((len(r.line_json.encode("utf-8")) + 1) for r in records),
                    has_more=False,
                    revision="demo-connector",
                )
            )
        if resume_kind == "offset":
            if "no-offset" in req.resource_path:
                return connector_pb.FetchSnapshotChunkResponse(
                    error=connector_pb.ConnectorError(
                        code="NOT_SUPPORTED",
                        message="offset resume is not supported for this resource",
                        retryable=False,
                    )
                )
            offset = req.resume.offset
            records = [
                connector_pb.SnapshotRecord(
                    record_key=f"rk-offset-{offset}",
                    ordering_key=f"ok-offset-{offset}",
                    line_json=_json_compact({"id": "m-offset", "offset": offset}),
                )
            ]
            return connector_pb.FetchSnapshotChunkResponse(
                response=connector_pb.SnapshotChunkResponse(
                    records=records,
                    emitted_bytes=sum((len(r.line_json.encode("utf-8")) + 1) for r in records),
                    has_more=False,
                    revision="demo-connector",
                )
            )
        return connector_pb.FetchSnapshotChunkResponse(
            error=connector_pb.ConnectorError(
                code="INVALID_ARGUMENT",
                message=f"unsupported resume kind: {resume_kind}",
                retryable=False,
            )
        )

    def FetchLivePage(self, request: connector_pb.FetchLivePageRequest, context: grpc.ServicerContext):
        _ = context
        context_error = _validate_connector_context(request)
        if context_error is not None:
            return connector_pb.FetchLivePageResponse(error=context_error)
        if not request.HasField("request"):
            return connector_pb.FetchLivePageResponse(
                error=connector_pb.ConnectorError(
                    code="INVALID_ARGUMENT",
                    message="missing request payload",
                    retryable=False,
                )
            )
        req = request.request
        if req.page_size <= 0:
            return connector_pb.FetchLivePageResponse(
                error=connector_pb.ConnectorError(
                    code="INVALID_ARGUMENT",
                    message="page_size must be > 0",
                    retryable=False,
                )
            )
        if req.cursor == "invalid":
            return connector_pb.FetchLivePageResponse(
                error=connector_pb.ConnectorError(
                    code="CURSOR_INVALID",
                    message="cursor is invalid",
                    retryable=False,
                )
            )
        if req.cursor == "expired":
            return connector_pb.FetchLivePageResponse(
                error=connector_pb.ConnectorError(
                    code="CURSOR_EXPIRED",
                    message="cursor has expired",
                    retryable=False,
                )
            )
        page_no = 2 if req.cursor == "cursor-1" else 1
        has_more = page_no == 1
        handle = req.handle_id if req.handle_id else "demo-live-handle-1"
        page_kwargs = {
            "handle_id": handle,
            "page_no": page_no,
            "has_more": has_more,
            "mode": connector_pb.LIVE_MODE_LIVE,
            "expires_at": _fixed_live_expires_at(),
        }
        if has_more:
            page_kwargs["next_cursor"] = "cursor-1"
        return connector_pb.FetchLivePageResponse(
            response=connector_pb.LivePageResponse(
                items_json=[_json_compact({"id": f"item-{page_no}", "resource": req.resource_path})],
                page=connector_pb.LivePageInfo(**page_kwargs),
            )
        )

    def SubmitAction(self, request: connector_pb.SubmitActionRequest, context: grpc.ServicerContext):
        context_error = _validate_connector_context(request)
        if context_error is not None:
            return connector_pb.SubmitActionResponse(error=context_error)
        if not request.HasField("request"):
            return connector_pb.SubmitActionResponse(
                error=connector_pb.ConnectorError(
                    code="INVALID_ARGUMENT",
                    message="missing request payload",
                    retryable=False,
                )
            )
        req = request.request
        path = req.path
        should_fail, remaining = FAULT_INJECTOR.maybe_fail_submit_action(path)
        if should_fail:
            context.abort(
                FAULT_INJECTOR.fail_status_code,
                f"fault injected for path={path}, remaining={remaining}",
            )

        if "invalid_payload" in path:
            return connector_pb.SubmitActionResponse(
                error=connector_pb.ConnectorError(
                    code="INVALID_PAYLOAD",
                    message="payload does not match schema",
                    retryable=False,
                )
            )
        if "rate_limited" in path:
            return connector_pb.SubmitActionResponse(
                error=connector_pb.ConnectorError(
                    code="RATE_LIMITED",
                    message="upstream rate limited",
                    retryable=True,
                )
            )
        try:
            payload_obj = json.loads(req.payload_json)
        except Exception:
            return connector_pb.SubmitActionResponse(
                error=connector_pb.ConnectorError(
                    code="INVALID_PAYLOAD",
                    message="payload does not match schema",
                    retryable=False,
                )
            )

        if req.execution_mode == connector_pb.ACTION_EXECUTION_MODE_INLINE:
            return connector_pb.SubmitActionResponse(
                response=connector_pb.SubmitActionOutput(
                    request_id=request.context.request_id,
                    estimated_duration_ms=120,
                    outcome=connector_pb.SubmitActionOutcome(
                        completed_content_json=_json_compact(
                            {"ok": True, "path": path, "echo": payload_obj}
                        )
                    ),
                )
            )

        return connector_pb.SubmitActionResponse(
            response=connector_pb.SubmitActionOutput(
                request_id=request.context.request_id,
                estimated_duration_ms=120,
                outcome=connector_pb.SubmitActionOutcome(
                    streaming_plan=connector_pb.ActionStreamingPlan(
                        accepted_content_json=_json_compact({"state": "accepted"}),
                        progress_content_json=_json_compact({"percent": 50}),
                        terminal_content_json=_json_compact({"ok": True}),
                        has_accepted_content=True,
                        has_progress_content=True,
                    )
                ),
            )
        )


class BridgeStructureService(structure_pb_grpc.AppfsStructureConnectorServicer):
    def GetAppStructure(
        self, request: structure_pb.GetAppStructureRequest, context: grpc.ServicerContext
    ):
        _ = context
        context_error = _validate_structure_context(request)
        if context_error is not None:
            return structure_pb.GetAppStructureResponse(error=context_error)
        if not request.HasField("request"):
            return structure_pb.GetAppStructureResponse(
                error=structure_pb.ConnectorError(
                    code="INVALID_ARGUMENT",
                    message="missing request payload",
                    retryable=False,
                )
            )
        req = request.request
        if not req.app_id.strip():
            return structure_pb.GetAppStructureResponse(
                error=structure_pb.ConnectorError(
                    code="INVALID_ARGUMENT",
                    message="app_id is required",
                    retryable=False,
                )
            )
        snapshot = _structure_snapshot(None)
        if req.known_revision and req.known_revision == snapshot["revision"]:
            return structure_pb.GetAppStructureResponse(
                response=structure_pb.AppStructureSyncResult(
                    unchanged=structure_pb.AppStructureSyncUnchanged(
                        app_id=req.app_id,
                        revision=str(snapshot["revision"]),
                        active_scope=str(snapshot["active_scope"]),
                    )
                )
            )
        return structure_pb.GetAppStructureResponse(
            response=structure_pb.AppStructureSyncResult(
                snapshot=structure_pb.AppStructureSyncSnapshot(
                    snapshot=_structure_snapshot_message(snapshot)
                )
            )
        )

    def RefreshAppStructure(
        self, request: structure_pb.RefreshAppStructureRequest, context: grpc.ServicerContext
    ):
        _ = context
        context_error = _validate_structure_context(request)
        if context_error is not None:
            return structure_pb.RefreshAppStructureResponse(error=context_error)
        if not request.HasField("request"):
            return structure_pb.RefreshAppStructureResponse(
                error=structure_pb.ConnectorError(
                    code="INVALID_ARGUMENT",
                    message="missing request payload",
                    retryable=False,
                )
            )
        req = request.request
        if not req.app_id.strip():
            return structure_pb.RefreshAppStructureResponse(
                error=structure_pb.ConnectorError(
                    code="INVALID_ARGUMENT",
                    message="app_id is required",
                    retryable=False,
                )
            )
        if req.reason == structure_pb.APP_STRUCTURE_SYNC_REASON_UNSPECIFIED:
            return structure_pb.RefreshAppStructureResponse(
                error=structure_pb.ConnectorError(
                    code="INVALID_ARGUMENT",
                    message="reason is required",
                    retryable=False,
                )
            )
        if (
            req.reason == structure_pb.APP_STRUCTURE_SYNC_REASON_ENTER_SCOPE
            and not req.target_scope.strip()
        ):
            return structure_pb.RefreshAppStructureResponse(
                error=structure_pb.ConnectorError(
                    code="STRUCTURE_SCOPE_INVALID",
                    message="target_scope is required for enter_scope refresh",
                    retryable=False,
                )
            )
        try:
            snapshot = _structure_snapshot(req.target_scope if req.target_scope else None)
        except ValueError as err:
            return structure_pb.RefreshAppStructureResponse(
                error=structure_pb.ConnectorError(
                    code="STRUCTURE_SCOPE_INVALID",
                    message=str(err),
                    retryable=False,
                )
            )
        if req.known_revision and req.known_revision == snapshot["revision"]:
            return structure_pb.RefreshAppStructureResponse(
                response=structure_pb.AppStructureSyncResult(
                    unchanged=structure_pb.AppStructureSyncUnchanged(
                        app_id=req.app_id,
                        revision=str(snapshot["revision"]),
                        active_scope=str(snapshot["active_scope"]),
                    )
                )
            )
        return structure_pb.RefreshAppStructureResponse(
            response=structure_pb.AppStructureSyncResult(
                snapshot=structure_pb.AppStructureSyncSnapshot(
                    snapshot=_structure_snapshot_message(snapshot)
                )
            )
        )


def main() -> None:
    server = grpc.server(futures.ThreadPoolExecutor(max_workers=8))
    pb1_grpc.add_AppfsAdapterBridgeServicer_to_server(BridgeServiceV1(), server)
    connector_pb_grpc.add_AppfsConnectorServicer_to_server(BridgeConnectorService(), server)
    structure_pb_grpc.add_AppfsStructureConnectorServicer_to_server(BridgeStructureService(), server)
    server.add_insecure_port("127.0.0.1:50051")
    server.start()
    print("AppFS gRPC bridge listening on 127.0.0.1:50051")
    print(
        "Fault injector: fail_next_submit_action=%d fail_status_code=%s fail_path_prefix=%r"
        % (
            FAULT_INJECTOR.fail_next_submit_action,
            FAULT_INJECTOR.fail_status_code.name,
            FAULT_INJECTOR.fail_path_prefix,
        )
    )
    print(f"Fault config path: {FAULT_INJECTOR.config_path}")
    server.wait_for_termination()


if __name__ == "__main__":
    main()
