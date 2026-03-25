from __future__ import annotations

import json
from typing import Any, Protocol

from .errors import internal_error, rejected_error
from .fault_injector import FaultInjector


class AdapterBackend(Protocol):
    def submit_action(self, path: str, execution_mode: str, payload: str) -> dict[str, object]:
        ...

    def submit_control_fetch_next(
        self,
        handle_id: str,
        page_no: int,
        has_more: bool,
    ) -> dict[str, object]:
        ...

    def submit_control_close(self, handle_id: str) -> dict[str, object]:
        ...


class ConnectorBackend(Protocol):
    def connector_info(self) -> dict[str, Any]:
        ...

    def health(self, context: dict[str, Any]) -> dict[str, Any]:
        ...

    def prewarm_snapshot_meta(
        self, request: dict[str, Any], context: dict[str, Any]
    ) -> dict[str, Any]:
        ...

    def fetch_snapshot_chunk(
        self, request: dict[str, Any], context: dict[str, Any]
    ) -> dict[str, Any]:
        ...

    def fetch_live_page(self, request: dict[str, Any], context: dict[str, Any]) -> dict[str, Any]:
        ...

    def submit_action_v2(
        self, request: dict[str, Any], context: dict[str, Any]
    ) -> dict[str, Any]:
        ...

    def get_app_structure(
        self, request: dict[str, Any], context: dict[str, Any]
    ) -> dict[str, Any]:
        ...

    def refresh_app_structure(
        self, request: dict[str, Any], context: dict[str, Any]
    ) -> dict[str, Any]:
        ...


ALLOWED_EXECUTION_MODES = {"inline", "streaming"}
ALLOWED_INPUT_MODES = {"json"}


def dispatch_submit_action(
    payload: dict[str, Any],
    *,
    fault_injector: FaultInjector,
    backend: AdapterBackend,
) -> tuple[int, dict[str, Any]]:
    path = payload.get("path")
    if not isinstance(path, str) or path.strip() == "":
        return (
            400,
            rejected_error("INVALID_ARGUMENT", "path is required and must be a string"),
        )
    if not _is_safe_action_path(path):
        return (400, rejected_error("INVALID_ARGUMENT", f"unsafe action path: {path}"))

    execution_mode = payload.get("execution_mode")
    if not isinstance(execution_mode, str) or execution_mode not in ALLOWED_EXECUTION_MODES:
        return (
            400,
            rejected_error(
                "INVALID_ARGUMENT",
                "execution_mode must be one of: inline, streaming",
            ),
        )

    input_mode = payload.get("input_mode")
    if not isinstance(input_mode, str) or input_mode not in ALLOWED_INPUT_MODES:
        return (
            400,
            rejected_error("INVALID_ARGUMENT", "input_mode must be json"),
        )

    raw_body = payload.get("payload")
    if not isinstance(raw_body, str):
        return (400, rejected_error("INVALID_PAYLOAD", "payload must be a string"))

    validation_error = _validate_action_payload(path, execution_mode, input_mode, raw_body)
    if validation_error is not None:
        return validation_error

    should_fail, remaining = fault_injector.maybe_fail_submit_action(path)
    if should_fail:
        return (
            fault_injector.fail_http_status,
            internal_error(f"fault injected for path={path}, remaining={remaining}"),
        )

    try:
        return (200, backend.submit_action(path, execution_mode, raw_body))
    except Exception as err:
        return (500, internal_error(f"backend submit_action failed: {err}"))


