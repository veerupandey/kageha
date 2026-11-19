import { useState, type ReactNode } from "react";
import type { ToolCard } from "../api/types";
import { Icon } from "../lib/icons";
import { cn } from "../lib/cn";
import { toolChipSummary } from "../lib/transcript";

function getToolIcon(name: string) {
  const lower = name.toLowerCase();
  if (lower.includes("browser") || lower.includes("navigate") || lower.includes("url") || lower.includes("page")) {
    return Icon.Globe;
  }
  if (lower.includes("file") || lower.includes("read") || lower.includes("write") || lower.includes("artifact")) {
    return Icon.File;
  }
  if (lower.includes("shell") || lower.includes("bash") || lower.includes("command") || lower.includes("terminal")) {
    return Icon.Terminal;
  }
  if (lower.includes("note") || lower.includes("edit") || lower.includes("update") || lower.includes("patch")) {
    return Icon.Compose;
  }
  return Icon.Activity;
}

export interface ActionSummaryCardProps {
  cards: ToolCard[];
  children?: ReactNode;
  defaultOpen?: boolean;
  onOpenBrowser?: (url?: string) => void;
  className?: string;
}

function safeHostname(raw: string): string {
  try {
    const url = new URL(raw);
    return url.hostname.replace(/^www\./, "");
  } catch {
    return raw.slice(0, 30);
  }
}

export function synthesizeActionSummary(cards: ToolCard[]): string {
  if (!cards.length) return "Action completed";

  const lastCard = cards[cards.length - 1];
  const chip = toolChipSummary(lastCard);
  const completedChip = chip.replace(/^(Opening|Running|Searching|Reading|Using|Reviewing|Writing|Inspecting)\b/, (verb) => ({
    Opening: "Opened", Running: "Ran", Searching: "Searched", Reading: "Read", Using: "Used",
    Reviewing: "Reviewed", Writing: "Wrote", Inspecting: "Inspected",
  })[verb] || verb);

  // If single card and toolChipSummary produced a descriptive label, keep it for test & precision
  if (cards.length === 1 && chip && chip !== "Integration read") {
    return completedChip;
  }

  // 1. Check Gmail connector operations
  const gmailCards = cards.filter(c => c.connectorOutcome?.connector === "gmail");
  if (gmailCards.length > 0) {
    const ops = gmailCards.map(c => c.connectorOutcome?.operation);
    const hasSearch = ops.includes("search");
    const hasRead = ops.includes("thread") || ops.includes("message");
    const hasStatus = ops.includes("status");

    if (hasSearch && hasRead) return "Searched and read Gmail messages";
    if (hasSearch) return "Searched mailbox for matching messages";
    if (hasRead) return cards.length > 1 ? "Read email thread and message details" : "Read email message details";
    if (hasStatus) return "Checked Gmail connector access";
  }

  // 2. Check browser actions
  const browserCards = cards.filter(
    (c) =>
      c.name.toLowerCase().includes("browser") ||
      Boolean(c.browserTarget) ||
      Boolean(c.observedSource?.url),
  );
  if (browserCards.length > 0) {
    const firstUrl = browserCards.find((c) => c.observedSource?.url);
    const host =
      firstUrl?.observedSource?.title ||
      (firstUrl?.observedSource?.url ? safeHostname(firstUrl.observedSource.url) : "");
    const writes = cards.filter(
      (c) => c.name.toLowerCase().includes("file") || c.name.toLowerCase().includes("write"),
    );
    if (writes.length > 0) {
      return host ? `Reviewed ${host} and saved file` : "Reviewed page content and saved file";
    }
    if (cards.length > 1) {
      return host ? `Reviewed page content · ${host}` : "Reviewed browser page content";
    }
    return host ? `Opened page · ${host}` : "Opened browser page";
  }

  // 3. Fallback
  if (chip && chip !== "Integration read") return completedChip;
  if (lastCard.connectorOutcome?.operation === "search") return "Searched Gmail mailbox";
  if (lastCard.connectorOutcome?.operation === "thread" || lastCard.connectorOutcome?.operation === "message") {
    return "Read email message details";
  }
  if (lastCard.connectorOutcome?.operation === "status") return "Checked Gmail connector access";

  return chip || "Action completed";
}

/**
 * Hyperagent-style collapsible Action Summary Card.
 * Wraps consecutive tool/shell executions into an intelligent, elegant overview
 * with action counts, human-readable summary, drillable items, and browser replay hint.
 */
