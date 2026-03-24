from __future__ import annotations

import json
import os
from http.server import BaseHTTPRequestHandler, HTTPServer
from typing import Any

from .errors import internal_error, rejected_error
from .fault_injector import FaultInjector
from .jsonplaceholder_backend import JsonPlaceholderBackend
from .mock_aiim import MockAiimBackend
from .protocol import (
    connector_error,
    dispatch_submit_action,
    dispatch_submit_control,
    dispatch_v2_connector_info,
    dispatch_v2_health,
    dispatch_v2_live_fetch_page,
    dispatch_v2_snapshot_fetch_chunk,
    dispatch_v2_snapshot_prewarm,
    dispatch_v2_submit_action,
)


def _json_response(handler: BaseHTTPRequestHandler, status: int, body: dict[str, Any]) -> None:
    encoded = json.dumps(body).encode("utf-8")
    handler.send_response(status)
    handler.send_header("Content-Type", "application/json")
    handler.send_header("Content-Length", str(len(encoded)))
    handler.end_headers()
    handler.wfile.write(encoded)


class BridgeApplication:
    def __init__(
        self,
        *,
        backend: object | None = None,
        fault_injector: FaultInjector | None = None,
    ) -> None:
        self.backend = backend or MockAiimBackend()
        self.fault_injector = fault_injector or FaultInjector()
        required_v2_methods = (
            "connector_info",
            "health",
            "prewarm_snapshot_meta",
            "fetch_snapshot_chunk",
            "fetch_live_page",
            "submit_action_v2",
        )
        missing = [name for name in required_v2_methods if not hasattr(self.backend, name)]
        if missing:
            raise ValueError(
                f"backend does not implement required v2 connector methods: {', '.join(missing)}"
            )

    def dispatch(self, route: str, payload: dict[str, Any]) -> tuple[int, dict[str, Any]]:
        if route == "/v2/connector/info":
            return dispatch_v2_connector_info(self.backend)
        if route == "/v2/connector/health":
            return dispatch_v2_health(payload, self.backend)
        if route == "/v2/connector/snapshot/prewarm":
            return dispatch_v2_snapshot_prewarm(payload, self.backend)
        if route == "/v2/connector/snapshot/fetch-chunk":
            return dispatch_v2_snapshot_fetch_chunk(
                payload,
                fault_injector=self.fault_injector,
                backend=self.backend,
            )
        if route == "/v2/connector/live/fetch-page":
            return dispatch_v2_live_fetch_page(payload, self.backend)
        if route == "/v2/connector/action/submit":
            return dispatch_v2_submit_action(
                payload,
                fault_injector=self.fault_injector,
                backend=self.backend,
            )

        if route == "/v1/submit-action":
            return dispatch_submit_action(
                payload,
                fault_injector=self.fault_injector,
                backend=self.backend,
            )
        if route == "/v1/submit-control-action":
            return dispatch_submit_control(payload, backend=self.backend)
        if route.startswith("/v2/connector/"):
            return (
                404,
                connector_error("NOT_SUPPORTED", f"unknown v2 connector path: {route}", False),
            )
        return (404, internal_error(f"unknown path: {route}"))


class _BridgeHandler(BaseHTTPRequestHandler):
    application: BridgeApplication

    def do_POST(self) -> None:
        is_v2 = self.path.startswith("/v2/connector/")
        raw_len = self.headers.get("Content-Length", "0")
        try:
            payload_len = int(raw_len)
        except ValueError:
            _json_response(
                self,
                400,
                connector_error("INVALID_ARGUMENT", "invalid content-length header", False)
                if is_v2
                else rejected_error("INVALID_ARGUMENT", "invalid content-length header"),
            )
            return

        raw_body = self.rfile.read(payload_len)
        try:
            payload = json.loads(raw_body.decode("utf-8"))
        except json.JSONDecodeError:
            _json_response(
                self,
                400,
                connector_error("INVALID_ARGUMENT", "invalid json body", False)
                if is_v2
                else rejected_error("INVALID_ARGUMENT", "invalid json body"),
            )
            return

        if not isinstance(payload, dict):
            _json_response(
                self,
                400,
                connector_error("INVALID_ARGUMENT", "json body must be an object", False)
                if is_v2
                else rejected_error("INVALID_ARGUMENT", "json body must be an object"),
            )
            return

        status, body = self.application.dispatch(self.path, payload)
        _json_response(self, status, body)

    def log_message(self, format: str, *args: object) -> None:
        # Keep output concise for contract-test runs.
        return


def create_http_server(
    host: str,
    port: int,
    *,
    application: BridgeApplication | None = None,
) -> HTTPServer:
    app = application or BridgeApplication()

    class BridgeHandler(_BridgeHandler):
        pass

    class ReusableHTTPServer(HTTPServer):
        allow_reuse_address = True

    BridgeHandler.application = app
    return ReusableHTTPServer((host, port), BridgeHandler)


def run_server() -> None:
    host = os.getenv("APPFS_BRIDGE_HOST", "127.0.0.1")
    port = int(os.getenv("APPFS_BRIDGE_PORT", "8080"))
    backend_mode = os.getenv("APPFS_HTTP_BRIDGE_BACKEND", "mock_aiim").strip().lower()

    if backend_mode in ("mock", "mock_aiim", "aiim"):
        backend = MockAiimBackend()
    elif backend_mode in ("jsonplaceholder", "real_jsonplaceholder"):
        raise ValueError(
            "APPFS_HTTP_BRIDGE_BACKEND=jsonplaceholder is v1-only and not supported for v0.3 HTTP connector v2 mode"
        )
    else:
        raise ValueError(
            "unsupported APPFS_HTTP_BRIDGE_BACKEND=%r (expected: mock_aiim|jsonplaceholder)"
            % backend_mode
        )

    app = BridgeApplication(backend=backend)
    server = create_http_server(host, port, application=app)
    snapshot = app.fault_injector.snapshot()

    print(f"AppFS HTTP bridge listening on http://{host}:{port}")
    print(f"Bridge backend mode: {backend_mode}")
    print(
        "Fault injector: fail_next_submit_action=%d fail_http_status=%d fail_path_prefix=%r"
        % (
            snapshot.fail_next_submit_action,
            snapshot.fail_http_status,
            snapshot.fail_path_prefix,
        )
    )
    print(f"Fault config path: {app.fault_injector.config_path}")
    server.serve_forever()
