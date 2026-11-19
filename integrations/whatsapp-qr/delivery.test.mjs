import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { BufferJSON } from '@whiskeysockets/baileys';
import { ReliableSender, deliveryMessageId } from './delivery.mjs';
import { RetryMessageStore } from './retry-store.mjs';
const generate = async (_sock, jid, payload, id) => ({ key: { remoteJid: jid, id }, message: { conversation: payload.text } });
function setup() {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'wa-delivery-'));
  return { directory, store: new RetryMessageStore(directory, BufferJSON) };
}
test('a lost acknowledgment and restart do not create another send', async () => {
  const { directory, store } = setup();
  try {
    let relays = 0;
    const sock = { relayMessage: async () => { relays++; } };
    await new ReliableSender(store, generate).send(sock, '120123@g.us', { text: 'hello' }, 'queue1', 'text');
    const restored = new ReliableSender(new RetryMessageStore(directory, BufferJSON), generate);
    const result = await restored.send(sock, '120123@g.us', { text: 'hello' }, 'queue1', 'text');
    assert.equal(relays, 1);
    assert.equal(result.key.id, deliveryMessageId('queue1', 'text'));
  } finally { fs.rmSync(directory, { recursive: true, force: true }); }
});
test('an uncertain relay retries the persisted body with the same protocol ID', async () => {
  const { directory, store } = setup();
  try {
    const ids = [];
    const failing = { relayMessage: async (_jid, _msg, options) => { ids.push(options.messageId); throw Error('socket closed'); } };
    await assert.rejects(new ReliableSender(store, generate).send(failing, '120123@g.us', { text: 'hello' }, 'queue2', 'text'));
    const recovered = { relayMessage: async (_jid, msg, options) => { ids.push(options.messageId); assert.equal(msg.conversation, 'hello'); } };
    await new ReliableSender(new RetryMessageStore(directory, BufferJSON), generate).send(recovered, '120123@g.us', { text: 'changed input' }, 'queue2', 'text');
    assert.equal(ids[0], ids[1]);
  } finally { fs.rmSync(directory, { recursive: true, force: true }); }
});
test('concurrent retries share one relay', async () => {
  const { directory, store } = setup();
  try {
    let relays = 0;
    const sock = { relayMessage: async () => { relays++; await new Promise(resolve => setTimeout(resolve, 10)); } };
    const sender = new ReliableSender(store, generate);
    await Promise.all([sender.send(sock, '120123@g.us', { text: 'hello' }, 'queue3', 'text'), sender.send(sock, '120123@g.us', { text: 'hello' }, 'queue3', 'text')]);
    assert.equal(relays, 1);
  } finally { fs.rmSync(directory, { recursive: true, force: true }); }
});
test('cache refresh preserves delivery receipts', () => {
  const { directory, store } = setup();
  try {
    const key = { remoteJid: '120123@g.us', id: 'sent1' };
    store.put(key, { conversation: 'hello' }, { accepted: true });
    store.put(key, { conversation: 'hello' });
    assert.equal(new RetryMessageStore(directory, BufferJSON).entry(key).accepted, true);
  } finally { fs.rmSync(directory, { recursive: true, force: true }); }
});
test('a damaged retry cache is preserved without touching encryption sessions', () => {
  const { directory } = setup();
  try {
    fs.writeFileSync(path.join(directory, 'sent-message-retries.json'), '{bad json');
    fs.writeFileSync(path.join(directory, 'session-peer.json'), 'encryption-state');
    const restored = new RetryMessageStore(directory, BufferJSON);
    assert.equal(restored.messages.size, 0);
    assert.ok(fs.readdirSync(directory).some(name => name.includes('.corrupt-')));
    assert.equal(fs.readFileSync(path.join(directory, 'session-peer.json'), 'utf8'), 'encryption-state');
  } finally { fs.rmSync(directory, { recursive: true, force: true }); }
});
