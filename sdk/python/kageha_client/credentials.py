"""Optional trusted-client SDK for the independent Rust credential service."""
from __future__ import annotations
import base64
import http.client
import json
import os
import socket
import ssl
import stat
from dataclasses import dataclass
from typing import Any, Callable
from urllib.parse import urlsplit

class CredentialError(RuntimeError):
    pass

@dataclass(frozen=True)
class CredentialRef:
    namespace: str
    id: str
    def wire(self) -> dict[str, str]:
        return {"namespace": self.namespace, "id": self.id}

@dataclass(frozen=True)
class CredentialAccess:
    purpose: str
    destination: str

class SecretLease:
    def __init__(self, value: bytes, metadata: dict[str, Any], valid_until_ms: int):
        self._value = bytearray(value)
        self.metadata = metadata
        self.valid_until_ms = valid_until_ms
        self._closed = False
    def expose(self) -> memoryview:
        if self._closed:
            raise CredentialError("Credential lease is disposed")
        return memoryview(self._value)
    def close(self) -> None:
        self._value[:] = b"\x00" * len(self._value)
        self._closed = True
    def __repr__(self) -> str:
        return "SecretLease(secret=[REDACTED])"
    def __enter__(self) -> SecretLease:
        return self
    def __exit__(self, *_: Any) -> None:
        self.close()

Transport = Callable[[str, dict[str, Any]], Any]
def _private_file(path: str, limit: int) -> bytearray:
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    try:
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077 or info.st_size > limit:
            raise CredentialError("Credential input must be a private bounded regular file")
        value = bytearray()
        while len(value) <= limit:
            chunk = os.read(fd, min(4096, limit + 1 - len(value)))
            if not chunk:
                return value
            value.extend(chunk)
        raise CredentialError("Credential input exceeds limit")
    finally:
        os.close(fd)

class _UnixConnection(http.client.HTTPConnection):
    def __init__(self, path: str, timeout: float):
        super().__init__("localhost", timeout=timeout)
        self._path = path
    def connect(self) -> None:
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(self.timeout)
        self.sock.connect(self._path)

def unix_transport(socket_path: str, token_file: str, timeout: float = 25) -> Transport:
    def call(route: str, body: dict[str, Any]) -> Any:
        token = _private_file(token_file, 4096)
        try:
            return _request(_UnixConnection(socket_path, timeout), route, body, {"Authorization": "Bearer " + token.decode().strip()})
        finally:
            token[:] = b"\x00" * len(token)
    return call

def https_transport(endpoint: str, identity_file: str, ca_file: str, timeout: float = 25) -> Transport:
    url = urlsplit(endpoint)
    if url.scheme != "https" or not url.hostname or url.username or url.password or url.query or url.fragment:
        raise CredentialError("Credential service requires a clean HTTPS endpoint")
    protected = _private_file(identity_file, 1024 * 1024)
    protected[:] = b"\x00" * len(protected)
    context = ssl.create_default_context(cafile=ca_file)
    context.load_cert_chain(identity_file)
    def call(route: str, body: dict[str, Any]) -> Any:
        return _request(http.client.HTTPSConnection(url.hostname, url.port or 443, timeout=timeout, context=context), url.path.rstrip("/") + route, body, {})
    return call

def _request(connection: http.client.HTTPConnection, route: str, body: dict[str, Any], headers: dict[str, str]) -> Any:
    try:
        connection.request("POST", route, json.dumps(body), {"Content-Type": "application/json", **headers})
        response = connection.getresponse()
        data = response.read(256 * 1024 + 1)
        if len(data) > 256 * 1024:
            raise CredentialError("Credential response exceeds limit")
        if response.status < 200 or response.status >= 300:
            raise CredentialError(f"Credential operation rejected (HTTP {response.status})")
        return None if response.status == 204 else json.loads(data)
    except CredentialError:
        raise
    except Exception:
        raise CredentialError("Credential transport failed; inspect operation status before retrying a mutation") from None
    finally:
        connection.close()

