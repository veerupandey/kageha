import { useAppStore } from "../store";

export function AttachmentUploadStatus() {
  const upload = useAppStore(s => s.attachmentUpload);
  const sessionId = useAppStore(s => s.sessionId);
  const files = useAppStore(s => s.pendingFiles);
  const draft = useAppStore(s => s.draft);
  const send = useAppStore(s => s.sendMessage);
  if (!upload || upload.sessionId !== sessionId || !files.length) return null;
  return (
    <div className="mb-2 flex items-center gap-3 text-xs text-muted" role="status" aria-live="polite">
      {upload.status === "uploading" ? <>
        <progress className="min-w-0 flex-1" aria-label="Attachment upload progress" value={upload.completed} max={upload.total} />
        <span>Uploading · {upload.completed}/{upload.total} files complete</span>
      </> : <>
        <span className="min-w-0 flex-1">Upload interrupted. Your files are ready to retry.</span>
        <button type="button" className="min-h-11 shrink-0 rounded-lg border border-line px-3 text-ink" onClick={() => void send(draft || upload.text)}>Retry upload</button>
      </>}
    </div>
  );
}
