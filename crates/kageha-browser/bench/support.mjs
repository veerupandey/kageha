import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import net from 'node:net';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { createInterface } from 'node:readline';
import { randomBytes, randomUUID } from 'node:crypto';
import { BrowserClient } from '../sdk/client.mjs';
export const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
export async function until(read, timeout = 4000) {
  const end = Date.now() + timeout;
  let last;
  while (Date.now() < end) { try { const value = await read(); if (value) return value; } catch (error) { last = error; } await sleep(20); }
  throw last ?? Error('Condition timed out');
}
export function checked(reply) { if (reply.status !== 'ok') throw Error(JSON.stringify({ error: reply.error, phase: reply.execution_phase, result: reply.result })); return reply.result; }
export async function runtime(transport) {
  const binary = process.env.KAGEHA_BROWSER_BIN;
  if (!binary || !path.isAbsolute(binary)) throw Error('Set absolute KAGEHA_BROWSER_BIN');
  const chromium = process.env.CHROMIUM_BIN ?? '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'kbr-bench-'));
  const token = randomBytes(32).toString('hex');
  const tokenFile = path.join(root, 'api.token'); fs.writeFileSync(tokenFile, token, { mode: 0o600 });
  const args = ['--state-dir', path.join(root, 'state'), '--instance-id', 'benchmark', '--application-id', 'benchmark', '--chromium', chromium, '--standalone', 'on'];
  let base;
  if (transport === 'http') {
    const probe = net.createServer(); probe.listen(0, '127.0.0.1'); await once(probe, 'listening');
    const port = probe.address().port; await new Promise(resolve => probe.close(resolve));
    base = `http://127.0.0.1:${port}`;
    args.push('--http-listen', `127.0.0.1:${port}`, '--http-token-file', tokenFile);
  } else args.push('--mcp', 'on', '--mcp-workspace', 'benchmark', '--mcp-client', 'benchmark-agent');
  const child = spawn(binary, args, { stdio: ['pipe', 'pipe', 'pipe'] });
  let stderr = ''; child.stderr.on('data', value => { stderr = (stderr + value).slice(-4096); });
  let failure; child.on('error', error => { failure = error; });
  let client, request, reader;
  const pending = new Map(); let id = 0;
  const latencies = [];
  let remaining = Infinity;
  try {
    if (transport === 'http') {
      client = new BrowserClient({ baseUrl: base, token, workspaceId: 'benchmark', clientId: 'benchmark-agent' });
      await until(async () => { if (failure) throw failure; if (child.exitCode !== null) throw Error(stderr); return client.connect(); }, 10000);
    } else {
      reader = createInterface({ input: child.stdout });
      reader.on('line', line => { const value = JSON.parse(line); const waiter = pending.get(value.id); if (waiter) { clearTimeout(waiter.timer); pending.delete(value.id); waiter.resolve(value); } });
      request = (method, params = {}) => new Promise((resolve, reject) => {
        const serial = ++id; const timer = setTimeout(() => { pending.delete(serial); reject(Error(`MCP timeout: ${method} ${stderr}`)); }, 35000);
        pending.set(serial, { resolve, timer }); child.stdin.write(JSON.stringify({ jsonrpc: '2.0', id: serial, method, params }) + '\n');
      });
      const init = await request('initialize', { protocolVersion: '2024-11-05', capabilities: {}, clientInfo: { name: 'benchmark', version: '1' } });
      if (init.error) throw Error(JSON.stringify(init.error));
      child.stdin.write(JSON.stringify({ jsonrpc: '2.0', method: 'notifications/initialized' }) + '\n');
      const info = await request('tools/call', { name: 'browser_info', arguments: {} });
      client = { info: JSON.parse(info.result.content[0].text), async call(method, params, options) {
        const response = await request('tools/call', { name: 'browser_call', arguments: { method, params, request_id: options.requestId ?? randomUUID(), service_epoch: this.info.service_epoch, authority: options.authority ?? null, timeout_ms: options.timeoutMs ?? 10000 } });
        if (response.error) throw Error(JSON.stringify(response.error));
        return JSON.parse(response.result.content[0].text);
      } };
    }
  } catch (error) { child.kill('SIGKILL'); fs.rmSync(root, { recursive: true, force: true }); throw error; }
  return {
    info: client.info, latencies, stateDir: path.join(root, 'state'),
    setBudget(calls) { remaining = calls; },
    async call(method, params = {}, options = {}) {
      if (remaining-- <= 0) throw Error('Benchmark exceeded RPC step budget');
      const start = performance.now();
      try { return await client.call(method, params, options); }
      finally { latencies.push({ method, ms: performance.now() - start }); }
    },
    async close() {
      for (const waiter of pending.values()) clearTimeout(waiter.timer);
      const hasExited=()=>child.exitCode!==null||child.signalCode!==null;
      if(!hasExited()){
        const exited = once(child, 'exit');
        if (transport === 'mcp') child.stdin.end(); else child.kill('SIGINT');
        await Promise.race([exited, sleep(2000)]);
        if (!hasExited()) { child.kill('SIGKILL'); await exited; }
      }
      reader?.close(); fs.rmSync(root, { recursive: true, force: true });
    },
  };
}
