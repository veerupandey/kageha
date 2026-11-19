import assert from 'node:assert/strict';
import fs from 'node:fs';
import http from 'node:http';
import net from 'node:net';
import path from 'node:path';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { randomUUID } from 'node:crypto';

const root = fs.mkdtempSync('/tmp/kbr-rel-');
const token = randomUUID();
fs.writeFileSync(path.join(root, 'token'), token, { mode: 0o600 });
const probe = net.createServer().listen(0, '127.0.0.1');
await once(probe, 'listening');
const port = probe.address().port;
await new Promise(resolve => probe.close(resolve));
const site = http.createServer((req, res) => {
  res.setHeader('content-type', 'text/html');
  if (req.url === '/delayed') {
    res.end(`<html><body><script>setTimeout(() => document.body.innerHTML = '<h1>Delayed research content</h1>', 250)</script></body></html>`);
  } else if (req.url === '/redirect') {
    res.writeHead(302, { location: '/replace' }); res.end();
  } else if (req.url === '/replace') {
    res.end(`<script>setTimeout(() => location.replace('/delayed'), 60)</script>`);
  } else if (req.url === '/empty') {
    res.end('<html><body></body></html>');
  } else if (req.url === '/missing') {
    res.writeHead(404); res.end('');
  } else {
    res.end(`<title>Inbox fixture</title><input id="fixture-email" type="email" required value="test@example.com"><input id="fixture-secret" type="password" value="never-expose"><input id="fixture-otp" autocomplete="one-time-code" value="938271"><label for="fixture-otp-other">Verification code</label><input id="fixture-otp-other" type="text" value="724719"><form><p>Enter the code we sent by email</p><label for="generic-otp">Code</label><input id="generic-otp" value="375918"></form><form><label for="promo-code">Code</label><input id="promo-code" value="SALE"></form><select id="fixture-country"><option value="ca" selected>Canada</option></select>${Array.from({ length: 120 }, (_, i) => `<button>Noise ${i}</button>`).join('')}<button id="message" onclick="document.querySelector('#result').textContent='Interview opened'">Seequent interview</button><p id="result"></p>
      <table role="grid"><tbody><tr role="row" id=":3i"><td>Seequent</td><td><div role="link" tabindex="0" onclick="document.querySelector('#result').textContent='Gmail thread opened'">Rakesh - Senior AI Developer Part I</div></td></tr></tbody></table>
      <button class="duplicate">Same</button><button class="duplicate">Same</button><div aria-hidden="true" id="sparse">Sparse text</div>`);
  }
}).listen(0, '127.0.0.1');
await once(site, 'listening');
const child = spawn(process.env.KAGEHA_BROWSER_BIN, ['--state-dir', path.join(root, 'browser'), '--instance-id', 'reliability', '--application-id', 'fixture', '--standalone', 'on', '--chromium', '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', '--http-listen', `127.0.0.1:${port}`, '--http-token-file', path.join(root, 'token')], { stdio: ['ignore', 'ignore', 'pipe'] });
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
  const scope = { workspace_id: 'reliability', client_id: 'agent', service_epoch: info.service_epoch };
  async function rpc(method, params = {}, extra = {}) {
    return await (await fetch(base + '/v1/rpc', { method: 'POST', headers, body: JSON.stringify({ ...scope, request_id: randomUUID(), method, params, ...extra }) })).json();
  }
  const launch = await rpc('launch_browser');
  assert.equal(launch.status, 'ok', JSON.stringify(launch));
  const browser_id = launch.result.browser_id;
  const tab = await rpc('open_tab', { browser_id, url: `http://127.0.0.1:${site.address().port}/` });
  assert.equal(tab.status, 'ok', JSON.stringify(tab));
  await pause(300);
  const selection = (await rpc('browser_selection')).result;
  const target_id = tab.result.target_id;
  const select = { browser_id, target_id, binding_id: selection.binding_id };
  const fields = await rpc('query_tab', { selection: select, selector: '#fixture-email, #fixture-secret, #fixture-country', limit: 3 });
  assert.equal(fields.status, 'ok', JSON.stringify(fields));
  const [email, secret, country] = fields.result.matches.items;
  assert.equal(email.value, 'test@example.com');
  assert.equal(email.validity.valid, true);
  assert.equal(email.value_truncated, false);
  assert.equal(country.value, 'ca');
  assert.equal(secret.secure_input, true);
  const inspectedPassword = fields.result.observation.controls.find(control => control.id === 'fixture-secret');
  assert.equal(inspectedPassword?.secure_input, true, JSON.stringify(fields.result.observation.controls));
  assert.equal(inspectedPassword?.input_type, 'password');
  assert.equal(secret.value, undefined);
  assert(!JSON.stringify(fields).includes('never-expose'));
  const otp = await rpc('query_tab', { selection: select, selector: '#fixture-otp' });
  assert.equal(otp.status, 'ok');
  assert.equal(otp.result.matches.items[0].one_time_code_input, true);
  assert.equal(otp.result.matches.items[0].has_value, true);
  assert.equal(otp.result.matches.items[0].value, undefined);
  assert(!JSON.stringify(otp).includes('938271'));
  const otherOtp = await rpc('query_tab', { selection: select, selector: '#fixture-otp-other' });
  assert.equal(otherOtp.status, 'ok');
  assert.equal(otherOtp.result.matches.items[0].one_time_code_input, true);
  assert.equal(otherOtp.result.matches.items[0].value, undefined);
  assert(!JSON.stringify(otherOtp).includes('724719'));
  const genericOtp = await rpc('query_tab', { selection: select, selector: '#generic-otp' });
  assert.equal(genericOtp.result.matches.items[0].one_time_code_input, true);
  assert(!JSON.stringify(genericOtp).includes('375918'));
  const promo = await rpc('query_tab', { selection: select, selector: '#promo-code' });
  assert.equal(promo.result.matches.items[0].value, 'SALE');
  const otpSnapshot = await rpc('observe_tab', { browser_id, target_id, compact: false });
  assert.equal(otpSnapshot.status, 'ok');
  assert(!JSON.stringify(otpSnapshot.result).includes('938271'), 'snapshot exposed a verification code');
  assert(!JSON.stringify(otpSnapshot.result).includes('724719'), 'snapshot exposed a labeled verification code');
  assert(!JSON.stringify(otpSnapshot.result).includes('375918'), 'snapshot exposed a generic verification code');
  const inspected = await rpc('query_tab', { selection: select, selector: '#message', limit: 1 });
  assert.equal(inspected.status, 'ok', JSON.stringify(inspected));
  const target = inspected.result.matches.items[0].ref;
  assert(target.startsWith('backend='), JSON.stringify(inspected));
  const grant = await rpc('claim_control');
  const clicked = await rpc('click_observed', { browser_id, target_id, observation_id: inspected.result.observation_id, target, expected_selector: '#result', expected_text: 'Interview opened' }, { authority: grant.result.authority });
  assert.equal(clicked.status, 'ok', JSON.stringify(clicked));
  const stale = await rpc('click_observed', { browser_id, target_id, observation_id: inspected.result.observation_id, target }, { authority: grant.result.authority });
  assert.equal(stale.status, 'error');
  assert.equal(stale.execution_phase, 'not_dispatched');
  // Recorded arguments from session 04a7cf4e1024, unchanged apart from selection.
  for (const args of [
    { selector: '[role="row"]', text: 'Seequent' },
    { selector: '#\\:3i', text: 'Rakesh - Senior AI Developer Part I' },
    { selector: 'tbody tr[role="row"] td div[role="link"]', text: 'Senior AI Developer Part I' },
    { selector: '#\\:3i > td > div[role="link"]' },
    { selector: 'body', text: 'AI' },
  ]) {
    const result = await rpc('query_tab', { selection: select, ...args });
    assert.equal(result.status, 'ok', JSON.stringify(result));
    assert(result.result.matches.items.length > 0, JSON.stringify(result));
    assert(result.result.matches.items.every(item => item.text.length <= 800));
  }
  const duplicate = await rpc('query_tab', { selection: select, selector: '.duplicate' });
  assert.equal(duplicate.result.matches.items.length, 2);
  assert.notEqual(duplicate.result.matches.items[0].ref, duplicate.result.matches.items[1].ref);
  const sparse = await rpc('query_tab', { selection: select, selector: '#sparse' });
  assert.equal(sparse.status, 'ok');
  const row = await rpc('query_tab', { selection: select, selector: '#\\:3i > td > div[role="link"]' });
  const opened = await rpc('click_observed', { browser_id, target_id, observation_id: row.result.observation_id, target: row.result.matches.items[0].ref, expected_selector: '#result', expected_text: 'Gmail thread opened' }, { authority: grant.result.authority });
  assert.equal(opened.status, 'ok', JSON.stringify(opened));
  const beforeNavigation = await rpc('query_tab', { selection: select, selector: '#message' });
  const navigated = await rpc('navigate_tab', { browser_id, target_id, observation_id: beforeNavigation.result.observation_id, url: `http://127.0.0.1:${site.address().port}/delayed` }, { authority: grant.result.authority });
  assert.equal(navigated.status, 'ok', JSON.stringify(navigated));
  const invalidated = await rpc('click_observed', { browser_id, target_id, observation_id: beforeNavigation.result.observation_id, target: beforeNavigation.result.matches.items[0].ref }, { authority: grant.result.authority });
  assert.equal(invalidated.status, 'error');
  assert.equal(invalidated.execution_phase, 'not_dispatched');
  const research = await rpc('read_page', { backend: 'chromium', url: `http://127.0.0.1:${site.address().port}/delayed`, max_chars: 8000 });
  assert.equal(research.status, 'ok', JSON.stringify(research));
  assert(research.result.text.includes('Delayed research content'), JSON.stringify(research));
  const redirected = await rpc('read_page', { backend: 'chromium', url: `http://127.0.0.1:${site.address().port}/redirect` });
  assert.equal(redirected.status, 'ok', JSON.stringify(redirected));
  assert(redirected.result.url.endsWith('/delayed'));
  for (const [route, code] of [['empty', 'research_unreadable'], ['missing', 'research_source_unavailable']]) {
    const result = await rpc('read_page', { backend: 'chromium', url: `http://127.0.0.1:${site.address().port}/${route}` }, { timeout_ms: 3000 });
    assert.equal(result.status, 'error', JSON.stringify(result));
    assert.equal(result.error.code, code, JSON.stringify(result));
  }
  for (const deadline = Date.now() + 3000; ; ) {
    const profiles = fs.readdirSync(path.join(root, 'browser')).filter(name => name.startsWith('chromium-'));
    if (profiles.length <= 1) break;
    assert(Date.now() < deadline, `Temporary research profiles leaked: ${profiles.length}`);
    await pause(25);
  }
  console.log('Passed recorded Gmail inspections, oversized body text, distinct duplicate refs, sparse AX, row click, stale refs, delayed empty-title pages, redirects, unreadable pages, and unavailable sources.');
} finally {
  if (child.exitCode !== null || child.signalCode !== null) console.error('Browser exited unexpectedly:', child.exitCode, child.signalCode, errors);
  child.kill('SIGTERM');
  await Promise.race([once(child, 'exit'), pause(2000)]);
  if (child.exitCode === null) child.kill('SIGKILL');
  site.closeAllConnections();
  await new Promise(resolve => site.close(resolve));
  fs.rmSync(root, { recursive: true, force: true });
}
