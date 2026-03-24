from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from appfs_http_bridge.fault_injector import FaultInjector, FaultState
from appfs_http_bridge.mock_aiim import MockAiimBackend
from appfs_http_bridge.protocol import (
    dispatch_submit_action,
    dispatch_submit_control,
    dispatch_v2_connector_info,
    dispatch_v2_health,
    dispatch_v2_live_fetch_page,
    dispatch_v2_snapshot_fetch_chunk,
    dispatch_v2_snapshot_prewarm,
    dispatch_v2_submit_action,
)


class ProtocolTests(unittest.TestCase):
    def setUp(self) -> None:
        self.backend = MockAiimBackend()
        self.fault = FaultInjector(
            config_path="",
            initial_state=FaultState(
                fail_next_submit_action=0,
                fail_http_status=503,
                fail_path_prefix="",
            ),
        )

    def test_inline_send_message_success(self) -> None:
        status, body = dispatch_submit_action(
            {
                "path": "/contacts/zhangsan/send_message.act",
                "execution_mode": "inline",
                "input_mode": "json",
                "payload": '{"text":"hello"}',
            },
            fault_injector=self.fault,
            backend=self.backend,
        )
        self.assertEqual(status, 200)
        self.assertEqual(body["kind"], "completed")

    def test_streaming_download_success(self) -> None:
        status, body = dispatch_submit_action(
            {
                "path": "/files/file-001/download.act",
                "execution_mode": "streaming",
                "input_mode": "json",
                "payload": '{"target":"/tmp/download.bin"}',
            },
            fault_injector=self.fault,
            backend=self.backend,
        )
        self.assertEqual(status, 200)
        self.assertEqual(body["kind"], "streaming")
        self.assertEqual(body["plan"]["terminal_content"]["saved_to"], "/tmp/download.bin")

    def test_invalid_execution_mode_rejected(self) -> None:
        status, body = dispatch_submit_action(
            {
                "path": "/contacts/zhangsan/send_message.act",
                "execution_mode": "background",
                "input_mode": "json",
                "payload": '{"text":"hello"}',
            },
            fault_injector=self.fault,
            backend=self.backend,
        )
        self.assertEqual(status, 400)
        self.assertEqual(body["kind"], "rejected")
        self.assertEqual(body["code"], "INVALID_ARGUMENT")
        self.assertFalse(body["retryable"])

    def test_invalid_download_payload_rejected(self) -> None:
        status, body = dispatch_submit_action(
            {
                "path": "/files/file-001/download.act",
                "execution_mode": "streaming",
                "input_mode": "json",
                "payload": '{"target":',
            },
            fault_injector=self.fault,
            backend=self.backend,
        )
        self.assertEqual(status, 400)
        self.assertEqual(body["kind"], "rejected")
        self.assertEqual(body["code"], "INVALID_PAYLOAD")
        self.assertFalse(body["retryable"])

    def test_paging_fetch_next_success(self) -> None:
        status, body = dispatch_submit_control(
            {
                "path": "/_paging/fetch_next.act",
                "action": {
                    "kind": "paging_fetch_next",
                    "handle_id": "h-1",
                    "page_no": 2,
                    "has_more": True,
                },
            },
            backend=self.backend,
        )
        self.assertEqual(status, 200)
        self.assertEqual(body["kind"], "completed")
        self.assertEqual(body["content"]["page"]["page_no"], 2)
        self.assertTrue(body["content"]["page"]["has_more"])

    def test_paging_close_idempotent(self) -> None:
        payload = {
            "path": "/_paging/close.act",
            "action": {"kind": "paging_close", "handle_id": "h-2"},
        }
        status_1, body_1 = dispatch_submit_control(payload, backend=self.backend)
        status_2, body_2 = dispatch_submit_control(payload, backend=self.backend)

        self.assertEqual(status_1, 200)
        self.assertEqual(status_2, 200)
        self.assertEqual(body_1["kind"], "completed")
        self.assertEqual(body_2["kind"], "completed")
        self.assertTrue(body_1["content"]["closed"])
        self.assertTrue(body_2["content"]["closed"])

    def test_unsupported_control_rejected(self) -> None:
        status, body = dispatch_submit_control(
            {
                "path": "/_paging/close.act",
                "action": {"kind": "unknown_control"},
            },
            backend=self.backend,
        )
        self.assertEqual(status, 400)
        self.assertEqual(body["kind"], "rejected")
        self.assertEqual(body["code"], "NOT_SUPPORTED")
        self.assertFalse(body["retryable"])

    def test_v2_connector_info(self) -> None:
        status, body = dispatch_v2_connector_info(self.backend)
        self.assertEqual(status, 200)
        self.assertEqual(body["transport"], "http_bridge")
        self.assertEqual(body["app_id"], "aiim")

    def test_v2_health(self) -> None:
        status, body = dispatch_v2_health(
            {
                "context": {
                    "app_id": "aiim",
                    "session_id": "sess-1",
                    "request_id": "req-1",
                }
            },
            self.backend,
        )
        self.assertEqual(status, 200)
        self.assertTrue(body["healthy"])

    def test_v2_snapshot_fetch_chunk(self) -> None:
        status, body = dispatch_v2_snapshot_fetch_chunk(
            {
                "context": {
                    "app_id": "aiim",
                    "session_id": "sess-1",
                    "request_id": "req-1",
                },
                "request": {
                    "resource_path": "/chats/chat-001/messages.res.jsonl",
                    "resume": {"kind": "start"},
                    "budget_bytes": 1024,
                },
            },
            fault_injector=self.fault,
            backend=self.backend,
        )
        self.assertEqual(status, 200)
        self.assertEqual(len(body["records"]), 2)
        self.assertTrue(body["has_more"])

    def test_v2_live_fetch_page(self) -> None:
        payload = {
            "context": {
                "app_id": "aiim",
                "session_id": "sess-1",
                "request_id": "req-1",
            },
            "request": {
                "resource_path": "/chats/chat-001/messages.res.json",
                "handle_id": "ph-1",
                "cursor": None,
                "page_size": 20,
            },
        }
        status, body = dispatch_v2_live_fetch_page(payload, self.backend)
        self.assertEqual(status, 200)
        self.assertEqual(body["page"]["page_no"], 1)

    def test_v2_live_fetch_page_cursor_invalid(self) -> None:
        status, body = dispatch_v2_live_fetch_page(
            {
                "context": {
                    "app_id": "aiim",
                    "session_id": "sess-1",
                    "request_id": "req-1",
                },
                "request": {
                    "resource_path": "/chats/chat-001/messages.res.json",
                    "handle_id": "ph-1",
                    "cursor": "invalid",
                    "page_size": 20,
                },
            },
            self.backend,
        )
        self.assertEqual(status, 400)
        self.assertEqual(body["code"], "CURSOR_INVALID")

    def test_v2_submit_action(self) -> None:
        status, body = dispatch_v2_submit_action(
            {
                "context": {
                    "app_id": "aiim",
                    "session_id": "sess-1",
                    "request_id": "req-1",
                },
                "request": {
                    "path": "/contacts/zhangsan/send_message.act",
                    "payload": {"text": "hello"},
                    "execution_mode": "inline",
                },
            },
            fault_injector=self.fault,
            backend=self.backend,
        )
        self.assertEqual(status, 200)
        self.assertEqual(body["outcome"]["kind"], "completed")

    def test_v2_prewarm_requires_timeout(self) -> None:
        status, body = dispatch_v2_snapshot_prewarm(
            {
                "context": {
                    "app_id": "aiim",
                    "session_id": "sess-1",
                    "request_id": "req-1",
                },
                "request": {
                    "resource_path": "/chats/chat-001/messages.res.jsonl",
                },
            },
            self.backend,
        )
        self.assertEqual(status, 400)
        self.assertEqual(body["code"], "INVALID_ARGUMENT")

    def test_v2_submit_action_rate_limited(self) -> None:
        status, body = dispatch_v2_submit_action(
            {
                "context": {
                    "app_id": "aiim",
                    "session_id": "sess-1",
                    "request_id": "req-1",
                },
                "request": {
                    "path": "/contacts/zhangsan/rate_limited.act",
                    "payload": {"text": "hello"},
                    "execution_mode": "inline",
                },
            },
            fault_injector=self.fault,
            backend=self.backend,
        )
        self.assertEqual(status, 429)
        self.assertEqual(body["code"], "RATE_LIMITED")


if __name__ == "__main__":
    unittest.main()
