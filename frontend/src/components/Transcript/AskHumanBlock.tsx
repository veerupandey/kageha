import { useMemo } from "react";
import type { TranscriptBlock } from "../../api/types";
import { credentialIntent, pendingApprovalFromBlock } from "../../lib/askHumanApproval";
import { useAppStore } from "../../store";
import { AskHumanPrompt } from "../AskHumanPrompt";
import { isCredentialApproval } from "../../store/helpers";

type AskHumanBlockData = Extract<TranscriptBlock, { type: "ask_human" }>;

export function AskHumanBlock({
  block,
  messageId,
}: {
  block: AskHumanBlockData;
  messageId: string;
}) {
  const pendingApproval = useAppStore((s) => s.pendingApproval);
  const submitAskHumanAnswer = useAppStore((s) => s.submitAskHumanAnswer);
  const deferAskHuman = useAppStore((s) => s.deferAskHuman);

  const pending = useMemo(
    () => pendingApprovalFromBlock(block, pendingApproval),
    [block, pendingApproval],
  );
  const isActive =
    block.status === "pending" &&
    pendingApproval?.approval_id === block.approvalId &&
    pending != null &&
    pending.approval_id === block.approvalId;

  // Credential entry has one owner: the live approval above the composer.
  // Historical blocks must never recreate a secret input after submission.
  if (block.approvalId.startsWith("credential:") ||
      credentialIntent(block.question || "") === "credential" ||
      (pendingApproval?.approval_id === block.approvalId && isCredentialApproval(pendingApproval))) {
    return null;
  }

  if (block.status === "answered" || block.status === "deferred") {
    const isDeferred = block.status === "deferred";
    return (
      <div
        data-ask-human-message={messageId}
        className="my-2 rounded-xl border border-line bg-surface/65 px-3.5 py-3"
      >
        <p className="text-xs text-muted">{block.question}</p>
        <p className="mt-1 text-sm font-medium text-ink">{isDeferred ? "Skipped" : block.answer || "Answered"}</p>
      </div>
    );
  }

  if (!isActive || !pending) {
    return (
      <div
        data-ask-human-message={messageId}
        className="my-2 rounded-xl border border-dashed border-line/70 bg-canvas/40 px-4 py-3 text-sm text-muted"
      >
        {block.question}
      </div>
    );
  }

  return (
    <div data-ask-human-message={messageId} className="my-2">
      {(() => {
        const intent = credentialIntent(block.question || "");
        if (intent === "verification") {
          return (
            <div className="rounded-xl border border-rose-400/30 bg-rose-50/50 p-4 dark:bg-rose-500/10">
              <p className="text-[0.68rem] font-semibold uppercase tracking-[0.16em] text-rose-600 dark:text-rose-400">
                Blocked for your safety
              </p>
              <p className="mt-1 text-sm font-medium text-ink">
                The agent asked for a verification / sign-in code. Kageha won't
                relay one-time codes — complete verification yourself in the
                browser, or dismiss this.
              </p>
              <div className="mt-3 flex justify-end">
                <button
                  type="button"
                  className="rounded-lg px-3 py-2 text-sm text-muted hover:bg-line/40 hover:text-ink"
                  onClick={() => void deferAskHuman()}
                >
                  Dismiss
                </button>
              </div>
            </div>
          );
        }
        return (
          <AskHumanPrompt
            pending={pending}
            compact={block.options.length > 0 && block.options.length <= 2}
            onAnswer={submitAskHumanAnswer}
            onDefer={deferAskHuman}
          />
        );
      })()}
    </div>
  );
}
