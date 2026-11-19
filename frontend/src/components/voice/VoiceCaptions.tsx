import { useEffect, useRef } from "react";

import { cn } from "../../lib/cn";
import { CAPTION_RETENTION_LIMIT, type CaptionEntry } from "./voiceTypes";

export interface VoiceCaptionsProps {
  /** Already chronological, already bounded upstream. */
  captions: CaptionEntry[];
  /** Optional cap for retained entries; defaults to CAPTION_RETENTION_LIMIT. */
  limit?: number;
}

const ROLE_LABEL: Record<CaptionEntry["role"], string> = {
  user: "You",
  assistant: "Assistant",
};

/**
 * Renders the rolling live-transcript caption list.
 *
 * - Empty list → placeholder indicating the transcript will appear (Req 5.8);
 *   the placeholder is removed once the first entry lands (Req 5.9).
 * - Entries render in `seq` order, attributed by role (Req 5.5).
 * - The newest entry is scrolled into the visible region (Req 5.6).
 * - Retention is enforced by the pure append upstream; rendering does not
 *   re-bound, but honors `limit` for display consistency (Req 5.7).
 */
export function VoiceCaptions({
  captions,
  limit = CAPTION_RETENTION_LIMIT,
}: VoiceCaptionsProps) {
  const bottomRef = useRef<HTMLDivElement | null>(null);

  // Rendering does not re-bound the list (the pure append owns retention), but
  // we defensively slice to the most-recent `limit` for display consistency so
  // a mis-sized input cannot overflow the region. (Req 5.7)
  const visible =
    limit >= 0 && captions.length > limit
      ? captions.slice(captions.length - limit)
      : captions;

  const isEmpty = visible.length === 0;
  const newestSeq = isEmpty ? null : visible[visible.length - 1].seq;

  // Scroll the newest entry into view whenever it changes. (Req 5.6)
  useEffect(() => {
    if (newestSeq === null) return;
    bottomRef.current?.scrollIntoView({ block: "end" });
  }, [newestSeq]);

  return (
    <div
      className="ka-voice-captions flex max-h-full flex-col gap-2 overflow-y-auto text-sm"
      role="log"
      aria-live="polite"
      aria-label="Conversation transcript"
    >
      {isEmpty ? (
        <p className="ka-voice-captions-placeholder text-center text-muted">
          The conversation transcript will appear here.
        </p>
      ) : (
        <>
          {visible.map((entry) => (
            <div
              key={entry.id}
              data-role={entry.role}
              className={cn(
                "ka-voice-caption flex flex-col gap-0.5",
                entry.role === "user" ? "items-end text-right" : "items-start text-left",
              )}
            >
              <span className="text-[0.65rem] font-medium uppercase tracking-wide text-faint">
                {ROLE_LABEL[entry.role]}
              </span>
              <span
                className={cn(
                  "max-w-[85%] rounded-2xl px-3 py-1.5 leading-relaxed",
                  entry.role === "user"
                    ? "bg-accent text-white"
                    : "bg-surface text-ink ring-1 ring-line",
                )}
              >
                {entry.text}
              </span>
            </div>
          ))}
          <div ref={bottomRef} aria-hidden="true" />
        </>
      )}
    </div>
  );
}

export default VoiceCaptions;
