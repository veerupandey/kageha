import { useEffect, useId, useRef, useState } from "react";
import { api } from "../api/client";
import { refreshSessionTruth } from "../lib/sessionTruth";
import { useAppStore } from "../store";

interface Source {
  chat_id: string;
  title: string | null;
  updated_at: number;
  path: string;
  filename: string;
  sha256: string;
}

export function PastChatAttachments({ sessionId }: { sessionId: string }) {
  const titleId = useId();
  const dialog = useRef<HTMLDialogElement>(null);
  const [query, setQuery] = useState("");
  const [files, setFiles] = useState<{ filename: string; sources: Source[] }[]>([]);
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [loading, setLoading] = useState(false);
  const [notice, setNotice] = useState("");
  const [truncated, setTruncated] = useState(false);
  const endpoint = `/api/sessions/${encodeURIComponent(sessionId)}/files/from-past-chats`;

  useEffect(() => {
    if (!open) return;
    const controller = new AbortController();
    setLoading(true);
    setFiles([]);
    setTruncated(false);
    setNotice("");
    const timer = setTimeout(() => {
      void api<{ files: typeof files; truncated: boolean }>(`${endpoint}?q=${encodeURIComponent(query)}`, {
        signal: controller.signal,
      }).then((result) => {
        if (controller.signal.aborted) return;
        if (!Array.isArray(result?.files) || result.files.some(file => !file || !Array.isArray(file.sources))) {
          throw new Error("Past-chat attachments are unavailable from this server.");
        }
        setFiles(result.files);
        setTruncated(result.truncated);
      }).catch((error: Error) => {
        if (!controller.signal.aborted) setNotice(error.message);
      }).finally(() => {
        if (!controller.signal.aborted) setLoading(false);
      });
    }, 200);
    return () => {
      clearTimeout(timer);
      controller.abort();
    };
  }, [endpoint, open, query]);

  async function attach(source: Source) {
    setBusy(true);
    setNotice("");
    try {
      const result = await api<{ path?: string }>(endpoint, {
        method: "POST",
        body: JSON.stringify({ chat_id: source.chat_id, path: source.path, sha256: source.sha256 }),
      });
      if (!result?.path?.startsWith("inputs/")) throw new Error("The server did not confirm the attachment.");
      setNotice(`Attached ${source.filename} to this chat.`);
      refreshSessionTruth(sessionId);
    } catch (error) {
      setNotice(error instanceof Error ? error.message : "Could not attach the file.");
    } finally {
      setBusy(false);
    }
  }

  return <>
    <button type="button" className="ka-icon-btn mb-1.5 shrink-0 px-2 text-xs"
      onClick={() => { dialog.current?.showModal(); setOpen(true); }}>
      From past chats
    </button>
    <dialog ref={dialog} onClose={() => setOpen(false)}
      aria-labelledby={titleId}
      className="m-auto max-h-[80dvh] w-[min(32rem,calc(100%-2rem))] overflow-auto rounded-xl border border-line bg-surface p-4 text-ink backdrop:bg-black/60">
      <div className="flex items-center justify-between gap-2">
        <h2 id={titleId} className="font-semibold">Attach from past chats</h2>
        <button type="button" className="ka-icon-btn px-2" onClick={() => dialog.current?.close()}>Close</button>
      </div>
      <label className="mt-3 block text-sm">
        Filename
        <input className="mt-1 w-full rounded-md border border-line bg-transparent px-3 py-2"
          value={query} onChange={(event) => setQuery(event.target.value)} maxLength={85} autoFocus />
      </label>
      <p className="mt-2 text-sm text-muted" role="status">
        {notice || (loading ? "Searching…" : files.length === 0 ? "No matching files." : "")}
      </p>
      <ul className="mt-2 divide-y divide-line">
        {files.map((file) => <li key={JSON.stringify(file.sources)} className="py-2">
          <p className="break-all text-sm font-medium">{file.filename}</p>
          {file.sources.map((source) => <div key={`${source.chat_id}/${source.path}`} className="mt-1 flex items-center justify-between gap-2">
            <span className="min-w-0 text-xs text-muted">
              <button type="button" disabled={busy} className="break-words text-left underline-offset-2 hover:underline"
                onClick={() => { dialog.current?.close(); void useAppStore.getState().openSession(source.chat_id); }}>
                {source.title || source.chat_id}
              </button>
              {" · "}{new Date(source.updated_at * 1000).toLocaleDateString()}
            </span>
            <button type="button" disabled={busy} className="ka-icon-btn px-2 text-xs"
              aria-label={`Attach ${source.filename} from ${source.title || source.chat_id}`}
              onClick={() => void attach(source)}>Attach</button>
          </div>)}
        </li>)}
      </ul>
      {truncated && <p className="text-xs text-muted">Search by a more specific filename to see additional matches.</p>}
    </dialog>
  </>;
}