def dispatch_submit_control(
    payload: dict[str, Any],
    *,
    backend: AdapterBackend,
) -> tuple[int, dict[str, Any]]:
    path = payload.get("path")
    if not isinstance(path, str) or path.strip() == "":
        return (
            400,
            rejected_error("INVALID_ARGUMENT", "path is required and must be a string"),
        )
    if not _is_safe_action_path(path):
        return (400, rejected_error("INVALID_ARGUMENT", f"unsafe control path: {path}"))

    action = payload.get("action")
    if not isinstance(action, dict):
        return (400, rejected_error("INVALID_ARGUMENT", "action object is required"))

    kind = action.get("kind")
    if not isinstance(kind, str) or kind.strip() == "":
        return (400, rejected_error("INVALID_ARGUMENT", "action.kind is required"))

    if kind == "paging_fetch_next":
        handle_id = action.get("handle_id")
        if not isinstance(handle_id, str) or handle_id.strip() == "":
            return (400, rejected_error("INVALID_ARGUMENT", "handle_id is required"))

        page_no = action.get("page_no", 1)
        if isinstance(page_no, bool) or not isinstance(page_no, int) or page_no < 1:
            return (400, rejected_error("INVALID_ARGUMENT", "page_no must be a positive integer"))

        has_more = action.get("has_more", False)
        if not isinstance(has_more, bool):
            return (400, rejected_error("INVALID_ARGUMENT", "has_more must be boolean"))

        try:
            return (
                200,
                backend.submit_control_fetch_next(handle_id, page_no, has_more),
            )
        except Exception as err:
            return (500, internal_error(f"backend fetch_next failed: {err}"))

    if kind == "paging_close":
        handle_id = action.get("handle_id")
        if not isinstance(handle_id, str) or handle_id.strip() == "":
            return (400, rejected_error("INVALID_ARGUMENT", "handle_id is required"))
        try:
            return (200, backend.submit_control_close(handle_id))
        except Exception as err:
            return (500, internal_error(f"backend close failed: {err}"))

    return (
        400,
        rejected_error("NOT_SUPPORTED", f"unsupported control action: {kind}"),
    )


def dispatch_v2_connector_info(backend: ConnectorBackend) -> tuple[int, dict[str, Any]]:
    try:
        return (200, backend.connector_info())
    except Exception as err:
        return (500, connector_error("INTERNAL", f"backend connector_info failed: {err}", True))


def dispatch_v2_health(payload: dict[str, Any], backend: ConnectorBackend) -> tuple[int, dict[str, Any]]:
    context = payload.get("context")
    context_error = validate_context(context)
    if context_error is not None:
        return context_error
    try:
        return (200, backend.health(context))
    except Exception as err:
        return (500, connector_error("UPSTREAM_UNAVAILABLE", f"health failed: {err}", True))


def dispatch_v2_snapshot_prewarm(
    payload: dict[str, Any],
    backend: ConnectorBackend,
) -> tuple[int, dict[str, Any]]:
    parsed = parse_v2_wrapped_request(payload)
    if "error" in parsed:
        return parsed["error"]
    context = parsed["context"]
    request = parsed["request"]
    resource_path = request.get("resource_path")
    if not isinstance(resource_path, str) or not resource_path.strip():
        return (400, connector_error("INVALID_ARGUMENT", "resource_path is required", False))
    timeout_ms = request.get("timeout_ms")
    if (
        isinstance(timeout_ms, bool)
        or not isinstance(timeout_ms, int)
        or timeout_ms <= 0
    ):
        return (400, connector_error("INVALID_ARGUMENT", "timeout_ms must be > 0", False))
    try:
        return (200, backend.prewarm_snapshot_meta(request, context))
    except PermissionError as err:
        return (403, connector_error("PERMISSION_DENIED", str(err), False))
    except TimeoutError as err:
        return (504, connector_error("TIMEOUT", str(err), True))
    except Exception as err:
        return (500, connector_error("INTERNAL", f"prewarm failed: {err}", True))


