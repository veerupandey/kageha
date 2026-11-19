import { useAppStore } from "../store";

function toastTone(text: string): "warn" | "ok" | "neutral" {
  const lower = text.toLowerCase();
  if (
    lower.includes("needs an api key") ||
    lower.includes("could not") ||
    lower.includes("failed") ||
    lower.includes("error")
  ) {
    return "warn";
  }
  if (lower.includes("saved") || lower.includes("ready") || lower.endsWith(" ok")) {
    return "ok";
  }
  return "neutral";
}

export function Toasts() {
  const toasts = useAppStore((s) => s.toasts);
  const dismissToast = useAppStore((s) => s.dismissToast);
  const cancelAskHumanSend = useAppStore((s) => s.cancelAskHumanSend);
  const requestPeerAlertNav = useAppStore((s) => s.requestPeerAlertNav);

  if (!toasts.length) return null;

  return (
    <div
      className="fixed bottom-4 right-4 z-[120] flex max-w-sm flex-col gap-2"
      id="toast-stack"
      aria-live="polite"
    >
      {toasts.map((t) => {
        const tone = toastTone(t.text);
        return (
          <div
            key={t.id}
            className={
              tone === "warn"
                ? "flex items-start gap-3 rounded-lg border border-warn/35 bg-surface px-3 py-2.5 text-sm shadow-lg"
                : tone === "ok"
                  ? "flex items-start gap-3 rounded-lg border border-accent/30 bg-surface px-3 py-2.5 text-sm shadow-lg"
                  : "flex items-start gap-3 rounded-lg border border-line bg-surface px-3 py-2.5 text-sm shadow-lg"
            }
            role="status"
          >
            <span className="min-w-0 flex-1">{t.text}</span>
            {t.actionLabel && t.actionId === "ask_human_undo" ? (
              <button
                type="button"
                className="shrink-0 font-medium text-accent hover:underline"
                onClick={() => cancelAskHumanSend()}
              >
                {t.actionLabel}
              </button>
            ) : t.actionLabel && t.actionId === "open_agent_network" ? (
              <button
                type="button"
                className="shrink-0 font-medium text-accent hover:underline"
                onClick={() => {
                  requestPeerAlertNav();
                  dismissToast(t.id);
                }}
              >
                {t.actionLabel}
              </button>
            ) : null}
            <button
              type="button"
              className="inline-flex h-7 w-7 shrink-0 items-center justify-center rounded-md text-faint hover:bg-line/50 hover:text-ink"
              aria-label="Dismiss"
              onClick={() => dismissToast(t.id)}
            >
              ×
            </button>
          </div>
        );
      })}
    </div>
  );
}
