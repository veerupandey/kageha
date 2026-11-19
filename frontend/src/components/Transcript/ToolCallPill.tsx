import { memo, useState } from "react";
import type { ToolCard } from "../../api/types";
import { activityTarget, activityUrl } from "../../lib/activityUi";
import { cn } from "../../lib/cn";
import { Icon } from "../../lib/icons";
import { browserToolBlocked, formatDuration, isShellTool, isSubagentSpawn, parseSubagentsFromCard, toolChipKind, toolChipSummary } from "../../lib/transcript";
import { AgentBriefSummary, LiveAgentBrief, LiveAgentBriefs } from "./LiveAgentBrief";
import { ToolDetails } from "./ToolDetails";
import { GmailAccessAction, gmailToolLabel } from "../GmailAccessAction";

function isIntegrationTool(name: string): boolean {
  const n = String(name || "").toLowerCase();
  return n.startsWith("mcp_") || n.startsWith("mcp.") || n.includes("__");
}

function toolIcon(name: string) {
  const low = String(name || "").toLowerCase();
  if (isShellTool(name) || low.includes("command") || low.includes("terminal") || low.includes("bash") || low.includes("exec")) return Icon.Terminal;
  if (isSubagentSpawn(low) || /^agent_(?:wait|wait_any|status|cancel)$/.test(low)) return Icon.Jobs;
  if (low.includes("peer")) return Icon.Network;
  if (isIntegrationTool(low)) return Icon.Plug;
  if (low.includes("browser") || low.includes("web") || low.includes("fetch")) return Icon.Globe;
  if (low.includes("edit")) return Icon.FileEdit;
  if (low.includes("write") || low.includes("read") || low.includes("file")) return Icon.File;
  if (low.includes("search") || low.includes("grep") || low.includes("glob")) return Icon.Search;
  return Icon.Activity;
}

// Host failures commonly arrive in resultPreview as a structured envelope.
// Show its user-facing summary, never raw JSON, alongside expanded details.
function failureSummary(card: ToolCard): string {
  const text = (card.errorPreview || card.resultPreview || "").trim();
  if (!text) return "";
  const raw = text.replace(/^Tool failed:\s*/, "");
  if (raw.startsWith("{")) {
    try {
      const parsed = JSON.parse(raw);
      const summary = parsed.summary || parsed.detail?.message;
      return typeof summary === "string" ? summary.trim().slice(0, 180) : "";
    } catch { return ""; }
  }
  return text.split(/\r?\n/, 1)[0].slice(0, 180);
}