def dispatch_v2_snapshot_fetch_chunk(
    payload: dict[str, Any],
    *,
    fault_injector: FaultInjector,
    backend: ConnectorBackend,
) -> tuple[int, dict[str, Any]]:
    parsed = parse_v2_wrapped_request(payload)
    if "error" in parsed:
        return parsed["error"]
    context = parsed["context"]
    request = parsed["request"]
    resource_path = request.get("resource_path")
    if not isinstance(resource_path, str) or not resource_path.strip():
        return (400, connector_error("INVALID_ARGUMENT", "resource_path is required", False))
    budget_bytes = request.get("budget_bytes")
    if isinstance(budget_bytes, bool) or not isinstance(budget_bytes, int) or budget_bytes <= 0:
        return (400, connector_error("INVALID_ARGUMENT", "budget_bytes must be > 0", False))
    resume = request.get("resume")
    if not isinstance(resume, dict) or not isinstance(resume.get("kind"), str):
        return (400, connector_error("INVALID_ARGUMENT", "resume.kind is required", False))
    resume_kind = resume.get("kind")
    resume_value = resume.get("value")
    if resume_kind == "start":
        if "value" in resume and resume_value is not None:
            return (400, connector_error("INVALID_ARGUMENT", "resume start must not include value", False))
    elif resume_kind == "cursor":
        if not isinstance(resume_value, str) or resume_value.strip() == "":
            return (400, connector_error("INVALID_ARGUMENT", "resume cursor requires non-empty string value", False))
    elif resume_kind == "offset":
        if isinstance(resume_value, bool) or not isinstance(resume_value, int) or resume_value < 0:
            return (400, connector_error("INVALID_ARGUMENT", "resume offset requires non-negative integer value", False))
    else:
        return (400, connector_error("INVALID_ARGUMENT", f"unsupported resume kind: {resume_kind}", False))
    try:
        return (200, backend.fetch_snapshot_chunk(request, context))
    except OverflowError as err:
        return (413, connector_error("SNAPSHOT_TOO_LARGE", str(err), False))
    except NotImplementedError as err:
        return (400, connector_error("NOT_SUPPORTED", str(err), False))
    except ValueError as err:
        return (400, connector_error("INVALID_ARGUMENT", str(err), False))
    except Exception as err:
        should_fail, remaining = fault_injector.maybe_fail_submit_action("/v2/connector/snapshot/fetch-chunk")
        if should_fail:
            return (
                fault_injector.fail_http_status,
                connector_error(
                    "UPSTREAM_UNAVAILABLE",
                    f"fault injected for snapshot chunk, remaining={remaining}",
                    True,
                ),
            )
        return (500, connector_error("INTERNAL", f"fetch_snapshot_chunk failed: {err}", True))


def dispatch_v2_live_fetch_page(
    payload: dict[str, Any], backend: ConnectorBackend
) -> tuple[int, dict[str, Any]]:
    parsed = parse_v2_wrapped_request(payload)
    if "error" in parsed:
        return parsed["error"]
    context = parsed["context"]
    request = parsed["request"]
    resource_path = request.get("resource_path")
    if not isinstance(resource_path, str) or not resource_path.strip():
        return (400, connector_error("INVALID_ARGUMENT", "resource_path is required", False))
    page_size = request.get("page_size")
    if isinstance(page_size, bool) or not isinstance(page_size, int) or page_size <= 0:
        return (400, connector_error("INVALID_ARGUMENT", "page_size must be > 0", False))
    handle_id = request.get("handle_id")
    if handle_id is not None:
        if not isinstance(handle_id, str) or handle_id.strip() == "":
            return (400, connector_error("INVALID_ARGUMENT", "handle_id must be a non-empty string when provided", False))
    cursor = request.get("cursor")
    if cursor is not None:
        if not isinstance(cursor, str) or cursor.strip() == "":
            return (400, connector_error("INVALID_ARGUMENT", "cursor must be a non-empty string when provided", False))
    try:
        return (200, backend.fetch_live_page(request, context))
    except ValueError as err:
        return (400, connector_error("CURSOR_INVALID", str(err), False))
    except TimeoutError as err:
        return (400, connector_error("CURSOR_EXPIRED", str(err), False))
    except Exception as err:
        return (500, connector_error("INTERNAL", f"fetch_live_page failed: {err}", True))


