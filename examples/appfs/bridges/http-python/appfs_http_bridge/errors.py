from __future__ import annotations

from typing import Any


def rejected_error(code: str, message: str, retryable: bool = False) -> dict[str, Any]:
    return {
        "kind": "rejected",
        "code": code,
        "message": message,
        "retryable": bool(retryable),
    }


def internal_error(message: str) -> dict[str, Any]:
    return {
        "kind": "internal",
        "message": message,
    }
