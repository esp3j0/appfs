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

ALLOWED_EXECUTION_MODES = {"inline", "streaming"}
ALLOWED_INPUT_MODES = {"text", "json", "text_or_json"}


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
            rejected_error("INVALID_ARGUMENT", "input_mode must be text, json, or text_or_json"),
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
    if path.endswith("/download.act"):
        if input_mode != "json":
            return (
                400,
                rejected_error(
                    "INVALID_ARGUMENT",
                    "download.act requires input_mode=json",
                ),
            )
        try:
            parsed = json.loads(raw_body)
        except json.JSONDecodeError:
            return (
                400,
                rejected_error(
                    "INVALID_PAYLOAD",
                    "download.act payload must be valid JSON",
                ),
            )
        if not isinstance(parsed, dict):
            return (
                400,
                rejected_error(
                    "INVALID_PAYLOAD",
                    "download.act payload must be a JSON object",
                ),
            )
        target = parsed.get("target")
        if not isinstance(target, str) or target.strip() == "":
            return (
                400,
                rejected_error(
                    "INVALID_PAYLOAD",
                    "download.act payload.target must be a non-empty string",
                ),
            )

    if path.endswith("/send_message.act") and execution_mode != "inline":
        return (
            400,
            rejected_error("INVALID_ARGUMENT", "send_message.act requires execution_mode=inline"),
        )

    return None
