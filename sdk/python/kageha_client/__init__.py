"""Optional operator client. The Rust backend does not import or launch Python."""
import ipaddress
import json
import urllib.error
import urllib.parse
import urllib.request

class ServiceError(Exception):
    def __init__(self, payload):
        super().__init__(payload.get("message", "Memory request failed"))
        self.code = payload.get("code", "unavailable")
        self.retryable = payload.get("retryable", False)

class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise ServiceError({"message": "Memory redirects are disabled"})

class MemoryClient:
    def __init__(self, endpoint, token, writer=None):
        url = urllib.parse.urlsplit(endpoint)
        try:
            local = ipaddress.ip_address(url.hostname).is_loopback
        except ValueError:
            local = url.hostname == "localhost"
        if not (url.scheme == "https" or url.scheme == "http" and local) or url.username or url.password or url.query or url.fragment or len(token) < 32:
            raise ValueError("HTTPS or loopback endpoint and operator token required")
        self.endpoint, self._token = endpoint.rstrip("/"), token
        self._writer = writer
        self._opener = urllib.request.build_opener(_NoRedirect)

    def _request(self, path, call=None):
        request = urllib.request.Request(self.endpoint + path, data=json.dumps(call).encode() if call is not None else None, headers={"Authorization": "Bearer " + self._token, "Content-Type": "application/json"})
        if self._writer is not None:
            request.add_header("X-Kageha-Writer", json.dumps(self._writer))
        try:
            response = self._opener.open(request, timeout=30)
        except urllib.error.HTTPError as error:
            response = error
        with response:
            data = response.read(32 * 1024 * 1024 + 1)
            if len(data) > 32 * 1024 * 1024:
                raise ServiceError({"message": "Memory response exceeds size limit"})
            payload = json.loads(data)
            if response.status >= 400:
                raise ServiceError(payload)
            return payload

    def info(self):
        return self._request("/v1/info")

    def with_writer(self, fence):
        return MemoryClient(self.endpoint, self._token, writer=fence)

    def writer_status(self):
        return self._request("/v1/fence")

    def claim_writer(self, owner, expected_epoch, request_id):
        return self._request("/v1/fence", {"operation": "claim", "owner": owner, "expected_epoch": expected_epoch, "request_id": request_id})

    def retire_writer(self, fence):
        return self._request("/v1/fence", {"operation": "retire", "fence": fence})

    def call(self, operation):
        return self._request("/v1/calls", operation)

from .agent import (AgentClient, AgentError, Events, RunSpec, RunRecord, RunEvent,
                    Authority, Steering, ApprovalRecord, ApprovalDecision, ApprovalPage)
