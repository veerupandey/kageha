// Native Rust MCP transport, no HTTP gateway or Python process.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { createInterface } from 'node:readline';
import { randomUUID } from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import http from 'node:http';
const binary = process.env.KAGEHA_BROWSER_BIN;
assert(binary && path.isAbsolute(binary), 'Set KAGEHA_BROWSER_BIN');
const chromium = process.env.CHROMIUM_BIN || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const root = fs.mkdtempSync(path.join(os.tmpdir(), 'kbr-mcp-'));
const page = http.createServer((_, response) => { response.setHeader('content-type', 'text/html'); response.end('<title>MCP fixture</title><label><input type="checkbox">Confirm</label>'); });
page.listen(0, '127.0.0.1'); await once(page, 'listening');
const child = spawn(binary, ['--state-dir', path.join(root, 'state'), '--instance-id', 'mcp-test', '--application-id', 'another-framework', '--chromium', chromium, '--standalone', 'on', '--mcp', 'on', '--mcp-workspace', 'demo', '--mcp-client', 'assistant'], { stdio: ['pipe', 'pipe', 'pipe'] });
let stderr = '';
child.stderr.on('data', bytes => { stderr = (stderr + bytes).slice(-4096); });
let serial = 0;
const pending = new Map();
const lines = createInterface({ input: child.stdout });
lines.on('line', line => {
  const reply = JSON.parse(line); // Every stdout line must be a protocol message.
  const target = pending.get(reply.id);
  assert(target, `Unexpected response: ${line}`);
  pending.delete(reply.id); clearTimeout(target.timer); target.resolve(reply);
});
function request(method, params = {}) {
  const id = ++serial;
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => { pending.delete(id); reject(Error(`MCP timeout ${method}: ${stderr}`)); }, 15000);
    pending.set(id, { resolve, timer });
    child.stdin.write(JSON.stringify({ jsonrpc: '2.0', id, method, params }) + '\n');
  });
}
try {
  assert.equal((await request('tools/list')).error.code, -32002);
  const init = await request('initialize', { protocolVersion: '2099-01-01', capabilities: {}, clientInfo: { name: 'test-agent', version: '1' } });
  assert.equal(init.result.protocolVersion, '2024-11-05');
  assert.equal((await request('tools/list')).error.code, -32002);
  child.stdin.write(JSON.stringify({ jsonrpc: '2.0', method: 'notifications/initialized' }) + '\n');
  const tools = (await request('tools/list')).result.tools;
  assert(tools.some(tool => tool.name === 'browser_call'));
  const info = JSON.parse((await request('tools/call', { name: 'browser_info', arguments: {} })).result.content[0].text);
  assert.equal(info.workspace_id, 'demo');
  assert(!info.methods.includes('shutdown'));
  async function call(method, params = {}, options = {}) {
    const response = await request('tools/call', { name: 'browser_call', arguments: { method, params, request_id: randomUUID(), service_epoch: info.service_epoch, ...options } });
    assert(!response.error, JSON.stringify(response));
    return { ...response.result, native: JSON.parse(response.result.content[0].text) };
  }
  const stale = await call('health', {}, { service_epoch: 'old' });
  assert.equal(stale.isError, true);
  assert.equal(stale.native.error.code, 'stale_service_epoch');
  const spoof = await request('tools/call', { name: 'browser_call', arguments: { method: 'health', params: {}, request_id: 'scope', service_epoch: info.service_epoch, workspace_id: 'other' } });
  assert.equal(spoof.error.code, -32602);
  const attempt = randomUUID();
  const browser = await call('launch_browser', {}, { request_id: attempt });
  assert.equal(browser.isError, false, JSON.stringify(browser));
  assert.equal((await call('launch_browser', {}, { request_id: attempt })).native.error.code, 'duplicate_request');
  const browser_id = browser.native.result.browser_id;
  const tab = (await call('open_tab', { browser_id, url: `http://127.0.0.1:${page.address().port}/` })).native.result;
  const snapshot = await call('observe_tab', { browser_id, target_id: tab.target_id, compact: false });
  assert.equal(snapshot.isError, false, JSON.stringify(snapshot));
  assert(JSON.stringify(snapshot.native.result).includes('Confirm'));
  const selection = (await call('browser_selection')).native.result;
  const preview = await call('preview_tab', { browser_id, target_id: tab.target_id, binding_id: selection.binding_id });
  assert.equal(preview.isError, false, JSON.stringify(preview));
  assert.equal(preview.content[1].type, 'image');
  assert.equal(preview.content[1].mimeType, 'image/jpeg');
  assert(Buffer.from(preview.content[1].data, 'base64').length > 100);
  assert.equal((await call('close_browser', { browser_id })).isError, false);
  const exited = once(child, 'exit');
  child.stdin.end();
  const status = await Promise.race([exited, new Promise((_, reject) => setTimeout(() => reject(Error('MCP EOF cleanup timed out')), 5000))]);
  assert.equal(status[0], 0, stderr);
  console.log('Native MCP passed lifecycle, protocol negotiation, fixed scope, epoch fencing, replay protection, real Chromium observation, image response and EOF cleanup.');
} finally {
  for (const { timer } of pending.values()) clearTimeout(timer);
  lines.close();
  child.stdin.destroy();
  if (child.exitCode === null) child.kill('SIGINT');
  page.closeAllConnections(); await new Promise(resolve => page.close(resolve));
  fs.rmSync(root, { recursive: true, force: true });
}