class CredentialClient:
    def __init__(self, transport: Transport):
        self._transport = transport
    @staticmethod
    def _query(reference: CredentialRef, access: CredentialAccess) -> dict[str, Any]:
        return {"reference": reference.wire(), "purpose": access.purpose, "destination": access.destination}
    def metadata(self, reference: CredentialRef, access: CredentialAccess) -> dict[str, Any]:
        return self._transport("/v1/metadata", self._query(reference, access))
    def resolve(self, reference: CredentialRef, access: CredentialAccess) -> SecretLease:
        import time
        result = self._transport("/v1/resolve", self._query(reference, access))
        if result.get("metadata", {}).get("reference") != reference.wire():
            raise CredentialError("Credential service returned a different reference")
        expiry = result.get("valid_until_ms", 0)
        now = int(time.time() * 1000)
        if not isinstance(expiry, int) or expiry <= now or expiry > now + 60000:
            raise CredentialError("Invalid credential lease expiry")
        value = base64.b64decode(result.pop("secret_base64"), validate=True)
        return SecretLease(value, result["metadata"], expiry)
    def validate(self, lease: SecretLease, access: CredentialAccess) -> None:
        ref = CredentialRef(**lease.metadata["reference"])
        self._transport("/v1/check", {**self._query(ref, access), "metadata": lease.metadata, "valid_until_ms": lease.valid_until_ms})
    def put(self, reference: CredentialRef, access: CredentialAccess, *, operation_id: str, expected_revision: int, kind: str, value: bytes, expires_at_ms: int | None = None) -> dict[str, Any]:
        return self._transport("/v1/put", {**self._query(reference, access), "operation_id": operation_id, "expected_revision": expected_revision, "kind": kind, "secret_base64": base64.b64encode(value).decode(), "expires_at_ms": expires_at_ms})
    def revoke(self, reference: CredentialRef, access: CredentialAccess, *, operation_id: str, expected_revision: int, delete: bool = False) -> dict[str, Any]:
        return self._transport("/v1/revoke", {**self._query(reference, access), "operation_id": operation_id, "expected_revision": expected_revision, "delete": delete})
    def operation(self, reference: CredentialRef, access: CredentialAccess, operation_id: str) -> dict[str, Any] | None:
        return self._transport("/v1/operation", {**self._query(reference, access), "operation_id": operation_id})
    def begin_refresh(self, reference: CredentialRef, access: CredentialAccess, expected_revision: int, operation_id: str) -> dict[str, Any]:
        return self._transport('/v1/oauth', {'query': self._query(reference, access), 'call': {'action':'begin', 'expected_revision':expected_revision, 'operation_id':operation_id}})['ticket']
    def dispatch_refresh(self, ticket: dict[str, Any], access: CredentialAccess, operation_id: str) -> tuple[dict[str, Any], SecretLease]:
        reply = self._transport('/v1/oauth', {'query': self._query(CredentialRef(**ticket['reference']), access), 'call': {'action':'dispatch', 'ticket':ticket, 'operation_id':operation_id}})
        value = base64.b64decode(reply.pop('grant_base64'), validate=True)
        return reply['ticket'], SecretLease(value, {'reference': ticket['reference'], 'revision':reply['ticket']['revision']}, reply['ticket']['expires_at_ms'])
    def complete_refresh(self, ticket: dict[str, Any], access: CredentialAccess, operation_id: str, grant: bytes, expires_at_ms: int | None = None) -> dict[str, Any]:
        return self._transport('/v1/oauth', {'query': self._query(CredentialRef(**ticket['reference']), access), 'call': {'action':'complete', 'ticket':ticket, 'operation_id':operation_id, 'grant_base64':base64.b64encode(grant).decode(), 'expires_at_ms':expires_at_ms}})['operation']
    def recover_refresh(self, reference: CredentialRef, access: CredentialAccess, expected_revision: int, operation_id: str) -> dict[str, Any]:
        return self._transport('/v1/oauth', {'query':self._query(reference,access), 'call':{'action':'recover','expected_revision':expected_revision,'operation_id':operation_id}})['operation']
    def fail_refresh(self, ticket: dict[str, Any], access: CredentialAccess, operation_id: str) -> dict[str, Any]:
        return self._transport('/v1/control', {'query':self._query(CredentialRef(**ticket['reference']),access),'call':{'action':'fail_refresh','ticket':ticket,'operation_id':operation_id}})
    def audits(self, namespace: str, access: CredentialAccess, after: int = 0) -> list[dict[str, Any]]:
        return self._transport('/v1/audit', {'namespace':namespace,'purpose':access.purpose,'destination':access.destination,'after':after})
    def replacement(self, access: CredentialAccess, action: dict[str, Any]) -> dict[str, Any]:
        return self._transport('/v1/replacements', {**action,'purpose':access.purpose,'destination':access.destination})
