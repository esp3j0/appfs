from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from appfs_http_bridge.fault_injector import FaultInjector, FaultState
from appfs_http_bridge.mock_aiim import MockAiimBackend
from appfs_http_bridge.protocol import dispatch_submit_action, dispatch_submit_control


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
                "input_mode": "text_or_json",
                "payload": "hello",
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
                "input_mode": "text",
                "payload": "hello",
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


if __name__ == "__main__":
    unittest.main()
