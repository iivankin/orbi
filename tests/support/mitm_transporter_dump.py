from __future__ import annotations

import gzip
from pathlib import Path

from mitmproxy import http


OUT = Path("/tmp/transporter-dump.txt")


def _decode_response(flow: http.HTTPFlow) -> str:
    response = flow.response
    if response is None:
        return ""
    body = response.raw_content or b""
    if response.headers.get("content-encoding", "").lower() == "gzip":
        try:
            body = gzip.decompress(body)
        except Exception:
            pass
    try:
        return body.decode("utf-8", errors="replace")
    except Exception:
        return repr(body[:4096])


def response(flow: http.HTTPFlow) -> None:
    request = flow.request
    if "contentdelivery.itunes.apple.com" not in request.pretty_host:
        return
    with OUT.open("a", encoding="utf-8") as handle:
        handle.write("---\n")
        handle.write(f"{request.method} {request.pretty_url}\n")
        if request.raw_content:
            try:
                handle.write(request.raw_content.decode("utf-8", errors="replace"))
            except Exception:
                handle.write(repr(request.raw_content[:4096]))
            handle.write("\n")
        if flow.response is not None:
            handle.write(f"STATUS {flow.response.status_code}\n")
            handle.write(_decode_response(flow))
            handle.write("\n")
