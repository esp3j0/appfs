from __future__ import annotations

import json
import os
import threading
from dataclasses import dataclass

DEFAULT_CONFIG_PATH = "/tmp/appfs-bridge-fault-config.json"


def _env_int(name: str, default: int) -> int:
    raw = os.getenv(name, "").strip()
    if raw == "":
        return default
    try:
        return int(raw)
    except ValueError:
        return default


@dataclass(frozen=True)
class FaultState:
    fail_next_submit_action: int = 0
    fail_http_status: int = 503
    fail_path_prefix: str = ""


class FaultInjector:
    def __init__(
        self,
        config_path: str | None = None,
        initial_state: FaultState | None = None,
    ) -> None:
        self._lock = threading.Lock()
        self._state = initial_state or self._state_from_env()
        self.config_path = (
            config_path
            if config_path is not None
            else os.getenv("APPFS_BRIDGE_FAULT_CONFIG_PATH", DEFAULT_CONFIG_PATH).strip()
        )
        self._last_config_mtime: float | None = None

    @staticmethod
    def _state_from_env() -> FaultState:
        return FaultState(
            fail_next_submit_action=max(0, _env_int("APPFS_BRIDGE_FAIL_NEXT_SUBMIT_ACTION", 0)),
            fail_http_status=_env_int("APPFS_BRIDGE_FAIL_HTTP_STATUS", 503),
            fail_path_prefix=os.getenv("APPFS_BRIDGE_FAIL_PATH_PREFIX", "").strip(),
        )

    @property
    def fail_http_status(self) -> int:
        with self._lock:
            self._reload_config_from_file()
            return self._state.fail_http_status

    def snapshot(self) -> FaultState:
        with self._lock:
            self._reload_config_from_file()
            return FaultState(
                fail_next_submit_action=self._state.fail_next_submit_action,
                fail_http_status=self._state.fail_http_status,
                fail_path_prefix=self._state.fail_path_prefix,
            )

    def maybe_fail_submit_action(self, path: str) -> tuple[bool, int]:
        with self._lock:
            self._reload_config_from_file()
            if self._state.fail_next_submit_action <= 0:
                return (False, 0)
            if self._state.fail_path_prefix and not path.startswith(self._state.fail_path_prefix):
                return (False, self._state.fail_next_submit_action)
            remaining = self._state.fail_next_submit_action - 1
            self._state = FaultState(
                fail_next_submit_action=remaining,
                fail_http_status=self._state.fail_http_status,
                fail_path_prefix=self._state.fail_path_prefix,
            )
            return (True, remaining)

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
            with open(self.config_path, "r", encoding="utf-8") as handle:
                data = json.load(handle)
        except Exception:
            return

        try:
            fail_next = max(
                0,
                int(data.get("fail_next_submit_action", self._state.fail_next_submit_action)),
            )
            fail_http_status = int(data.get("fail_http_status", self._state.fail_http_status))
            fail_path_prefix = str(
                data.get("fail_path_prefix", self._state.fail_path_prefix)
            ).strip()
        except Exception:
            return

        self._state = FaultState(
            fail_next_submit_action=fail_next,
            fail_http_status=fail_http_status,
            fail_path_prefix=fail_path_prefix,
        )
        self._last_config_mtime = mtime
