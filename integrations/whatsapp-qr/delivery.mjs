import { createHash } from "node:crypto";

export function deliveryMessageId(queueId, part) {
  return `3EB0${createHash("sha256").update(`${queueId}:${part}`).digest("hex").slice(0, 18).toUpperCase()}`;
}

/** Persist the actual protocol body before relay; replay uses the same ID. */
export class ReliableSender {
  constructor(store, generate) {
    this.store = store;
    this.generate = generate;
    this.pending = new Map();
  }
  async send(sock, jid, payload, queueId, part) {
    const id = deliveryMessageId(queueId, part);
    const key = { remoteJid: jid, id };
    const existing = this.pending.get(id);
    if (existing) return existing;
    const work = this._send(sock, jid, payload, key);
    this.pending.set(id, work);
    try { return await work; } finally { this.pending.delete(id); }
  }
  async _send(sock, jid, payload, key) {
    const saved = this.store.entry(key);
    if (saved?.accepted) return { key, message: saved.message };
    const full = saved ? { key, message: saved.message } : await this.generate(sock, jid, payload, key.id);
    this.store.put(full.key, full.message, { accepted: false });
    await sock.relayMessage(jid, full.message, { messageId: key.id });
    this.store.put(full.key, full.message, { accepted: true });
    return full;
  }
}
