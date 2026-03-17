from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from appfs_http_bridge.jsonplaceholder_backend import JsonPlaceholderBackend


class JsonPlaceholderBackendTests(unittest.TestCase):
    def test_inline_send_message_uses_upstream_post(self) -> None:
        backend = JsonPlaceholderBackend()
        backend._post_json = lambda _path, _body: {"id": 123}  # type: ignore[method-assign]

        out = backend.submit_action("/contacts/zhangsan/send_message.act", "inline", "hello")
        self.assertEqual(out["kind"], "completed")
        content = out["content"]
        self.assertEqual(content["provider"], "jsonplaceholder")
        self.assertEqual(content["post_id"], 123)
        self.assertEqual(content["contact_id"], "zhangsan")

    def test_streaming_download_uses_upstream_get(self) -> None:
        backend = JsonPlaceholderBackend()
        backend._get_json = lambda _path: {"id": 1, "title": "hello"}  # type: ignore[method-assign]

        out = backend.submit_action(
            "/files/file-001/download.act",
            "streaming",
            '{"target":"/tmp/file.bin"}',
        )
        self.assertEqual(out["kind"], "streaming")
        terminal = out["plan"]["terminal_content"]
        self.assertEqual(terminal["provider"], "jsonplaceholder")
        self.assertEqual(terminal["saved_to"], "/tmp/file.bin")
        self.assertEqual(terminal["source_id"], 1)


if __name__ == "__main__":
    unittest.main()
