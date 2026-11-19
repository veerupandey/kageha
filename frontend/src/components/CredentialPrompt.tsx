import { useCallback, useEffect, useRef, useState } from "react";
import type { PendingApproval } from "../api/types";
import { cn } from "../lib/cn";

/**
 * Secure credential entry for the browser-login flow. Unlike AskHumanPrompt,
 * the value here is a SECRET: it is masked, never echoed into the transcript,
 * and submitted to a dedicated endpoint (store.submitCredential) that deposits
 * it encrypted and hands the agent only an opaque handle. It must never be
 * routed through sendMessage.
 */
export function CredentialPrompt({
  pending,
  onSubmit,
  onCancel,
  className,
}: {
  pending: PendingApproval;
  onSubmit: (value: string) => void;
  onCancel: () => void;
  className?: string;
}) {
  const label = (pending.label || pending.question || "credential").trim();
  const [value, setValue] = useState("");
  const [fired, setFired] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);
  const canSubmit = Boolean(value) && !fired;

  useEffect(() => {
    requestAnimationFrame(() => inputRef.current?.focus());
  }, []);

  const submit = useCallback(() => {
    if (!value || fired) return;
    setFired(true);
    onSubmit(value);
    setValue(""); // drop the secret from component state immediately
  }, [value, fired, onSubmit]);

  return (
    <div
      className={cn(
        "mt-2 overflow-hidden rounded-xl border border-line/80 bg-surface shadow-sm",
        className,
      )}
      role="group"
      aria-label="Provide a credential securely"
    >
      <div className="border-b border-line/40 px-3.5 py-3">
        <div className="flex items-start gap-3">
          <span className="mt-0.5 flex h-7 w-7 shrink-0 items-center justify-center rounded-lg border border-line bg-canvas text-xs text-muted">
            <svg width="14" height="14" viewBox="0 0 16 16" fill="none" aria-hidden="true">
              <path
                d="M4.5 7V5.5a3.5 3.5 0 017 0V7M4 7h8v6H4V7z"
                stroke="currentColor"
                strokeWidth="1.4"
                strokeLinecap="round"
                strokeLinejoin="round"
              />
            </svg>
          </span>
          <div className="min-w-0">
            <p className="text-[0.68rem] font-semibold uppercase tracking-[0.14em] text-muted">
              Secure send
            </p>
            <p className="mt-1 text-[0.95rem] font-medium leading-relaxed text-ink">
              Enter your {label} to sign in.
            </p>
            <p className="mt-0.5 text-xs text-muted">
              Stored encrypted and passed by handle. Never shown to the
              agent, the chat, or the logs.
            </p>
          </div>
        </div>
      </div>

      <div className="space-y-3 px-3.5 py-3.5">
        <input
          ref={inputRef}
          type="password"
          autoComplete="off"
          disabled={fired}
          value={value}
          onChange={(e) => setValue(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && canSubmit) {
              e.preventDefault();
              submit();
            } else if (e.key === "Escape") {
              e.preventDefault();
              if (!fired) onCancel();
            }
          }}
          placeholder={`Your ${label}…`}
          className="w-full rounded-lg border border-line bg-canvas px-3 py-2.5 text-sm outline-none placeholder:text-faint focus:border-accent/40 focus:ring-3 focus:ring-accent/10 disabled:opacity-40"
        />
        <div className="flex flex-wrap items-center justify-between gap-3 border-t border-line/40 pt-3">
          <p className="text-xs text-muted">Press Enter to send securely, Esc to cancel.</p>
          <div className="flex items-center gap-2">
            <button
              type="button"
              disabled={fired}
              className="rounded-lg px-3 py-2 text-sm text-muted hover:bg-line/40 hover:text-ink disabled:opacity-40"
              onClick={onCancel}
            >
              Cancel
            </button>
            <button
              type="button"
              disabled={!canSubmit}
              className="rounded-lg bg-accent px-4 py-2 text-sm font-semibold text-white shadow-sm hover:opacity-90 active:scale-[0.98] disabled:cursor-not-allowed disabled:opacity-40"
              onClick={submit}
            >
              Send securely
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
