from __future__ import annotations

import json
from datetime import datetime, timezone
from pathlib import Path

from mitmproxy import http


LOG_PATH = Path("/tmp/transporter-mitm.jsonl")


def _is_textual(content_type: str) -> bool:
    lowered = content_type.lower()
    return (
        lowered.startswith("text/")
        or "json" in lowered
        or "xml" in lowered
        or "plist" in lowered
        or "x-www-form-urlencoded" in lowered
    )


def _preview(content: bytes, content_type: str) -> str | None:
    if not content:
        return None
    if len(content) > 32_768:
        return None
    if not _is_textual(content_type):
        return None
    try:
        return content.decode("utf-8", errors="replace")
    except Exception:
        return None


def _write(payload: dict) -> None:
    payload["timestamp"] = datetime.now(timezone.utc).isoformat()
    with LOG_PATH.open("a", encoding="utf-8") as handle:
        handle.write(json.dumps(payload, ensure_ascii=False) + "\n")


class TransporterCapture:
    def request(self, flow: http.HTTPFlow) -> None:
        request = flow.request
        content_type = request.headers.get("content-type", "")
        _write(
            {
                "event": "request",
                "method": request.method,
                "scheme": request.scheme,
                "host": request.host,
                "port": request.port,
                "path": request.path,
                "url": request.pretty_url,
                "http_version": request.http_version,
                "headers": dict(request.headers),
                "content_length": len(request.raw_content or b""),
                "body_preview": _preview(request.raw_content or b"", content_type),
            }
        )

    def response(self, flow: http.HTTPFlow) -> None:
        request = flow.request
        response = flow.response
        if response is None:
            return
        content_type = response.headers.get("content-type", "")
        _write(
            {
                "event": "response",
                "method": request.method,
                "host": request.host,
                "path": request.path,
                "url": request.pretty_url,
                "status_code": response.status_code,
                "reason": response.reason,
                "headers": dict(response.headers),
                "content_length": len(response.raw_content or b""),
                "body_preview": _preview(response.raw_content or b"", content_type),
            }
        )

    def error(self, flow: http.HTTPFlow) -> None:
        request = flow.request
        _write(
            {
                "event": "error",
                "method": request.method if request else None,
                "host": request.host if request else None,
                "path": request.path if request else None,
                "url": request.pretty_url if request else None,
                "error": str(flow.error) if flow.error else "unknown error",
            }
        )


addons = [TransporterCapture()]
