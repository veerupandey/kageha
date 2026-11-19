"""Optional MCP client bridge to the native Rust browser; no framework dependency."""
from __future__ import annotations
import json
import uuid
from datetime import timedelta
from mcp import ClientSession


class BrowserMcpError(Exception):
    """Protocol/transport failure. A mutation may already have dispatched."""


class McpBrowserClient:
    """Use an initialized, persistent MCP session with operator-fixed identity.

    Never reconnects or retries browser input. The caller owns MCP session lifetime.
    Returned native errors/receipts remain intact for host policy and recovery.
    """
    def __init__(self, session: ClientSession, workspace_id: str, client_id: str):
        for scope in (workspace_id, client_id):
            if not isinstance(scope, str) or not scope or len(scope.encode()) > 128 or any(ord(c) < 32 or ord(c) == 127 for c in scope):
                raise ValueError('Invalid browser scope')
        self.session = session
        self.workspace_id, self.client_id = workspace_id, client_id
        self.info = None

    @staticmethod
    def decode(result):
        blocks = result.content
        if not blocks or blocks[0].type != 'text' or len(blocks[0].text.encode()) > 32 * 1024 * 1024:
            raise BrowserMcpError('Invalid or oversized native MCP browser response')
        try:
            value = json.loads(blocks[0].text)
        except (TypeError, ValueError):
            raise BrowserMcpError('Invalid native MCP browser JSON') from None
        if not isinstance(value, dict):
            raise BrowserMcpError('Invalid native MCP browser object')
        return value

    async def connect(self):
        catalog = await self.session.list_tools()
        if not {'browser_info', 'browser_call'} <= {t.name for t in catalog.tools}:
            raise BrowserMcpError('Native Rust browser MCP tools are unavailable')
        result = await self.session.call_tool('browser_info', {})
        info = self.decode(result)
        if result.isError or info.get('protocol_version') != 1 or not info.get('service_epoch') or not isinstance(info.get('methods'), list):
            raise BrowserMcpError('Unsupported native MCP browser contract')
        if info.get('workspace_id') != self.workspace_id or info.get('client_id') != self.client_id:
            raise BrowserMcpError('MCP launcher identity differs from requested scope')
        if self.info and self.info['service_epoch'] != info['service_epoch']:
            raise BrowserMcpError('Browser restarted; use a new client and inspect original receipts')
        self.info = info
        return dict(info)

    async def call(self, method, params=None, *, request_id=None, authority=None, timeout_ms=10000):
        if not self.info:
            raise BrowserMcpError('Connect before using the MCP browser')
        if method not in self.info['methods']:
            raise BrowserMcpError('Browser method is not advertised')
        if isinstance(timeout_ms, bool) or not isinstance(timeout_ms, int) or not 1 <= timeout_ms <= min(30000, self.info.get('limits', {}).get('deadline_ms', 30000)):
            raise ValueError('Invalid browser timeout')
        request_id = request_id or str(uuid.uuid4())
        if not isinstance(request_id, str) or not request_id or len(request_id.encode()) > 128 or any(ord(c) < 32 or ord(c) == 127 for c in request_id):
            raise ValueError('Invalid browser request identity')
        params = {} if params is None else params
        if not isinstance(params, dict):
            raise ValueError('Browser params must be an object')
        result = await self.session.call_tool('browser_call', {'method': method, 'params': params, 'request_id': request_id, 'service_epoch': self.info['service_epoch'], 'authority': authority, 'timeout_ms': timeout_ms}, read_timeout_seconds=timedelta(milliseconds=timeout_ms + 2000))
        reply = self.decode(result)
        if reply.get('protocol_version') != 1 or any(reply.get(k) != v for k, v in {'request_id': request_id, 'workspace_id': self.workspace_id, 'client_id': self.client_id, 'service_epoch': self.info['service_epoch']}.items()):
            raise BrowserMcpError('Native MCP reply identity mismatch; inspect original receipt')
        if result.isError and reply.get('status') == 'ok' and reply.get('execution_phase') not in {'uncertain', 'partial'}:
            raise BrowserMcpError('MCP tool outcome is ambiguous; inspect original receipt')
        images = []
        for block in result.content[1:]:
            if block.type == 'image' and block.mimeType in {'image/png', 'image/jpeg'}:
                if len(block.data) > 8 * 1024 * 1024:
                    raise BrowserMcpError('MCP browser image exceeds size limit')
                images.append({'type': 'image_url', 'image_url': {'url': f'data:{block.mimeType};base64,{block.data}'}})
        if images:
            reply['_mcp_images'] = images
        return reply
