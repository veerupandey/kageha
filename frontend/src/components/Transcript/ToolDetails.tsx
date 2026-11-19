import { PastChatAttachments } from "../PastChatAttachments";
import { useEffect, useState } from "react";
import { api } from "../../api/client";
import type { ToolCard } from "../../api/types";
import { Icon } from "../../lib/icons";
import { isShellTool, unwrapShellPayload } from "../../lib/transcript";
import { useAppStore } from "../../store";

interface StoredDetails {
  arguments: Record<string, unknown>;
  result: string;
  error: string;
  truncated?: boolean;
  browser_timing?: { wall_ms: number; spans: { stage: string; duration_ms: number; status: string }[]; dropped_spans: number };
  script_timing?: Record<string, number>;
  browser_receipts?: { id: string; action: string; step: number | null; state: string; dispatch: string; verification: string }[];
}

const TIMING_LABELS: Readonly<Record<string, string>> = {
  execution: "Tool execution", connect: "Browser connection", cdp_attach: "Browser attachment",
  document_probe: "Page identity check", freshness_check: "Observation check",
  locator_resolution: "Find control", click_dispatch: "Click", fill_dispatch: "Fill",
  verification: "Check expected condition", ax_capture: "Read accessibility tree",
  snapshot: "Page observation", screenshot: "Screenshot", script_target: "Select script tab",
  script_process: "Script process", script_resync: "Sync after script",
  playwright_import_ms: "Load Playwright", playwright_start_ms: "Start Playwright",
  cdp_attach_ms: "Attach to browser", target_resolution_ms: "Select target tab",
  user_code_ms: "Run script", final_observation_ms: "Read final page", child_total_ms: "Runner total",
};
const UNRESOLVED_RECEIPT_STATES = new Set(["dispatching", "returned", "uncertain"]);

function timingLabel(name: string) {
  return TIMING_LABELS[name] || name;
}

