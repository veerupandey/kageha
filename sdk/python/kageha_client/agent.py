"""Synchronous client for the standalone Rust agent runtime; no backend dependency."""
import codecs
import ipaddress
import json
import math
import urllib.error
import urllib.parse
import urllib.request
from typing import Any, TypedDict, Optional

LIMIT = 32 * 1024 * 1024
TERMINAL = {"succeeded", "failed", "cancelled", "interrupted"}

class RunSpec(TypedDict):
    id: str
    session: str
    turn: str
    request: str
    agent: str
    objective: str
    depth: int
    parent: Optional[str]
    parent_run: Optional[str]
    input: dict[str, Any]

class Authority(TypedDict):
    session: str
    turn: str
    binding: str
    generation: int
    resource: str
    input_digest: str

class Steering(TypedDict):
    id: str
    body: str

class RunRecord(TypedDict):
    spec: RunSpec
    state: str
    revision: int
    lease: Optional[str]
    completion: Optional[dict[str, Any]]
    cancel_requested: bool

class RunEvent(TypedDict):
    sequence: int
    record: RunRecord

# Wire key is "class" (a Python keyword); expose it accurately through functional syntax.
ApprovalRequest = TypedDict("ApprovalRequest", {"authority": Authority, "action": str, "class": str, "detail": str})

class ApprovalRecord(TypedDict):
    id: str
    request: ApprovalRequest
    state: str
    revision: int
    decision: Optional[bool]
    scope: str
    automatic: bool

class ApprovalDecision(TypedDict):
    authority: Authority
    expected_revision: int
    approved: bool
    scope: str

class ApprovalPage(TypedDict):
    approvals: list[ApprovalRecord]
    next_cursor: Optional[int]

class AgentError(Exception):
    def __init__(self, message, *, status=None, code="sdk_protocol", retryable=False):
        super().__init__(message)
        self.status, self.code, self.retryable = status, code, retryable

class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None

def _id(value):
    if not isinstance(value, str) or not value or len(value.encode("utf-8")) > 128 or value in {".", ".."} or any(ord(c) < 32 or 127 <= ord(c) <= 159 for c in value):
        raise AgentError("Invalid resource identity")
    return urllib.parse.quote(value, safe="")

def _cursor(value):
    if type(value) is not int or not 0 <= value <= 2**63 - 1:
        raise AgentError("Invalid event cursor")
    return value

class AgentClient:
    def __init__(self, endpoint, token, *, timeout=30):
        url = urllib.parse.urlsplit(endpoint)
        try:
            local = ipaddress.ip_address(url.hostname).is_loopback
        except ValueError:
            local = url.hostname == "localhost"
        if not (url.scheme == "https" or url.scheme == "http" and local) or not url.hostname or url.username or url.password or url.query or url.fragment or not isinstance(token, str) or len(token) < 24 or not all(33 <= ord(c) <= 126 for c in token):
            raise AgentError("HTTPS or loopback endpoint and an operator token required")
        if not isinstance(timeout, (int, float)) or not math.isfinite(timeout) or timeout <= 0:
            raise AgentError("Positive timeout required")
        self.endpoint, self._token, self.timeout = endpoint.rstrip("/"), token, timeout
        self._opener = urllib.request.build_opener(_NoRedirect)

    def _open(self, path, body=None, events=False):
        request = urllib.request.Request(self.endpoint + path, data=None if body is None else json.dumps(body).encode(), headers={"Authorization": "Bearer " + self._token, "Content-Type": "application/json", "Accept": "text/event-stream" if events else "application/json"})
        try:
            return self._opener.open(request, timeout=self.timeout)
        except urllib.error.HTTPError as error:
            return error
        except (OSError, urllib.error.URLError):
            raise AgentError("Runtime transport failed; request outcome may be unknown. Inspect status before retrying input.") from None

    @staticmethod
    def _json(response):
        data = response.read(LIMIT + 1)
        if len(data) > LIMIT:
            raise AgentError("Runtime response exceeds size limit")
        try:
            value = json.loads(data)
        except (ValueError, UnicodeError):
            value = None
        if not 200 <= response.status < 300:
            value = value if isinstance(value, dict) else {}
            raise AgentError(value.get("message", "Runtime request rejected"), status=response.status, code=value.get("code", "http_error"), retryable=value.get("retryable") is True)
        if value is None:
            raise AgentError("Invalid runtime JSON response")
        return value

    def _request(self, path, body=None):
        try:
            with self._open(path, body) as response:
                return self._json(response)
        except AgentError:
            raise
        except (OSError, ValueError):
            raise AgentError("Runtime response failed; request outcome may be unknown. Inspect status before retrying input.") from None

    def info(self) -> dict[str, Any]:
        return self._request("/v1/info")

    def run(self, spec: RunSpec) -> RunRecord:
        _id(spec["id"])
        return self._request("/v1/runs", spec)

    def status(self, run: str) -> RunRecord:
        return self._request("/v1/runs/" + _id(run))

    def steer(self, run: str, message: Steering) -> dict[str, Any]:
        _id(message["id"])
        return self._request("/v1/runs/" + _id(run) + "/steer", message)

    def cancel(self, run: str) -> RunRecord:
        return self._request("/v1/runs/" + _id(run) + "/cancel", {})

    def events(self, run: str, *, after: int = 0) -> "Events":
        return Events(self, run, _cursor(after))

    def approvals(self, *, session: Optional[str] = None, turn: Optional[str] = None, pending: Optional[bool] = None, after: Optional[int] = None) -> ApprovalPage:
        if after is not None:
            _cursor(after)
        query = {k: str(v).lower() if type(v) is bool else v for k, v in {"session": session, "turn": turn, "pending": pending, "after": after}.items() if v is not None}
        return self._request("/v1/approvals?" + urllib.parse.urlencode(query))

    def approval(self, approval: str) -> ApprovalRecord:
        return self._request("/v1/approvals/" + _id(approval))

    def decide(self, approval: str, decision: ApprovalDecision) -> ApprovalRecord:
        return self._request("/v1/approvals/" + _id(approval) + "/decision", decision)

