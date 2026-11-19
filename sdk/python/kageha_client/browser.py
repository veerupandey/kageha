"""Optional async client for the standalone Rust browser. No automation implementation."""
from __future__ import annotations

import ipaddress
import json
import uuid
from urllib.parse import urlsplit

import httpx

LIMIT = 32 * 1024 * 1024


class BrowserError(Exception):
    def __init__(self, message: str, *, status: int | None = None, code: str | None = None):
        super().__init__(message)
        self.status, self.code = status, code


class BrowserClient:
    """One fixed workspace/client scope and service epoch; never retries input."""
    def __init__(self, base_url: str, token: str, workspace_id: str, client_id: str, *, transport=None):
        url = urlsplit(base_url)
        try:
            local = ipaddress.ip_address(url.hostname or "").is_loopback
        except ValueError:
            local = url.hostname == "localhost"
        if (url.scheme != "https" and not (url.scheme == "http" and local)) or url.username or url.password or url.query or url.fragment or url.path not in ("", "/"):
            raise ValueError("HTTPS or loopback browser API origin required")
        if not token or any(c in token for c in "\r\n"):
            raise ValueError("Browser API token required")
        for scope in (workspace_id, client_id):
            if not isinstance(scope, str) or not scope or len(scope.encode()) > 128 or any(ord(c) < 32 or ord(c) == 127 for c in scope):
                raise ValueError("Invalid browser scope")
        self.base_url = f"{url.scheme}://{url.netloc}"
        self.workspace_id, self.client_id = workspace_id, client_id
        self.info = None
        self._http = httpx.AsyncClient(base_url=self.base_url, headers={"Authorization": f"Bearer {token}"}, follow_redirects=False, trust_env=False, transport=transport)

    def __repr__(self):
        return f"BrowserClient(workspace_id={self.workspace_id!r}, connected={self.info is not None})"

    async def _request(self, route, body=None, timeout_ms=10000):
        try:
            async with self._http.stream("POST" if body is not None else "GET", route, json=body, timeout=(timeout_ms + 2000) / 1000) as response:
                data = bytearray()
                async for chunk in response.aiter_bytes():
                    data.extend(chunk)
                    if len(data) > LIMIT:
                        raise BrowserError("Browser response exceeds size limit")
                if not 200 <= response.status_code < 300:
                    raise BrowserError("Browser HTTP request rejected", status=response.status_code)
                value = json.loads(data)
                if not isinstance(value, dict):
                    raise BrowserError("Invalid browser response")
                return value
        except (httpx.HTTPError, ValueError):
            raise BrowserError("Browser transport or response failed; inspect original attempt before repeating input") from None

    async def connect(self):
        info = await self._request("/v1/info")
        if info.get("api_version") != 1 or info.get("protocol_version") != 1 or not isinstance(info.get("service_epoch"), str) or not isinstance(info.get("methods"), list):
            raise BrowserError("Unsupported browser API contract")
        if self.info and self.info["service_epoch"] != info["service_epoch"]:
            raise BrowserError("Browser restarted; construct a new client and observe fresh state")
        self.info = info
        return dict(info)

    async def call(self, method, params=None, *, request_id=None, authority=None, timeout_ms=10000):
        if not self.info:
            raise BrowserError("Connect before using the browser")
        if method not in self.info["methods"]:
            raise BrowserError("Browser method is not advertised")
        if isinstance(timeout_ms, bool) or not isinstance(timeout_ms, int) or not 1 <= timeout_ms <= 30000:
            raise ValueError("timeout_ms must be 1..30000")
        request_id = request_id or str(uuid.uuid4())
        if not isinstance(request_id, str) or not request_id or len(request_id.encode()) > 128 or any(ord(c) < 32 for c in request_id):
            raise ValueError("Invalid request identity")
        params = {} if params is None else params
        if not isinstance(params, dict):
            raise ValueError("Browser params must be an object")
        body = {"workspace_id": self.workspace_id, "client_id": self.client_id, "request_id": request_id, "service_epoch": self.info["service_epoch"], "method": method, "params": params, "authority": authority, "expected_binding": None, "timeout_ms": timeout_ms}
        reply = await self._request("/v1/rpc", body, timeout_ms)
        if reply.get("protocol_version") != 1 or any(reply.get(key) != body[key] for key in ("workspace_id", "client_id", "request_id", "service_epoch")):
            raise BrowserError("Browser reply identity mismatch; inspect original attempt before continuing")
        return reply

    async def aclose(self):
        await self._http.aclose()

    async def __aenter__(self):
        try:
            await self.connect()
            return self
        except BaseException:
            await self.aclose()
            raise

    async def __aexit__(self, *_):
        await self.aclose()
