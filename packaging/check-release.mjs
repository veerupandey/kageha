// Exercise the archive as a new user, outside the checkout and without a paid provider.
import assert from 'node:assert/strict';
import { execFileSync, spawn } from 'node:child_process';
import { once } from 'node:events';
import fs from 'node:fs';
import http from 'node:http';
import https from 'node:https';
import os from 'node:os';
import path from 'node:path';
import { setTimeout as sleep } from 'node:timers/promises';
import { credentialFixture } from '../crates/kageha-loop/tests/credential_fixture.mjs';

const archive = path.resolve(process.argv[2]);
const home = fs.mkdtempSync(path.join(os.tmpdir(), 'kageha release '));
const bundle = path.join(home, path.basename(archive, '.tar.gz'));
const reply = 'Your first Kageha chat is ready.';
const provider = http.createServer((request, response) => {
  request.resume();
  response.writeHead(200, {'Content-Type': 'application/json'});
  response.end(JSON.stringify({choices: [{message: {role: 'assistant', content: reply}}]}));
});
const env = { HOME: home, PATH: '/usr/bin:/bin', KAGEHA_WEBUI_PORT: '0' };
const testTls = process.env.KAGEHA_TEST_HTTPS === '1';
let child, done, base, cookie, proxy, ca;

async function transport(url, options = {}) {
  if (!testTls) return fetch(url, options);
  const { body, headers, method, signal } = new Request(url, options);
  const payload = body ? Buffer.from(await new Response(body).arrayBuffer()) : undefined;
  const requestHeaders = Object.fromEntries(headers);
  if (payload) requestHeaders['content-length'] = payload.length;
  return new Promise((resolve, reject) => {
    const request = https.request(url, {method, headers: requestHeaders, ca,
      signal: options.signal ? signal : AbortSignal.timeout(15000)}, response => {
      const chunks = [];
      response.on('data', chunk => chunks.push(chunk));
      response.on('error', reject);
      response.on('end', () => resolve(new Response(Buffer.concat(chunks), {status:response.statusCode,headers:response.headers})));
    });
    request.on('error', reject);
    request.end(payload);
  });
}