export function ActionSummaryCard({
  cards,
  children,
  defaultOpen = false,
  onOpenBrowser,
  className,
}: ActionSummaryCardProps) {
  const [expanded, setExpanded] = useState(defaultOpen);
  if (!cards.length) return null;

  const count = cards.length;
  const summaryText = synthesizeActionSummary(cards);

  const hasBrowserAction = cards.some(
    (c) =>
      c.name.toLowerCase().includes("browser") ||
      Boolean(c.browserTarget) ||
      Boolean(c.observedSource?.url),
  );

  const observedSources = cards
    .map((c) => c.observedSource)
    .filter((s): s is { url: string; title?: string } => Boolean(s?.url))
    .filter((s, idx, arr) => arr.findIndex((x) => x.url === s.url) === idx);

  return (
    <details
      open={expanded}
      onToggle={event => {
        if (event.target === event.currentTarget) setExpanded(event.currentTarget.open);
      }}
      className={cn(
        "action-summary-card group my-0.5 overflow-hidden rounded-md hover:bg-surface/70",
        className,
      )}
    >
      <summary onClick={event => { event.preventDefault(); setExpanded(value => !value); }} className="flex w-full cursor-pointer list-none items-center gap-2 px-3 py-1.5 text-xs select-none text-ink/90 [&::-webkit-details-marker]:hidden">
        {/* Steps glyph icon */}
        <Icon.Check size={14} className="shrink-0 text-accent" aria-hidden="true" />

        {/* Natural language summary label */}
        <span className="flex-1 truncate font-medium text-[12.5px] text-ink">
          {summaryText}
        </span>

        {/* Action count pill badge */}
        <span className="shrink-0 text-[11px] tabular-nums text-faint">
          {count} {count === 1 ? "action" : "actions"}
        </span>

        {/* Spring-animated chevron */}
        <Icon.ChevronRight
          size={13}
          className="shrink-0 text-faint group-hover:text-ink transition-transform duration-200 ease-[cubic-bezier(0.34,1.56,0.64,1)] group-open:rotate-90"
        />
      </summary>

      {/* Expanded drilldown view */}
      {expanded && <div className="ml-5 border-l border-line/60 pl-2 py-1 space-y-1.5">
        {children ? (
          <div className="space-y-1">{children}</div>
        ) : (
          <div className="space-y-1">
            {cards.map((card) => {
              const Glyph = getToolIcon(card.name);
              const cardLabel = card.name.replace(/_/g, " ");
              const detail = card.argsPreview || card.resultPreview;
              return (
                <div
                  key={card.id}
                  className="flex items-center gap-2.5 rounded-lg px-2.5 py-1.5 text-xs text-muted hover:bg-surface hover:text-ink transition-colors"
                >
                  <Glyph size={12} className="shrink-0 text-faint" />
                  <span className="flex-1 truncate capitalize font-medium">
                    {cardLabel}
                  </span>
                  {detail && (
                    <span className="max-w-[140px] truncate text-[11px] text-faint font-mono">
                      {detail}
                    </span>
                  )}
                  <Icon.ChevronRight
                    size={11}
                    className="shrink-0 opacity-30"
                  />
                </div>
              );
            })}
          </div>
        )}

        {/* Replay browser session item if browser tools were used */}
        {hasBrowserAction && (
          <div className="pt-1.5 border-t border-line/30 flex items-center justify-between">
            <button
              type="button"
              onClick={() => onOpenBrowser?.()}
              className="inline-flex items-center gap-1.5 text-xs font-medium text-accent hover:text-accent/80 hover:underline transition-colors cursor-pointer"
            >
              <Icon.Globe size={12} className="shrink-0" />
              <span>Replay browser session</span>
              <Icon.ChevronRight size={11} className="shrink-0 opacity-60" />
            </button>
          </div>
        )}

        {/* Visited source chips */}
        {observedSources.length > 0 && (
          <div className="pt-1.5 border-t border-line/30 flex flex-wrap gap-1.5">
            {observedSources.slice(0, 4).map((source) => {
              let domain = source.url;
              try {
                domain = new URL(source.url).hostname;
              } catch {
                /* use raw url */
              }
              return (
                <a
                  key={source.url}
                  href={source.url}
                  target="_blank"
                  rel="noreferrer"
                  className="inline-flex items-center gap-1.5 rounded-md border border-line/60 bg-surface px-2 py-0.5 text-[11px] text-muted hover:border-accent/40 hover:text-ink transition-colors"
                >
                  <img
                    src={`https://www.google.com/s2/favicons?domain=${domain}&sz=16`}
                    width={11}
                    height={11}
                    alt=""
                    className="shrink-0 rounded-xs"
                    onError={(e) => {
                      (e.currentTarget as HTMLElement).style.display = "none";
                    }}
                  />
                  <span className="max-w-[120px] truncate font-medium">
                    {source.title || domain}
                  </span>
                </a>
              );
            })}
          </div>
        )}
      </div>}
    </details>
  );
}
