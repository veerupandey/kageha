import { useAppStore } from "../../store";
import { ApprovalBanner } from "../ApprovalBanner";
import { MessageList } from "../MessageList";
import { MiniComposer } from "./MiniComposer";
import { DelegatedWork } from "../DelegatedWork";
import type { DurableJob } from "../DurableJobs";
import { isEarlyStopLabel } from "../../lib/runStatus";

export function ConversationPanel({ onInspectWorker }: { onInspectWorker?: (job: DurableJob) => void }) {
  const messages = useAppStore((s) => s.messages);
  const error = useAppStore((s) => s.error);
  const clearError = useAppStore((s) => s.clearError);
  const errorShownInConversation = Boolean(error && messages.some(message =>
    message.role === "assistant" && !message.streaming && !isEarlyStopLabel(message.statusLabel) && message.failureCause === error));


  return (
    <div className="ka-conversation-panel relative flex min-h-0 flex-1 flex-col overflow-hidden">
      <section
        className="ka-conversation min-h-0 flex-1 overflow-y-auto overscroll-contain px-3 py-3 sm:px-4 sm:py-4"
        id="conversation"
        tabIndex={0}
        aria-label="Conversation"
      >
        <MessageList messages={messages} />
      </section>

      <div id="conversation-navigation" className="ka-conversation-navigation" />

      <DelegatedWork onInspectWorker={onInspectWorker} />

      <div className="shrink-0 max-h-[45dvh] overflow-y-auto" aria-live="polite">
        <ApprovalBanner />
      </div>

      {error && !errorShownInConversation && (
        <p
          className="flex items-start gap-3 border-t border-[var(--color-danger)]/20 bg-[var(--color-danger-soft)] px-4 py-2 text-sm text-[var(--color-danger)]"
          role="alert"
        >
          <span className="min-w-0 flex-1">{error}</span>
          <button
            type="button"
            className="shrink-0 underline-offset-2 hover:underline"
            onClick={clearError}
          >
            Dismiss
          </button>
        </p>
      )}

      <div className="ka-composer-slot shrink-0">
        <MiniComposer />
      </div>
    </div>
  );
}
