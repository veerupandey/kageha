import { useCallback, useMemo, useState } from "react";
import { useAppStore } from "../store";
import { renderMarkdownHtml } from "../lib/markdownHtml";
import { resolveAskHumanMessageId, detailText, credentialIntent } from "../lib/askHumanApproval";
import { isContinueApproval, isPlanApproval, isQuestionApproval, isCredentialApproval, isApprovalSuppressed } from "../store/helpers";
import { AskHumanPrompt } from "./AskHumanPrompt";
import { PluginConnectionCard } from "./PluginConnectionCard";
import { SecureLoginCard } from "./SecureLoginCard";
import { CredentialPrompt } from "./CredentialPrompt";

const oneTimeToolPrompts: Record<string, { title: string; label: string }> = {
  browser_upload: { title: "Approve file upload", label: "Allow upload once" },
  browser_dialog: { title: "Approve browser dialog", label: "Allow response once" },
  browser_review_action: { title: "Review earlier browser click", label: "Continue without repeating" },
  host_import_files: { title: "Read host files into this chat", label: "Allow these files" },
  computer_request_control: { title: "Use keyboard and pointer in this turn", label: "Allow control this turn" },
  computer_launch_app: { title: "Open an installed application", label: "Allow launch once" },
};

/** Plan Build approval — focused on approve/suggest/deny. */
function PlanApprovalBanner({
  detail,
  onApprove,
  onSuggest,
  onDeny,
}: {
  detail: string;
  onApprove: () => void;
  onSuggest: (feedback: string) => void;
  onDeny: () => void;
}) {
  const [suggestion, setSuggestion] = useState("");
  const [expanded, setExpanded] = useState(false);
  const [fired, setFired] = useState(false);
  // Warn the user if they click Build while there's unsent suggestion text.
  const [buildWithDraft, setBuildWithDraft] = useState(false);

  const fire = useCallback((fn: () => void) => {
    if (fired) return;
    setFired(true);
    fn();
  }, [fired]);

  // Parse plan steps — detail is stable for the lifetime of this banner mount.
  const { steps, tldr, objective } = useMemo(() => {
    const steps: { id: string; text: string }[] = [];
    let tldr = "";
    let objective = "";
    if (detail) {
      for (const line of detail.split("\n")) {
        // Try: - [ ] `id`: description
        let stepMatch = line.match(/^-\s*\[[ x]]\s*`([^`]+)`[:\s]+(.+)/);
        if (!stepMatch) {
          // Try: - [ ] description (no id)
          stepMatch = line.match(/^-\s*\[[ x]]\s+(.{3,})/);
          if (stepMatch) {
            steps.push({ id: String(steps.length + 1), text: stepMatch[1].trim() });
            continue;
          }
        }
        if (stepMatch) {
          steps.push({ id: stepMatch[1], text: (stepMatch[2] || stepMatch[1]).trim() });
        }
        // Extract metadata
        if (line.includes("**Objective:**")) {
          objective = line.replace(/.*\*\*Objective:\*\*\s*/, "").trim();
        } else if (line.includes("**TL;DR:**")) {
          tldr = line.replace(/.*\*\*TL;DR:\*\*\s*/, "").trim();
        }
      }
    }
    return { steps, tldr, objective };
  }, [detail]);

  // Pre-render markdown once — also stable for the lifetime of the banner.
  const detailHtml = useMemo(
    () => detail ? renderMarkdownHtml(detail) : "",
    [detail],
  );

  return (
    <div
      className="border-t border-accent/20 bg-accent-soft/30 px-4 py-5 md:px-5 animate-[slideUp_200ms_ease-out]"
      role="alertdialog"
      aria-label="Plan ready for approval"
    >
      <div className="mx-auto max-w-3xl">
        {/* Header */}
        <div className="flex items-center gap-2.5 mb-3">
          <span className="flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-accent/15">
            <svg width="18" height="18" viewBox="0 0 16 16" fill="none" className="text-accent">
              <path d="M3 8.5L6.5 12L13 4" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"/>
            </svg>
          </span>
          <div>
            <p className="text-sm font-semibold text-ink">
              Plan ready — approve to build
            </p>
            {(objective || tldr) && (
              <p className="text-xs text-muted mt-0.5">
                {tldr || objective}
              </p>
            )}
          </div>
        </div>

        {/* Plan steps — rendered as a visual checklist */}
        {steps.length > 0 ? (
          <div className="mb-3 rounded-lg border border-line/60 bg-surface overflow-hidden">
            <div className="px-3 py-2 border-b border-line/40 flex items-center justify-between">
              <span className="text-[0.65rem] font-semibold uppercase tracking-wider text-muted">
                {steps.length} step{steps.length === 1 ? "" : "s"}
              </span>
              <button
                type="button"
                className="text-[0.65rem] text-accent hover:underline"
                onClick={() => setExpanded((v) => !v)}
              >
                {expanded ? "Collapse" : "Show full plan"}
              </button>
            </div>
            <ol className="divide-y divide-line/30">
              {steps.map((step, i) => (
                <li key={step.id} className="flex items-start gap-2.5 px-3 py-2">
                  <span className="mt-0.5 flex h-5 w-5 shrink-0 items-center justify-center rounded-full bg-line/60 text-[0.6rem] font-bold text-muted">
                    {i + 1}
                  </span>
                  <span className="text-[0.82rem] leading-snug text-ink">{step.text}</span>
                </li>
              ))}
            </ol>
            {expanded && detailHtml && (
              <div
                className="markdown border-t border-line/40 px-3 py-3 text-[0.8rem] overflow-auto max-h-[50vh]"
                dangerouslySetInnerHTML={{ __html: detailHtml }}
              />
            )}
          </div>
        ) : detail ? (
          <div className="mb-3 rounded-lg border border-line/60 bg-surface overflow-hidden">
            <div className="px-3 py-2 border-b border-line/40 flex items-center justify-between">
              <span className="text-[0.65rem] font-semibold uppercase tracking-wider text-muted">
                Plan
              </span>
              <button
                type="button"
                className="text-[0.65rem] text-accent hover:underline"
                onClick={() => setExpanded((v) => !v)}
              >
                {expanded ? "Collapse" : "Expand"}
              </button>
            </div>
            <div
              className={`markdown px-3 py-3 text-[0.8rem] overflow-auto ${expanded ? "max-h-[50vh]" : "max-h-48"}`}
              dangerouslySetInnerHTML={{ __html: detailHtml }}
            />
          </div>
        ) : null}

        {/* Suggestion input */}
        <div className="flex items-center gap-2">
          <input
            type="text"
            disabled={fired}
            className="min-w-0 flex-1 rounded-lg border border-line bg-surface px-3 py-2 text-sm outline-none placeholder:text-faint focus:border-accent/40 focus:ring-2 focus:ring-accent/10 disabled:opacity-40"
            placeholder="Suggest changes to the plan…"
            value={suggestion}
            onChange={(e) => { setSuggestion(e.target.value); setBuildWithDraft(false); }}
            onKeyDown={(e) => {
              if (e.key === "Enter" && suggestion.trim()) {
                e.preventDefault();
                fire(() => { onSuggest(suggestion.trim()); });
              }
            }}
          />
        </div>

        {/* Draft-discard warning */}
        {buildWithDraft && (
          <p className="mt-2 text-xs text-warn">
            You have unsent suggestion text. Click <strong>Build</strong> again to proceed without it, or <strong>Suggest</strong> to send it first.
          </p>
        )}

        {/* Action buttons */}
        <div className="mt-3 flex flex-wrap items-center gap-2">
          <button
            type="button"
            disabled={fired}
            className="rounded-lg bg-accent px-5 py-2 text-sm font-medium text-white shadow-sm hover:opacity-90 active:scale-[0.97] transition-all disabled:opacity-40 disabled:cursor-not-allowed"
            onClick={() => {
              if (suggestion.trim() && !buildWithDraft) {
                // First click with unsent draft — warn instead of firing.
                setBuildWithDraft(true);
                return;
              }
              fire(onApprove);
            }}
          >
            ▶ Build
          </button>
          <button
            type="button"
            disabled={fired || !suggestion.trim()}
            className="rounded-lg border border-line bg-surface px-4 py-2 text-sm font-medium text-ink shadow-sm hover:bg-line/50 active:scale-[0.97] disabled:opacity-40 disabled:cursor-not-allowed transition-all"
            onClick={() => fire(() => { onSuggest(suggestion.trim()); })}
          >
            ✎ Suggest
          </button>
          <button
            type="button"
            disabled={fired}
            className="rounded-lg px-3 py-2 text-sm text-danger hover:bg-danger-soft transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
            onClick={() => fire(onDeny)}
          >
            Reject
          </button>
        </div>
      </div>
    </div>
  );
}

/** Tool approval — Once / Session / Full / Deny. */
function ToolApprovalBanner({
  title,
  detail,
  onceLabel,
  onApprove,
  onDeny,
}: {
  title: string;
  detail: string;
  onceLabel?: string;
  onApprove: (scope: "once" | "session" | "full") => void;
  onDeny: () => void;
}) {
  const [fired, setFired] = useState(false);
  // Full access persists until changed; destructive actions still ask.
  const [confirmFull, setConfirmFull] = useState(false);

  const fire = useCallback((fn: () => void) => {
    if (fired) return;
    setFired(true);
    fn();
  }, [fired]);

  return (
    <div
      className="border-t border-warn/25 bg-warn-soft px-4 py-4 md:px-5 animate-[slideUp_200ms_ease-out]"
      role="alertdialog"
      aria-label="Tool needs approval"
    >
      <div className="mx-auto max-w-3xl">
        <div className="flex items-center gap-2 mb-2">
          <svg width="16" height="16" viewBox="0 0 16 16" fill="none" className="text-warn shrink-0">
            <path d="M8 5v3.5M8 10.5h.007M14 8A6 6 0 112 8a6 6 0 0112 0z" stroke="currentColor" strokeWidth="1.25" strokeLinecap="round"/>
          </svg>
          <p className="text-sm font-semibold text-ink">{title}</p>
        </div>

        {detail ? (
          <pre className="mt-1 max-h-24 overflow-auto rounded-md border border-line bg-surface p-2.5 font-mono text-xs text-muted whitespace-pre-wrap">
            {detail}
          </pre>
        ) : null}

        <div className="mt-3 flex flex-wrap items-center gap-2">
          <button
            type="button"
            disabled={fired}
            className="rounded-md bg-accent px-3 py-1.5 text-sm font-medium text-white hover:opacity-90 active:scale-[0.97] disabled:opacity-40 disabled:cursor-not-allowed"
            onClick={() => fire(() => onApprove("once"))}
          >
            {onceLabel || "Allow permission"}
          </button>
          {!onceLabel && <button
            type="button"
            disabled={fired}
            className="rounded-md border border-line bg-surface px-3 py-1.5 text-sm font-medium text-ink hover:bg-line/50 disabled:opacity-40 disabled:cursor-not-allowed"
            onClick={() => fire(() => onApprove("session"))}
          >
            All routine permissions
          </button>}

          {/* Full access — two-step confirm to prevent accidental permanent grants */}
          {!onceLabel && (confirmFull ? (
            <button
              type="button"
              disabled={fired}
              className="rounded-md border border-danger/60 bg-danger-soft px-3 py-1.5 text-sm font-semibold text-danger hover:bg-danger/10 active:scale-[0.97] disabled:opacity-40 disabled:cursor-not-allowed"
              title="Skip routine approval prompts; destructive actions still ask"
              onClick={() => fire(() => onApprove("full"))}
            >
              Confirm full access
            </button>
          ) : (
            <button
              type="button"
              disabled={fired}
              className="rounded-md border border-line bg-surface px-3 py-1.5 text-sm font-medium text-muted hover:text-ink hover:border-danger/40 hover:bg-danger-soft/50 disabled:opacity-40 disabled:cursor-not-allowed"
              title="Keep routine tool access across sessions; destructive actions still ask"
              onClick={() => setConfirmFull(true)}
            >
              Full access
            </button>
          ))}

          <span className="mx-1 text-line-strong" aria-hidden="true">|</span>
          <button
            type="button"
            disabled={fired}
            className="rounded-md px-3 py-1.5 text-sm text-muted hover:text-ink hover:bg-line/50 disabled:opacity-40 disabled:cursor-not-allowed"
            onClick={() => fire(onDeny)}
          >
            Deny
          </button>
        </div>

        {!onceLabel && confirmFull && !fired && (
          <p className="mt-2 text-xs text-warn">
            Full access keeps routine tool access across sessions and restarts. Destructive actions still ask.{" "}
            <button
              type="button"
              className="underline underline-offset-2 hover:text-ink"
              onClick={() => setConfirmFull(false)}
            >
              Cancel
            </button>
          </p>
        )}
      </div>
    </div>
  );
}

function SkillProposalBanner({
  title,
  detail,
  onSave,
  onDismiss,
}: {
  title: string;
  detail: string;
  onSave: () => void;
  onDismiss: () => void;
}) {
  const [fired, setFired] = useState(false);

  const fire = useCallback((fn: () => void) => {
    if (fired) return;
    setFired(true);
    fn();
  }, [fired]);

  return (
    <div className="border-t border-accent/20 bg-accent-soft/50 px-4 py-4 md:px-5 animate-[slideUp_200ms_ease-out]" role="alertdialog" aria-label="Reusable skill suggestion">
      <div className="mx-auto max-w-3xl">
        <p className="text-sm font-semibold text-ink">{title}</p>
        <pre className="mt-2 max-h-40 overflow-auto rounded-md border border-line bg-surface p-2.5 font-mono text-xs text-muted whitespace-pre-wrap">{detail}</pre>
        <div className="mt-3 flex gap-2">
          <button
            type="button"
            disabled={fired}
            className="rounded-md bg-accent px-3 py-1.5 text-sm font-medium text-white hover:opacity-90 disabled:opacity-40 disabled:cursor-not-allowed"
            onClick={() => fire(onSave)}
          >
            Save skill
          </button>
          <button
            type="button"
            disabled={fired}
            className="rounded-md px-3 py-1.5 text-sm text-muted hover:bg-line/50 disabled:opacity-40 disabled:cursor-not-allowed"
            onClick={() => fire(onDismiss)}
          >
            Dismiss
          </button>
        </div>
      </div>
    </div>
  );
}

/** Continue — keep going after a budget / no-progress pause. */
function ContinueBanner({
  detail,
  onContinue,
  onStop,
}: {
  detail: string;
  onContinue: () => void;
  onStop: () => void;
}) {
  const [fired, setFired] = useState(false);
  const fire = useCallback((fn: () => void) => {
    if (fired) return;
    setFired(true);
    fn();
  }, [fired]);

  return (
    <div
      className="border-t border-accent/20 bg-accent-soft/50 px-4 py-4 md:px-5 animate-[slideUp_200ms_ease-out]"
      role="alertdialog"
      aria-label="Continue the agent"
    >
      <div className="mx-auto max-w-3xl">
        <div className="flex items-start gap-3">
          <span className="mt-0.5 flex h-7 w-7 shrink-0 items-center justify-center rounded-full bg-accent text-white text-xs font-bold">
            ▶
          </span>
          <div className="min-w-0 flex-1">
            <p className="text-sm font-medium text-ink leading-relaxed">
              Keep going?
            </p>
            {detail ? (
              <p className="mt-1 text-sm text-muted leading-relaxed whitespace-pre-wrap">
                {detail}
              </p>
            ) : null}
          </div>
        </div>
        <div className="mt-3 ml-10 flex flex-wrap gap-2">
          <button
            type="button"
            disabled={fired}
            className="rounded-md bg-accent px-4 py-2 text-sm font-medium text-white hover:opacity-90 active:scale-[0.97] disabled:opacity-40 disabled:cursor-not-allowed"
            onClick={() => fire(onContinue)}
          >
            Continue
          </button>
          <button
            type="button"
            disabled={fired}
            className="rounded-md border border-line bg-surface px-4 py-2 text-sm font-medium text-ink hover:bg-line/50 active:scale-[0.97] disabled:opacity-40 disabled:cursor-not-allowed"
            onClick={() => fire(onStop)}
          >
            Stop
          </button>
        </div>
      </div>
    </div>
  );
}

/** Routes to the correct banner type based on approval context. */
export function ApprovalBanner() {
  const pending = useAppStore((s) => s.pendingApproval);
  const messages = useAppStore((s) => s.messages);
  const sessionId = useAppStore((s) => s.sessionId);
  const resolveApproval = useAppStore((s) => s.resolveApproval);
  const submitAskHumanAnswer = useAppStore((s) => s.submitAskHumanAnswer);
  const deferAskHuman = useAppStore((s) => s.deferAskHuman);
  const submitCredential = useAppStore((s) => s.submitCredential);
  const cancelCredential = useAppStore((s) => s.cancelCredential);

  const isPlan = pending ? isPlanApproval(pending) : false;
  const isQuestion = pending ? isQuestionApproval(pending) : false;
  const isCredential = pending ? isCredentialApproval(pending) : false;
  const isContinue = pending ? isContinueApproval(pending) : false;
  const detail = pending ? detailText(pending.detail) : "";
  // UI safety net: never let a secret / verification code ride the plain-text
  // ask_human field, even if the backend guard were bypassed.
  const questionText = pending
    ? String(pending.question || "") + " " + detail
    : "";
  const askIntent = pending && isQuestion ? credentialIntent(questionText) : "";
  const inlineAskHumanId = useMemo(
    () => resolveAskHumanMessageId(messages, pending, sessionId),
    [messages, pending, sessionId],
  );
  if (!pending?.approval_id || isApprovalSuppressed(sessionId, pending)) return null;

  if (pending.action === "request_plugin_connection") {
    let request: { server?: string; purpose?: string } = {};
    try { request = JSON.parse(detail); } catch { /* malformed details remain cancellable */ }
    if (!request.server) return <button onClick={() => void resolveApproval(false)}>Cancel invalid connection request</button>;
    return <div className="border-t border-line p-4"><PluginConnectionCard
      key={`${sessionId}:${pending.approval_id}`} id={request.server} purpose={request.purpose}
      onReady={() => void resolveApproval(true)} onCancel={() => void resolveApproval(false)} /></div>;
  }

  if (pending.action === "request_secure_login") {
    return <SecureLoginCard key={`${sessionId}:${pending.approval_id}`} detail={detail} />;
  }

  // A credential-intent ask_human is diverted to the masked secure prompt.
  if (isCredential || askIntent === "credential") {
    return (
      <div className="border-t border-line bg-canvas px-4 py-3 md:px-6">
        <div className="mx-auto max-w-3xl">
          <CredentialPrompt
            key={`${sessionId}:${pending.approval_id}`}
            pending={pending}
            onSubmit={(value) => void submitCredential(value)}
            onCancel={() => void cancelCredential()}
          />
        </div>
      </div>
    );
  }

  // A verification / OTP-code request is refused: no input is offered. The user
  // completes their own 2FA in the browser they control.
  if (askIntent === "verification") {
    return (
      <div className="border-t border-rose-400/25 bg-canvas px-4 py-4 md:px-6">
        <div className="mx-auto max-w-3xl rounded-2xl border border-rose-400/30 bg-rose-50/50 p-4 dark:bg-rose-500/10">
          <p className="text-[0.68rem] font-semibold uppercase tracking-[0.16em] text-rose-600 dark:text-rose-400">
            Blocked for your safety
          </p>
          <p className="mt-1 text-sm font-medium text-ink">
            The agent asked for a verification / sign-in code. Kageha won't relay
            one-time codes.
          </p>
          <p className="mt-1 text-xs text-muted">
            If a login needs verification, take control of the browser panel and
            complete it yourself, or dismiss this.
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
      </div>
    );
  }

  if (isQuestion && !inlineAskHumanId) {
    return (
      <div className="border-t border-accent/20 bg-canvas px-4 py-4 md:px-6">
        <div className="mx-auto max-w-3xl">
          <AskHumanPrompt
            pending={pending}
            onAnswer={(answer) => submitAskHumanAnswer(answer)}
            onDefer={() => void deferAskHuman()}
          />
        </div>
      </div>
    );
  }

  if (isQuestion) return null;

  if (isContinue) {
    return (
      <ContinueBanner
        detail={detail}
        onContinue={() => void resolveApproval(true, "", "once")}
        onStop={() => void resolveApproval(false)}
      />
    );
  }

  if (isPlan) {
    return (
      <PlanApprovalBanner
        detail={detail}
        onApprove={() => void resolveApproval(true, "", "once")}
        onSuggest={(feedback) => void resolveApproval(false, feedback)}
        onDeny={() => void resolveApproval(false)}
      />
    );
  }

  if (pending.action === "save_skill") {
    return (
      <SkillProposalBanner
        title={pending.label || "Save this workflow as a reusable skill?"}
        detail={detail}
        onSave={() => void resolveApproval(true, "", "once")}
        onDismiss={() => void resolveApproval(false)}
      />
    );
  }

  // Default: tool approval
  const once = oneTimeToolPrompts[pending.action ?? ""];
  const title = once?.title || pending.label || pending.action || "Approval needed";
  return (
    <ToolApprovalBanner
      key={pending.approval_id}
      title={title}
      detail={detail}
      onceLabel={once?.label ?? (pending.profile_scoped ? "Allow once" : undefined)}
      onApprove={(scope) => void resolveApproval(true, "", scope)}
      onDeny={() => void resolveApproval(false)}
    />
  );
}
