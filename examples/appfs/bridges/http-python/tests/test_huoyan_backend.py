from __future__ import annotations

import sys
import tempfile
import unittest
import urllib.error
from pathlib import Path
from typing import Any
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from appfs_http_bridge.huoyan_backend import HuoyanBackend, _json_request


class _FakeHuoyanClient:
    def __init__(self, *, case_locations: dict[int, str]) -> None:
        self.case_locations = case_locations
        self.opened_paths: list[str] = []
        self.exited_case_ids: list[int] = []

    def list_cases(
        self, *, limit: int, offset: int, desc: bool, column: str, keyword: str
    ) -> dict[str, Any]:
        _ = (limit, offset, desc, column, keyword)
        return {
            "cases": [
                {
                    "Id": 7,
                    "CaseNumber": "CASE-7",
                    "Name": "测试入库12",
                    "DisplayName": "测试入库12",
                    "Location": self.case_locations[7],
                    "EvidenceNum": 1,
                    "RecordCount": 52,
                    "Status": 1,
                    "UpdateAt": "2026-03-26T06:54:10Z",
                    "InvestigatorList": ["张虎"],
                },
                {
                    "Id": 8,
                    "CaseNumber": "CASE-8",
                    "Name": "思语",
                    "DisplayName": "思语",
                    "Location": self.case_locations[8],
                    "EvidenceNum": 1,
                    "RecordCount": 23,
                    "Status": 1,
                    "UpdateAt": "2026-03-26T07:00:00Z",
                    "InvestigatorList": ["川"],
                }
            ]
        }

    def get_app_options(self) -> dict[str, Any]:
        return {"storagehost": "http://127.0.0.1:58700"}

    def open_case(self, *, path: str) -> dict[str, Any]:
        self.opened_paths.append(path)
        return {"status": "success"}

    def exit_case(self, *, cid: int) -> dict[str, Any]:
        self.exited_case_ids.append(cid)
        return {"status": "success"}

    def list_evidences(self, *, case_id: int) -> dict[str, Any]:
        self.last_case_id = case_id
        return {"evidences": [{"Id": 1, "Name": "检材A"}]}

    def list_nodes(self, *, analysis_cid: int, pid: int) -> dict[str, Any]:
        _ = analysis_cid
        if pid == 1:
            return {
                "nodes": [
                    {
                        "Nid": 4000000001,
                        "Pid": 1,
                        "Eid": 1,
                        "Name": "微信",
                        "NodeType": "weixin",
                        "SubNodeType": "treenode",
                        "HasChildNode": 1,
                    }
                ]
            }
        if pid == 4000000001:
            return {
                "nodes": [
                    {
                        "Nid": 4000000002,
                        "Pid": 4000000001,
                        "Eid": 1,
                        "Name": "mzs(wxid_ykx86h8vvs7v12)",
                        "NodeType": "user",
                        "SubNodeType": "treenode",
                        "HasChildNode": 1,
                    }
                ]
            }
        if pid == 4000000002:
            return {
                "nodes": [
                    {
                        "Nid": 4000000302,
                        "Pid": 4000000002,
                        "Eid": 1,
                        "Name": "好友消息/张三",
                        "NodeType": "buddymsgobject",
                        "SubNodeType": "immsginfo",
                        "HasChildNode": 0,
                    }
                ]
            }
        return {"nodes": []}

    def fetch_leaf_rows(self, *, params: dict[str, Any]) -> dict[str, Any]:
        self.last_fetch_params = params
        return {
            "count": 2,
            "data": [
                {
                    "Id": 198,
                    "Nid": 4000000323,
                    "Time": "2024-07-08T09:14:21Z",
                    "Content": "文本",
                    "MsgType": "text",
                },
                {
                    "Id": 199,
                    "Nid": 4000000324,
                    "Time": "2024-07-08T09:19:28Z",
                    "Content": "表情😂😃😏😍😌",
                    "MsgType": "text",
                },
            ],
            "table": "evidence_1_immsginfo",
        }


class HuoyanBackendTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tempdir = tempfile.TemporaryDirectory()
        self.case_dir_7 = Path(self.tempdir.name) / "测试入库12"
        self.case_dir_7.mkdir()
        (self.case_dir_7 / "测试入库12.gec").write_bytes(b"case7")
        self.case_dir_8 = Path(self.tempdir.name) / "思语"
        self.case_dir_8.mkdir()
        (self.case_dir_8 / "思语.gec").write_bytes(b"case8")
        self.client = _FakeHuoyanClient(
            case_locations={
                7: str(self.case_dir_7),
                8: str(self.case_dir_8),
            }
        )
        self.backend = HuoyanBackend(client=self.client, open_wait_sec=0, open_on_enter=True)
        self.context = {
            "app_id": "huoyan",
            "session_id": "sess-1",
            "request_id": "req-1",
        }

    def tearDown(self) -> None:
        self.tempdir.cleanup()

    def test_home_structure_contains_case_directory_and_info_snapshot(self) -> None:
        body = self.backend.get_app_structure({"app_id": "huoyan"}, self.context)
        snapshot = body["result"]["snapshot"]
        paths = {node["path"] for node in snapshot["nodes"]}
        self.assertEqual(snapshot["active_scope"], "home")
        self.assertIn("测试入库12", paths)
        self.assertIn("测试入库12/info.res.jsonl", paths)
        self.assertIn("_app/enter_scope.act", paths)

    def test_home_info_snapshot_can_be_read(self) -> None:
        self.backend.get_app_structure({"app_id": "huoyan"}, self.context)
        body = self.backend.fetch_snapshot_chunk(
            {
                "resource_path": "/测试入库12/info.res.jsonl",
                "resume": {"kind": "start"},
                "budget_bytes": 4096,
            },
            self.context,
        )
        self.assertEqual(len(body["records"]), 1)
        self.assertEqual(body["records"][0]["line"]["case_id"], 7)
        self.assertEqual(body["records"][0]["line"]["target_scope"], "case:7")

    def test_enter_scope_builds_case_tree_and_fetches_message_snapshot(self) -> None:
        refreshed = self.backend.refresh_app_structure(
            {
                "app_id": "huoyan",
                "reason": "enter_scope",
                "target_scope": "case:7",
                "trigger_action_path": "/_app/enter_scope.act",
            },
            self.context,
        )
        snapshot = refreshed["result"]["snapshot"]
        paths = {node["path"] for node in snapshot["nodes"]}
        self.assertEqual(snapshot["active_scope"], "case:7")
        self.assertIn(str(self.case_dir_7 / "测试入库12.gec"), self.client.opened_paths)
        target_path = "检材A/微信/mzs(wxid_ykx86h8vvs7v12)/好友消息_张三.res.jsonl"
        self.assertIn(target_path, paths)

        fetched = self.backend.fetch_snapshot_chunk(
            {
                "resource_path": f"/{target_path}",
                "resume": {"kind": "start"},
                "budget_bytes": 4096,
            },
            self.context,
        )
        self.assertEqual(len(fetched["records"]), 2)
        self.assertEqual(fetched["records"][0]["line"]["Content"], "文本")
        self.assertEqual(self.client.last_fetch_params["cid"], 1)
        self.assertEqual(self.client.last_fetch_params["pid"], 4000000302)
        self.assertEqual(self.client.last_fetch_params["datatype"], "immsginfo")

    def test_enter_scope_home_exits_current_case(self) -> None:
        self.backend.refresh_app_structure(
            {
                "app_id": "huoyan",
                "reason": "enter_scope",
                "target_scope": "case:7",
                "trigger_action_path": "/_app/enter_scope.act",
            },
            self.context,
        )
        refreshed = self.backend.refresh_app_structure(
            {
                "app_id": "huoyan",
                "reason": "enter_scope",
                "target_scope": "home",
                "trigger_action_path": "/_app/enter_scope.act",
            },
            self.context,
        )
        snapshot = refreshed["result"]["snapshot"]
        self.assertEqual(snapshot["active_scope"], "home")
        self.assertEqual(self.client.exited_case_ids, [1])

    def test_switching_case_exits_previous_case_before_opening_next(self) -> None:
        self.backend.refresh_app_structure(
            {
                "app_id": "huoyan",
                "reason": "enter_scope",
                "target_scope": "case:7",
                "trigger_action_path": "/_app/enter_scope.act",
            },
            self.context,
        )
        self.backend.refresh_app_structure(
            {
                "app_id": "huoyan",
                "reason": "enter_scope",
                "target_scope": "case:8",
                "trigger_action_path": "/_app/enter_scope.act",
            },
            self.context,
        )
        self.assertEqual(self.client.exited_case_ids, [1])
        self.assertIn(str(self.case_dir_8 / "思语.gec"), self.client.opened_paths)

    def test_json_request_normalizes_windows_network_error_to_ascii(self) -> None:
        reason = ConnectionRefusedError(10061, "由于目标计算机积极拒绝，无法连接。")
        with mock.patch("urllib.request.urlopen", side_effect=urllib.error.URLError(reason)):
            with self.assertRaisesRegex(
                RuntimeError,
                r"network_error url=http://127\.0\.0\.1:8924/api/v1/cases (winerror|errno)=10061",
            ):
                _json_request(
                    "GET",
                    "http://127.0.0.1:8924/api/v1/cases",
                    body=None,
                    timeout_sec=1.0,
                )

    def test_case_open_path_prefers_gec_file_inside_case_directory(self) -> None:
        import tempfile

        with tempfile.TemporaryDirectory() as tmpdir:
            case_dir = Path(tmpdir) / "测试入库12"
            case_dir.mkdir()
            gec_path = case_dir / "测试入库12.gec"
            gec_path.write_bytes(b"test")
            resolved = self.backend._case_open_path(
                {
                    "Location": str(case_dir),
                    "Name": "测试入库12",
                    "DisplayName": "测试入库12",
                    "CaseNumber": "CASE-7",
                }
            )
            self.assertEqual(resolved, str(gec_path))


if __name__ == "__main__":
    unittest.main()
