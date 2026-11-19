import type { ToolCard } from "../api/types";

export interface SourceCardsProps {
  toolCards?: ToolCard[];
  sources?: Array<{ url: string; title?: string }>;
}

/**
 * Hyperagent-style Source Attribution Cards.
 * Shows visited page domain, favicon, and title as clean clickable chips
 * at the bottom of the agent's completed response.
 */
export function SourceCards({ toolCards, sources }: SourceCardsProps) {
  const list = [
    ...(sources || []),
    ...(toolCards
      ?.map((c) => c.observedSource)
      .filter((s): s is { url: string; title?: string } => Boolean(s?.url)) || []),
  ].filter(
    (item, index, self) =>
      self.findIndex((other) => other.url === item.url) === index,
  );

  if (list.length === 0) return null;

  return (
    <div className="mt-2.5 flex flex-wrap gap-2" role="region" aria-label="Referenced sources">
      {list.slice(0, 6).map((item) => {
        let domain = item.url;
        try {
          domain = new URL(item.url).hostname.replace(/^www\./, "");
        } catch {
          /* keep original */
        }

        return (
          <a
            key={item.url}
            href={item.url}
            target="_blank"
            rel="noreferrer"
            title={item.title ? `${item.title} (${item.url})` : item.url}
            className="group/src inline-flex items-center gap-1.5 rounded-lg border border-line/70 bg-surface/80 px-2.5 py-1 text-xs text-muted hover:border-accent/50 hover:bg-surface hover:text-ink transition-all duration-150 shadow-2xs"
          >
            <img
              src={`https://www.google.com/s2/favicons?domain=${domain}&sz=16`}
              width={13}
              height={13}
              alt=""
              className="shrink-0 rounded-xs opacity-80 group-hover/src:opacity-100"
              onError={(e) => {
                (e.currentTarget as HTMLElement).style.display = "none";
              }}
            />
            <span className="max-w-[140px] truncate font-medium text-ink/90">
              {item.title || domain}
            </span>
            <span className="text-[10px] text-faint">
              {domain}
            </span>
          </a>
        );
      })}
    </div>
  );
}
