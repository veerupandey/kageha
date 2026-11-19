/** Recovery records are independent: one tab never rewrites another request's text. */
import type { QueuedMessage } from "../api/types";
const LEGACY = "kageha.delivery.v1";
const PREFIX = "kageha.delivery.v2.";
const TTL = 7 * 86400_000;
export type PendingDelivery = { id: string; sessionId: string; text: string; createdAt: number; state: "pending" | "received" | "cancelled" };
const volatile = new Map<string, string>();
const unpersisted = new Set<string>();
function keys(): string[] {
  try {
    if (typeof localStorage.length !== "number" || typeof localStorage.key !== "function") throw new Error("Storage unavailable");
    return [...new Set([...Array.from({ length: localStorage.length }, (_, i) => localStorage.key(i)).filter((key): key is string => Boolean(key)), ...unpersisted])];
  } catch { return [...volatile.keys()]; }
}
function read(key: string): unknown {
  try {
    let raw: string | null;
    try { raw = unpersisted.has(key) ? volatile.get(key) || null : localStorage.getItem(key); } catch { raw = volatile.get(key) || null; }
    return raw ? JSON.parse(raw) : null;
  } catch { return null; }
}
function write(key: string, value: unknown): boolean {
  const raw = JSON.stringify(value);
  volatile.set(key, raw);
  try { localStorage.setItem(key, raw); unpersisted.delete(key); return true; } catch { unpersisted.add(key); return false; }
}
function remove(key: string) {
  volatile.delete(key); unpersisted.delete(key);
  try { localStorage.removeItem(key); } catch { /* In-memory recovery still works. */ }
}
function pendingKey(id: string) { return PREFIX + "pending." + id; }
function stateKey(id: string) { return PREFIX + "state." + id; }
function ackKey(id: string) { return PREFIX + "ack." + id; }
function queueKey(sessionId: string) { return PREFIX + "queue." + encodeURIComponent(sessionId); }
function deletedKey(sessionId: string) { return PREFIX + "deleted." + encodeURIComponent(sessionId); }
function valid(item: unknown): item is PendingDelivery {
  const x = item as PendingDelivery | null;
  return Boolean(x && typeof x.id === "string" && typeof x.sessionId === "string" && typeof x.text === "string"
    && Number.isFinite(x.createdAt) && x.createdAt <= Date.now() && Date.now() - x.createdAt < TTL);
}
function migrate() {
  // Tombstones/status are metadata only, but must not grow without bound.
  for (const key of keys().filter(k => k.startsWith(PREFIX))) {
    const item = read(key) as {savedAt?:number;createdAt?:number} | null;
    const timestamp = item?.savedAt ?? item?.createdAt;
    if (typeof timestamp === "number" && Date.now() - timestamp >= TTL) remove(key);
  }
  const saved = read(LEGACY) as { pending?: unknown[]; queues?: Record<string, unknown> } | null;
  if (!saved || typeof saved !== "object") return;
  let persisted = true;
  for (const item of Array.isArray(saved.pending) ? saved.pending : []) {
    if (valid(item) && !read(deletedKey(item.sessionId)) && !read(pendingKey(item.id)) && !read(ackKey(item.id))) {
      persisted = write(pendingKey(item.id), item) && persisted;
      if (read(ackKey(item.id)) || read(deletedKey(item.sessionId))) remove(pendingKey(item.id));
    }
  }
  if (saved.queues && typeof saved.queues === "object") {
    for (const [sid, queue] of Object.entries(saved.queues)) {
      if (Array.isArray(queue) && !read(deletedKey(sid)) && !read(queueKey(sid))) persisted = write(queueKey(sid), { sessionId: sid, savedAt: Date.now(), items: queue }) && persisted;
    }
  }
  if (persisted) remove(LEGACY);
}
export function rememberDelivery(item: PendingDelivery): boolean {
  migrate();
  // Text is immutable after admission. Status writes never recreate removed text.
  if (read(deletedKey(item.sessionId)) || read(ackKey(item.id))) return false;
  const key = pendingKey(item.id);
  const persisted = read(key) ? !unpersisted.has(key) : write(key, item);
  if (read(ackKey(item.id)) || read(deletedKey(item.sessionId))) remove(key);
  return persisted;
}
export function markDelivery(id: string, state: PendingDelivery["state"]) {
  migrate();
  const item = read(pendingKey(id)) as PendingDelivery | null;
  if (item && !read(ackKey(id))) write(stateKey(id), { state, sessionId: item.sessionId, savedAt: Date.now() });
}
export function acknowledgeDelivery(id: string) {
  migrate();
  if (read(ackKey(id))) { remove(pendingKey(id)); remove(stateKey(id)); return; }
  const item = read(pendingKey(id)) as PendingDelivery | null;
  // A tombstone prevents a stale state update or v1 migration resurrecting a reply.
  write(ackKey(id), { sessionId: item?.sessionId, savedAt: Date.now() });
  remove(pendingKey(id)); remove(stateKey(id));
}
export function pendingDeliveries(sessionId: string): PendingDelivery[] {
  migrate();
  if (read(deletedKey(sessionId))) return [];
  const result: PendingDelivery[] = [];
  for (const key of keys().filter(k => k.startsWith(PREFIX + "pending."))) {
    const item = read(key);
    if (!valid(item)) { remove(key); continue; }
    if (read(ackKey(item.id))) { remove(key); continue; }
    if (item.sessionId !== sessionId) continue;
    const state = read(stateKey(item.id)) as { state?: PendingDelivery["state"] } | null;
    result.push({ ...item, state: state?.state || item.state });
  }
  return result.sort((a, b) => a.createdAt - b.createdAt);
}
export function rememberQueue(sessionId: string, queue: QueuedMessage[]) {
  migrate();
  if (read(deletedKey(sessionId))) return;
  if (!queue.length) remove(queueKey(sessionId));
  else write(queueKey(sessionId), { sessionId, savedAt: Date.now(), items: queue.slice(0, 50).map(x => ({ text: x.text, needsFiles: Boolean(x.needsFiles || x.files.length) })) });
}
export function restoredQueue(sessionId: string): QueuedMessage[] {
  migrate();
  if (read(deletedKey(sessionId))) return [];
  const saved = read(queueKey(sessionId)) as { savedAt?: number; items?: unknown[] } | null;
  if (!saved || !Number.isFinite(saved.savedAt) || Number(saved.savedAt) > Date.now() || Date.now() - Number(saved.savedAt) >= TTL) return [];
  return (Array.isArray(saved.items) ? saved.items : []).filter((x): x is {text:string;needsFiles?:boolean} => Boolean(x && typeof (x as {text?:unknown}).text === "string"))
    .map(x => ({ text: x.text, files: [], needsFiles: Boolean(x.needsFiles), needsReview: true }));
}
export function clearDeliverySession(sessionId: string) {
  migrate();
  write(deletedKey(sessionId), { sessionId, savedAt: Date.now() });
  for (const key of keys().filter(k => k.startsWith(PREFIX) && k !== deletedKey(sessionId))) {
    const item = read(key) as {sessionId?:string} | null;
    if (item?.sessionId === sessionId) remove(key);
  }
}