class Events:
    """Use as a context manager. Closing a stream does not cancel its server run."""
    def __init__(self, client, run, after):
        self.last_sequence = after
        self._response = client._open("/v1/runs/" + _id(run) + "/events?after=" + str(after), events=True)
        try:
            if not 200 <= self._response.status < 300:
                client._json(self._response)
            if self._response.headers.get_content_type() != "text/event-stream":
                raise AgentError("Expected runtime event stream")
        except BaseException:
            self._response.close()
            raise
        self._iterator = self._events(run)
        self._closed = False

    def __enter__(self):
        return self

    def __exit__(self, *args):
        self.close()

    def __iter__(self):
        return self

    def __next__(self):
        if self._closed:
            raise StopIteration
        try:
            return next(self._iterator)
        except BaseException:
            self.close()
            raise

    def close(self):
        if not self._closed:
            self._closed = True
            self._iterator.close()
            self._response.close()

    def _events(self, run):
        decoder = codecs.getincrementaldecoder("utf-8")("strict")
        buffer, data, kind, event_id, size = "", [], "", "", 0
        try:
            while True:
                chunk = self._response.read1(65536)
                done = not chunk
                buffer += decoder.decode(chunk, final=done)
                while True:
                    indices = [i for i in [buffer.find("\r"), buffer.find("\n")] if i >= 0]
                    if not indices:
                        break
                    index = min(indices)
                    if buffer[index] == "\r" and index == len(buffer) - 1 and not done:
                        break
                    line = buffer[:index]
                    length = 2 if buffer[index:index+2] == "\r\n" else 1
                    buffer = buffer[index+length:]
                    size += len(line.encode()) + length
                    if size > LIMIT:
                        raise AgentError("Runtime event exceeds size limit")
                    if not line:
                        if data:
                            if kind != "run":
                                raise AgentError("Runtime event stream reported an error or unknown event")
                            event = json.loads("\n".join(data))
                            sequence = _cursor(event["sequence"])
                            if str(sequence) != event_id or sequence <= self.last_sequence or event["record"]["spec"]["id"] != run:
                                raise AgentError("Out-of-order event or wrong run identity")
                            self.last_sequence = sequence
                            yield event
                            if event["record"]["state"] in TERMINAL:
                                return
                        data, kind, event_id, size = [], "", "", 0
                    elif not line.startswith(":"):
                        field, _, value = line.partition(":")
                        if value.startswith(" "):
                            value = value[1:]
                        if field == "data":
                            data.append(value)
                        elif field == "event":
                            kind = value
                        elif field == "id":
                            event_id = value
                if size + len(buffer.encode()) > LIMIT:
                    raise AgentError("Runtime event exceeds size limit")
                if done:
                    if buffer or data:
                        raise AgentError("Truncated runtime event; resume from the last delivered cursor")
                    return
        except AgentError:
            raise
        except (OSError, ValueError, KeyError, TypeError):
            raise AgentError("Runtime event transport or frame failed; resume from the last delivered cursor") from None
