import { useState } from "react";
import { createClientId } from "../lib/clientId";
import type { DurableJob } from "./DurableJobs";
import { ContributionSources } from "./ContributionSources";
import { workerName, workerStatus, workerActivity, workerArtifactUrl } from "../lib/workerPresentation";
import { cn } from "../lib/cn";
import { Icon } from "../lib/icons";

export function AgentContributionDetails({ job }: { job: DurableJob }) {
  const contribution = job.result.contribution || job.progress?.contribution;
  if (!contribution) return null;
  return (
    <div className="space-y-4 text-sm leading-relaxed">
      <div className="rounded-xl border border-line/70 bg-sidebar/40 p-4">
        <p className="whitespace-pre-wrap break-words text-sm leading-relaxed text-ink font-sans selection:bg-accent/20">
          {contribution.finding}
        </p>
      </div>
      <ContributionSources job={job} />
      {([
        ["Reference notes", contribution.evidence],
        ["Limitations", contribution.limitations],
        ["Decisions to consider", contribution.decisions],
      ] as const).map(
        ([title, items]) =>
          items.length > 0 && (
            <section
              key={title}
              className="rounded-xl border border-line/60 bg-surface/50 p-4"
            >
              <h4 className="mb-2 text-xs font-semibold  text-muted flex items-center gap-1.5">
                <span
                  className={cn(
                    "h-2 w-2 rounded-full",
                    title.includes("Decisions")
                      ? "bg-amber-400"
                      : title.includes("Limitations")
                        ? "bg-rose-400"
                        : "bg-accent",
                  )}
                />
                {title}
              </h4>
              <ul className="list-disc space-y-2 pl-4 text-ink">
                {items.map((item, index) => (
                  <li
                    className="whitespace-pre-wrap break-words text-sm leading-relaxed text-ink/90"
                    key={index}
                  >
                    {item}
                  </li>
                ))}
              </ul>
            </section>
          ),
      )}
    </div>
  );
}