def dispatch_v2_submit_action(
    payload: dict[str, Any],
    *,
    fault_injector: FaultInjector,
    backend: ConnectorBackend,
) -> tuple[int, dict[str, Any]]:
    parsed = parse_v2_wrapped_request(payload)
    if "error" in parsed:
        return parsed["error"]
    context = parsed["context"]
    request = parsed["request"]
    path = request.get("path")
    if not isinstance(path, str) or path.strip() == "":
        return (400, connector_error("INVALID_ARGUMENT", "path is required", False))
    if not _is_safe_action_path(path):
        return (400, connector_error("INVALID_ARGUMENT", f"unsafe action path: {path}", False))
    execution_mode = request.get("execution_mode")
    if not isinstance(execution_mode, str) or execution_mode not in ALLOWED_EXECUTION_MODES:
        return (400, connector_error("INVALID_ARGUMENT", "execution_mode must be inline|streaming", False))
    payload_obj = request.get("payload")
    if not isinstance(payload_obj, dict):
        return (400, connector_error("INVALID_PAYLOAD", "payload must be object", False))

    should_fail, remaining = fault_injector.maybe_fail_submit_action(path)
    if should_fail:
        return (
            fault_injector.fail_http_status,
            connector_error(
                "UPSTREAM_UNAVAILABLE",
                f"fault injected for path={path}, remaining={remaining}",
                True,
            ),
        )

    try:
        return (200, backend.submit_action_v2(request, context))
    except ValueError as err:
        return (400, connector_error("INVALID_PAYLOAD", str(err), False))
    except RuntimeError as err:
        message = str(err)
        if "rate limit" in message.lower():
            return (429, connector_error("RATE_LIMITED", message, True))
        return (503, connector_error("UPSTREAM_UNAVAILABLE", message, True))
    except Exception as err:
        return (500, connector_error("INTERNAL", f"submit_action failed: {err}", True))


def dispatch_v3_get_app_structure(
    payload: dict[str, Any],
    backend: ConnectorBackend,
) -> tuple[int, dict[str, Any]]:
    parsed = parse_v2_wrapped_request(payload)
    if "error" in parsed:
        return parsed["error"]
    context = parsed["context"]
    request = parsed["request"]
    app_id = request.get("app_id")
    if not isinstance(app_id, str) or not app_id.strip():
        return (400, connector_error("INVALID_ARGUMENT", "app_id is required", False))
    known_revision = request.get("known_revision")
    if known_revision is not None and (
        not isinstance(known_revision, str) or known_revision.strip() == ""
    ):
        return (
            400,
            connector_error("INVALID_ARGUMENT", "known_revision must be non-empty string when provided", False),
        )
    try:
        return (200, backend.get_app_structure(request, context))
    except ValueError as err:
        return (400, connector_error("INVALID_ARGUMENT", str(err), False))
    except Exception as err:
        return (500, connector_error("INTERNAL", f"get_app_structure failed: {err}", True))


def dispatch_v3_refresh_app_structure(
    payload: dict[str, Any],
    backend: ConnectorBackend,
) -> tuple[int, dict[str, Any]]:
    parsed = parse_v2_wrapped_request(payload)
    if "error" in parsed:
        return parsed["error"]
    context = parsed["context"]
    request = parsed["request"]
    app_id = request.get("app_id")
    if not isinstance(app_id, str) or not app_id.strip():
        return (400, connector_error("INVALID_ARGUMENT", "app_id is required", False))
    reason = request.get("reason")
    if not isinstance(reason, str) or reason.strip() == "":
        return (400, connector_error("INVALID_ARGUMENT", "reason is required", False))
    known_revision = request.get("known_revision")
    if known_revision is not None and (
        not isinstance(known_revision, str) or known_revision.strip() == ""
    ):
        return (
            400,
            connector_error("INVALID_ARGUMENT", "known_revision must be non-empty string when provided", False),
        )
    target_scope = request.get("target_scope")
    if target_scope is not None and (
        not isinstance(target_scope, str) or target_scope.strip() == ""
    ):
        return (
            400,
            connector_error("INVALID_ARGUMENT", "target_scope must be non-empty string when provided", False),
        )
    trigger_action_path = request.get("trigger_action_path")
    if trigger_action_path is not None and (
        not isinstance(trigger_action_path, str) or trigger_action_path.strip() == ""
    ):
        return (
            400,
            connector_error(
                "INVALID_ARGUMENT",
                "trigger_action_path must be non-empty string when provided",
                False,
            ),
        )
    try:
        return (200, backend.refresh_app_structure(request, context))
    except ValueError as err:
        message = str(err)
        code = "STRUCTURE_SCOPE_INVALID" if "scope" in message.lower() else "INVALID_ARGUMENT"
        return (400, connector_error(code, message, False))
    except Exception as err:
        return (500, connector_error("INTERNAL", f"refresh_app_structure failed: {err}", True))


