import type { StoreApi } from "zustand";
import type { AppState } from "../store/types";

type SavedAttachments = {
  sessionId: string | null;
  savedAt: number;
  files: Array<{ blob: Blob; name: string; type: string; lastModified: number }>;
};
const MAX_AGE = 7 * 24 * 60 * 60 * 1000;

async function openDatabase(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const request = indexedDB.open("kageha-composer", 1);
    request.onupgradeneeded = () => request.result.createObjectStore("attachments");
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
    request.onblocked = () => reject(new Error("Attachment storage is busy"));
  });
}

export async function attachmentRecord(key: string, value?: SavedAttachments | null): Promise<SavedAttachments | undefined> {
  const db = await openDatabase();
  try {
    return await new Promise((resolve, reject) => {
      const tx = db.transaction("attachments", value === undefined ? "readonly" : "readwrite");
      const store = tx.objectStore("attachments");
      const request = value === undefined ? store.get(key) : value === null ? store.delete(key) : store.put(value, key);
      tx.oncomplete = () => resolve(value === undefined ? request.result : undefined);
      tx.onerror = () => reject(tx.error);
      tx.onabort = () => reject(tx.error || new Error("Attachment storage unavailable"));
    });
  } finally { db.close(); }
}

/** Start after session boot so restoring history cannot erase recovered files. */
export async function startAttachmentRecovery(
  store: Pick<StoreApi<AppState>, "getState" | "subscribe">,
  record = attachmentRecord,
): Promise<() => void> {
  let key: string;
  try {
    key = sessionStorage.getItem("kageha.attachmentTab") || `tab-${Date.now()}-${Math.random().toString(36).slice(2)}`;
    sessionStorage.setItem("kageha.attachmentTab", key);
  } catch { return () => {}; }
  const initial = store.getState();
  let changed = false;
  let ready = false;
  let warned = false;
  let writes = Promise.resolve();
  const warn = () => {
    if (warned) return;
    warned = true;
    store.getState().showToast("This browser could not save attachments for reload recovery. Keep this page open until sending finishes.");
  };
  const save = (state: AppState) => {
    const value: SavedAttachments | null = state.pendingFiles.length ? {
      sessionId: state.sessionId, savedAt: Date.now(),
      files: state.pendingFiles.map(file => ({ blob: file, name: file.name, type: file.type, lastModified: file.lastModified })),
    } : null;
    // Serialize writes so a slow photo save cannot resurrect a removed file.
    writes = writes.then(() => record(key, value)).then(() => {}, warn);
  };
  const unsubscribe = store.subscribe((state, previous) => {
    if (state.pendingFiles === previous.pendingFiles && state.sessionId === previous.sessionId) return;
    changed = true;
    if (ready) save(state);
  });
  try {
    const saved = await record(key);
    if (!changed && saved?.sessionId === initial.sessionId && Date.now() - saved.savedAt < MAX_AGE) {
      const files = saved.files.map(file => new File([file.blob], file.name, { type: file.type, lastModified: file.lastModified }));
      store.getState().addPendingFiles(files);
    }
    ready = true;
    // A failed/offline boot may not have restored the saved session yet.
    // Keep that record instead of replacing it with the empty home screen.
    if (changed || !saved || saved.sessionId === initial.sessionId) save(store.getState());
  } catch {
    ready = true;
    warn();
  }
  return unsubscribe;
}
