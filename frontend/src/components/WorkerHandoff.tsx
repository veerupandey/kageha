import type { WorkerHandoff as Handoff } from "./DurableJobs";

const statusText: Record<Handoff["status"], string> = {
  preview: "Changes ready for integration",
  review_required: "Human review required",
  conflict: "Conflicts need review",
  applying: "Applying changes",
  applied: "Changes applied",
  uncertain: "Integration needs review",
  rolled_back: "Changes rolled back",
};

/** Review bounded, server-provided text without interpreting it as HTML. */
export function WorkerHandoff({ handoff }: { handoff: Handoff }) {
  const files = Array.isArray(handoff.files) ? handoff.files : [];
  const conflicts = Array.isArray(handoff.conflicts) ? handoff.conflicts : [];
  return <section aria-label="Code handoff" className="space-y-2 rounded-xl border border-line/70 bg-surface/50 p-3 text-xs">
    <div className="flex flex-wrap justify-between gap-2"><h3 className="font-semibold text-ink">Code handoff</h3>
      <span className={handoff.status === "conflict" || handoff.status === "uncertain" || handoff.status === "rolled_back" ? "text-warn" : "text-muted"}>{statusText[handoff.status] || handoff.status}</span></div>
    <p className="text-muted">{handoff.target === "project" ? "Git project" : "Task workspace"} · {files.length} {files.length === 1 ? "file" : "files"}</p>
    {handoff.review_required && <p className="text-muted">This patch is retained in the worker’s worktree for human review. The agent cannot apply it.</p>}
    {conflicts.length > 0 && <div role="alert" className="text-warn"><p>Changes were not applied because these paths conflict:</p><ul className="mt-1 list-inside list-disc">{conflicts.map(path => <li key={path} className="break-all">{path}</li>)}</ul></div>}
    {files.map(file => <details key={file.path} className="rounded-lg border border-line/60 p-2">
      <summary className="cursor-pointer break-all text-ink">{file.path} · {file.change}</summary>
      {file.binary ? <p className="mt-2 text-muted">Binary content is not shown.</p> : <div className="mt-2 space-y-2">
        {file.before_text != null && <div><p className="mb-1 font-medium text-muted">Before</p><pre className="max-h-48 overflow-auto whitespace-pre-wrap break-all rounded bg-canvas/70 p-2 text-ink">{file.before_text}</pre></div>}
        {file.after_text != null && <div><p className="mb-1 font-medium text-muted">After</p><pre className="max-h-48 overflow-auto whitespace-pre-wrap break-all rounded bg-canvas/70 p-2 text-ink">{file.after_text}</pre></div>}
      </div>}
      {file.truncated && <p className="mt-2 text-muted">Preview shortened.</p>}
    </details>)}
  </section>;
}
