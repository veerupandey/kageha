import { memo, useState } from "react";
import type { ToolCard } from "../../api/types";
import { cn } from "../../lib/cn";
import { Icon } from "../../lib/icons";
import { ToolDetails } from "./ToolDetails";
import { formatDuration, shellSummary } from "../../lib/transcript";

export const ShellBlock = memo(function ShellBlock({ card, compact = false }: { card: ToolCard; compact?: boolean }) {
  const [open, setOpen] = useState(false);
  const summary = shellSummary(card);
  const preparing = card.status === "pending" || card.status === "requested";
  const running = !card.status || card.status === "running" || card.status === "pending" || card.status === "requested";
  const failed = Boolean(card.errorPreview) || ["error", "failed", "denied"].includes(card.status || "");
  const completed = card.status === "success" || card.status === "completed" || card.status === "ok" || card.status === "done";
  const statusLabel = failed ? "Failed" : preparing ? "Preparing" : running ? "Running" : completed ? "Done" : card.status || "Unknown";
  const duration = card.durationMs != null ? formatDuration(card.durationMs) : "";
  const hasBody = Boolean(card.attemptId || card.argsPreview || card.resultPreview || card.outputPreview || card.errorPreview);
  const errorText = (card.errorPreview || "").trim();
  const rawError = errorText.replace(/^Tool failed:\s*/, "");
  let errorSummary = "";
  if (compact && failed && rawError) {
    if (rawError.startsWith("{")) {
      try {
        const parsed = JSON.parse(rawError);
        if (typeof parsed.summary === "string") errorSummary = parsed.summary.trim().slice(0, 180);
      } catch { /* The full preview remains available in details. */ }
    } else errorSummary = errorText.split(/\r?\n/, 1)[0].slice(0, 180);
  }

  return (
    <div className={cn("ka-tool-call mb-2", compact && "ka-tool-call--compact")} data-status={failed ? "failed" : running ? "running" : completed ? "complete" : "unknown"}>
      <button
        type="button"
        className={cn(
          "flex w-full items-center gap-2 rounded-lg bg-transparent px-3 py-2 text-left text-[0.8rem] text-muted",
          hasBody && "hover:bg-line/30",
          !hasBody && "cursor-default",
          failed && "text-danger",
        )}
        aria-expanded={hasBody ? open : undefined}
        disabled={!hasBody}
        onClick={() => hasBody && setOpen((v) => !v)}
      >
        {running ? (
          <Icon.Loader size={14} className="shrink-0 animate-spin text-faint" />
        ) : compact && failed ? (
          <Icon.Alert size={14} className="shrink-0 text-danger" />
        ) : (
          <Icon.Terminal size={14} className="shrink-0 text-faint" />
        )}
        <span className="min-w-0 flex-1 truncate">
          <span className="text-ink">{preparing ? "Preparing command" : running ? "Running command" : "Command"}</span>
          {summary ? (
            <>
              <span className="text-faint"> · </span>
              <span>{summary}</span>
            </>
          ) : null}
        </span>
        <span className="ka-tool-status">{statusLabel}</span>
        {duration && !running ? (
          <span className="shrink-0 tabular-nums text-[0.65rem] text-faint">{duration}</span>
        ) : null}
        {hasBody ? (
          <Icon.ChevronRight
            size={14}
            className={cn("shrink-0 text-faint transition-transform", open && "rotate-90")}
          />
        ) : null}
      </button>
      {errorSummary && !open ? <p className="ml-8 mr-3 mb-1 line-clamp-2 text-xs text-danger">{errorSummary}</p> : null}
      {open && hasBody ? (
        <div className="mt-1 rounded-lg border border-line bg-canvas px-3 py-2">
          <ToolDetails card={card} />
        </div>
      ) : null}
    </div>
  );
});
