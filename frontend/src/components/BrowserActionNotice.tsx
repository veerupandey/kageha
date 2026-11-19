import { useState } from "react";
import type { BrowserState } from "../api/types";
import { browserActionNeedsReview } from "../lib/browserPreview";
import { Icon } from "../lib/icons";
import { ToolDetails } from "./Transcript/ToolDetails";

export function BrowserActionNotice({ actions, hold, onNewTab, switching, defaultExpanded = false }: {
  actions: NonNullable<BrowserState["pendingBrowserActions"]>;
  hold?: BrowserState["browserActionHold"];
  onNewTab?: () => void;
  switching?: boolean;
  defaultExpanded?: boolean;
}) {
  const [expanded, setExpanded] = useState(defaultExpanded);
  const [technical, setTechnical] = useState(false);
  if (!actions.length) return null;
  const currentTabHeld = browserActionNeedsReview(actions, hold);
  const attempts = [...new Set(actions.map(action => action.attemptId))];
  return (
    <aside aria-label="Browser action needs review" data-blocking={currentTabHeld} className={`browser-action-notice shrink-0 border-b border-line px-3 py-1 text-xs ${currentTabHeld ? "bg-warn-soft text-warn" : "bg-surface text-muted"}`}>
      <div className="flex flex-wrap items-center justify-between gap-x-3">
        <span className="flex min-w-0 items-center gap-2 font-medium">
          <Icon.Alert size={14} className="shrink-0" />
          {currentTabHeld ? "Earlier browser action unverified" : "Unverified action on another tab"}
        </span>
        <button type="button" aria-expanded={expanded} onClick={() => { setExpanded(!expanded); setTechnical(false); }}
          className="min-h-8 rounded px-2 font-medium hover:bg-surface-hover">{expanded ? "Hide action details" : "Review action details"}</button>
      </div>
      {expanded && <div className="mb-2 max-h-64 space-y-2 overflow-auto">
        <p>Kageha couldn’t confirm the result of an earlier action. Check the original page before repeating it.</p>
        {actions.some(action => action.canRequestReview) &&
          <p>If the result is visible, tell Kageha what you see and ask it to continue on this tab. It will ask you to review the click.</p>}
        <p>{hold?.unknownTarget ? "Kageha can’t confirm which tab is safe to use. Review the earlier action before the agent continues." : currentTabHeld ? "Open a new tab to continue other browsing." : "You can continue unrelated browsing on this tab."}</p>
        {hold?.truncated && <p>Showing {actions.length} of {hold.total} actions that need review.</p>}
        {currentTabHeld && !hold?.unknownTarget && onNewTab && (
          <button type="button" disabled={switching} onClick={onNewTab}
            className="mr-2 min-h-8 rounded border border-line bg-surface px-2 font-medium text-ink hover:bg-surface-hover disabled:opacity-60">
            {switching ? "Opening tab…" : "Open a new tab"}
          </button>
        )}
        <button type="button" aria-expanded={technical} onClick={() => setTechnical(!technical)}
          className="min-h-8 rounded px-2 font-medium hover:bg-surface-hover">
          {technical ? "Hide technical log" : "Show technical log"}
        </button>
        {technical && attempts.map(attemptId => <ToolDetails key={attemptId} card={{ id: attemptId, attemptId, name: "browser_action", status: "failed" }} />)}
      </div>}
    </aside>
  );
}
