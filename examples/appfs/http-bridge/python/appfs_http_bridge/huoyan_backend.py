from __future__ import annotations

import hashlib
import json
import os
import time
from pathlib import Path
import urllib.parse
import urllib.error
import urllib.request
from dataclasses import dataclass, field
from datetime import datetime, timezone
from typing import Any, Protocol


ROOT_NODE_ID = 1
DEFAULT_CASES_LIMIT = 100
DEFAULT_HOME_SCOPE = "home"


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def _compact_json(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"))


def _env_bool(name: str, default: bool) -> bool:
    raw = os.getenv(name, "").strip().lower()
    if raw == "":
        return default
    return raw in ("1", "true", "yes", "on")


def _env_int(name: str, default: int) -> int:
    raw = os.getenv(name, "").strip()
    if raw == "":
        return default
    try:
        return int(raw)
    except ValueError:
        return default


def _env_float(name: str, default: float) -> float:
    raw = os.getenv(name, "").strip()
    if raw == "":
        return default
    try:
        return float(raw)
    except ValueError:
        return default


def _safe_segment(name: str, fallback: str) -> str:
    raw = (name or "").strip()
    if raw == "":
        raw = fallback
    out_chars: list[str] = []
    for ch in raw:
        if ch in '\\/:*?"<>|\x00':
            out_chars.append("_")
        elif ord(ch) < 32:
            out_chars.append("_")
        else:
            out_chars.append(ch)
    out = "".join(out_chars).strip().rstrip(".")
    return out or fallback


def _safe_leaf_name(name: str, fallback: str) -> str:
    base = _safe_segment(name.replace("/", "_"), fallback)
    return f"{base}.res.jsonl"


def _dedupe_name(name: str, seen: dict[str, int], suffix_seed: str) -> str:
    count = seen.get(name, 0)
    seen[name] = count + 1
    if count == 0:
        return name
    return f"{name}__{suffix_seed}"


def _network_error_message(url: str, err: BaseException) -> str:
    if isinstance(err, urllib.error.HTTPError):
        return f"http_error url={url} status={err.code}"
    if isinstance(err, urllib.error.URLError):
        reason = err.reason
        if isinstance(reason, OSError):
            winerror = getattr(reason, "winerror", None)
            errno = getattr(reason, "errno", None)
            if isinstance(winerror, int):
                return f"network_error url={url} winerror={winerror}"
            if isinstance(errno, int):
                return f"network_error url={url} errno={errno}"
            return f"network_error url={url} kind={reason.__class__.__name__}"
        return f"network_error url={url} kind={reason.__class__.__name__}"
    if isinstance(err, OSError):
        winerror = getattr(err, "winerror", None)
        errno = getattr(err, "errno", None)
        if isinstance(winerror, int):
            return f"os_error url={url} winerror={winerror}"
        if isinstance(errno, int):
            return f"os_error url={url} errno={errno}"
    return f"{err.__class__.__name__} url={url}"


def _json_request(method: str, url: str, *, body: dict[str, Any] | None, timeout_sec: float) -> dict[str, Any]:
    data = None
    headers: dict[str, str] = {}
    if body is not None:
        data = json.dumps(body, ensure_ascii=False).encode("utf-8")
        headers["Content-Type"] = "application/json; charset=UTF-8"
    request = urllib.request.Request(url, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(request, timeout=timeout_sec) as response:
            raw = response.read().decode("utf-8")
    except Exception as err:
        raise RuntimeError(_network_error_message(url, err)) from err
    parsed = json.loads(raw)
    if not isinstance(parsed, dict):
        raise RuntimeError(f"unexpected response shape from {url}")
    return parsed


class HuoyanApiClientProtocol(Protocol):
    def list_cases(
        self, *, limit: int, offset: int, desc: bool, column: str, keyword: str
    ) -> dict[str, Any]:
        ...

    def get_app_options(self) -> dict[str, Any]:
        ...

    def open_case(self, *, path: str) -> dict[str, Any]:
        ...

    def exit_case(self, *, cid: int) -> dict[str, Any]:
        ...

    def list_evidences(self, *, case_id: int) -> dict[str, Any]:
        ...

    def list_nodes(self, *, analysis_cid: int, pid: int) -> dict[str, Any]:
        ...

    def fetch_leaf_rows(self, *, params: dict[str, Any]) -> dict[str, Any]:
        ...


@dataclass
class HuoyanClient(HuoyanApiClientProtocol):
    host: str = field(default_factory=lambda: os.getenv("APPFS_HUOYAN_HOST", "http://127.0.0.1:8924").rstrip("/"))
    getoken: str = field(default_factory=lambda: os.getenv("APPFS_HUOYAN_GETOKEN", ""))
    storage_host: str | None = field(default_factory=lambda: os.getenv("APPFS_HUOYAN_STORAGE_HOST", "").strip() or None)
    timeout_sec: float = field(default_factory=lambda: _env_float("APPFS_HUOYAN_TIMEOUT_SEC", 10.0))

    def _params_with_token(self, params: dict[str, Any]) -> dict[str, Any]:
        out = dict(params)
        if self.getoken.strip() != "":
            out["getoken"] = self.getoken.strip()
        return out

    def _build_url(self, base: str, path: str, params: dict[str, Any] | None = None) -> str:
        query = urllib.parse.urlencode(self._params_with_token(params or {}), doseq=True)
        if query:
            return f"{base.rstrip('/')}{path}?{query}"
        return f"{base.rstrip('/')}{path}"

    def _ensure_storage_host(self) -> str:
        if self.storage_host is not None:
            return self.storage_host.rstrip("/")
        options = self.get_app_options()
        storage_host = str(options.get("storagehost", "")).strip()
        if storage_host == "":
            raise RuntimeError("fireeye app options did not include storagehost")
        self.storage_host = storage_host.rstrip("/")
        return self.storage_host

    def list_cases(
        self, *, limit: int, offset: int, desc: bool, column: str, keyword: str
    ) -> dict[str, Any]:
        url = self._build_url(
            self.host,
            "/api/v1/cases",
            {
                "limit": limit,
                "offset": offset,
                "desc": str(desc).lower(),
                "column": column,
                "keyword": keyword,
            },
        )
        return _json_request("GET", url, body=None, timeout_sec=self.timeout_sec)

    def get_app_options(self) -> dict[str, Any]:
        url = self._build_url(self.host, "/internal/v1/app/options")
        return _json_request("GET", url, body=None, timeout_sec=self.timeout_sec)

    def open_case(self, *, path: str) -> dict[str, Any]:
        url = self._build_url(self.host, "/api/v1/case/open")
        body: dict[str, Any] = {"path": path}
        if self.getoken.strip() != "":
            body["getoken"] = self.getoken.strip()
        return _json_request("POST", url, body=body, timeout_sec=self.timeout_sec)

    def exit_case(self, *, cid: int) -> dict[str, Any]:
        url = self._build_url(self.host, "/api/v1/case/exit")
        body: dict[str, Any] = {"cid": cid}
        if self.getoken.strip() != "":
            body["getoken"] = self.getoken.strip()
        return _json_request("POST", url, body=body, timeout_sec=self.timeout_sec)

    def list_evidences(self, *, case_id: int) -> dict[str, Any]:
        storage_host = self._ensure_storage_host()
        url = self._build_url(storage_host, "/internal/v1/evidence/cid", {"cid": case_id})
        return _json_request("GET", url, body=None, timeout_sec=self.timeout_sec)

    def list_nodes(self, *, analysis_cid: int, pid: int) -> dict[str, Any]:
        url = self._build_url(self.host, "/api/v1/data/node", {"cid": analysis_cid, "pid": pid})
        return _json_request("GET", url, body=None, timeout_sec=self.timeout_sec)

    def fetch_leaf_rows(self, *, params: dict[str, Any]) -> dict[str, Any]:
        storage_host = self._ensure_storage_host()
        internal_url = self._build_url(storage_host, "/internal/v1/data", params)
        try:
            return _json_request("GET", internal_url, body=None, timeout_sec=self.timeout_sec)
        except Exception:
            public_url = self._build_url(self.host, "/api/v1/data", params)
            return _json_request("GET", public_url, body=None, timeout_sec=self.timeout_sec)


@dataclass
class _LeafSpec:
    case_id: int
    analysis_cid: int
    eid: int
    pid: int
    datatype: str
    category: str
    name: str


@dataclass
class _BuiltScope:
    snapshot: dict[str, Any]
    leaf_specs: dict[str, _LeafSpec] = field(default_factory=dict)
    info_specs: dict[str, dict[str, Any]] = field(default_factory=dict)


@dataclass
class HuoyanBackend:
    client: HuoyanApiClientProtocol | None = None
    app_id: str = field(default_factory=lambda: os.getenv("APPFS_HUOYAN_APP_ID", "huoyan"))
    user_id: int = field(default_factory=lambda: _env_int("APPFS_HUOYAN_USER_ID", 1))
    case_mode: str = field(default_factory=lambda: os.getenv("APPFS_HUOYAN_CASE_MODE", "singlebox").strip().lower() or "singlebox")
    default_case_id: int | None = field(
        default_factory=lambda: (_env_int("APPFS_HUOYAN_CASE_ID", 0) or None)
    )
    default_scope: str = field(default_factory=lambda: os.getenv("APPFS_HUOYAN_DEFAULT_SCOPE", "home").strip().lower() or "home")
    open_on_enter: bool = field(default_factory=lambda: _env_bool("APPFS_HUOYAN_OPEN_ON_ENTER", True))
    open_wait_sec: float = field(default_factory=lambda: _env_float("APPFS_HUOYAN_OPEN_WAIT_SEC", 0.5))
    blocked_names: set[str] = field(
        default_factory=lambda: {
            item.strip()
            for item in os.getenv("APPFS_HUOYAN_BLOCKED_NAMES", "").split(",")
            if item.strip()
        }
    )
    session_scopes: dict[str, str] = field(default_factory=dict)
    scope_cache: dict[str, _BuiltScope] = field(default_factory=dict)

    def __post_init__(self) -> None:
        if self.client is None:
            self.client = HuoyanClient()

    def _snapshot_manifest(self, template: str) -> dict[str, Any]:
        return {
            "template": template,
            "kind": "resource",
            "output_mode": "jsonl",
            "snapshot": {
                "max_materialized_bytes": 20 * 1024 * 1024,
                "prewarm": False,
                "prewarm_timeout_ms": 5000,
                "read_through_timeout_ms": 10000,
                "on_timeout": "return_stale",
            },
        }

    def _action_manifest(self, template: str) -> dict[str, Any]:
        return {
            "template": template,
            "kind": "action",
            "input_mode": "json",
            "execution_mode": "inline",
        }

    def _initial_scope(self) -> str:
        if self.default_scope == "case" and self.default_case_id is not None:
            return self._case_scope(self.default_case_id)
        return DEFAULT_HOME_SCOPE

    def _case_scope(self, case_id: int) -> str:
        return f"case:{case_id}"

    def _parse_case_scope(self, scope: str) -> int:
        if not scope.startswith("case:"):
            raise ValueError(f"unknown structure scope: {scope}")
        try:
            case_id = int(scope.split(":", 1)[1])
        except ValueError as err:
            raise ValueError(f"invalid case scope: {scope}") from err
        if case_id <= 0:
            raise ValueError(f"invalid case scope: {scope}")
        return case_id

    def _analysis_cid(self, case_id: int) -> int:
        return 1 if self.case_mode == "singlebox" else case_id

    def connector_info(self) -> dict[str, Any]:
        return {
            "connector_id": "huoyan-http-v1",
            "version": "0.1.0",
            "app_id": self.app_id,
            "transport": "http_bridge",
            "supports_snapshot": True,
            "supports_live": False,
            "supports_action": False,
            "optional_features": ["structure_sync", "case_scope", "fireeye_tree"],
        }

    def health(self, context: dict[str, Any]) -> dict[str, Any]:
        _ = context
        options = self.client.get_app_options()
        return {
            "healthy": True,
            "auth_status": "valid",
            "message": "huoyan backend reachable",
            "checked_at": _now_iso(),
            "storage_host": options.get("storagehost", ""),
        }

    def get_app_structure(self, request: dict[str, Any], context: dict[str, Any]) -> dict[str, Any]:
        session_id = str(context.get("session_id", ""))
        scope = self.session_scopes.get(session_id, self._initial_scope())
        built = self._build_scope(scope, force_refresh=True)
        self.session_scopes[session_id] = scope
        return self._structure_response(request, built.snapshot)

    def refresh_app_structure(self, request: dict[str, Any], context: dict[str, Any]) -> dict[str, Any]:
        session_id = str(context.get("session_id", ""))
        previous_scope = self.session_scopes.get(session_id, self._initial_scope())
        reason = str(request.get("reason", ""))
        if reason == "enter_scope":
            target_scope = request.get("target_scope")
            if not isinstance(target_scope, str) or target_scope.strip() == "":
                raise ValueError("target_scope is required for enter_scope refresh")
            scope = target_scope.strip()
        else:
            scope = previous_scope

        self._transition_scope(previous_scope, scope, reason)
        built = self._build_scope(scope, force_refresh=True)
        self.session_scopes[session_id] = scope
        return self._structure_response(request, built.snapshot)

    def prewarm_snapshot_meta(self, request: dict[str, Any], context: dict[str, Any]) -> dict[str, Any]:
        resource_path = str(request.get("resource_path", ""))
        spec = self._resolve_snapshot_spec(resource_path, context)
        if isinstance(spec, dict):
            line = _compact_json(spec) + "\n"
            return {
                "size_bytes": len(line.encode("utf-8")),
                "revision": self._build_scope(DEFAULT_HOME_SCOPE, force_refresh=False).snapshot["revision"],
                "last_modified": _now_iso(),
                "item_count": 1,
            }

        rows = self._fetch_rows(spec, skip=0, limit=1)
        data = rows.get("data", [])
        first_len = 0
        if isinstance(data, list) and data:
            first_len = len(_compact_json(data[0]).encode("utf-8")) + 1
        return {
            "size_bytes": first_len,
            "revision": self._build_scope(self._case_scope(spec.case_id), force_refresh=False).snapshot["revision"],
            "last_modified": _now_iso(),
            "item_count": int(rows.get("count", 0)),
        }

    def fetch_snapshot_chunk(self, request: dict[str, Any], context: dict[str, Any]) -> dict[str, Any]:
        resource_path = str(request.get("resource_path", ""))
        budget_bytes = int(request.get("budget_bytes", 0))
        resume = request.get("resume", {})
        spec = self._resolve_snapshot_spec(resource_path, context)

        if isinstance(spec, dict):
            return self._fetch_case_info_chunk(spec, budget_bytes=budget_bytes, resume=resume)

        skip = self._resume_to_offset(resume)
        api_limit = max(1, min(100, budget_bytes // 256 if budget_bytes > 0 else 100))
        rows = self._fetch_rows(spec, skip=skip, limit=api_limit)
        raw_items = rows.get("data", [])
        if not isinstance(raw_items, list):
            raise ValueError("leaf data response must contain list data")

        emitted_bytes = 0
        out_records: list[dict[str, Any]] = []
        current_offset = skip
        for item in raw_items:
            line = item if isinstance(item, dict) else {"value": item}
            encoded_len = len(_compact_json(line).encode("utf-8")) + 1
            if out_records and emitted_bytes + encoded_len > budget_bytes:
                break
            record_id = line.get("Id") or line.get("Nid") or current_offset
            ordering_key = f"{line.get('Time', '')}:{record_id}"
            out_records.append(
                {
                    "record_key": f"row-{record_id}",
                    "ordering_key": ordering_key,
                    "line": line,
                }
            )
            emitted_bytes += encoded_len
            current_offset += 1

        total_count = int(rows.get("count", len(out_records)))
        has_more = current_offset < total_count
        return {
            "records": out_records,
            "emitted_bytes": emitted_bytes,
            "next_cursor": f"offset:{current_offset}" if has_more else None,
            "has_more": has_more,
            "revision": self._build_scope(self._case_scope(spec.case_id), force_refresh=False).snapshot["revision"],
        }

    def fetch_live_page(self, request: dict[str, Any], context: dict[str, Any]) -> dict[str, Any]:
        _ = (request, context)
        return {
            "items": [],
            "page": {
                "handle_id": "huoyan-no-live",
                "page_no": 1,
                "has_more": False,
                "mode": "live",
                "expires_at": _now_iso(),
                "next_cursor": None,
                "retry_after_ms": None,
            },
        }

    def submit_action_v2(self, request: dict[str, Any], context: dict[str, Any]) -> dict[str, Any]:
        _ = (request, context)
        raise ValueError("huoyan backend does not expose custom action files yet")

    def _open_case(self, case_id: int) -> None:
        cases = self._list_cases()
        target = next((case for case in cases if int(case.get("Id", 0)) == case_id), None)
        if target is None:
            raise ValueError(f"unknown case id: {case_id}")
        path = self._case_open_path(target)
        if path == "":
            raise ValueError(f"case {case_id} is missing openable path")
        self.client.open_case(path=path)
        if self.open_wait_sec > 0:
            time.sleep(self.open_wait_sec)

    def _exit_case(self, case_id: int) -> None:
        self.client.exit_case(cid=self._analysis_cid(case_id))
        if self.open_wait_sec > 0:
            time.sleep(self.open_wait_sec)

    def _transition_scope(self, previous_scope: str, next_scope: str, reason: str) -> None:
        if previous_scope == next_scope:
            return

        if previous_scope != DEFAULT_HOME_SCOPE:
            previous_case_id = self._parse_case_scope(previous_scope)
            self._exit_case(previous_case_id)

        if next_scope != DEFAULT_HOME_SCOPE and reason == "enter_scope" and self.open_on_enter:
            next_case_id = self._parse_case_scope(next_scope)
            self._open_case(next_case_id)

    def _case_open_path(self, case: dict[str, Any]) -> str:
        location = str(case.get("Location", "")).strip()
        if location == "":
            return ""

        location_path = Path(location)
        if location_path.is_file() and location_path.suffix.lower() == ".gec":
            return str(location_path)

        candidate_names = [
            str(case.get("DisplayName", "")).strip(),
            str(case.get("Name", "")).strip(),
            location_path.name.strip(),
            str(case.get("CaseNumber", "")).strip(),
        ]

        if location_path.is_dir():
            gec_files = sorted(
                child for child in location_path.iterdir() if child.is_file() and child.suffix.lower() == ".gec"
            )
            if gec_files:
                return str(gec_files[0])

            for name in candidate_names:
                if name == "":
                    continue
                candidate = location_path / f"{name}.gec"
                if candidate.exists():
                    return str(candidate)

        return location

    def _list_cases(self) -> list[dict[str, Any]]:
        response = self.client.list_cases(
            limit=DEFAULT_CASES_LIMIT,
            offset=0,
            desc=True,
            column="update_at",
            keyword="",
        )
        cases = response.get("cases", [])
        if not isinstance(cases, list):
            raise ValueError("case list response must contain cases list")
        if cases:
            return [case for case in cases if isinstance(case, dict)]
        if self.default_case_id is not None:
            return [
                {
                    "Id": self.default_case_id,
                    "DisplayName": f"案件-{self.default_case_id}",
                    "Name": f"案件-{self.default_case_id}",
                    "Location": "",
                    "EvidenceNum": 0,
                    "RecordCount": 0,
                    "UpdateAt": _now_iso(),
                }
            ]
        return []

    def _build_scope(self, scope: str, *, force_refresh: bool) -> _BuiltScope:
        if not force_refresh and scope in self.scope_cache:
            return self.scope_cache[scope]
        if scope == DEFAULT_HOME_SCOPE:
            built = self._build_home_scope()
        else:
            built = self._build_case_scope(self._parse_case_scope(scope))
        self.scope_cache[scope] = built
        return built

    def _build_home_scope(self) -> _BuiltScope:
        cases = self._list_cases()
        nodes: list[dict[str, Any]] = self._control_nodes()
        info_specs: dict[str, dict[str, Any]] = {}
        top_level_seen: dict[str, int] = {}
        ownership_prefixes = ["_app"]

        for case in cases:
            case_id = int(case.get("Id", 0))
            display_name = str(case.get("DisplayName") or case.get("Name") or f"案件-{case_id}")
            case_dir = _dedupe_name(_safe_segment(display_name, f"案件-{case_id}"), top_level_seen, f"case_{case_id}")
            ownership_prefixes.append(case_dir)
            nodes.append(
                {
                    "path": case_dir,
                    "kind": "directory",
                    "manifest_entry": None,
                    "seed_content": None,
                    "mutable": False,
                    "scope": DEFAULT_HOME_SCOPE,
                }
            )
            info_path = f"{case_dir}/info.res.jsonl"
            case_info = {
                "case_id": case_id,
                "case_number": case.get("CaseNumber", ""),
                "name": case.get("Name", ""),
                "display_name": display_name,
                "location": case.get("Location", ""),
                "investigators": case.get("InvestigatorList", []),
                "evidence_num": case.get("EvidenceNum", 0),
                "record_count": case.get("RecordCount", 0),
                "status": case.get("Status", 0),
                "updated_at": case.get("UpdateAt", ""),
                "target_scope": self._case_scope(case_id),
            }
            info_specs[f"/{info_path}"] = case_info
            nodes.append(
                {
                    "path": info_path,
                    "kind": "snapshot_resource",
                    "manifest_entry": self._snapshot_manifest("{case_name}/info.res.jsonl"),
                    "seed_content": None,
                    "mutable": False,
                    "scope": DEFAULT_HOME_SCOPE,
                }
            )

        revision = self._revision_from_payload(
            "home",
            [{"id": case.get("Id"), "updated_at": case.get("UpdateAt", "")} for case in cases],
        )
        snapshot = {
            "app_id": self.app_id,
            "revision": revision,
            "active_scope": DEFAULT_HOME_SCOPE,
            "ownership_prefixes": ownership_prefixes,
            "nodes": nodes,
        }
        return _BuiltScope(snapshot=snapshot, info_specs=info_specs)

    def _build_case_scope(self, case_id: int) -> _BuiltScope:
        analysis_cid = self._analysis_cid(case_id)
        raw_evidences = self.client.list_evidences(case_id=case_id).get("evidences", [])
        evidences: dict[int, dict[str, Any]] = {}
        if isinstance(raw_evidences, list):
            for evidence in raw_evidences:
                if isinstance(evidence, dict):
                    evidences[int(evidence.get("Id", 0))] = evidence

        nodes_by_pid: dict[int, list[dict[str, Any]]] = {}
        queue = [ROOT_NODE_ID]
        visited: set[int] = set()
        while queue:
            pid = queue.pop(0)
            if pid in visited:
                continue
            visited.add(pid)
            response = self.client.list_nodes(analysis_cid=analysis_cid, pid=pid)
            children = response.get("nodes", [])
            if not isinstance(children, list):
                children = []
            normalized = [child for child in children if isinstance(child, dict)]
            nodes_by_pid[pid] = normalized
            for child in normalized:
                if self._has_children(child):
                    queue.append(int(child.get("Nid", 0)))

        for child in nodes_by_pid.get(ROOT_NODE_ID, []):
            eid = int(child.get("Eid", 0))
            if eid > 0 and eid not in evidences:
                evidences[eid] = {
                    "Id": eid,
                    "Name": f"检材-{eid}",
                }

        structure_nodes = self._control_nodes(scope=self._case_scope(case_id))
        leaf_specs: dict[str, _LeafSpec] = {}
        top_level_seen: dict[str, int] = {}
        evidence_groups: dict[int, list[dict[str, Any]]] = {}
        for child in nodes_by_pid.get(ROOT_NODE_ID, []):
            evidence_groups.setdefault(int(child.get("Eid", 0)), []).append(child)

        ownership_prefixes = ["_app"]
        revision_payload: list[dict[str, Any]] = []

        for eid, top_nodes in sorted(evidence_groups.items(), key=lambda item: item[0]):
            evidence_name = str(evidences.get(eid, {}).get("Name") or f"检材-{eid}")
            evidence_dir = _dedupe_name(
                _safe_segment(evidence_name, f"检材-{eid}"),
                top_level_seen,
                f"eid_{eid}",
            )
            ownership_prefixes.append(evidence_dir)
            structure_nodes.append(
                {
                    "path": evidence_dir,
                    "kind": "directory",
                    "manifest_entry": None,
                    "seed_content": None,
                    "mutable": False,
                    "scope": self._case_scope(case_id),
                }
            )
            revision_payload.append({"eid": eid, "name": evidence_name})
            seen: dict[str, int] = {}
            for child in top_nodes:
                self._append_case_node(
                    structure_nodes=structure_nodes,
                    leaf_specs=leaf_specs,
                    path_prefix=evidence_dir,
                    node=child,
                    seen=seen,
                    nodes_by_pid=nodes_by_pid,
                    case_id=case_id,
                    analysis_cid=analysis_cid,
                    revision_payload=revision_payload,
                )

        revision = self._revision_from_payload(self._case_scope(case_id), revision_payload)
        snapshot = {
            "app_id": self.app_id,
            "revision": revision,
            "active_scope": self._case_scope(case_id),
            "ownership_prefixes": ownership_prefixes,
            "nodes": structure_nodes,
        }
        return _BuiltScope(snapshot=snapshot, leaf_specs=leaf_specs)

    def _append_case_node(
        self,
        *,
        structure_nodes: list[dict[str, Any]],
        leaf_specs: dict[str, _LeafSpec],
        path_prefix: str,
        node: dict[str, Any],
        seen: dict[str, int],
        nodes_by_pid: dict[int, list[dict[str, Any]]],
        case_id: int,
        analysis_cid: int,
        revision_payload: list[dict[str, Any]],
    ) -> None:
        raw_name = str(node.get("Name", ""))
        if raw_name in self.blocked_names:
            return
        nid = int(node.get("Nid", 0))
        if self._has_children(node):
            segment = _dedupe_name(_safe_segment(raw_name, f"节点-{nid}"), seen, f"nid_{nid}")
            current_path = f"{path_prefix}/{segment}"
            structure_nodes.append(
                {
                    "path": current_path,
                    "kind": "directory",
                    "manifest_entry": None,
                    "seed_content": None,
                    "mutable": False,
                    "scope": self._case_scope(case_id),
                }
            )
            revision_payload.append({"path": current_path, "nid": nid, "leaf": False})
            child_seen: dict[str, int] = {}
            for child in nodes_by_pid.get(nid, []):
                self._append_case_node(
                    structure_nodes=structure_nodes,
                    leaf_specs=leaf_specs,
                    path_prefix=current_path,
                    node=child,
                    seen=child_seen,
                    nodes_by_pid=nodes_by_pid,
                    case_id=case_id,
                    analysis_cid=analysis_cid,
                    revision_payload=revision_payload,
                )
            return

        segment = _dedupe_name(_safe_leaf_name(raw_name, f"节点-{nid}"), seen, f"nid_{nid}")
        current_path = f"{path_prefix}/{segment}"
        structure_nodes.append(
            {
                "path": current_path,
                "kind": "snapshot_resource",
                "manifest_entry": self._snapshot_manifest(current_path),
                "seed_content": None,
                "mutable": False,
                "scope": self._case_scope(case_id),
            }
        )
        leaf_specs[f"/{current_path}"] = _LeafSpec(
            case_id=case_id,
            analysis_cid=analysis_cid,
            eid=int(node.get("Eid", 0)),
            pid=nid,
            datatype=str(node.get("SubNodeType", "")),
            category=str(node.get("NodeType", "")),
            name=raw_name,
        )
        revision_payload.append({"path": current_path, "nid": nid, "leaf": True})

    def _has_children(self, node: dict[str, Any]) -> bool:
        value = node.get("HasChildNode", 0)
        return value == 1 or value is True

    def _structure_response(self, request: dict[str, Any], snapshot: dict[str, Any]) -> dict[str, Any]:
        known_revision = request.get("known_revision")
        if known_revision == snapshot["revision"]:
            return {
                "result": {
                    "kind": "unchanged",
                    "app_id": str(request.get("app_id", self.app_id)),
                    "revision": snapshot["revision"],
                    "active_scope": snapshot["active_scope"],
                }
            }
        return {"result": {"kind": "snapshot", "snapshot": snapshot}}

    def _control_nodes(self, scope: str | None = None) -> list[dict[str, Any]]:
        return [
            {
                "path": "_app",
                "kind": "directory",
                "manifest_entry": None,
                "seed_content": None,
                "mutable": False,
                "scope": scope,
            },
            {
                "path": "_app/enter_scope.act",
                "kind": "action_file",
                "manifest_entry": self._action_manifest("_app/enter_scope.act"),
                "seed_content": None,
                "mutable": True,
                "scope": scope,
            },
            {
                "path": "_app/refresh_structure.act",
                "kind": "action_file",
                "manifest_entry": self._action_manifest("_app/refresh_structure.act"),
                "seed_content": None,
                "mutable": True,
                "scope": scope,
            },
        ]

    def _revision_from_payload(self, scope: str, payload: Any) -> str:
        digest = hashlib.sha1(_compact_json(payload).encode("utf-8")).hexdigest()[:12]
        return f"huoyan-{scope}-{digest}"

    def _resolve_snapshot_spec(self, resource_path: str, context: dict[str, Any]) -> _LeafSpec | dict[str, Any]:
        if resource_path.startswith("/"):
            normalized_path = resource_path
        else:
            normalized_path = f"/{resource_path}"

        home = self._build_scope(DEFAULT_HOME_SCOPE, force_refresh=False)
        if normalized_path in home.info_specs:
            return home.info_specs[normalized_path]

        session_id = str(context.get("session_id", ""))
        session_scope = self.session_scopes.get(session_id, self._initial_scope())
        if session_scope != DEFAULT_HOME_SCOPE:
            case_scope = self._build_scope(session_scope, force_refresh=False)
            spec = case_scope.leaf_specs.get(normalized_path)
            if spec is not None:
                return spec

        for built in self.scope_cache.values():
            spec = built.leaf_specs.get(normalized_path)
            if spec is not None:
                return spec

        raise ValueError(f"unknown snapshot resource: {resource_path}")

    def _resume_to_offset(self, resume: dict[str, Any]) -> int:
        kind = str(resume.get("kind", "start"))
        value = resume.get("value")
        if kind == "start":
            return 0
        if kind == "offset":
            if isinstance(value, bool) or not isinstance(value, int) or value < 0:
                raise ValueError("resume offset requires non-negative integer value")
            return value
        if kind == "cursor":
            if not isinstance(value, str) or not value.startswith("offset:"):
                raise ValueError("resume cursor is invalid")
            try:
                offset = int(value.split(":", 1)[1])
            except ValueError as err:
                raise ValueError("resume cursor is invalid") from err
            if offset < 0:
                raise ValueError("resume cursor is invalid")
            return offset
        raise ValueError(f"unsupported resume kind: {kind}")

    def _fetch_case_info_chunk(
        self, case_info: dict[str, Any], *, budget_bytes: int, resume: dict[str, Any]
    ) -> dict[str, Any]:
        skip = self._resume_to_offset(resume)
        if skip > 0:
            return {
                "records": [],
                "emitted_bytes": 0,
                "next_cursor": None,
                "has_more": False,
                "revision": self._build_scope(DEFAULT_HOME_SCOPE, force_refresh=False).snapshot["revision"],
            }
        line = case_info
        emitted_bytes = len(_compact_json(line).encode("utf-8")) + 1
        if budget_bytes > 0 and emitted_bytes > budget_bytes:
            emitted_bytes = emitted_bytes
        return {
            "records": [
                {
                    "record_key": f"case-{case_info['case_id']}",
                    "ordering_key": str(case_info["case_id"]),
                    "line": line,
                }
            ],
            "emitted_bytes": emitted_bytes,
            "next_cursor": None,
            "has_more": False,
            "revision": self._build_scope(DEFAULT_HOME_SCOPE, force_refresh=False).snapshot["revision"],
        }

    def _fetch_rows(self, spec: _LeafSpec, *, skip: int, limit: int) -> dict[str, Any]:
        params = {
            "cid": spec.analysis_cid,
            "eid": spec.eid,
            "pid": spec.pid,
            "skip": skip,
            "limit": limit,
            "userid": self.user_id,
            "datatype": spec.datatype,
            "category": spec.category,
            "keyword": "",
            "oncolumns": "",
            "desc": "false",
            "columns": "",
            "deepsearch": "false",
            "withTagIds": "",
            "treeType": "application",
        }
        return self.client.fetch_leaf_rows(params=params)
