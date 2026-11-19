import fs from "node:fs";
import path from "node:path";

/** Sent message bodies must survive restarts so WhatsApp can request a resend. */
export class RetryMessageStore {
  constructor(directory, codec, { limit = 500, ttlMs = 7 * 86400000 } = {}) {
    this.file = path.join(directory, "sent-message-retries.json");
    this.codec = codec;
    this.limit = limit;
    this.ttlMs = ttlMs;
    this.messages = new Map();
    try {
      const rows = JSON.parse(fs.readFileSync(this.file, "utf8"), codec.reviver);
      if (!Array.isArray(rows)) throw new Error("Invalid retry cache");
      for (const row of rows) {
        if (row?.id && row.message && row.at + ttlMs > Date.now()) this.messages.set(row.id, row);
      }
    } catch (error) {
      if (error.code !== "ENOENT") {
        // Quarantine only the message cache. Encryption credentials stay intact.
        fs.renameSync(this.file, `${this.file}.corrupt-${Date.now()}`);
        this.messages.clear();
      }
    }
  }
  key(key) { return `${key.remoteJid || ""}:${key.id || ""}`; }
  get(key) { return this.messages.get(this.key(key))?.message; }
  entry(key) { return this.messages.get(this.key(key)); }
  hasId(id) { return [...this.messages.values()].some((row) => row.messageId === id); }
  put(key, message, metadata = {}) {
    if (!key?.id || !message) return;
    const id = this.key(key);
    const previous = this.messages.get(id) || {};
    this.messages.delete(id);
    this.messages.set(id, { ...previous, id, messageId: key.id, at: Date.now(), message, ...metadata });
    for (const [storedId, row] of this.messages) {
      if (row.at + this.ttlMs <= Date.now() || this.messages.size > this.limit) this.messages.delete(storedId);
    }
    fs.mkdirSync(path.dirname(this.file), { recursive: true });
    const temporary = `${this.file}.tmp`;
    fs.writeFileSync(temporary, JSON.stringify([...this.messages.values()], this.codec.replacer), { mode: 0o600 });
    fs.renameSync(temporary, this.file);
  }
}