def parse_v2_wrapped_request(
    payload: dict[str, Any]
) -> dict[str, Any]:
    context = payload.get("context")
    context_error = validate_context(context)
    if context_error is not None:
        return {"error": context_error}
    request = payload.get("request")
    if not isinstance(request, dict):
        return {
            "error": (
                400,
                connector_error("INVALID_ARGUMENT", "request object is required", False),
            )
        }
    return {"context": context, "request": request}


def validate_context(context: Any) -> tuple[int, dict[str, Any]] | None:
    if not isinstance(context, dict):
        return (400, connector_error("INVALID_ARGUMENT", "context object is required", False))
    required = ["app_id", "session_id", "request_id"]
    for field in required:
        value = context.get(field)
        if not isinstance(value, str) or value.strip() == "":
            return (
                400,
                connector_error("INVALID_ARGUMENT", f"context.{field} must be non-empty string", False),
            )
    return None


def connector_error(
    code: str, message: str, retryable: bool, details: str | None = None
) -> dict[str, Any]:
    out: dict[str, Any] = {
        "code": code,
        "message": message,
        "retryable": bool(retryable),
    }
    if details is not None and details.strip():
        out["details"] = details
    return out


def _is_safe_action_path(path: str) -> bool:
    if not path.startswith("/") or not path.endswith(".act"):
        return False
    if "\x00" in path or "\\" in path or ":" in path:
        return False

    for segment in path.split("/"):
        if segment in (".", ".."):
            return False
    return True


def _validate_action_payload(
    path: str,
    execution_mode: str,
    input_mode: str,
    raw_body: str,
) -> tuple[int, dict[str, Any]] | None:
    if input_mode != "json":
        return (
            400,
            rejected_error(
                "INVALID_ARGUMENT",
                "input_mode must be json",
            ),
        )

    try:
        parsed_payload = json.loads(raw_body)
    except json.JSONDecodeError:
        return (
            400,
            rejected_error(
                "INVALID_PAYLOAD",
                "payload must be valid JSON",
            ),
        )

    if path.endswith("/send_message.act"):
        if execution_mode != "inline":
            return (
                400,
                rejected_error("INVALID_ARGUMENT", "send_message.act requires execution_mode=inline"),
            )
        if not isinstance(parsed_payload, dict):
            return (
                400,
                rejected_error(
                    "INVALID_PAYLOAD",
                    "send_message.act payload must be a JSON object",
                ),
            )
        text = parsed_payload.get("text")
        if not isinstance(text, str) or text.strip() == "":
            return (
                400,
                rejected_error(
                    "INVALID_PAYLOAD",
                    "send_message.act payload.text must be a non-empty string",
                ),
            )

    if path.endswith("/download.act"):
        if not isinstance(parsed_payload, dict):
            return (
                400,
                rejected_error(
                    "INVALID_PAYLOAD",
                    "download.act payload must be a JSON object",
                ),
            )
        target = parsed_payload.get("target")
        if not isinstance(target, str) or target.strip() == "":
            return (
                400,
                rejected_error(
                    "INVALID_PAYLOAD",
                    "download.act payload.target must be a non-empty string",
                ),
            )

    return None