async function startProxy(upstream) {
  const listener = http.createServer();
  listener.listen(0, '127.0.0.1');
  await once(listener, 'listening');
  const port = listener.address().port;
  await new Promise(resolve => listener.close(resolve));
  const proxyRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'kageha-nginx-'));
  const cert = path.join(proxyRoot, 'certificate.pem');
  const key = path.join(proxyRoot, 'key.pem');
  execFileSync('openssl',['req','-x509','-newkey','rsa:2048','-nodes','-days','1','-subj','/CN=127.0.0.1',
    '-addext','subjectAltName=IP:127.0.0.1','-out',cert,'-keyout',key],{stdio:'ignore'});
  ca = fs.readFileSync(cert);
  const template = fs.readFileSync(path.join(bundle, 'share/kageha/linux/kageha.nginx.conf'), 'utf8')
    .replace('listen 443 ssl;', `listen 127.0.0.1:${port} ssl;`)
    .replace('kageha.example.org','127.0.0.1')
    .replace('/etc/ssl/kageha/fullchain.pem',cert).replace('/etc/ssl/kageha/privkey.pem',key)
    .replace('127.0.0.1:8788',upstream);
  fs.writeFileSync(path.join(proxyRoot,'nginx.conf'), `pid nginx.pid; error_log stderr; events {} http {
    client_body_temp_path client_body; proxy_temp_path proxy_temp; fastcgi_temp_path fastcgi_temp;
    uwsgi_temp_path uwsgi_temp; scgi_temp_path scgi_temp; access_log off; ${template} }`);
  const nginx = process.env.KAGEHA_NGINX || 'nginx';
  const args = ['-p',proxyRoot,'-c','nginx.conf'];
  try {
    execFileSync(nginx,[...args,'-t'],{stdio:'pipe'});
    const worker = spawn(nginx,[...args,'-g','daemon off;'],{stdio:['ignore','ignore','inherit']});
    proxy = {worker,done:once(worker,'exit'),root:proxyRoot};
    const url = `https://127.0.0.1:${port}`;
    for (let i = 0; i < 60; i++) {
      try { if ((await transport(url + '/login')).ok) return url; } catch {}
      await sleep(50);
    }
    throw new Error('Packaged nginx HTTPS proxy failed to start');
  } catch (error) {
    if (!proxy) fs.rmSync(proxyRoot,{recursive:true,force:true});
    throw error;
  }
}
async function start() {
  let log = '';
  child = spawn(path.join(bundle, 'start'), {cwd: home, env, stdio: ['ignore', 'pipe', 'pipe']});
  done = once(child, 'exit');
  for (const stream of [child.stdout, child.stderr]) stream.on('data', b => { log += b; });
  for (let i = 0; i < 400; i++) {
    const match = log.match(/Rust chat service listening on (127\.0\.0\.1:\d+)/);
    if (match) { base = testTls ? await startProxy(match[1]) : 'http://' + match[1]; return; }
    if (child.exitCode !== null || child.signalCode) break;
    await sleep(25);
  }
  throw Error('Release failed to start: ' + log);
}
async function stop() {
  if (proxy) {
    if (proxy.worker.exitCode === null && !proxy.worker.signalCode) { proxy.worker.kill(); await proxy.done; }
    fs.rmSync(proxy.root,{recursive:true,force:true});
    proxy = undefined;
  }
  if (child && child.exitCode === null && !child.signalCode) {
    const descriptor = path.join(home,'.kageha-local/browser/daemon.json');
    const browserPid = fs.existsSync(descriptor) ? JSON.parse(fs.readFileSync(descriptor,'utf8')).pid : undefined;
    child.kill();
    await done;
    if (browserPid) {
      assert.throws(()=>process.kill(browserPid,0), error=>error.code === 'ESRCH', 'Packaged browser must stop with its launcher');
    }
  }
}
async function api(route, body) {
  const res = await transport(base + route, {
    method: body ? 'POST' : 'GET',
    headers: { Cookie: cookie, 'Content-Type': 'application/json', Origin: base },
    body: body && JSON.stringify(body), signal: AbortSignal.timeout(30000)
  });
  const value = await res.json();
  assert.equal(res.status, 200, JSON.stringify(value));
  return value;
}
try {
  execFileSync('tar', ['-xzf', archive, '-C', home]);
  assert(fs.existsSync(path.join(bundle, 'docs/LINUX_DEPLOYMENT.md')), 'Packaged README must link to the Linux deployment guide');
  // Isolate from the tester's macOS Keychain. Linux exercises first-run key generation.
  if (process.platform === 'darwin') Object.assign(env, credentialFixture(home));
  provider.listen(0, '127.0.0.1');
  await once(provider, 'listening');
  await start();
  const browserDescriptor = path.join(home,'.kageha-local/browser/daemon.json');
  for (let i=0; i<200 && !fs.existsSync(browserDescriptor); i++) await sleep(25);
  assert(fs.existsSync(browserDescriptor), 'Packaged browser service must be ready');
  const token = execFileSync(path.join(bundle, 'start'), ['token'], {env, encoding: 'utf8'}).trim();
  const login = await transport(base + '/login', {
    method: 'POST', body: new URLSearchParams({token}), redirect: 'manual', headers: {'Content-Type':'application/x-www-form-urlencoded'}
  });
  assert.equal(login.status, 303);
  if (testTls) assert.match(login.headers.get('set-cookie'), /;\s*secure/i);
  cookie = login.headers.get('set-cookie').split(';')[0];
  const html = await (await transport(base, {headers: {Cookie: cookie}})).text();
  const script = html.match(/src="([^"]+\.js)"/);
  assert.ok(script, 'Signed-in page must load the compiled web interface');
  assert.equal((await transport(new URL(script[1], base))).status, 200);
  await api('/api/provider-setup/connect', {
    provider_name: 'release-check', protocol: 'openai_compat',
    base_url: 'http://127.0.0.1:' + provider.address().port + '/v1',
    model: 'fixture', model_id: 'release-check/fixture'
  });
  const chat = (await api('/api/sessions', {})).session_id;
  await api('/api/chat/send', {session_id: chat, client_request_id: 'first-chat',
    model: 'release-check/fixture', message: 'Hello!'});
  async function hasReply() {
    return (await api('/api/sessions/' + chat)).messages.some(m => m.text === reply);
  }
  for (let i = 0; i < 200 && !await hasReply(); i++) await sleep(50);
  assert.ok(await hasReply(), 'First chat must get the fixture reply');
  await stop();
  await start();
  assert.ok(await hasReply(), 'Login and chat must survive a restart');
  console.log('Release passed first login, web assets, provider setup, first reply, and restart.');
} finally {
  await stop();
  provider.close();
  fs.rmSync(home, {recursive: true, force: true});
}
