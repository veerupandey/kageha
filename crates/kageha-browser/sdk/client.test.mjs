import { test } from 'node:test';
import assert from 'node:assert/strict';
import { BrowserClient } from './client.mjs';
const info = { api_version: 1, protocol_version: 1, service_epoch: 'epoch1', methods: ['health', 'click_observed'] };
function fixture(handler) {
  const calls = [];
  const client = new BrowserClient({ baseUrl: 'http://127.0.0.1:9999', token: 'private-secret', workspaceId: 'job', clientId: 'agent', fetch: async (url, options) => {
    calls.push({ url, options });
    const value = options.body ? await handler(JSON.parse(options.body)) : info;
    return { ok: true, json: async () => value };
  } });
  return { client, calls };
}
const reply = request => ({ ...request, protocol_version: 1, status: 'ok', execution_phase: 'not_dispatched', result: {} });
test('scope and epoch stay outside model arguments; token never serialized', async () => {
  const { client, calls } = fixture(reply);
  await client.connect();
  const result = await client.call('health', {}, { requestId: 'stable-id' });
  assert.equal(result.workspace_id, 'job');
  assert.equal(result.service_epoch, 'epoch1');
  assert.equal(calls[1].options.headers.Authorization, 'Bearer private-secret');
  assert(!JSON.stringify(client).includes('private-secret'));
  assert.equal(calls[1].options.redirect, 'error');
  assert(!('workspace_id' in client.tool().inputSchema.properties));
});
test('unknown outcome is never retried', async () => {
  const { client, calls } = fixture(() => { throw new Error('Connection lost'); });
  await client.connect();
  await assert.rejects(client.call('click_observed'), /Connection lost/);
  assert.equal(calls.length, 2);
});
test('native action error preserves receipt and dispatch status', async () => {
  const { client } = fixture(request => ({ ...reply(request), status: 'error', execution_phase: 'dispatched', error: { code: 'outcome_uncertain' }, result: { receipt_id: 'receipt1' } }));
  await client.connect();
  const result = await client.call('click_observed');
  assert.equal(result.result.receipt_id, 'receipt1');
  assert.equal(result.execution_phase, 'dispatched');
});
test('rejects stale reply identity and unsupported method before dispatch', async () => {
  const { client, calls } = fixture(request => ({ ...reply(request), service_epoch: 'epoch2' }));
  await client.connect();
  await assert.rejects(client.call('health'), /identity mismatch/);
  await assert.rejects(client.call('shutdown'), /unavailable/);
  await assert.rejects(client.call('health', {}, { timeoutMs: 30001 }), /timeoutMs/);
  assert.equal(calls.length, 2);
});
test('reconnect never silently adopts a restarted daemon', async () => {
  let epoch = 'one';
  const client = new BrowserClient({ baseUrl: 'https://browser.example', token: 'secret', workspaceId: 'w', clientId: 'c', fetch: async () => ({ ok: true, json: async () => ({ ...info, service_epoch: epoch }) }) });
  await client.connect();
  epoch = 'two';
  await assert.rejects(client.connect(), /restarted/);
  assert.equal(client.info.service_epoch, 'one');
});
