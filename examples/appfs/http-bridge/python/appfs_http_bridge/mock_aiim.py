from __future__ import annotations

import json
import os
import time
from dataclasses import dataclass, field
from datetime import datetime, timezone
from typing import Any


def _now_iso() -> str:
    return datetime.now(timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z")


def _fixed_checked_at() -> str:
    return "2026-03-24T00:00:00Z"


def _fixed_live_expires_at() -> str:
    return "2026-03-24T01:00:00Z"


def _compact_json(value: Any) -> str:
    return json.dumps(value, separators=(",", ":"))


def _env_delay_ms(name: str) -> int:
    raw = os.getenv(name, "").strip()
    if raw == "":
        return 0
    try:
        return max(0, int(raw))
    except ValueError:
        return 0


@dataclass
class MockAiimBackend:
    closed_handles: set[str] = field(default_factory=set)
    live_pages: dict[str, int] = field(default_factory=dict)

    def _action_manifest(self, template: str, execution_mode: str = "inline") -> dict[str, Any]:
        return {
            "template": template,
            "kind": "action",
            "input_mode": "json",
            "execution_mode": execution_mode,
        }

    def _snapshot_manifest(self, template: str, max_bytes: int) -> dict[str, Any]:
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

    def _live_manifest(self, template: str) -> dict[str, Any]:
        return {
            "template": template,
            "kind": "resource",
            "output_mode": "json",
            "paging": {"enabled": True, "mode": "live"},
        }

    def _structure_nodes(self, active_scope: str) -> list[dict[str, Any]]:
        nodes: list[dict[str, Any]] = [
            {
                "path": "contacts",
                "kind": "directory",
                "manifest_entry": None,
                "seed_content": None,
                "mutable": False,
                "scope": None,
            },
            {
                "path": "contacts/zhangsan",
                "kind": "directory",
                "manifest_entry": None,
                "seed_content": None,
                "mutable": False,
                "scope": None,
            },
            {
                "path": "contacts/zhangsan/send_message.act",
                "kind": "action_file",
                "manifest_entry": self._action_manifest(
                    "contacts/{contact_id}/send_message.act", "inline"
                ),
                "seed_content": None,
                "mutable": True,
                "scope": None,
            },
            {
                "path": "feed",
                "kind": "directory",
                "manifest_entry": None,
                "seed_content": None,
                "mutable": False,
                "scope": None,
            },
            {
                "path": "feed/recommendations.res.json",
                "kind": "live_resource",
                "manifest_entry": self._live_manifest("feed/recommendations.res.json"),
                "seed_content": {
                    "items": [],
                    "page": {"handle_id": "", "page_no": 0, "has_more": True, "mode": "live"},
                },
                "mutable": False,
                "scope": None,
            },
            {
                "path": "_paging",
                "kind": "directory",
                "manifest_entry": None,
                "seed_content": None,
                "mutable": False,
                "scope": None,
            },
            {
                "path": "_paging/fetch_next.act",
                "kind": "action_file",
                "manifest_entry": self._action_manifest("_paging/fetch_next.act", "inline"),
                "seed_content": None,
                "mutable": True,
                "scope": None,
            },
            {
                "path": "_paging/close.act",
                "kind": "action_file",
                "manifest_entry": self._action_manifest("_paging/close.act", "inline"),
                "seed_content": None,
                "mutable": True,
                "scope": None,
            },
            {
                "path": "_app",
                "kind": "directory",
                "manifest_entry": None,
                "seed_content": None,
                "mutable": False,
                "scope": None,
            },
            {
                "path": "_app/enter_scope.act",
                "kind": "action_file",
                "manifest_entry": self._action_manifest("_app/enter_scope.act", "inline"),
                "seed_content": None,
                "mutable": True,
                "scope": None,
            },
            {
                "path": "_app/refresh_structure.act",
                "kind": "action_file",
                "manifest_entry": self._action_manifest("_app/refresh_structure.act", "inline"),
                "seed_content": None,
                "mutable": True,
                "scope": None,
            },
        ]

        if active_scope == "chat-long":
            nodes.extend(
                [
                    {
                        "path": "chats",
                        "kind": "directory",
                        "manifest_entry": None,
                        "seed_content": None,
                        "mutable": False,
                        "scope": "chat-long",
                    },
                    {
                        "path": "chats/chat-long",
                        "kind": "directory",
                        "manifest_entry": None,
                        "seed_content": None,
                        "mutable": False,
                        "scope": "chat-long",
                    },
                    {
                        "path": "chats/chat-long/messages.res.jsonl",
                        "kind": "snapshot_resource",
                        "manifest_entry": self._snapshot_manifest(
                            "chats/chat-long/messages.res.jsonl", 1024
                        ),
                        "seed_content": None,
                        "mutable": False,
                        "scope": "chat-long",
                    },
                ]
            )
        else:
            nodes.extend(
                [
                    {
                        "path": "chats",
                        "kind": "directory",
                        "manifest_entry": None,
                        "seed_content": None,
                        "mutable": False,
                        "scope": "chat-001",
                    },
                    {
                        "path": "chats/chat-001",
                        "kind": "directory",
                        "manifest_entry": None,
                        "seed_content": None,
                        "mutable": False,
                        "scope": "chat-001",
                    },
                    {
                        "path": "chats/chat-001/messages.res.jsonl",
                        "kind": "snapshot_resource",
                        "manifest_entry": self._snapshot_manifest(
                            "chats/chat-001/messages.res.jsonl", 10 * 1024 * 1024
                        ),
                        "seed_content": None,
                        "mutable": False,
                        "scope": "chat-001",
                    },
                ]
            )

        return nodes

    def _structure_snapshot(self, scope: str | None) -> dict[str, Any]:
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
            "nodes": self._structure_nodes(active_scope),
        }

    def connector_info(self) -> dict[str, Any]:
        return {
            "connector_id": "mock-aiim-http-v2",
            "version": "0.3.0-demo",
            "app_id": "aiim",
            "transport": "http_bridge",
            "supports_snapshot": True,
            "supports_live": True,
            "supports_action": True,
            "optional_features": ["demo_mode"],
        }

    def health(self, context: dict[str, Any]) -> dict[str, Any]:
        trace_id = context.get("trace_id")
        if trace_id == "force-upstream-unavailable":
            raise ConnectionError("upstream endpoint is unavailable")
        auth_status = "expired" if trace_id == "force-auth-expired" else "valid"
        healthy = auth_status == "valid"
        return {
            "healthy": healthy,
            "auth_status": auth_status,
            "message": "demo connector healthy",
            "checked_at": _fixed_checked_at(),
        }

    def prewarm_snapshot_meta(
        self, request: dict[str, Any], context: dict[str, Any]
    ) -> dict[str, Any]:
        _ = context
        resource_path = str(request.get("resource_path", ""))
        if "/forbidden/" in resource_path:
            raise PermissionError("resource is forbidden")
        timeout_ms = request.get("timeout_ms")
        timeout_ms = int(timeout_ms) if isinstance(timeout_ms, int) else 0
        delay_ms = _env_delay_ms("APPFS_V3_PREWARM_DELAY_MS")
        if delay_ms > timeout_ms > 0:
            time.sleep(timeout_ms / 1000.0)
            raise TimeoutError(
                f"prewarm timeout resource={resource_path} delay_ms={delay_ms} timeout_ms={timeout_ms}"
            )
        if delay_ms > 0:
            time.sleep(delay_ms / 1000.0)
        return {
            "size_bytes": 5000,
            "revision": "demo-v2",
            "last_modified": _fixed_checked_at(),
            "item_count": 2,
        }

    def fetch_snapshot_chunk(
        self, request: dict[str, Any], context: dict[str, Any]
    ) -> dict[str, Any]:
        _ = context
        resource_path = str(request.get("resource_path", ""))
        budget_bytes = request.get("budget_bytes")
        if isinstance(budget_bytes, bool) or not isinstance(budget_bytes, int) or budget_bytes <= 0:
            raise ValueError("budget_bytes must be > 0")
        if "too_large" in resource_path:
            raise OverflowError("snapshot exceeds configured limit")

        resume = request.get("resume", {})
        kind = resume.get("kind")
        value = resume.get("value")

        if kind == "start":
            records = [
                {
                    "record_key": "rk-001",
                    "ordering_key": "ok-001",
                    "line": {"id": "m-1", "text": "hello"},
                },
                {
                    "record_key": "rk-002",
                    "ordering_key": "ok-002",
                    "line": {"id": "m-2", "text": "world"},
                },
            ]
            next_cursor = "cursor-2"
            has_more = True
        elif kind == "cursor":
            if value == "cursor-invalid":
                raise ValueError("resume cursor is invalid")
            if value != "cursor-2":
                raise ValueError("resume cursor is unknown")
            records = [
                {
                    "record_key": "rk-003",
                    "ordering_key": "ok-003",
                    "line": {"id": "m-3", "text": "done"},
                }
            ]
            next_cursor = None
            has_more = False
        else:
            if kind == "offset" and "no-offset" in resource_path:
                raise NotImplementedError("offset resume is not supported for this resource")
            offset_value = int(value) if isinstance(value, int) else 0
            records = [
                {
                    "record_key": f"rk-offset-{offset_value}",
                    "ordering_key": f"ok-offset-{offset_value}",
                    "line": {"id": "m-offset", "offset": offset_value},
                }
            ]
            next_cursor = None
            has_more = False

        emitted_bytes = 0
        for record in records:
            emitted_bytes += len(_compact_json(record["line"])) + 1
        return {
            "records": records,
            "emitted_bytes": emitted_bytes,
            "next_cursor": next_cursor,
            "has_more": has_more,
            "revision": "demo-v2",
        }

    def fetch_live_page(self, request: dict[str, Any], context: dict[str, Any]) -> dict[str, Any]:
        _ = context
        handle_id = request.get("handle_id") or "demo-live-handle-1"
        if not isinstance(handle_id, str):
            handle_id = "demo-live-handle-1"
        cursor = request.get("cursor")
        if cursor == "invalid":
            raise ValueError("cursor is invalid")
        if cursor == "expired":
            raise TimeoutError("cursor has expired")

        page_no = 2 if cursor == "cursor-1" else 1
        has_more = page_no == 1
        next_cursor = "cursor-1" if has_more else None
        return {
            "items": [{"id": f"item-{page_no}", "resource": request.get("resource_path")}],
            "page": {
                "handle_id": handle_id,
                "page_no": page_no,
                "has_more": has_more,
                "mode": "live",
                "expires_at": _fixed_live_expires_at(),
                "next_cursor": next_cursor,
                "retry_after_ms": None,
            },
        }

    def submit_action_v2(self, request: dict[str, Any], context: dict[str, Any]) -> dict[str, Any]:
        path = str(request.get("path", ""))
        payload = request.get("payload", {})
        if "invalid_payload" in path:
            raise ValueError("payload does not match schema")
        if "rate_limited" in path:
            raise RuntimeError("upstream rate limited")
        execution_mode = request.get("execution_mode")

        outcome: dict[str, Any]
        if execution_mode == "inline":
            outcome = {
                "kind": "completed",
                "content": {
                    "ok": True,
                    "path": path,
                    "echo": payload,
                },
            }
        else:
            outcome = {
                "kind": "streaming",
                "plan": {
                    "accepted_content": {"state": "accepted"},
                    "progress_content": {"percent": 50},
                    "terminal_content": {"ok": True},
                },
            }

        return {
            "request_id": str(context.get("request_id", "req-mock")),
            "estimated_duration_ms": 120,
            "outcome": outcome,
        }

    def get_app_structure(
        self, request: dict[str, Any], context: dict[str, Any]
    ) -> dict[str, Any]:
        _ = context
        snapshot = self._structure_snapshot(None)
        if request.get("known_revision") == snapshot["revision"]:
            return {
                "result": {
                    "kind": "unchanged",
                    "app_id": str(request.get("app_id", "aiim")),
                    "revision": snapshot["revision"],
                    "active_scope": snapshot["active_scope"],
                }
            }
        return {"result": {"kind": "snapshot", "snapshot": snapshot}}

    def refresh_app_structure(
        self, request: dict[str, Any], context: dict[str, Any]
    ) -> dict[str, Any]:
        _ = context
        reason = request.get("reason")
        target_scope = request.get("target_scope")
        if reason == "enter_scope" and not isinstance(target_scope, str):
            raise ValueError("target_scope is required for enter_scope refresh")
        snapshot = self._structure_snapshot(target_scope if isinstance(target_scope, str) else None)
        if request.get("known_revision") == snapshot["revision"]:
            return {
                "result": {
                    "kind": "unchanged",
                    "app_id": str(request.get("app_id", "aiim")),
                    "revision": snapshot["revision"],
                    "active_scope": snapshot["active_scope"],
                }
            }
        return {"result": {"kind": "snapshot", "snapshot": snapshot}}

    # Legacy v1 methods kept for baseline compatibility
    def submit_action(self, path: str, execution_mode: str, payload: str) -> dict[str, object]:
        if execution_mode == "inline":
            return self._submit_inline(path, payload)
        return self._submit_streaming(path, payload)

    def submit_control_fetch_next(
        self,
        handle_id: str,
        page_no: int,
        has_more: bool,
    ) -> dict[str, object]:
        return {
            "kind": "completed",
            "content": {
                "items": [{"id": f"m-{page_no}", "text": "generated by python bridge"}],
                "page": {
                    "handle_id": handle_id,
                    "page_no": page_no,
                    "has_more": has_more,
                    "mode": "live",
                },
            },
        }

    def submit_control_close(self, handle_id: str) -> dict[str, object]:
        self.closed_handles.add(handle_id)
        return {
            "kind": "completed",
            "content": {"closed": True, "handle_id": handle_id},
        }

    def _submit_inline(self, path: str, payload: str) -> dict[str, object]:
        if path.endswith("/send_message.act"):
            if payload.strip() == "":
                return {"kind": "completed", "content": "send failed: empty message"}
            return {"kind": "completed", "content": "send success"}
        return {"kind": "completed", "content": "action completed"}

    def _submit_streaming(self, path: str, payload: str) -> dict[str, object]:
        terminal_content: dict[str, object] = {"ok": True}
        if path.endswith("/download.act"):
            target = "unknown"
            try:
                parsed = json.loads(payload)
                if isinstance(parsed, dict):
                    parsed_target = parsed.get("target", "unknown")
                    if isinstance(parsed_target, str) and parsed_target.strip() != "":
                        target = parsed_target
            except Exception:
                target = "unknown"
            terminal_content = {"saved_to": target}

        return {
            "kind": "streaming",
            "plan": {
                "accepted_content": "accepted",
                "progress_content": {"percent": 50},
                "terminal_content": terminal_content,
            },
        }