export function AgentContributionCard({
  job,
  onInspect,
  minimized,
  onMinimize,
}: {
  job: DurableJob;
  onInspect?: (job: DurableJob) => void;
  minimized?: boolean;
  onMinimize?: (minimized: boolean) => void;
}) {
  const [localMinimized, setLocalMinimized] = useState(false);
  const isMinimized = typeof minimized === "boolean" ? minimized : localMinimized;
  const toggleMinimized = () => {
    if (onMinimize) {
      onMinimize(!isMinimized);
    } else {
      setLocalMinimized(!localMinimized);
    }
  };

  const [showGuide, setShowGuide] = useState(false);
  const [guideText, setGuideText] = useState("");
  const [sendingGuide, setSendingGuide] = useState(false);
  const [guideFeedback, setGuideFeedback] = useState("");

  const handleSendGuide = async () => {
    if (!guideText.trim() || sendingGuide) return;
    setSendingGuide(true);
    setGuideFeedback("");
    try {
      const res = await fetch(`/api/jobs/${job.id}/message`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ message: guideText.trim(), message_id: createClientId() }),
      });
      if (!res.ok) throw new Error("Could not deliver guidance");
      setGuideText("");
      setGuideFeedback("Guidance sent to specialist");
      setTimeout(() => setGuideFeedback(""), 3000);
      setShowGuide(false);
    } catch {
      setGuideFeedback("Could not send guidance");
    } finally {
      setSendingGuide(false);
    }
  };

  const contribution = job.result.contribution || job.progress?.contribution;
  const successful = job.state === "succeeded";

  if (isMinimized) {
    return (
      <article
        aria-label={`${workerName(job)} contribution`}
        className="flex items-center justify-between gap-3 rounded-xl border border-line/70 bg-surface hover:bg-surface hover:border-accent/40 px-3.5 py-2 transition-colors cursor-pointer group"
        onClick={() => toggleMinimized()}
      >
        <div className="flex items-center gap-2.5 min-w-0 flex-1">
          <div className="relative shrink-0">
            <span
              aria-hidden="true"
              className="flex h-7 w-7 items-center justify-center rounded-lg bg-surface border border-accent/20 text-xs font-bold text-accent"
            >
              {workerName(job).charAt(0).toUpperCase()}
            </span>
            <span
              className={cn(
                "absolute -bottom-0.5 -right-0.5 h-2 w-2 rounded-full ring-1 ring-surface",
                job.state === "running" && "bg-emerald-400",
                job.state === "waiting_input" && "bg-amber-400",
                job.state === "failed" && "bg-rose-500",
                job.state === "succeeded" && "bg-emerald-500",
                !["running", "waiting_input", "failed", "succeeded"].includes(job.state) && "bg-muted",
              )}
            />
          </div>
          <strong className="min-w-0 truncate text-xs font-semibold text-ink group-hover:text-accent transition-colors">
            {workerName(job)}
          </strong>
          <span className="truncate text-xs text-muted hidden sm:inline">
            · {contribution?.finding?.slice(0, 55) || workerActivity(job) || (successful ? "Assignment completed" : "Partial findings")}
          </span>
        </div>
        <div className="flex items-center gap-2 shrink-0">
          <span
            className={cn(
              "inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-[10px] font-medium border",
              !successful
                ? "bg-amber-500/10 text-amber-400 border-amber-500/20"
                : "bg-emerald-500/10 text-emerald-400 border-emerald-500/15"
            )}
          >
            {workerStatus(job)}
          </span>
          <button
            type="button"
            className="inline-flex h-6 w-6 items-center justify-center rounded-md text-muted hover:text-ink hover:bg-accent/10 transition-colors cursor-pointer"
            title={`Expand ${workerName(job)} in chat`}
            aria-label={`Expand ${workerName(job)}`}
            onClick={(e) => {
              e.stopPropagation();
              toggleMinimized();
            }}
          >
            <Icon.ChevronRight size={13} />
          </button>
        </div>
      </article>
    );
  }

  return (
    <article
      aria-label={`${workerName(job)} contribution`}
      className="space-y-3 rounded-lg border border-line/80 bg-surface p-4 hover:border-accent/40 transition-colors duration-200"
    >
      <div className="flex items-center justify-between gap-2">
        <div className="flex items-center gap-2.5 min-w-0 flex-1">
          <div className="relative shrink-0">
            <span
              aria-hidden="true"
              className="flex h-9 w-9 items-center justify-center rounded-xl bg-surface border border-accent/25 text-xs font-bold text-accent"
            >
              {workerName(job).charAt(0).toUpperCase()}
            </span>
            <span
              className={cn(
                "absolute -bottom-0.5 -right-0.5 h-2.5 w-2.5 rounded-full ring-2 ring-surface",
                job.state === "running" && "bg-emerald-400",
                job.state === "waiting_input" && "bg-amber-400",
                job.state === "failed" && "bg-rose-500",
                job.state === "succeeded" && "bg-emerald-500",
                !["running", "waiting_input", "failed", "succeeded"].includes(job.state) && "bg-muted",
              )}
            />
          </div>
          <div className="min-w-0 flex-1">
            <strong className="block truncate text-sm font-semibold text-ink leading-tight">
              {workerName(job)}
            </strong>
            <span className="block truncate text-[11px] text-muted leading-tight mt-0.5">
              {workerActivity(job) || (successful ? "Finished" : "Active")}
            </span>
          </div>
        </div>
        <div className="flex items-center gap-2 shrink-0">
          <span
            className={cn(
              "inline-flex items-center gap-1.5 rounded-full px-2.5 py-0.5 text-[11px] font-medium border",
              !successful
                ? "bg-amber-500/10 text-amber-400 border-amber-500/30"
                : "bg-emerald-500/10 text-emerald-400 border-emerald-500/20"
            )}
          >
            <span
              className={cn(
                "h-1.5 w-1.5 rounded-full",
                job.state === "running" && "bg-emerald-400",
                job.state === "waiting_input" && "bg-amber-400",
                job.state === "failed" && "bg-rose-500",
                job.state === "succeeded" && "bg-emerald-400",
                !["running", "waiting_input", "failed", "succeeded"].includes(job.state) && "bg-muted"
              )}
              aria-hidden="true"
            />
            <span>{workerStatus(job)}</span>
          </span>
          <button
            type="button"
            className="inline-flex h-7 items-center gap-1 rounded-lg px-2 text-xs text-muted hover:text-ink hover:bg-line/40 transition-colors border border-line/60 cursor-pointer"
            title="Minimize subagent card in chat"
            aria-label={`Minimize ${workerName(job)}`}
            onClick={() => toggleMinimized()}
          >
            <Icon.Minimize size={12} />
            <span className="hidden sm:inline">Minimize</span>
          </button>
        </div>
      </div>

      <div className="rounded-xl border border-line/60 bg-canvas/50 p-3.5">
        <p className="line-clamp-4 whitespace-pre-wrap break-words text-xs sm:text-sm leading-relaxed text-ink/90 font-sans">
          {contribution?.finding ||
            job.result.message ||
            (successful
              ? "Assignment completed. No summary was provided."
              : "No completed result is available.")}
        </p>
      </div>

      {!successful && contribution && (
        <p className="inline-flex items-center gap-1.5 text-xs text-amber-400/90 font-medium bg-amber-500/10 border border-amber-500/20 px-2.5 py-1 rounded-lg">
          <Icon.Alert size={12} className="shrink-0" />
          <span>Partial findings · the assignment did not complete.</span>
        </p>
      )}

      {contribution && (
        <div className="flex flex-wrap gap-1.5 pt-0.5">
          {contribution.evidence.length + (contribution.sources?.length || 0) > 0 && (
            <span className="inline-flex items-center gap-1 rounded-md bg-line/30 px-2 py-0.5 text-[11px] text-muted font-medium border border-line/40">
              <Icon.File size={11} className="text-muted shrink-0" />
              <span>{contribution.evidence.length + (contribution.sources?.length || 0)} references</span>
            </span>
          )}
          {contribution.limitations.length > 0 && (
            <span className="inline-flex items-center gap-1 rounded-md bg-rose-500/10 px-2 py-0.5 text-[11px] text-rose-300 font-medium border border-rose-500/20">
              <Icon.Alert size={11} className="text-rose-400 shrink-0" />
              <span>
                {contribution.limitations.length}{" "}
                {contribution.limitations.length === 1 ? "limitation" : "limitations"}
              </span>
            </span>
          )}
          {contribution.decisions.length > 0 && (
            <span className="inline-flex items-center gap-1 rounded-md bg-amber-500/10 px-2 py-0.5 text-[11px] text-amber-300 font-medium border border-amber-500/20">
              <Icon.Activity size={11} className="text-amber-400 shrink-0" />
              <span>
                {contribution.decisions.length}{" "}
                {contribution.decisions.length === 1 ? "decision" : "decisions"} to consider
              </span>
            </span>
          )}
        </div>
      )}

      {job.artifacts.length > 0 && (
        <div className="flex flex-wrap gap-2 pt-1">
          {job.artifacts.slice(0, 3).map((path, index) => {
            const filename = path.split("/").pop() || "file";
            const ext = (filename.includes(".") ? filename.split(".").pop() : "file")?.toUpperCase();
            return (
              <a
                key={`${index}-${path}`}
                href={workerArtifactUrl(job, path, index)}
                target="_blank"
                rel="noreferrer"
                className="inline-flex items-center gap-2 rounded-xl border border-line/60 bg-canvas/60 hover:bg-surface hover:border-accent/40 px-2.5 py-1.5 text-xs text-ink transition-colors group/file"
              >
                <span className="flex h-5 w-5 shrink-0 items-center justify-center rounded-md bg-accent/15 text-accent font-mono text-[9px] font-bold" aria-hidden="true">
                  {ext}
                </span>
                <span className="font-mono text-xs text-accent group-hover/file:underline truncate max-w-[190px]">
                  {filename}
                </span>
                <Icon.Export size={11} className="text-muted group-hover/file:text-ink shrink-0 ml-auto opacity-60 transition-opacity" aria-hidden="true" />
              </a>
            );
          })}
        </div>
      )}

      {showGuide && (
        <form
          className="mt-2 flex gap-2 pt-2 border-t border-line/40 animate-[slideDown_150ms_ease-out]"
          onSubmit={(e) => {
            e.preventDefault();
            void handleSendGuide();
          }}
        >
          <input
            value={guideText}
            onChange={(e) => setGuideText(e.target.value)}
            placeholder={`Guide ${workerName(job)} (e.g. prioritize tests, check edge cases)…`}
            className="min-w-0 flex-1 rounded-full border border-line/80 bg-canvas/80 px-3.5 py-1.5 text-xs text-ink placeholder:text-muted/60 focus:border-accent focus:outline-hidden focus:ring-2 focus:ring-accent/20 transition-colors"
            disabled={sendingGuide}
          />
          <button
            type="submit"
            disabled={sendingGuide || !guideText.trim()}
            className="rounded-full bg-accent px-3.5 py-1.5 text-xs font-semibold text-white hover:bg-accent/90 active:scale-95 disabled:opacity-40 transition-colors cursor-pointer"
          >
            {sendingGuide ? "Sending…" : "Send"}
          </button>
        </form>
      )}
      {guideFeedback && (
        <p role="status" className="text-xs text-emerald-400 font-medium">
          {guideFeedback}
        </p>
      )}

      <div className="flex items-center justify-between gap-2 pt-2 border-t border-line/40">
        <div className="flex items-center gap-2.5">
          <button
            disabled={!onInspect}
            onClick={() => onInspect?.(job)}
            className="inline-flex items-center gap-1.5 min-h-8 rounded-full bg-accent/15 hover:bg-accent/25 border border-accent/25 px-3 py-1 text-xs font-semibold text-accent hover:text-accent-hover hover:underline disabled:opacity-50 text-left transition-colors cursor-pointer"
          >
            <span>View {workerName(job)}’s contribution</span>
            {job.artifacts.length > 2 && <span className="text-muted text-[11px]">· {job.artifacts.length} files</span>}
            <Icon.Forward size={12} className="shrink-0" aria-hidden="true" />
          </button>
          <button
            type="button"
            onClick={() => setShowGuide(prev => !prev)}
            className="inline-flex items-center gap-1 text-xs text-muted hover:text-ink px-3 py-1 rounded-full bg-surface/80 hover:bg-surface border border-line/60 transition-colors cursor-pointer"
            title={`Send steering guidance to ${workerName(job)}`}
          >
            <Icon.Compose size={11} />
            <span>Guide</span>
          </button>
        </div>
        <button
          type="button"
          onClick={() => toggleMinimized()}
          className="inline-flex items-center gap-1 text-xs text-muted hover:text-ink transition-colors px-2.5 py-1 rounded-full hover:bg-line/30 shrink-0 cursor-pointer"
          title="Minimize subagent card in chat"
        >
          <Icon.Minimize size={11} />
          <span>Minimize</span>
        </button>
      </div>
    </article>
  );
}
