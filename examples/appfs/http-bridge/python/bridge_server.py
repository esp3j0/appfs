#!/usr/bin/env python3
import json
import os
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer


def _json_response(handler: BaseHTTPRequestHandler, status: int, body: dict) -> None:
    payload = json.dumps(body).encode("utf-8")
    handler.send_response(status)
    handler.send_header("Content-Type", "application/json")
    handler.send_header("Content-Length", str(len(payload)))
    handler.end_headers()
    handler.wfile.write(payload)


def _env_int(name: str, default: int) -> int:
    raw = os.getenv(name, "").strip()
    if raw == "":
        return default
    try:
        return int(raw)
    except ValueError:
        return default


class FaultInjector:
    def __init__(self) -> None:
        self._lock = threading.Lock()
        self.fail_next_submit_action = max(0, _env_int("APPFS_BRIDGE_FAIL_NEXT_SUBMIT_ACTION", 0))
        self.fail_http_status = _env_int("APPFS_BRIDGE_FAIL_HTTP_STATUS", 503)
        self.fail_path_prefix = os.getenv("APPFS_BRIDGE_FAIL_PATH_PREFIX", "").strip()
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
            self.fail_http_status = int(data.get("fail_http_status", self.fail_http_status))
            self.fail_path_prefix = str(data.get("fail_path_prefix", self.fail_path_prefix)).strip()
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


class BridgeHandler(BaseHTTPRequestHandler):
    def do_POST(self) -> None:
        length = int(self.headers.get("Content-Length", "0"))
        raw = self.rfile.read(length)
        try:
            data = json.loads(raw.decode("utf-8"))
        except json.JSONDecodeError:
            _json_response(
                self,
                400,
                {
                    "kind": "rejected",
                    "code": "INVALID_ARGUMENT",
                    "message": "invalid json body",
                    "retryable": False,
                },
            )
            return

        if self.path == "/v1/submit-action":
            self.handle_submit_action(data)
            return
        if self.path == "/v1/submit-control-action":
            self.handle_submit_control(data)
            return

        _json_response(
            self,
            404,
            {"kind": "internal", "message": f"unknown path: {self.path}"},
        )

    def handle_submit_action(self, data: dict) -> None:
        path = str(data.get("path", ""))
        execution_mode = str(data.get("execution_mode", "inline"))
        payload = str(data.get("payload", ""))
        should_fail, remaining = FAULT_INJECTOR.maybe_fail_submit_action(path)

        if should_fail:
            _json_response(
                self,
                FAULT_INJECTOR.fail_http_status,
                {
                    "kind": "internal",
                    "message": f"fault injected for path={path}, remaining={remaining}",
                },
            )
            return

        if execution_mode == "inline":
            if path.endswith("/send_message.act"):
                _json_response(self, 200, {"kind": "completed", "content": "send success"})
                return
            _json_response(self, 200, {"kind": "completed", "content": "action completed"})
            return

        terminal_content = {"ok": True}
        if path.endswith("/download.act"):
            try:
                parsed = json.loads(payload)
                target = parsed.get("target", "unknown")
            except Exception:
                target = "unknown"
            terminal_content = {"saved_to": target}

        _json_response(
            self,
            200,
            {
                "kind": "streaming",
                "plan": {
                    "accepted_content": "accepted",
                    "progress_content": {"percent": 50},
                    "terminal_content": terminal_content,
                },
            },
        )

    def handle_submit_control(self, data: dict) -> None:
        action = data.get("action", {}) or {}
        kind = str(action.get("kind", ""))

        if kind == "paging_fetch_next":
            handle_id = str(action.get("handle_id", ""))
            page_no = int(action.get("page_no", 1))
            has_more = bool(action.get("has_more", False))
            _json_response(
                self,
                200,
                {
                    "kind": "completed",
                    "content": {
                        "items": [{"id": f"m-{page_no}", "text": "generated by python bridge"}],
                        "page": {
                            "handle_id": handle_id,
                            "page_no": page_no,
                            "has_more": has_more,
                            "mode": "snapshot",
                        },
                    },
                },
            )
            return

        if kind == "paging_close":
            handle_id = str(action.get("handle_id", ""))
            _json_response(
                self,
                200,
                {
                    "kind": "completed",
                    "content": {"closed": True, "handle_id": handle_id},
                },
            )
            return

        _json_response(
            self,
            400,
            {
                "kind": "rejected",
                "code": "NOT_SUPPORTED",
                "message": f"unsupported control action: {kind}",
                "retryable": False,
            },
        )

    def log_message(self, format: str, *args) -> None:
        # Keep output concise for contract-test runs.
        return


def main() -> None:
    server = HTTPServer(("127.0.0.1", 8080), BridgeHandler)
    print("AppFS HTTP bridge listening on http://127.0.0.1:8080")
    print(
        "Fault injector: fail_next_submit_action=%d fail_http_status=%d fail_path_prefix=%r"
        % (
            FAULT_INJECTOR.fail_next_submit_action,
            FAULT_INJECTOR.fail_http_status,
            FAULT_INJECTOR.fail_path_prefix,
        )
    )
    print(f"Fault config path: {FAULT_INJECTOR.config_path}")
    server.serve_forever()


if __name__ == "__main__":
    main()