export const ToolCallPill = memo(function ToolCallPill({
  card,
  onOpenBrowser,
  onOpenComputer,
  compact = false,
}: {
  card: ToolCard;
  onOpenBrowser?: (url?: string, target?: ToolCard["browserTarget"]) => void;
  onOpenComputer?: () => void;
  compact?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const preparing = card.status === "pending" || card.status === "requested";
  const running = !card.status || card.status === "running" || card.status === "pending" || card.status === "requested";
  const failed = card.status === "error" || card.status === "failed" || card.status === "denied";
  const connector = card.connectorOutcome;
  const blocked = card.status === "blocked" || Boolean(connector?.recovery) || browserToolBlocked(card);
  const unconfirmed = card.status === "unconfirmed";
  const attention = blocked || unconfirmed;
  const completed = card.status === "success" || card.status === "completed" || card.status === "ok" || card.status === "done";
  const subagents = parseSubagentsFromCard(card);
  const jobIds = subagents.flatMap(item => item.job_id ? [item.job_id] : []);
  const delegated = !failed && jobIds.length > 0;
  const statusLabel = delegated ? "Delegated" : blocked ? "Blocked" : unconfirmed ? "Unconfirmed" : failed ? "Failed" : preparing ? "Preparing" : running ? "Running" : completed && card.researchRecovery?.recovered ? "Recovered" : completed ? "Done" : card.status === "interrupted" ? "Interrupted" : card.status || "Unknown";
  const kind = preparing ? "Preparing tool" : running ? toolChipKind(card.name) : toolChipKind(card.name).replace(/^Running |^Using |^Creating /, "").replace(/^./, (letter) => letter.toUpperCase());
  const summary = gmailToolLabel(connector) || toolChipSummary(card);
  const duration = card.durationMs != null ? formatDuration(card.durationMs) : "";
  const target = activityTarget(`${card.name} ${summary}`);
  const url = activityUrl(card.argsPreview, card.resultPreview);
  const Glyph = toolIcon(card.name);
  const isSubagent = subagents.length > 0;
  const hasBody = Boolean(isSubagent || card.attemptId || card.argsPreview || card.resultPreview || card.outputPreview || card.errorPreview);
  const canExpand = hasBody || Boolean(compact && target);
  const errorSummary = failed || attention ? failureSummary(card) : "";
  const structuredFailure = (card.errorPreview || card.resultPreview || "").trim().replace(/^Tool failed:\s*/, "").startsWith("{");

  return (
    <div className={cn("ka-tool-call mb-2", compact && "ka-tool-call--compact")} data-status={delegated ? "delegated" : blocked ? "blocked" : unconfirmed ? "unconfirmed" : failed ? "failed" : running ? "running" : completed ? "complete" : "unknown"}>
      <button
        type="button"
        className={cn(
          "flex w-full items-center gap-2 rounded-lg bg-transparent px-3 py-2 text-left text-[0.8rem] text-muted",
          canExpand && "hover:bg-line/30",
          !canExpand && "cursor-default",
          failed && !blocked && "text-danger",
          attention && "text-warn",
        )}
        aria-expanded={canExpand ? open : undefined}
        aria-label={`${kind}: ${isSubagent ? `${subagents.length} specialist${subagents.length === 1 ? "" : "s"}` : summary || card.name} — ${statusLabel}`}
        onClick={() => {
          if (canExpand) setOpen((v) => !v);
        }}
      >
        {running ? (
          <Icon.Loader size={14} className="shrink-0 animate-spin text-faint" />
        ) : compact && (failed || attention) ? (
          <Icon.Alert size={14} className={cn("shrink-0", attention ? "text-warn" : "text-danger")} />
        ) : (
          <Glyph size={14} className="shrink-0 text-faint" />
        )}
        <span className="min-w-0 flex-1 truncate">
          <span className="text-ink">{kind}</span>
          {(isSubagent || summary) ? (
            <>
              <span className="text-faint"> · </span>
              <span>{isSubagent ? subagents.map((item, index) => item.name || `Worker ${index + 1}`).join(" · ") : summary}</span>
            </>
          ) : null}
        </span>
        <span className="ka-tool-status">{statusLabel}</span>
        {duration && !running ? (
          <span className="shrink-0 tabular-nums text-[0.65rem] text-faint">{duration}</span>
        ) : null}
        {canExpand ? (
          <Icon.ChevronRight
            size={14}
            className={cn("shrink-0 text-faint transition-transform", open && "rotate-90")}
          />
        ) : null}
      </button>
      {errorSummary && !open && (!compact || attention) ? <p className={cn("ml-8 mr-3 mb-1 line-clamp-2 text-xs", attention ? "text-warn" : "text-danger")}>{errorSummary}</p> : null}
      {connector?.recovery && connector.showRecovery !== false && <GmailAccessAction outcome={connector} />}
      {target && (!compact || open) ? (
        <button
          type="button"
          className="mt-1 inline-flex items-center gap-1 rounded px-1.5 py-1 text-[0.68rem] text-accent hover:bg-accent-soft"
          onClick={() => {
            if (target === "browser") onOpenBrowser?.(url || undefined, card.browserTarget);
            else if (target === "computer") onOpenComputer?.();
          }}
        >
          {target === "browser" ? "Open browser" : "Watch live"} <span aria-hidden="true">↗</span>
        </button>
      ) : null}
      {isSubagent && open ? (
        <LiveAgentBriefs enabled={subagents.some(item => Boolean(item.job_id))}><div className="mt-2 space-y-1.5 rounded-lg border border-line bg-canvas/60 p-2.5">
          <div className="flex items-center justify-between text-[0.7rem] font-medium text-muted">
            <span className="flex items-center gap-1.5">
              <Icon.Jobs size={13} className="text-accent shrink-0" />
              <span>Subagents ({subagents.length})</span>
            </span>
            <span className="text-[0.68rem] text-faint">
              {jobIds.length === subagents.length ? <AgentBriefSummary jobIds={jobIds} /> : running
                ? `${subagents.filter((s) => s.status === "completed").length}/${subagents.length} done`
                : statusLabel}
            </span>
          </div>
          <div className="space-y-1.5 pt-1">
            {subagents.map((item, index) => (
              <div
                key={item.id}
                className="rounded border border-line/60 bg-surface/80 p-2 text-xs transition-colors"
              >
                <div className="flex items-center justify-between gap-2">
                  <div className="flex items-center gap-1.5 min-w-0">
                    {item.status === "completed" ? (
                      <Icon.Check size={13} className="shrink-0 text-emerald-500" />
                    ) : item.status === "failed" ? (
                      <Icon.Alert size={13} className="shrink-0 text-danger" />
                    ) : running && !item.job_id ? (
                      <Icon.Loader size={13} className="shrink-0 animate-spin text-accent" />
                    ) : (
                      <Icon.Activity size={13} className="shrink-0 text-faint" />
                    )}
                    <span className="font-semibold text-[0.75rem] text-ink truncate">
                      {item.name || `Worker ${index + 1}`}
                    </span>
                  </div>
                  {!item.job_id && <span
                    className={cn(
                      "shrink-0 rounded px-1.5 py-0.5 text-[0.62rem] font-medium uppercase tracking-wider",
                      item.status === "completed" && "bg-emerald-500/10 text-emerald-500",
                      item.status === "failed" && "bg-rose-500/10 text-danger",
                      item.status === "running" && "bg-accent/10 text-accent",
                      (!item.status || item.status === "pending") && "bg-line/40 text-faint",
                    )}
                  >
                    {item.status || (running ? "running" : "ready")}
                  </span>}
                </div>
                <p className="mt-1 max-h-28 overflow-auto whitespace-pre-wrap text-[0.72rem] leading-relaxed text-muted">
                  {item.task}
                </p>
                {item.job_id && <LiveAgentBrief jobId={item.job_id} />}
                {item.result ? (
                  <div className="mt-1.5 border-t border-line/40 pt-1.5">
                    <div className="text-[0.65rem] font-medium text-faint">Output:</div>
                    <p className="mt-0.5 max-h-28 overflow-y-auto text-[0.72rem] text-ink whitespace-pre-wrap">
                      {item.result}
                    </p>
                  </div>
                ) : null}
                {item.error ? (
                  <p className="mt-1 text-[0.7rem] text-danger">{item.error}</p>
                ) : null}
              </div>
            ))}
          </div>
        </div></LiveAgentBriefs>
      ) : null}
      {open && hasBody ? (
        <div className="mt-1.5 space-y-1.5 rounded-lg border border-line bg-canvas px-2.5 py-2">
          {compact && failed && !blocked && structuredFailure && errorSummary ? <p className="text-xs text-danger">{errorSummary}</p> : null}
          {isSubagent ? <details><summary className="cursor-pointer text-xs text-muted">Technical details</summary><ToolDetails card={card} /></details> : <ToolDetails card={card} />}
        </div>
      ) : null}
    </div>
  );
});
