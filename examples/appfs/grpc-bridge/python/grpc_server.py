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
import appfs_connector_v2_pb2 as pb2
import appfs_connector_v2_pb2_grpc as pb2_grpc


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


class BridgeServiceV2(pb2_grpc.AppfsConnectorV2Servicer):
    def GetConnectorInfo(self, request: pb2.GetConnectorInfoRequest, context: grpc.ServicerContext):
        _ = request
        return pb2.GetConnectorInfoResponse(
            info=pb2.ConnectorInfoV2(
                connector_id="mock-grpc-v2",
                version="0.3.0-demo",
                app_id="aiim",
                transport=pb2.CONNECTOR_TRANSPORT_V2_GRPC_BRIDGE,
                supports_snapshot=True,
                supports_live=True,
                supports_action=True,
                optional_features=["demo_mode"],
            )
        )

    def Health(self, request: pb2.HealthRequest, context: grpc.ServicerContext):
        _ = context
        trace_id = request.context.trace_id if request.context else ""
        if trace_id == "force-upstream-unavailable":
            return pb2.HealthResponse(
                error=pb2.ConnectorErrorV2(
                    code="UPSTREAM_UNAVAILABLE",
                    message="upstream endpoint is unavailable",
                    retryable=True,
                )
            )
        auth_status = pb2.AUTH_STATUS_V2_EXPIRED if trace_id == "force-auth-expired" else pb2.AUTH_STATUS_V2_VALID
        healthy = auth_status == pb2.AUTH_STATUS_V2_VALID
        return pb2.HealthResponse(
            status=pb2.HealthStatusV2(
                healthy=healthy,
                auth_status=auth_status,
                message="demo connector healthy",
                checked_at=_fixed_checked_at(),
            )
        )

    def PrewarmSnapshotMeta(self, request: pb2.PrewarmSnapshotMetaRequest, context: grpc.ServicerContext):
        _ = context
        if "/forbidden/" in request.resource_path:
            return pb2.PrewarmSnapshotMetaResponse(
                error=pb2.ConnectorErrorV2(
                    code="PERMISSION_DENIED",
                    message="resource is forbidden",
                    retryable=False,
                )
            )
        delay_ms = _env_delay_ms("APPFS_V3_PREWARM_DELAY_MS")
        timeout_ms = max(1, request.timeout_ms)
        if delay_ms > timeout_ms:
            time.sleep(timeout_ms / 1000.0)
            return pb2.PrewarmSnapshotMetaResponse(
                error=pb2.ConnectorErrorV2(
                    code="TIMEOUT",
                    message=f"prewarm timeout resource={request.resource_path} delay_ms={delay_ms} timeout_ms={timeout_ms}",
                    retryable=True,
                )
            )
        if delay_ms > 0:
            time.sleep(delay_ms / 1000.0)
        return pb2.PrewarmSnapshotMetaResponse(
            meta=pb2.SnapshotMetaV2(
                size_bytes=5000,
                revision="demo-v2",
                last_modified=_fixed_checked_at(),
                item_count=2,
            )
        )

    def FetchSnapshotChunk(self, request: pb2.FetchSnapshotChunkRequest, context: grpc.ServicerContext):
        _ = context
        if not request.HasField("request"):
            return pb2.FetchSnapshotChunkResponse(
                error=pb2.ConnectorErrorV2(
                    code="INVALID_ARGUMENT",
                    message="missing request payload",
                    retryable=False,
                )
            )
        req = request.request
        if req.budget_bytes <= 0:
            return pb2.FetchSnapshotChunkResponse(
                error=pb2.ConnectorErrorV2(
                    code="INVALID_ARGUMENT",
                    message="budget_bytes must be > 0",
                    retryable=False,
                )
            )
        if "too_large" in req.resource_path:
            return pb2.FetchSnapshotChunkResponse(
                error=pb2.ConnectorErrorV2(
                    code="SNAPSHOT_TOO_LARGE",
                    message="snapshot exceeds configured limit",
                    retryable=False,
                )
            )
        resume_kind = req.resume.WhichOneof("kind") if req.resume else None
        if resume_kind == "start":
            records = [
                pb2.SnapshotRecordV2(
                    record_key="rk-001",
                    ordering_key="ok-001",
                    line_json=_json_compact({"id": "m-1", "text": "hello"}),
                ),
                pb2.SnapshotRecordV2(
                    record_key="rk-002",
                    ordering_key="ok-002",
                    line_json=_json_compact({"id": "m-2", "text": "world"}),
                ),
            ]
            emitted_bytes = sum((len(r.line_json.encode("utf-8")) + 1) for r in records)
            return pb2.FetchSnapshotChunkResponse(
                response=pb2.FetchSnapshotChunkResponseV2(
                    records=records,
                    emitted_bytes=emitted_bytes,
                    next_cursor="cursor-2",
                    has_more=True,
                    revision="demo-v2",
                )
            )
        if resume_kind == "cursor":
            if req.resume.cursor == "cursor-invalid":
                return pb2.FetchSnapshotChunkResponse(
                    error=pb2.ConnectorErrorV2(
                        code="INVALID_ARGUMENT",
                        message="resume cursor is invalid",
                        retryable=False,
                    )
                )
            if req.resume.cursor != "cursor-2":
                return pb2.FetchSnapshotChunkResponse(
                    error=pb2.ConnectorErrorV2(
                        code="INVALID_ARGUMENT",
                        message="resume cursor is unknown",
                        retryable=False,
                    )
                )
            records = [
                pb2.SnapshotRecordV2(
                    record_key="rk-003",
                    ordering_key="ok-003",
                    line_json=_json_compact({"id": "m-3", "text": "done"}),
                )
            ]
            return pb2.FetchSnapshotChunkResponse(
                response=pb2.FetchSnapshotChunkResponseV2(
                    records=records,
                    emitted_bytes=sum((len(r.line_json.encode("utf-8")) + 1) for r in records),
                    has_more=False,
                    revision="demo-v2",
                )
            )
        if resume_kind == "offset":
            if "no-offset" in req.resource_path:
                return pb2.FetchSnapshotChunkResponse(
                    error=pb2.ConnectorErrorV2(
                        code="NOT_SUPPORTED",
                        message="offset resume is not supported for this resource",
                        retryable=False,
                    )
                )
            offset = req.resume.offset
            records = [
                pb2.SnapshotRecordV2(
                    record_key=f"rk-offset-{offset}",
                    ordering_key=f"ok-offset-{offset}",
                    line_json=_json_compact({"id": "m-offset", "offset": offset}),
                )
            ]
            return pb2.FetchSnapshotChunkResponse(
                response=pb2.FetchSnapshotChunkResponseV2(
                    records=records,
                    emitted_bytes=sum((len(r.line_json.encode("utf-8")) + 1) for r in records),
                    has_more=False,
                    revision="demo-v2",
                )
            )
        return pb2.FetchSnapshotChunkResponse(
            error=pb2.ConnectorErrorV2(
                code="INVALID_ARGUMENT",
                message=f"unsupported resume kind: {resume_kind}",
                retryable=False,
            )
        )

    def FetchLivePage(self, request: pb2.FetchLivePageRequest, context: grpc.ServicerContext):
        _ = context
        if not request.HasField("request"):
            return pb2.FetchLivePageResponse(
                error=pb2.ConnectorErrorV2(
                    code="INVALID_ARGUMENT",
                    message="missing request payload",
                    retryable=False,
                )
            )
        req = request.request
        if req.page_size <= 0:
            return pb2.FetchLivePageResponse(
                error=pb2.ConnectorErrorV2(
                    code="INVALID_ARGUMENT",
                    message="page_size must be > 0",
                    retryable=False,
                )
            )
        if req.cursor == "invalid":
            return pb2.FetchLivePageResponse(
                error=pb2.ConnectorErrorV2(
                    code="CURSOR_INVALID",
                    message="cursor invalid",
                    retryable=False,
                )
            )
        if req.cursor == "expired":
            return pb2.FetchLivePageResponse(
                error=pb2.ConnectorErrorV2(
                    code="CURSOR_EXPIRED",
                    message="cursor expired",
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
            "mode": pb2.LIVE_MODE_V2_LIVE,
            "expires_at": _fixed_live_expires_at(),
        }
        if has_more:
            page_kwargs["next_cursor"] = "cursor-1"
        return pb2.FetchLivePageResponse(
            response=pb2.FetchLivePageResponseV2(
                items_json=[_json_compact({"id": f"item-{page_no}", "resource": req.resource_path})],
                page=pb2.LivePageInfoV2(**page_kwargs),
            )
        )

    def SubmitAction(self, request: pb2.SubmitActionRequest, context: grpc.ServicerContext):
        if not request.HasField("request"):
            return pb2.SubmitActionResponse(
                error=pb2.ConnectorErrorV2(
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
            return pb2.SubmitActionResponse(
                error=pb2.ConnectorErrorV2(
                    code="INVALID_PAYLOAD",
                    message="payload does not match schema",
                    retryable=False,
                )
            )
        if "rate_limited" in path:
            return pb2.SubmitActionResponse(
                error=pb2.ConnectorErrorV2(
                    code="RATE_LIMITED",
                    message="upstream rate limited",
                    retryable=True,
                )
            )
        try:
            payload_obj = json.loads(req.payload_json)
        except Exception:
            return pb2.SubmitActionResponse(
                error=pb2.ConnectorErrorV2(
                    code="INVALID_PAYLOAD",
                    message="payload does not match schema",
                    retryable=False,
                )
            )

        if req.execution_mode == pb2.ACTION_EXECUTION_MODE_V2_INLINE:
            return pb2.SubmitActionResponse(
                response=pb2.SubmitActionResponseV2(
                    request_id=request.context.request_id if request.context else "req-v2",
                    estimated_duration_ms=120,
                    outcome=pb2.SubmitActionOutcomeV2(
                        completed_content_json=_json_compact(
                            {"ok": True, "path": path, "echo": payload_obj}
                        )
                    ),
                )
            )

        return pb2.SubmitActionResponse(
            response=pb2.SubmitActionResponseV2(
                request_id=request.context.request_id if request.context else "req-v2",
                estimated_duration_ms=120,
                outcome=pb2.SubmitActionOutcomeV2(
                    streaming_plan=pb2.ActionStreamingPlanV2(
                        accepted_content_json=_json_compact({"state": "accepted"}),
                        progress_content_json=_json_compact({"percent": 50}),
                        terminal_content_json=_json_compact({"ok": True}),
                        has_accepted_content=True,
                        has_progress_content=True,
                    )
                ),
            )
        )


def main() -> None:
    server = grpc.server(futures.ThreadPoolExecutor(max_workers=8))
    pb1_grpc.add_AppfsAdapterBridgeServicer_to_server(BridgeServiceV1(), server)
    pb2_grpc.add_AppfsConnectorV2Servicer_to_server(BridgeServiceV2(), server)
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
