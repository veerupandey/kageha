// A selected tab that cannot be attached (here a crashed renderer) must not make the
// whole browser report as disconnected or fail every later selection read.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import http from 'node:http';
import net from 'node:net';
import path from 'node:path';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { randomUUID } from 'node:crypto';
const root = fs.mkdtempSync('/tmp/kbr-dead-');
const token = randomUUID();
fs.writeFileSync(path.join(root, 'token'), token, { mode: 0o600 });
const probe = net.createServer().listen(0, '127.0.0.1');
await once(probe, 'listening');
const port = probe.address().port;
await new Promise(resolve => probe.close(resolve));
const site = http.createServer((req, res) => {
  res.setHeader('content-type', 'text/html');
  res.end('<title>Fixture</title><button>Press</button>');
}).listen(0, '127.0.0.1');
await once(site, 'listening');
const chromium = process.env.CHROMIUM_BIN || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const state = path.join(root, 'browser');
const child = spawn(process.env.KAGEHA_BROWSER_BIN, ['--state-dir', state, '--instance-id', 'dead-tab',
  '--application-id', 'fixture', '--standalone', 'on', '--chromium', chromium,
  '--http-listen', `127.0.0.1:${port}`, '--http-token-file', path.join(root, 'token')], { stdio: ['ignore', 'ignore', 'pipe'] });
let errors = '';
child.stderr.on('data', data => errors += data);
const base = `http://127.0.0.1:${port}`;
const headers = { authorization: `Bearer ${token}`, 'content-type': 'application/json' };
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
try {
  for (const deadline = Date.now() + 10000; ; ) {
    try { if ((await fetch(base + '/health', { headers })).ok) break; } catch {}
    assert(child.exitCode === null && Date.now() < deadline, errors);
    await pause(25);
  }
  const info = await (await fetch(base + '/v1/info', { headers })).json();
  const scope = { workspace_id: 'dead-tab', client_id: 'agent', service_epoch: info.service_epoch };
  let last = Date.now();
  const rpc = async (method, params = {}, authority = null) => {
    const idle = Date.now() - last;
    const started = Date.now();
    try {
      return await (await fetch(base + '/v1/rpc', { method: 'POST', headers, keepalive: false,
        body: JSON.stringify({ ...scope, request_id: randomUUID(), method, params, authority }) })).json();
    } finally {
      last = Date.now();
      if (process.env.KBR_TRACE) console.error(`${method}: idle ${idle} ms, took ${Date.now() - started} ms`);
    }
  };
  const launch = await rpc('launch_browser');
  assert.equal(launch.status, 'ok', JSON.stringify(launch));
  const { browser_id } = launch.result;
  const opened = await rpc('open_tab', { browser_id, url: `http://127.0.0.1:${site.address().port}/` });
  assert.equal(opened.status, 'ok', JSON.stringify(opened));
  await pause(300);
  const before = await rpc('browser_selection');
  assert.equal(before.status, 'ok', JSON.stringify(before));
  assert.equal(before.result.target_id, opened.result.target_id);

  // Crash the selected tab's renderer through a separate debugging connection.
  const profile = fs.readdirSync(state).find(name => name.startsWith('chromium-'));
  const [debugPort, debugPath] = fs.readFileSync(path.join(state, profile, 'DevToolsActivePort'), 'utf8').trim().split('\n');
  const socket = new WebSocket(`ws://127.0.0.1:${debugPort}${debugPath}`);
  await once(socket, 'open');
  let nextId = 0;
  const pending = new Map();
  socket.addEventListener('message', event => {
    const message = JSON.parse(event.data);
    pending.get(message.id)?.(message);
  });
  const cdp = (method, params = {}, sessionId) => new Promise(resolve => {
    const id = ++nextId;
    pending.set(id, resolve);
    socket.send(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) }));
  });
  const attached = await cdp('Target.attachToTarget', { targetId: opened.result.target_id, flatten: true });
  cdp('Page.crash', {}, attached.result.sessionId);
  await pause(500);
  socket.close();

  for (let attempt = 0; attempt < 3; attempt++) {
    const after = await rpc('browser_selection');
    assert.equal(after.status, 'ok', `selection read ${attempt} failed: ${JSON.stringify(after)}`);
    assert.equal(after.result.browser_id, browser_id, 'the browser must stay connected');
    const current = after.result.profiles.find(entry => entry.browser_id === browser_id);
    assert.equal(current.connected, true, 'the profile must not be reported disconnected');
    await pause(200);
  }
  const brokenSelection = (await rpc('browser_selection')).result;
  const grant = (await rpc('claim_control')).result;
  const closed = await rpc('close_tab', {binding_id: brokenSelection.binding_id, browser_id,
    target_id: opened.result.target_id}, grant.authority);
  assert.equal(closed.status, 'ok', JSON.stringify(closed));
  assert.equal(closed.result.closed, true, 'An unreadable owned tab can be explicitly closed');
  // The browser is usable: a new tab opens and becomes the selection.
  const next = await rpc('open_tab', { browser_id, url: `http://127.0.0.1:${site.address().port}/` });
  assert.equal(next.status, 'ok', JSON.stringify(next));
  await pause(300);
  const recovered = await rpc('browser_selection');
  assert.equal(recovered.status, 'ok', JSON.stringify(recovered));
  assert.equal(recovered.result.target_id, next.result.target_id);
  console.log('dead selected tab: ok');
} catch (error) {
  console.error('daemon exit code', child.exitCode, 'signal', child.signalCode, 'stderr:', errors.slice(-1500));
  throw error;
} finally {
  child.kill('SIGTERM');
  site.close();
  await pause(300);
  fs.rmSync(root, { recursive: true, force: true });
}
