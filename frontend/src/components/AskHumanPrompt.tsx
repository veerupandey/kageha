import { useId, useState } from "react";
import type { PendingApproval } from "../api/types";
import { parseAskHumanApproval } from "../lib/askHumanApproval";
import { cn } from "../lib/cn";

export interface AskHumanPromptProps {
  pending: PendingApproval;
  onAnswer: (answer: string) => void | Promise<void>;
  onDefer: () => void | Promise<void>;
  compact?: boolean;
  className?: string;
}

export function AskHumanPrompt({ pending, onAnswer, onDefer, compact, className }: AskHumanPromptProps) {
  const view = parseAskHumanApproval(pending);
  const name = useId();
  const [selected, setSelected] = useState<number | null>(null);
  const [custom, setCustom] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const own = !view.options.length || selected === view.options.length;
  const answer = own ? custom.trim() : selected === null ? "" : view.options[selected];
  const send = async (submit: () => void | Promise<void>) => {
    if (busy) return;
    setBusy(true);
    setError("");
    try {
      await submit();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Could not send your response.");
      setBusy(false);
    }
  };

  return (
    <form
      className={cn("rounded-xl border border-line bg-surface", compact ? "p-3" : "p-4", className)}
      aria-label="Answer the agent's question"
      onSubmit={(event) => {
        event.preventDefault();
        if (answer) void send(() => onAnswer(answer));
      }}
    >
      <fieldset disabled={busy} className="min-w-0">
        <legend className="w-full text-sm font-semibold leading-relaxed text-ink">{view.question}</legend>
        {view.options.length > 0 && (
          <div className="mt-2 space-y-0.5">
            {[...view.options, "Your own answer"].map((label, index) => (
              <label key={index} className={cn(
                "flex cursor-pointer items-start gap-2.5 rounded-md px-2 py-2 text-sm text-ink hover:bg-line/40",
                selected === index && "bg-line/40",
              )}>
                <input type="radio" name={name} checked={selected === index} onChange={() => setSelected(index)}
                  className="mt-0.5 size-4 shrink-0 accent-accent" />
                <span className="min-w-0 break-words">{label}</span>
              </label>
            ))}
          </div>
        )}
        {own && (
          <input aria-label="Your answer" type="text" value={custom} maxLength={4096}
            onChange={(event) => setCustom(event.target.value)}
            placeholder={view.options.length ? "Something else…" : "Type your answer…"}
            className="mt-2 w-full rounded-md border border-line bg-canvas px-3 py-2 text-sm text-ink outline-none focus-visible:ring-2 focus-visible:ring-accent" />
        )}
      </fieldset>
      {error && <p role="alert" className="mt-2 text-xs text-danger">{error}</p>}
      <div className="mt-3 flex justify-end gap-2">
        <button type="button" disabled={busy} onClick={() => void send(onDefer)}
          className="rounded-md border border-line px-3 py-1.5 text-xs font-medium text-ink hover:bg-line/40 disabled:opacity-50">Skip</button>
        <button type="submit" disabled={!answer || busy}
          className="rounded-md bg-ink px-3 py-1.5 text-xs font-semibold text-canvas disabled:opacity-40">Send answer</button>
      </div>
    </form>
  );
}
