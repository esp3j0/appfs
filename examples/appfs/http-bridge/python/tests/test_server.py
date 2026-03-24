from __future__ import annotations

import http.client
import json
import sys
import threading
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from appfs_http_bridge.server import create_http_server


class ServerTests(unittest.TestCase):
    def setUp(self) -> None:
        self.server = create_http_server("127.0.0.1", 0)
        self.port = self.server.server_address[1]
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def tearDown(self) -> None:
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)

    def test_invalid_json_rejected(self) -> None:
        status, body = self._post("/v1/submit-action", "{")
        self.assertEqual(status, 400)
        self.assertEqual(body["kind"], "rejected")
        self.assertEqual(body["code"], "INVALID_ARGUMENT")

    def test_unknown_route_returns_internal_404(self) -> None:
        status, body = self._post("/v1/unknown", "{}")
        self.assertEqual(status, 404)
        self.assertEqual(body["kind"], "internal")

    def test_submit_action_route_success(self) -> None:
        status, body = self._post(
            "/v1/submit-action",
            json.dumps(
                {
                    "path": "/contacts/zhangsan/send_message.act",
                    "execution_mode": "inline",
                    "input_mode": "json",
                    "payload": '{"text":"hello"}',
                }
            ),
        )
        self.assertEqual(status, 200)
        self.assertEqual(body["kind"], "completed")

    def test_v2_connector_info_success(self) -> None:
        status, body = self._post("/v2/connector/info", "{}")
        self.assertEqual(status, 200)
        self.assertEqual(body["transport"], "http_bridge")

    def test_v2_submit_action_success(self) -> None:
        status, body = self._post(
            "/v2/connector/action/submit",
            json.dumps(
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
                }
            ),
        )
        self.assertEqual(status, 200)
        self.assertEqual(body["outcome"]["kind"], "completed")

    def _post(self, path: str, payload: str) -> tuple[int, dict[str, object]]:
        conn = http.client.HTTPConnection("127.0.0.1", self.port, timeout=5)
        conn.request(
            "POST",
            path,
            body=payload.encode("utf-8"),
            headers={"Content-Type": "application/json"},
        )
        resp = conn.getresponse()
        raw = resp.read()
        conn.close()
        return resp.status, json.loads(raw.decode("utf-8"))


if __name__ == "__main__":
    unittest.main()