function CopyButton({ text }: { text: string }) {
  const [copied, setCopied] = useState(false);
  const copy = () => {
    try {
      void navigator.clipboard?.writeText(text);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {}
  };
  return (
    <button
      type="button"
      onClick={copy}
      title="Copy to clipboard"
      aria-label="Copy to clipboard"
      className="inline-flex items-center gap-1 px-1.5 py-0.5 rounded text-[0.65rem] text-faint hover:text-ink hover:bg-surface/80 transition-colors"
    >
      {copied ? <Icon.Check size={11} className="text-emerald-500" /> : <Icon.Copy size={11} />}
      <span>{copied ? "Copied" : "Copy"}</span>
    </button>
  );
}

function memorySearchSummary(name: string, result: string): string | undefined {
  if (name !== "memory_search" || !result) return undefined;
  try {
    const parsed = JSON.parse(result);
    const matches = Array.isArray(parsed) ? parsed : parsed?.value;
    if (!Array.isArray(matches)) return undefined;
    return matches.length === 0 ? "No memories matched." :
      matches.length === 1 ? "1 memory matched." : matches.length + " memories matched.";
  } catch {
    return undefined;
  }
}

function researchData(text: string): Record<string, unknown> | undefined {
  try {
    const data = JSON.parse(text.replace(/^Tool failed: /, ""));
    return data && typeof data === "object" && !Array.isArray(data) ? data : undefined;
  } catch { return undefined; }
}

function ResearchSuccess({ text, card }: { text: string; card: ToolCard }) {
  const data = researchData(text);
  const recovery = data?.recovery as { recovered?: boolean } | undefined;
  const recovered = recovery?.recovered === true || card.researchRecovery?.recovered;
  if (!recovered && typeof data?.source_id !== "string") return null;
  return <section aria-label="Source reading" className="rounded-md border border-line bg-surface/50 px-2.5 py-2 text-xs">
    {recovered && <><p className="font-medium text-ink">Read successfully using browser extraction</p>
      <p className="mt-1 text-muted">{data?.source_cached === true ? "Reading the retained browser extraction. No new download was needed." : "HTTP reading failed. Browser extraction succeeded."}</p></>}
    {typeof data?.source_id === "string" && <p className="mt-1 text-muted">Source text retained for further reading without downloading again.</p>}
    {data?.source_truncated === true && <p className="mt-1 text-muted">Only part of this source was retained. A missing search result does not establish absence.</p>}
  </section>;
}

function ResearchFailure({ text }: { text: string }) {
  const data = researchData(text);
  const summary = typeof data?.summary === "string" ? data.summary.slice(0, 1000) : "Other sources can still be checked.";
  const attempted = Array.isArray(data?.attempted_backends) ? data.attempted_backends.filter((value) => value === "http" || value === "chromium") : [];
  return (
    <section className="rounded-md border border-danger/30 bg-danger/5 px-2.5 py-2 text-xs">
      <p className="font-medium text-ink">Couldn’t read this source</p>
      <p className="mt-1 text-muted">{summary}</p>
      {attempted.length > 0 && <p className="mt-1 text-muted">Tried: {attempted.map((backend) => backend === "http" ? "HTTP reading" : "browser extraction").join(", ")}.</p>}
      <details className="mt-2">
        <summary className="cursor-pointer text-muted">Technical details</summary>
        <div className="mt-2 flex justify-end"><CopyButton text={text} /></div>
        <pre className="max-h-48 overflow-auto whitespace-pre-wrap break-words text-muted">{text}</pre>
      </details>
    </section>
  );
}

function FileRecovery({ text, sessionId }: { text: string; sessionId: string | null }) {
  const start = text.indexOf("{");
  let summary: string | undefined;
  if (start >= 0) {
    try {
      const error = JSON.parse(text.slice(start));
      if (typeof error.error_code === "string" && error.error_code.startsWith("file_")
          && typeof error.summary === "string") summary = error.summary;
    } catch {}
  }
  return (
    <section className="rounded-md border border-danger/30 bg-danger/5 px-2.5 py-2 text-xs">
      <p className="font-medium text-ink">{summary || "Couldn’t read this file"}</p>
      {sessionId && <PastChatAttachments sessionId={sessionId} />}
      <details>
        <summary className="cursor-pointer text-muted">Technical details</summary>
        <pre className="max-h-48 overflow-auto whitespace-pre-wrap break-words text-muted">{text}</pre>
      </details>
    </section>
  );
}

function SavedPreview({ card }: { card: ToolCard }) {
  const sessionId = useAppStore((state) => state.sessionId);
  const failed = ["failed", "error", "denied"].includes(card.status ?? "") || (isShellTool(card.name) && Boolean(card.errorPreview));
  const text = card.errorPreview || card.resultPreview || card.outputPreview || card.argsPreview;
  const label = failed ? "Recorded failure preview" : "Recorded preview";
  if (failed && card.name === "read_file" && text) return <FileRecovery text={text} sessionId={sessionId} />;
  if (failed && card.name === "web_fetch" && text) return <ResearchFailure text={text} />;
  if (memorySearchSummary(card.name, text ?? "") === "No memories matched.") {
    return <p className="px-1 text-xs text-muted">No memories matched.</p>;
  }
  return text ? (
    <>{card.name === "web_fetch" && !failed && <div className="mb-2"><ResearchSuccess text={text} card={card} /></div>}
    <section aria-label={label} className="overflow-hidden rounded-md border border-line text-xs">
      <header className="flex items-center justify-between border-b border-line px-2.5 py-1.5 font-medium text-muted">
        {label}<CopyButton text={text} />
      </header>
      <pre className="max-h-48 overflow-auto whitespace-pre-wrap break-words px-2.5 py-2 text-ink">{text}</pre>
    </section></>
  ) : (
    <p className="px-1 py-0.5 text-xs text-muted">No output was recorded.</p>
  );
}

/** Load recorded details on expansion and distinguish preview-only history. */
export function ToolDetails({ card }: { card: ToolCard }) {
  const sessionId = useAppStore((state) => state.sessionId);
  const key = `${sessionId}/${card.attemptId}/${card.status}`;
  const [loaded, setLoaded] = useState<{ key: string } & ({ details: StoredDetails } | { error: string })>();
  useEffect(() => {
    if (!sessionId || !card.attemptId) return;
    const attemptId = card.attemptId;
    const abort = new AbortController();
    let timer: ReturnType<typeof setTimeout> | undefined;
    const read = () => {
      void api<StoredDetails>(
        `/api/sessions/${encodeURIComponent(sessionId)}/tool-attempts/${encodeURIComponent(attemptId)}`,
        { signal: abort.signal },
      ).then((details) => {
        if (abort.signal.aborted) return;
        setLoaded({ key, details });
        if ((card.status === "running" && card.name.startsWith("browser_")) || details.browser_receipts?.some(receipt =>
          UNRESOLVED_RECEIPT_STATES.has(receipt.state))) {
          timer = setTimeout(read, 2000);
        }
      }).catch((error: unknown) => {
        if (!abort.signal.aborted) setLoaded({ key, error: String(error) });
      });
    };
    read();
    return () => { abort.abort(); clearTimeout(timer); };
  }, [sessionId, card.attemptId, card.name, card.status, key]);

  const current = loaded?.key === key ? loaded : undefined;
  if (!sessionId || !card.attemptId) return <SavedPreview card={card} />;
  if (!current) return <p role="status" className="text-xs text-muted">Loading tool details…</p>;
  if ("error" in current) return (
    <div className="space-y-2">
      <p role="alert" className="text-xs text-muted">Full tool details couldn’t be loaded.</p>
      <SavedPreview card={card} />
    </div>
  );
  const details = current.details;
  const attention = card.status === "unconfirmed" || card.status === "blocked";
  const shell = isShellTool(card.name);
  const { command, ...otherArgs } = details.arguments;
  const args = shell && typeof command === "string" ? otherArgs : details.arguments;
  const result = shell ? unwrapShellPayload(details.result) : details.result;
  const memorySummary = memorySearchSummary(card.name, details.result);
  const hasOutput = Boolean(details.error || result || Object.keys(details.arguments).length ||
    details.browser_receipts?.length || details.browser_timing || Object.keys(details.script_timing || {}).length);
  if (!hasOutput) return <p className="text-xs text-muted">No output was recorded.</p>;
  return (
    <div className="space-y-2">
      {card.name === "web_fetch" && !details.error && <ResearchSuccess text={result} card={card} />}
      {memorySummary && <p className="px-1 text-xs font-medium text-ink">{memorySummary}</p>}
      {details.truncated && <p className="px-1 text-xs text-muted">Showing the first part of the recorded output.</p>}
      {details.browser_receipts?.length ? (
        <section aria-label="Browser action receipts" className="rounded-md border border-line p-2 text-xs space-y-2">
          <p className="font-medium">Browser action receipts</p>
          <ul className="space-y-1">
            {details.browser_receipts.map((receipt) => (
              <li key={receipt.id}>
                {receipt.step != null ? `Step ${receipt.step + 1}: ` : ""}{receipt.action}
                {" · "}{receipt.action === "batch"
                  ? "Plan recorded. See step receipts for input status."
                  : UNRESOLVED_RECEIPT_STATES.has(receipt.state)
                  ? "Outcome uncertain — check before retrying"
                  : receipt.state === "reviewed" ? "User reviewed the page. Outcome remains unverified"
                  : receipt.state === "not_dispatched" || receipt.dispatch === "not_started" ? "Input was not dispatched"
                  : receipt.verification === "attachment_visible" ? "Approved filename visible in attachment list"
                  : receipt.verification !== "unverified" ? "Expected condition observed"
                  : "Dispatch acknowledged; outcome not verified"}
              </li>
            ))}
          </ul>
          <p className="text-muted">A page condition does not by itself confirm the overall task or a server-side commit.</p>
        </section>
      ) : null}
      {(details.browser_timing || Object.keys(details.script_timing || {}).length > 0) && (
        <details className="rounded-md border border-line p-2 text-xs">
          <summary className="cursor-pointer font-medium">Browser execution timing{details.browser_timing ? ` · ${Math.round(details.browser_timing.wall_ms)} ms` : ""}</summary>
          <div className="mt-2 max-h-48 overflow-auto space-y-2">
            <p className="text-muted">Timings can overlap. They exclude model time and do not prove task completion.</p>
            {details.browser_timing && <dl className="space-y-1">
              {details.browser_timing.spans.map((span, index) => (
                <div key={index} className="flex items-baseline justify-between gap-3">
                  <dt>{timingLabel(span.stage)}{span.status === "interrupted" ? " (interrupted)" : ""}</dt>
                  <dd className="shrink-0 tabular-nums">{span.duration_ms.toFixed(1)} ms</dd>
                </div>
              ))}
              {details.browser_timing.dropped_spans > 0 && <p className="text-muted">Additional timing spans omitted.</p>}
            </dl>}
            {Object.keys(details.script_timing || {}).length > 0 && <>
              <p className="font-medium">Inside script process</p>
              <dl className="space-y-1">{Object.entries(details.script_timing || {}).map(([name, duration]) => (
                <div key={name} className="flex items-baseline justify-between gap-3">
                  <dt>{timingLabel(name)}</dt><dd className="shrink-0 tabular-nums">{duration.toFixed(1)} ms</dd>
                </div>
              ))}</dl>
              <p className="text-muted">Reported by the script process; excludes shell and interpreter launch before the runner starts.</p>
            </>}
          </div>
        </details>
      )}
      {shell && typeof command === "string" ? (
        <div className="rounded-md border border-line/70 bg-sidebar/50 overflow-hidden shadow-xs">
          <div className="flex items-center justify-between px-2.5 py-1 border-b border-line/40 bg-surface/50 text-[0.68rem] text-muted font-mono">
            <span className="flex items-center gap-1.5 font-medium">
              <Icon.Terminal size={12} className="text-accent" />
              <span>Command</span>
            </span>
            <CopyButton text={command} />
          </div>
          <pre className="p-2.5 max-h-36 overflow-auto whitespace-pre-wrap break-words font-mono text-[0.7rem] text-ink selection:bg-accent/20">
            $ {command}
          </pre>
        </div>
      ) : null}
      {Object.keys(args).length ? (
        <div className="rounded-md border border-line/70 bg-sidebar/50 overflow-hidden shadow-xs">
          <div className="flex items-center justify-between px-2.5 py-1 border-b border-line/40 bg-surface/50 text-[0.68rem] text-muted">
            <span className="font-medium text-faint">Arguments</span>
            <CopyButton text={JSON.stringify(args, null, 2)} />
          </div>
          <pre className="p-2.5 max-h-36 overflow-auto whitespace-pre-wrap break-all font-mono text-[0.65rem] text-muted">
            {JSON.stringify(args, null, 2)}
          </pre>
        </div>
      ) : null}
      {details.error && card.name === "web_fetch" ? <ResearchFailure text={details.error} /> : details.error && card.name === "read_file" ? <FileRecovery text={details.error} sessionId={sessionId} /> : details.error ? (
        <div className={`rounded-md border overflow-hidden shadow-xs ${attention ? "border-warn/40 bg-warn/10 text-warn" : "border-danger/40 bg-danger/10 text-danger"}`}>
          <div className="flex items-center justify-between px-2.5 py-1 border-b border-current/20 text-[0.68rem] font-medium">
            <span>{card.status === "unconfirmed" ? "Unconfirmed action" : card.status === "blocked" ? "Blocked action" : "Error"}</span>
            <CopyButton text={details.error} />
          </div>
          <pre className="p-2.5 max-h-64 overflow-auto whitespace-pre-wrap break-words font-mono text-[0.7rem]">
            {details.error}
          </pre>
        </div>
      ) : null}
      {result && memorySummary !== "No memories matched." ? (
        <div className="rounded-md border border-line/70 bg-sidebar/50 overflow-hidden shadow-xs">
          <div className="flex items-center justify-between px-2.5 py-1 border-b border-line/40 bg-surface/50 text-[0.68rem] text-muted">
            <span className="font-medium text-faint">Output</span>
            <CopyButton text={result} />
          </div>
          <pre className="p-2.5 max-h-64 overflow-auto whitespace-pre-wrap break-words font-mono text-[0.7rem] text-ink selection:bg-accent/20">
            {result}
          </pre>
        </div>
      ) : null}
    </div>
  );
}
