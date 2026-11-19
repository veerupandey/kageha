import { modelActivityLabel } from "../../lib/activityUi";
import { memo, useEffect, useLayoutEffect, useRef, useState } from "react";
import { formatDuration } from "../../lib/transcript";
import { cn } from "../../lib/cn";
import { Icon } from "../../lib/icons";

export const ReasoningBlock = memo(function ReasoningBlock({
  durationMs,
  text,
  streaming,
  streamingDetail,
  activityLabel,
  openLiveByDefault = false,
}: {
  durationMs?: number;
  text?: string;
  streaming?: boolean;
  streamingDetail?: string;
  activityLabel?: string;
  openLiveByDefault?: boolean;
}) {
  const [manualOpen, setManualOpen] = useState<boolean | undefined>(undefined);
  const [elapsed, setElapsed] = useState(0);
  const bodyRef = useRef<HTMLParagraphElement>(null);
  const followRef = useRef(true);
  const duration =
    durationMs != null && durationMs > 0 ? formatDuration(durationMs) : "";
  const bodyText = text || "";
  const waiting = Boolean(streaming && !bodyText.trim());
  useEffect(() => {
    if (!waiting) return;
    const started = performance.now();
    setElapsed(0);
    const timer = window.setInterval(() => setElapsed(performance.now() - started), 1000);
    return () => window.clearInterval(timer);
  }, [waiting, activityLabel]);
  const modelLabel = modelActivityLabel(streamingDetail);
  const detail = modelLabel ? undefined : streamingDetail;
  const elapsedLabel = waiting && elapsed >= 1000 && !detail?.startsWith("Elapsed:")
    ? formatDuration(elapsed) : "";
  const waitingLabel = activityLabel && !/^thinking[.…]*$/i.test(activityLabel.trim()) ? activityLabel : "Waiting for model…";
  const phaseLabel = bodyText.trim() ? (activityLabel || "Thinking…") : modelLabel ?? waitingLabel;
  const label = streaming
    ? [phaseLabel, detail].filter(Boolean).join(" · ")
    : duration ? `Thought for ${duration}` : "Thought";
  const showBody = manualOpen ?? Boolean(openLiveByDefault && streaming && bodyText.trim());

  // A manual choice lasts for this phase, not for a later completed thought.
  // Token updates do not reopen a body the reader deliberately collapsed.
  useLayoutEffect(() => { setManualOpen(undefined); }, [streaming]);

  useLayoutEffect(() => {
    if (!streaming) {
      followRef.current = true;
      return;
    }
    const el = bodyRef.current;
    if (!el || !followRef.current) return;
    el.scrollTop = el.scrollHeight;
  }, [streaming, text, showBody]);

  if (!streaming && !bodyText.trim()) return null;

  return (
    <div className="ka-reasoning-block mb-1.5">
      <button
        type="button"
        className="inline-flex items-center gap-1 rounded-md py-0.5 text-[0.75rem] text-faint hover:text-muted"
        aria-expanded={showBody}
        onClick={() => setManualOpen(!showBody)}
      >
        <Icon.ChevronRight
            size={12}
            className={cn(
              "transition-transform",
              showBody ? "rotate-90" : "rotate-0",
              "shrink-0",
            )}
          />
        <span className="font-medium text-left">{label}</span>
        {elapsedLabel && <span className="shrink-0 rounded bg-accent/10 px-1.5 text-accent tabular-nums" aria-label={`Elapsed ${elapsedLabel}`}>{elapsedLabel}</span>}
        {streaming ? (
          <span className="ka-thinking-dots ml-1 shrink-0" aria-hidden="true">
            <span className="ka-thinking-dot" />
            <span className="ka-thinking-dot" />
            <span className="ka-thinking-dot" />
          </span>
        ) : null}
      </button>
      {showBody ? (
        <p
          ref={bodyRef}
          data-testid="reasoning-body"
          onScroll={() => {
            const el = bodyRef.current;
            if (!el || !streaming) return;
            followRef.current =
              el.scrollHeight - el.scrollTop - el.clientHeight < 40;
          }}
          className={cn(
            "mt-1 max-h-48 w-full max-w-2xl overflow-auto whitespace-pre-wrap break-words pl-4 text-[0.8rem] font-normal leading-relaxed text-faint",
            streaming && !bodyText && "animate-pulse",
          )}
        >
          {bodyText || "No reasoning text received yet. Some providers do not share reasoning text; this activity indicator does not mean tokens are available."}
        </p>
      ) : null}
    </div>
  );
});
