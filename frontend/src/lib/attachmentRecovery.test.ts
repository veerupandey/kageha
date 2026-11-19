import { afterEach, expect, it, vi } from "vitest";
import { createStore } from "zustand/vanilla";
import type { AppState } from "../store/types";
import { startAttachmentRecovery, type attachmentRecord } from "./attachmentRecovery";

function composer(sessionId: string | null = "phone") {
  const store = createStore<AppState>(() => ({sessionId, pendingFiles: [], showToast: vi.fn()} as unknown as AppState));
  store.setState({addPendingFiles: files => store.setState(s => ({pendingFiles: [...s.pendingFiles, ...Array.from(files || [])]}))});
  return store;
}
const flush = async () => { for (let i = 0; i < 12; i++) await Promise.resolve(); };
afterEach(() => { sessionStorage.clear(); });

it("restores photo and document metadata after reload, then removes sent files", async () => {
  let saved: Awaited<ReturnType<typeof attachmentRecord>>;
  const record: typeof attachmentRecord = async (_key, value) => {
    if (value !== undefined) saved = value || undefined;
    return saved;
  };
  const first = composer();
  const stop = await startAttachmentRecovery(first, record);
  first.getState().addPendingFiles([
    new File(["photo"], "photo.png", {type:"image/png", lastModified:123}),
    new File(["document"], "brief.pdf", {type:"application/pdf", lastModified:456}),
  ]);
  await flush();
  stop();
  const reloaded = composer();
  const stopReloaded = await startAttachmentRecovery(reloaded, record);
  expect(reloaded.getState().pendingFiles.map(f => [f.name, f.type, f.size, f.lastModified])).toEqual([
    ["photo.png", "image/png", 5, 123], ["brief.pdf", "application/pdf", 8, 456],
  ]);
  reloaded.setState({pendingFiles: []});
  await flush();
  expect(saved).toBeUndefined();
  stopReloaded();
});

it("does not restore another session's files or overwrite changes made during recovery", async () => {
  const file = new File(["old"], "old.txt");
  const saved = {sessionId:"other", savedAt:Date.now(),files:[{blob:file,name:file.name,type:file.type,lastModified:file.lastModified}]};
  const store = composer();
  const stop = await startAttachmentRecovery(store, async () => saved);
  expect(store.getState().pendingFiles).toEqual([]);
  stop();
  let resolve!: (value: typeof saved) => void;
  const pending = startAttachmentRecovery(store, vi.fn().mockReturnValueOnce(new Promise(r => {resolve=r;})).mockResolvedValue(undefined));
  const fresh = new File(["new"], "new.txt");
  store.getState().addPendingFiles([fresh]);
  resolve({...saved,sessionId:"phone"});
  (await pending)();
  expect(store.getState().pendingFiles).toEqual([fresh]);
});

it("keeps files usable and reports unavailable browser storage", async () => {
  const store = composer();
  const stop = await startAttachmentRecovery(store, async () => {throw new Error("quota");});
  store.getState().addPendingFiles([new File(["x"], "x.txt")]);
  await flush();
  expect(store.getState().pendingFiles).toHaveLength(1);
  expect(store.getState().showToast).toHaveBeenCalledTimes(1);
  stop();
});
