from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from appfs_http_bridge.fault_injector import FaultInjector, FaultState


class FaultInjectorTests(unittest.TestCase):
    def test_prefix_filter_and_counter(self) -> None:
        injector = FaultInjector(
            config_path="",
            initial_state=FaultState(
                fail_next_submit_action=2,
                fail_http_status=503,
                fail_path_prefix="/contacts/resilience-",
            ),
        )

        should_fail, remaining = injector.maybe_fail_submit_action("/contacts/zhangsan/send_message.act")
        self.assertFalse(should_fail)
        self.assertEqual(remaining, 2)

        should_fail, remaining = injector.maybe_fail_submit_action(
            "/contacts/resilience-1/send_message.act"
        )
        self.assertTrue(should_fail)
        self.assertEqual(remaining, 1)

        should_fail, remaining = injector.maybe_fail_submit_action(
            "/contacts/resilience-2/send_message.act"
        )
        self.assertTrue(should_fail)
        self.assertEqual(remaining, 0)

        should_fail, remaining = injector.maybe_fail_submit_action(
            "/contacts/resilience-3/send_message.act"
        )
        self.assertFalse(should_fail)
        self.assertEqual(remaining, 0)

    def test_config_file_reload(self) -> None:
        with tempfile.TemporaryDirectory() as tmp_dir:
            config_path = Path(tmp_dir) / "fault.json"
            config_path.write_text(
                json.dumps(
                    {
                        "fail_next_submit_action": 1,
                        "fail_http_status": 504,
                        "fail_path_prefix": "/files/",
                    }
                ),
                encoding="utf-8",
            )

            injector = FaultInjector(
                config_path=str(config_path),
                initial_state=FaultState(
                    fail_next_submit_action=0,
                    fail_http_status=503,
                    fail_path_prefix="",
                ),
            )

            should_fail, remaining = injector.maybe_fail_submit_action("/files/file-001/download.act")
            self.assertTrue(should_fail)
            self.assertEqual(remaining, 0)
            self.assertEqual(injector.fail_http_status, 504)


if __name__ == "__main__":
    unittest.main()
