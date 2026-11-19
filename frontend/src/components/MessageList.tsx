import { memo, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import * as DropdownMenu from "@radix-ui/react-dropdown-menu";
import type { BrowserActivityTarget, ChatMessage } from "../api/types";
import { OutcomeChecks, type RecoverOutcome } from "./OutcomeChecks";
import { friendlyActivityLabel } from "../lib/activityUi";
import { requestBrowserTarget, useBrowserControls } from "../lib/browserWorkspace";
import { deliveredPaths, observedSources } from "../lib/deliverables";
import { artifactDownloadUrl, fileBasename } from "../lib/artifactMedia";
import { isEarlyStopLabel } from "../store/helpers";
import { isDeliveryUnconfirmed, terminalRunState } from "../lib/runStatus";
import { cn } from "../lib/cn";
import { Icon } from "../lib/icons";
import {
  collectWrittenArtifactRefs,
  messageBubbleArtifactPaths,
} from "../lib/messageArtifacts";
import { initJsonCardHandlers } from "../lib/jsonRenderer";
import {
  initCodeBlockHandlers,
  initMermaidBlocks,
} from "../lib/codeEnhancer";
import { renderMarkdownCached } from "../lib/markdownHtml";
import { chatArtifactFromClick, shouldLightboxArtifact } from "../lib/chatArtifactClick";
import { useShallow } from "zustand/react/shallow";
import { useAppStore } from "../store";
import { resolveAskHumanMessageId } from "../lib/askHumanApproval";
import { messageNarrativeText } from "../lib/transcript";
import { orderSteeringMessages } from "../lib/steering";
import { ComputerFrames, MessageArtifacts } from "./MessageRenderers";
import { TerminalActivity } from "./TerminalActivity";
import { Transcript } from "./Transcript/Transcript";
import { StreamingMarkdown } from "./Transcript/StreamingMarkdown";
import { SourceCards } from "./SourceCards";
import { ProjectSelectorPopover, getProjectColor } from "./shared/ProjectSelectorPopover";

function ChatBrowserHandoff({ sessionId, onOpen }: { sessionId: string | null; onOpen: () => void }) {
  const browser = useBrowserControls(sessionId);
  const collapsed = useAppStore(s => s.prefs.rightPanelCollapsed);
  const surface = useAppStore(s => s.workspaceSurface);
  const liveSurface = useAppStore(s => s.liveSurface);
  const browserVisible = !collapsed && surface === "live" && liveSurface === "browser";
  // Controls are unpublished when the workspace unmounts. The selected live
  // surface still lets the user reopen it after collapsing the panel.
  if (!sessionId || browserVisible || (!browser?.connected && liveSurface !== "browser")) return null;
  return <button type="button" onClick={onOpen}
    className="my-1 inline-flex items-center gap-1.5 rounded px-1 py-1 text-xs text-accent hover:bg-accent-soft">
    <Icon.Globe size={14} aria-hidden="true" /> {browser?.connected ? "Open live browser" : "Open browser"}
  </button>;
}

const STREAM_THROTTLE_MS = 50;

function useThrottledStreamText(text: string, streaming: boolean): string {
  const [display, setDisplay] = useState(text);
  const latest = useRef(text);
  const lastFlush = useRef(0);
  const timerRef = useRef(0);

  latest.current = text;

  useEffect(() => {
    if (!streaming) {
      if (timerRef.current) {
        clearTimeout(timerRef.current);
        timerRef.current = 0;
      }
      setDisplay(text);
      return;
    }

    const now = performance.now();
    if (now - lastFlush.current >= STREAM_THROTTLE_MS) {
      lastFlush.current = now;
      setDisplay(text);
      return;
    }

    if (timerRef.current) return;

    const wait = Math.max(0, STREAM_THROTTLE_MS - (now - lastFlush.current));
    timerRef.current = window.setTimeout(() => {
      timerRef.current = 0;
      lastFlush.current = performance.now();
      setDisplay(latest.current);
    }, wait);
  }, [text, streaming]);

  useEffect(() => {
    return () => {
      if (timerRef.current) clearTimeout(timerRef.current);
    };
  }, []);

  return display;
}

async function copyText(text: string) {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    const ta = document.createElement("textarea");
    ta.value = text;
    document.body.appendChild(ta);
    ta.select();
    document.execCommand("copy");
    ta.remove();
  }
}

/** Display old generated verification footers without rewriting saved history. */
function readableVerificationFooter(text: string): string {
  return text.replace(/(^|\n\n---\n)_Required checks failed — (\d+) required criterion\(s\) unmet: ([\s\S]+)_\s*$/, (_match, prefix, count, detail) => {
    let topic = String(detail).split(": ", 1)[0].replace(/\s+/g, " ").trim().replace(/\.$/, "");
    if (topic.length > 160) topic = topic.slice(0, 157).replace(/\s+\S*$/, "") + "…";
    const scope = Number(count) === 1 ? "this part of the request" : `${count} parts of the request, including`;
    return `${prefix}_I couldn't verify ${scope}: ${topic}._`;
  });
}

const MessageRow = memo(function MessageRow({
  message: rawMessage,
  index,
  showRetry,
  onRetry,
  showContinue,
  onContinue,
  onRecover,
  showComputerLive,
  steeringActive,
  resuming,
}: {
  message: ChatMessage;
  index: number;
  showRetry?: boolean;
  onRetry?: () => void;
  /** Early-stopped last turn: offer a one-click resume. */
  showContinue?: boolean;
  onContinue?: () => void;
  onRecover?: RecoverOutcome;
  showComputerLive?: boolean;
  steeringActive?: boolean;
  /** A "Resume task" request for this chat is being submitted. */
  resuming?: boolean;
}) {
  const m = useMemo(() => {
    if (rawMessage.role !== "assistant" || rawMessage.streaming) return rawMessage;
    const cause = rawMessage.failureCause?.startsWith("Required checks failed — ")
      ? readableVerificationFooter(`_${rawMessage.failureCause}_`).slice(1, -1)
      : rawMessage.failureCause;
    const displayBody = (body: string) => {
      const text = readableVerificationFooter(body);
      const footer = `\n\n---\n_${cause}_`;
      return cause && ["unverified", "invalid_output"].includes(rawMessage.resultStatus || "") && text.endsWith(footer)
        ? text.slice(0, -footer.length) : text;
    };
    return {
      ...rawMessage,
      text: displayBody(rawMessage.text || ""),
      failureCause: cause,
      blocks: rawMessage.blocks?.map((block) => block.type === "narrative"
        ? { ...block, text: displayBody(block.text) } : block),
    };
  }, [rawMessage]);
  const streaming = Boolean(m.streaming);
  const copyableText = messageNarrativeText(m);
  const failureOnly = m.role === "assistant" && isLocalStorageFailure(m.failureCause || "") &&
    ((m.text || "").trim() === (m.failureCause || "").trim() || (m.text || "").startsWith("I couldn't complete that request"));
  const throttledText = useThrottledStreamText(failureOnly ? "" : m.text || "", streaming);
  const [copied, setCopied] = useState(false);
  const sessionId = useAppStore((s) => s.sessionId);
  const canvasItemPaths = useAppStore((s) => (m.role === "assistant" ? s.canvasItems : undefined));
  const upsertCanvasPaths = useAppStore((s) => s.upsertCanvasPaths);
  const openCanvasItem = useAppStore((s) => s.openCanvasItem);
  const setPrefs = useAppStore((s) => s.setPrefs);
  const setComputerPanelMode = useAppStore((s) => s.setComputerPanelMode);
  const setBrowserPanelMode = useAppStore((s) => s.setBrowserPanelMode);
  const pinLiveSurface = useAppStore((s) => s.pinLiveSurface);
  const deleteTurn = useAppStore((s) => s.deleteTurn);
  const tagTurnProject = useAppStore((s) => s.tagTurnProject);
  const [projectSelectorOpen, setProjectSelectorOpen] = useState(false);
  const currentSessionProject = useAppStore(
    useShallow((s) => s.sessions?.find((sess) => sess.session_id === s.sessionId)?.project),
  );
  const isTurnTagged = useAppStore((s) => {
    const cur = s.sessions?.find((sess) => sess.session_id === s.sessionId);
    return Array.isArray(cur?.tagged_turns) && cur.tagged_turns.includes(index);
  });
  const currentProjectId = useAppStore((s) => {
    const cur = s.sessions?.find((sess) => sess.session_id === s.sessionId);
    return cur?.project_id || cur?.project?.id;
  });
  const artifactPaths = useMemo(() => {
    const paths = canvasItemPaths ? canvasItemPaths.map((item) => item.path) : [];
    return messageBubbleArtifactPaths(m, paths);
  }, [m, canvasItemPaths]);
  const delivery = deliveredPaths(m, (canvasItemPaths || []).filter(item => item.available !== false).map(item => item.path));
  const sources = observedSources(m);

  useEffect(() => {
    if (!m.streaming || m.role !== "assistant") return;
    const refs = collectWrittenArtifactRefs(m);
    if (refs.length) upsertCanvasPaths(refs);
  }, [m, m.streaming, upsertCanvasPaths]);

  const bodyHtml = useMemo(() => {
    if (!throttledText || streaming) return "";
    return renderMarkdownCached(m.id, throttledText, sessionId);
  }, [m.id, throttledText, sessionId, streaming]);

  const markdownRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const el = markdownRef.current;
    if (!el || !bodyHtml) return;
    initJsonCardHandlers(el);
    initCodeBlockHandlers(el);
    initMermaidBlocks(el);
  }, [bodyHtml]);

  const stepList = m.steps || [];
  const isUser = m.role === "user";
  const author = m.speaker?.name || "Kageha";
  const avatar = m.speaker?.avatar;
  const authorInitial = author.trim().charAt(0).toUpperCase() || "K";
  const showActivity =
    !isUser && (streaming || stepList.length > 0);
  const openBrowserActivity = (_url?: string, target?: BrowserActivityTarget) => {
    if (sessionId) requestBrowserTarget(sessionId, target);
    setPrefs({ rightPanelCollapsed: false });
    pinLiveSurface("browser");
    setBrowserPanelMode("expanded");
  };
  const openComputerActivity = () => {
    setPrefs({ rightPanelCollapsed: false });
    pinLiveSurface("computer");
    setComputerPanelMode("expanded");
  };

  return (
    <article
      className={cn(
        "ka-message group relative mx-auto w-full max-w-3xl px-3 py-2 sm:px-4 sm:py-2.5 md:px-6",
        isUser ? "ka-message--user" : "ka-message--assistant",
      )}
    >
      <div className="ka-message-header mb-1.5 flex items-center gap-2 text-xs">
        {/* Avatar dot */}
        <span
          className={cn(
            "flex h-5 w-5 shrink-0 items-center justify-center rounded-full text-[0.55rem] font-bold",
            isUser
              ? "bg-line/80 text-muted"
              : "bg-accent text-white",
          )}
        >
          {isUser ? (
            <svg width="11" height="11" viewBox="0 0 16 16" fill="currentColor" aria-hidden="true">
              <path d="M8 8a3 3 0 1 0 0-6 3 3 0 0 0 0 6Zm2-3a2 2 0 1 1-4 0 2 2 0 0 1 4 0Zm4 8c0-2.21-2.686-4-6-4s-6 1.79-6 4a1 1 0 0 0 1 1h10a1 1 0 0 0 1-1Z" />
            </svg>
          ) : avatar && !/^(https?:|data:|\/)/.test(avatar) ? (
            avatar
          ) : (
            authorInitial
          )}
        </span>
        <span
          className={cn(
            "font-semibold tracking-wide",
            isUser ? "text-ink/90" : "text-accent",
          )}
        >
          {isUser ? "You" : author}
        </span>
        {isUser && m.steering ? (
          <span role="status" className="text-faint">
            {m.steering === "applied" ? "Update received" : steeringActive ? "Update waiting to be applied" : "Update was not applied"}
          </span>
        ) : null}
        {!streaming &&
        m.statusLabel &&
        !m.failureCause &&
        (!(m.blocks && m.blocks.length) ||
          /^(cancelled|error|interrupted|stopped)/i.test(m.statusLabel.trim())) ? (
          <span
            className={cn(
              isEarlyStopLabel(m.statusLabel)
                ? "rounded-full bg-warn-soft px-2 py-0.5 text-[0.65rem] font-medium text-warn"
                : "text-faint",
            )}
            role={isEarlyStopLabel(m.statusLabel) ? "status" : undefined}
          >
            {friendlyActivityLabel(m.statusLabel) || m.statusLabel}
          </span>
        ) : null}
        {/* Cost/steps badge — shown on completed assistant messages */}
        {!isUser && !streaming && (m.turnSteps || m.turnCostUsd) ? (
          <span className="inline-flex items-center gap-1.5 rounded-full bg-line/60 px-2 py-0.5 text-[0.6rem] font-medium tabular-nums text-faint">
            {m.turnSteps ? <span>{m.turnSteps} steps</span> : null}
            {m.turnSteps && m.turnCostUsd ? <span className="text-line-strong">·</span> : null}
            {m.turnCostUsd ? <span>${m.turnCostUsd.toFixed(4)}</span> : null}
          </span>
        ) : null}
        {isTurnTagged && currentSessionProject ? (
          <span
            className="inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-[0.6rem] font-medium"
            style={{
              backgroundColor: `${getProjectColor(currentSessionProject.color)}18`,
              color: getProjectColor(currentSessionProject.color),
            }}
            title={`Tagged to ${currentSessionProject.name}`}
          >
            <Icon.Projects size={10} />
            <span>{currentSessionProject.name}</span>
          </span>
        ) : null}
        {!streaming && (copyableText || showRetry) ? (
          <div className="ka-message-actions ml-auto flex gap-1 opacity-0 transition-opacity group-hover:opacity-100 focus-within:opacity-100">
            {copyableText ? (
              <button
                type="button"
                className="rounded px-1.5 py-0.5 text-faint hover:bg-line/70 hover:text-ink"
                onClick={() => {
                  void copyText(copyableText).then(() => {
                    setCopied(true);
                    window.setTimeout(() => setCopied(false), 1400);
                  });
                }}
              >
                {copied ? "Copied" : "Copy"}
              </button>
            ) : null}
            {showRetry ? (
              <button
                type="button"
                className="rounded px-1.5 py-0.5 text-faint hover:bg-line/70 hover:text-ink"
                onClick={onRetry}
              >
                Retry
              </button>
            ) : null}
            <DropdownMenu.Root>
              <DropdownMenu.Trigger asChild>
                <button type="button" aria-label="More message actions" className="ka-message-more">
                  <Icon.More size={18} />
                </button>
              </DropdownMenu.Trigger>
              <DropdownMenu.Portal>
                <DropdownMenu.Content sideOffset={6} align="end" className="ka-message-action-menu">
                  <DropdownMenu.Item asChild>
                    <button
                      type="button"
                      className="w-full text-left rounded px-2 py-1 text-xs text-ink hover:bg-line/50 flex items-center gap-1.5"
                      onClick={() => setProjectSelectorOpen(true)}
                    >
                      <Icon.Projects size={13} className="text-faint" />
                      <span>Tag to Project...</span>
                    </button>
                  </DropdownMenu.Item>
                  <DropdownMenu.Item asChild>
                    <button
                      type="button"
                      className="w-full text-left rounded px-2 py-1 text-xs text-faint hover:bg-danger-soft hover:text-danger"
                      title="Delete this turn and everything after it"
                      onClick={() => {
                        if (window.confirm("Delete this message and all messages after it?")) {
                          void deleteTurn(index);
                        }
                      }}
                    >
                      Delete
                    </button>
                  </DropdownMenu.Item>
                </DropdownMenu.Content>
              </DropdownMenu.Portal>
            </DropdownMenu.Root>
            {sessionId && (
              <ProjectSelectorPopover
                open={projectSelectorOpen}
                onOpenChange={setProjectSelectorOpen}
                currentProjectId={currentProjectId}
                onSelectProject={(proj) => {
                  if (proj) {
                    void tagTurnProject(sessionId, index, proj.id);
                  }
                }}
                trigger={<span className="hidden" />}
              />
            )}
          </div>
        ) : null}
      </div>

      {(() => {
        const blocks = m.blocks || [];
        const hasTranscript = !isUser && blocks.length > 0;
        const hasNarrative = blocks.some((b) => b.type === "narrative");
        const showTranscript = !isUser && (hasTranscript || streaming);
        const awaitingReply = m.role === "assistant" && streaming && (m.turnId || m.streamCursor?.turnId) && !m.text && !m.approval &&
          blocks.every((block) => block.type === "status" || block.type === "reasoning");
        return (
          <>
            {awaitingReply && (
              <p role="status" className="my-2 text-sm text-muted">
                Message received.
              </p>
            )}
            {showTranscript ? (
              <Transcript
                blocks={blocks}
                streaming={streaming}
                liveLabel={streaming ? m.statusLabel : undefined}
                liveDetail={streaming ? m.statusDetail : undefined}
                messageId={m.id}
                onOpenBrowser={openBrowserActivity}
                onOpenComputer={openComputerActivity}
              />
            ) : showActivity ? (
              <TerminalActivity
                steps={stepList}
                liveLabel={m.statusLabel || m.statusDetail}
                streaming={false}
                onOpenBrowser={openBrowserActivity}
                onOpenComputer={openComputerActivity}
              />
            ) : null}

            {!isUser && streaming && <ChatBrowserHandoff sessionId={sessionId} onOpen={() => openBrowserActivity()} />}
            {isUser && artifactPaths.length > 0 ? (
              <MessageArtifacts paths={artifactPaths} />
            ) : null}
            {showComputerLive && m.computerFrames && m.computerFrames.length > 0 ? (
              <ComputerFrames frames={m.computerFrames} />
            ) : null}

            {!isUser && !streaming && (m.resultStatus === "unverified" || m.resultStatus === "invalid_output") && (
              <div role="status" data-testid="unverified-status" className="mb-3 rounded-md border border-warn/30 bg-warn-soft px-3 py-2 text-sm text-warn">
                Task incomplete: some outcomes remain unverified. The saved response below does not establish completion.
              </div>
            )}

            {isUser || !hasTranscript || !hasNarrative ? (
              streaming ? (
                throttledText ? <StreamingMarkdown
                  id={m.id}
                  text={throttledText}
                  streaming
                  sessionId={sessionId}
                  isUser={isUser}
                  onArtifactClick={(path, expand) =>
                    openCanvasItem(path, { expand })
                  }
                /> : null
              ) : bodyHtml ? (
                <div
                  ref={markdownRef}
                  className={cn(
                    "markdown",
                    isUser &&
                      "rounded-xl bg-surface px-3.5 py-2.5 shadow-[0_0_0_1px_var(--color-line)]",
                  )}
                  dangerouslySetInnerHTML={{ __html: bodyHtml }}
                  onClick={(e) => {
                    const target = e.target as HTMLElement | null;
                    if (!target) return;

                    const pre = target.closest("pre") as HTMLPreElement | null;
                    if (pre && target.tagName !== "A") {
                      const rect = pre.getBoundingClientRect();
                      const clickX = e.clientX - rect.left;
                      const clickY = e.clientY - rect.top;
                      if (clickX > rect.width - 60 && clickY < 32) {
                        e.preventDefault();
                        const code = pre.querySelector("code")?.textContent || pre.textContent || "";
                        void copyText(code);
                        pre.style.outline = "2px solid var(--color-accent)";
                        setTimeout(() => { pre.style.outline = ""; }, 600);
                        return;
                      }
                    }

                    const hit = chatArtifactFromClick(target);
                    if (hit) {
                      e.preventDefault();
                      openCanvasItem(hit.path, { expand: hit.expand });
                      return;
                    }

                    if (target.tagName !== "IMG" && target.tagName !== "A") {
                      const clickedText = (target.textContent || "").trim();
                      const filenameMatch = clickedText.match(
                        /^([A-Za-z0-9][A-Za-z0-9._-]*\.(?:html?|pdf|pptx?|docx?|xlsx?|png|jpe?g|gif|webp|svg|mp4|webm|mov|wav|mp3|csv|zip|md|txt))$/i,
                      );
                      if (filenameMatch) {
                        const name = filenameMatch[1];
                        const canvasItems = useAppStore.getState().canvasItems;
                        const match = canvasItems.find(
                          (item) => item.path.endsWith(`/${name}`) || item.path === `artifacts/${name}`,
                        );
                        if (match) {
                          e.preventDefault();
                          openCanvasItem(match.path, {
                            expand: shouldLightboxArtifact(match.path),
                          });
                          return;
                        }
                        e.preventDefault();
                        openCanvasItem(`artifacts/${name}`, {
                          expand: shouldLightboxArtifact(`artifacts/${name}`),
                        });
                      }
                    }
                  }}
                />
              ) : null
            ) : null}
            {delivery.length > 0 && <div className="my-3 flex flex-wrap gap-2" aria-label="Deliverables">
              {delivery.map(path => {
                const item = canvasItemPaths?.find(item => item.path === path);
                const ready = Boolean(item && item.available !== false);
                const filename = item?.caption || fileBasename(path);
                const download = ready ? artifactDownloadUrl(sessionId, path) : null;
                const preview = ready && item?.kind === "image" ? item.url : null;
                return <div key={path} className={preview ? "w-full" : undefined}>
                  {preview && (
                    <button type="button" onClick={() => openCanvasItem(path)} className="mb-2 block max-w-full">
                      <img src={preview} alt={filename} loading="lazy" className="max-h-96 max-w-full rounded-lg object-contain" />
                    </button>
                  )}
                  <div className="inline-flex items-center gap-2 rounded-lg border border-line px-2.5 py-1.5 text-xs">
                  <button type="button" disabled={!ready} onClick={() => openCanvasItem(path)} className="inline-flex items-center gap-1.5">
                    <Icon.File size={14} /> {filename}
                  </button>
                  {download ? <a href={download} download={filename} className="text-accent">Download</a>
                    : <span className="text-muted">File unavailable</span>}
                  </div>
                </div>;
              })}
            </div>}
            {sources.length > 0 && <SourceCards sources={sources} />}
            {!isUser && !streaming && showContinue && m.resume ? (
              <div className="mt-2 flex items-center gap-2 rounded-md border border-line px-3 py-2 text-xs text-muted" role="status">
                <span className="flex-1">{m.resume.reason || "A checkpoint is ready. Completed steps are saved."}</span>
                {m.resume.allowed && <button type="button" className="ka-icon-btn px-2" onClick={onContinue} disabled={resuming} aria-busy={resuming || undefined}>{resuming ? "Resuming…" : "Resume task"}</button>}
              </div>
            ) : !isUser && !streaming && (m.failureCause || m.verificationOutcomes?.length) && !isEarlyStopLabel(m.statusLabel) ? (
              <FailureBanner cause={m.failureCause || ""} detail={m.failureDetail} outcomeOnly={!m.failureCause} answer={m.text} outcomes={m.verificationOutcomes} resultStatus={m.resultStatus} statusLabel={m.statusLabel} onResume={showContinue ? onContinue : undefined} onRecover={showContinue ? onRecover : undefined} resuming={resuming} />
            ) : null}
            {!isUser && !streaming && showContinue && !m.resume && isEarlyStopLabel(m.statusLabel) ? (
              <div
                className="mt-2 flex flex-wrap items-center gap-2 rounded-md border border-warn/30 bg-warn-soft px-3 py-2 text-xs text-warn"
                role="status"
              >
                <span className="min-w-0 flex-1">
                  The agent stopped before finishing ({m.statusLabel?.replace(/^Stopped · /, "")}).
                  Your work so far is kept.
                </span>
                <button
                  type="button"
                  className="shrink-0 rounded-md bg-accent px-2.5 py-1 font-medium text-white hover:opacity-90 focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent"
                  onClick={onContinue}
                  disabled={resuming}
                  aria-busy={resuming || undefined}
                >
                  {resuming ? "Resuming…" : "Resume task"}
                </button>
              </div>
            ) : null}
          </>
        );
      })()}
    </article>
  );
});

/**
 * Distinct, accessible banner shown when a run ends in a terminal failure
 * (model/provider unavailable, watchdog kill, budget/step limit). It makes a
 * dead run unmistakable versus a normal reply and, for model-availability
 * failures, offers a one-click path to the Models settings.
 */
function isLocalStorageFailure(text: string) {
  return /no such column:|table resume_grants has \d+ columns|database (?:is locked|disk image is malformed)|sqlite/i.test(text);
}

function FailureBanner({ cause, detail, answer, outcomes, outcomeOnly, resultStatus, statusLabel, onResume, onRecover, resuming }: { cause: string; detail?: string; answer?: string; outcomes?: ChatMessage["verificationOutcomes"]; outcomeOnly?: boolean; resultStatus?: string; statusLabel?: string; onResume?: () => void; onRecover?: RecoverOutcome; resuming?: boolean }) {
  const deliveryUnconfirmed = isDeliveryUnconfirmed(resultStatus, statusLabel);
  const unresolved = outcomeOnly || resultStatus === "invalid_output" || resultStatus === "unverified";
  const paused = deliveryUnconfirmed || terminalRunState(resultStatus || "error").status === "interrupted";
  const openSettings = useAppStore((s) => s.openSettings);
  const repeatsAnswer = unresolved && (cause || "").trim() === (answer || "").trim();
  const text = outcomeOnly ? (outcomes?.some(o => o.status !== "pass")
    ? "This check finished. Other saved outcomes still need attention." : "All listed outcomes are confirmed.") : repeatsAnswer
    ? "The response is saved, but some outcomes remain unverified. Review the reported results before continuing."
    : (cause || "").trim() || "The run failed.";
  const isBudgetCause = /spending allocation is exhausted|root task allocation exhausted/i.test(text);
  const isLocalCause = isLocalStorageFailure(text);
  // A model/provider-availability cause points the user at Settings → Models.
  const isModelCause = !isBudgetCause && !isLocalCause &&
    /settings\s*→?\s*models|connect at least one provider|pick another model|switch to another provider|provider|api key|quota|rate-limit/i.test(
      text,
    );

  const neededMatch = isBudgetCause ? text.match(/estimated\s+(\$[0-9.]+)/i) : null;
  const limitMatch = isBudgetCause ? text.match(/task limit\s+(\$[0-9.]+)/i) : null;
  const spentMatch = isBudgetCause ? text.match(/(\$[0-9.]+)\s+spent/i) : null;
  const reservedMatch = isBudgetCause ? text.match(/(\$[0-9.]+)\s+reserved/i) : null;

  return (
    <div
      role={outcomeOnly || paused ? "status" : "alert"}
      className={cn(
        "mt-3 flex items-start gap-3 rounded-xl border p-4 text-sm shadow-xs backdrop-blur-xs transition-all",
        outcomeOnly ? "border-line bg-surface text-ink" : isBudgetCause
          ? "border-amber-500/30 bg-amber-500/[0.06] text-ink"
          : paused
            ? "border-warn/25 bg-warn-soft text-warn"
            : "border-danger/30 bg-danger-soft text-danger",
      )}
    >
      {outcomeOnly ? <Icon.Check size={18} className="mt-0.5 shrink-0" />
        : <Icon.Alert size={18} className={cn("mt-0.5 shrink-0", isBudgetCause && "text-amber-500")} />}
      <div className="min-w-0 flex-1">
        <div className={cn("font-semibold text-sm", isBudgetCause && "text-amber-500")}>
          {outcomeOnly ? "Saved outcome checks" : deliveryUnconfirmed ? statusLabel || "Delivery unconfirmed" : unresolved ? "Some results need checking" : isBudgetCause ? "Spending allocation reached" : paused ? "Task paused" : "Run failed"}
        </div>
        <div className="mt-1 whitespace-pre-wrap break-words opacity-90 leading-relaxed text-xs sm:text-sm">
          {isLocalCause ? "Something interrupted this task. Your progress is saved. Resume to continue." : text}
        </div>
        {!outcomeOnly && detail?.trim() ? (
          <div className="mt-2 whitespace-pre-wrap break-words font-mono text-xs opacity-75" data-testid="failure-detail">
            <span className="font-sans font-semibold">Details: </span>{detail.trim()}
          </div>
        ) : null}
        {unresolved && outcomes?.length ? <OutcomeChecks outcomes={outcomes} onRecover={onRecover} /> : null}
        {isBudgetCause && (limitMatch || spentMatch || neededMatch || reservedMatch) && (
          <div className="mt-3 grid grid-cols-2 sm:grid-cols-4 gap-2 rounded-lg border border-amber-500/20 bg-canvas/70 p-2.5 text-xs">
            {limitMatch && (
              <div>
                <span className="block text-[0.62rem] font-semibold uppercase tracking-wider text-muted">Limit</span>
                <span className="font-mono text-sm font-semibold text-ink">{limitMatch[1]}</span>
              </div>
            )}
            {spentMatch && (
              <div>
                <span className="block text-[0.62rem] font-semibold uppercase tracking-wider text-muted">Spent</span>
                <span className="font-mono text-sm font-semibold text-ink">{spentMatch[1]}</span>
              </div>
            )}
            {reservedMatch && (
              <div>
                <span className="block text-[0.62rem] font-semibold uppercase tracking-wider text-muted">Reserved</span>
                <span className="font-mono text-sm font-semibold text-ink">{reservedMatch[1]}</span>
              </div>
            )}
            {neededMatch && (
              <div>
                <span className="block text-[0.62rem] font-semibold uppercase tracking-wider text-amber-500">Next Call</span>
                <span className="font-mono text-sm font-semibold text-amber-500">{neededMatch[1]}</span>
              </div>
            )}
          </div>
        )}
        {isLocalCause && (
          <details className="mt-2 text-xs opacity-75">
            <summary className="cursor-pointer">Technical details</summary>
            <div className="mt-1 whitespace-pre-wrap break-words">{text}</div>
          </details>
        )}
        {onResume && !(unresolved && outcomes?.length && onRecover) && (
          <button
            type="button"
            onClick={onResume}
            disabled={resuming && !deliveryUnconfirmed}
            aria-busy={(resuming && !deliveryUnconfirmed) || undefined}
            className="mt-3 inline-flex items-center gap-1.5 rounded-lg bg-accent px-3 py-1.5 text-xs font-medium text-white shadow-xs hover:opacity-90 active:scale-[0.98] transition-all focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent"
          >
            {!deliveryUnconfirmed && <Icon.Play size={12} />}
            <span>{deliveryUnconfirmed ? "Check delivery" : resuming ? "Resuming…" : unresolved ? "Continue from saved progress" : "Resume task"}</span>
          </button>
        )}
        {onResume && unresolved ? <p className="mt-2 text-xs opacity-75">{onRecover && outcomes?.length
          ? "Rechecking only reads existing results. Continuing works on the unfinished outcomes."
          : "Continues the saved task using its existing progress."}</p> : null}
        {isModelCause ? (
          <button
            type="button"
            className="mt-2 inline-flex items-center gap-1 rounded-md px-2 py-1 text-xs font-medium text-danger border border-danger/30 hover:bg-danger/10 transition-colors"
            onClick={() => openSettings("models")}
          >
            <Icon.Settings size={13} className="shrink-0" />
            <span>Open Settings → Models</span>
          </button>
        ) : null}
      </div>
    </div>
  );
}

function MessageSkeletons() {
  return (
    <div
      className="mx-auto w-full max-w-3xl space-y-3 px-3 py-3 sm:px-4 sm:py-4 md:px-6"
      aria-busy="true"
      aria-label="Loading messages"
    >
      {[0, 1, 2].map((i) => (
        <div key={i} className="animate-pulse space-y-2">
          <div className="h-3 w-16 rounded bg-line" />
          <div className="h-4 w-full rounded bg-line/80" />
          <div className="h-4 w-2/3 rounded bg-line/60" />
        </div>
      ))}
    </div>
  );
}

function EmptyState() {
  const setDraft = useAppStore((s) => s.setDraft);

  const suggestions = [
    { label: "Create something", detail: "Turn an idea into something worth sharing", icon: Icon.Logo, text: "Help me create a carousel for my product. " },
    { label: "Explore a topic", detail: "Find the signal, with sources to back it up", icon: Icon.Search, text: "Research and summarize this topic: " },
    { label: "Build an app", detail: "Go from a rough idea to a working prototype", icon: Icon.NewThread, text: "Help me build an app that " },
    { label: "Solve a problem", detail: "Work through the details together", icon: Icon.Compose, text: "Help me figure out " },
  ];

  return (
    <div className="ka-welcome">
      <div className="ka-welcome-mark">
        <Icon.Logo size={28} />
      </div>
      <p className="ka-welcome-eyebrow">A little clarity. A lot of possibility.</p>
      <h1>Make room for your next idea.</h1>
      <p className="ka-welcome-description">
        A thinking partner for the research, the building, and everything in between.
      </p>
      <div className="ka-starter-grid">
        {suggestions.map((s) => (
          <button
            key={s.label}
            type="button"
            className="ka-starter-card"
            onClick={() => {
              setDraft(s.text);
              // Focus the composer
              const el = document.getElementById("message-input") as HTMLTextAreaElement | null;
              el?.focus();
            }}
          >
            <s.icon size={20} />
            <span><strong>{s.label}</strong><small>{s.detail}</small></span>
            <Icon.ArrowUp size={16} className="ka-starter-arrow" />
          </button>
        ))}
      </div>
    </div>
  );
}

export function MessageList({ messages: sourceMessages }: { messages: ChatMessage[] }) {
  const messages = useMemo(() => orderSteeringMessages(sourceMessages), [sourceMessages]);
  const sessionLoading = useAppStore((s) => s.sessionLoading);
  const runStatus = useAppStore((s) => s.runStatus);
  const sending = useAppStore((s) => s.sending);
  const retryLastTurn = useAppStore((s) => s.retryLastTurn);
  const resumeLastTask = useAppStore((s) => s.resumeLastTask);
  const resumingSessionId = useAppStore((s) => s.resumingSessionId);
  const recoverLiveProgress = useAppStore((s) => s.recoverLiveProgress);
  const showToast = useAppStore((s) => s.showToast);
  const sessionId = useAppStore((s) => s.sessionId);
  const pendingApproval = useAppStore((s) => s.pendingApproval);
  const activeTurnId = messages.findLast((m) => m.role === "assistant" && m.streaming)?.turnId;

  const askHumanMessageId = useMemo(
    () => resolveAskHumanMessageId(messages, pendingApproval, sessionId),
    [messages, pendingApproval, sessionId],
  );

  // Mount recent rows first. A long journal can otherwise create tens of
  // thousands of DOM nodes before a cached thread gets its first paint.
  const [historyWindow, setHistoryWindow] = useState<{ sessionId: string | null; firstId: string | null }>({ sessionId: null, firstId: null });
  const anchorIndex = historyWindow.sessionId === sessionId && historyWindow.firstId
    ? messages.findIndex(message => message.id === historyWindow.firstId) : -1;
  let visibleStart = anchorIndex >= 0 ? anchorIndex : Math.max(0, messages.length - 20);
  const approvalIndex = askHumanMessageId ? messages.findIndex(message => message.id === askHumanMessageId) : -1;
  if (approvalIndex >= 0) visibleStart = Math.min(visibleStart, approvalIndex);
  const historyScroll = useRef<{ top: number; height: number } | null>(null);
  useLayoutEffect(() => {
    const previous = historyScroll.current;
    const el = document.getElementById("conversation");
    if (previous && el) el.scrollTop = previous.top + el.scrollHeight - previous.height;
    historyScroll.current = null;
  }, [visibleStart]);

  const stickToBottom = useRef(true);
  const [showJump, setShowJump] = useState(false);
  const [showScrollTop, setShowScrollTop] = useState(false);
  const initialScrollDone = useRef<string | null>(null);
  // Guards against the scroll event fired by our own programmatic scroll (and
  // mobile momentum/rubber-band jitter) from being misread as a deliberate
  // user scroll-up. Only a real, moving user gesture may detach
  // stickToBottom while inside this window — and "real" requires BOTH a
  // recent touch/wheel/pointer interaction AND the scroll position actually
  // having moved away from where that gesture started. Gesture presence
  // alone is not enough: touchstart fires on almost any tap in the chat
  // area (expanding "Thinking…", tapping a link, etc.), so treating it by
  // itself as "the user scrolled away" broke the default streaming
  // behavior — replies stopped following to the bottom on their own even
  // when the user never touched the scroll.
  //
  // Deliberately NOT inferring intent from the scrollTop delta alone either
  // (with no gesture check at all): a shrinking scrollHeight (a streaming
  // placeholder collapsing, an image reflowing smaller, a virtual-keyboard
  // resize) can move scrollTop with no user interaction, which looked
  // identical to a real scroll-up and wrongly detached stickToBottom right
  // after a successful "jump to latest".
  const programmaticUntil = useRef(0);
  const gestureActiveUntil = useRef(0);
  const gestureStartScrollTop = useRef<number | null>(null);
  const lastScrollHeight = useRef<number | null>(null);

  const getScroller = () =>
    document.getElementById("conversation") as HTMLElement | null;

  const scrollToBottom = (el: HTMLElement | null) => {
    if (!el || !stickToBottom.current) return;
    // A scheduled stream/resize frame must respect a swipe that began since
    // it was queued. Never write scrollTop while the user is reading history.
    // Mark a short window during which scroll events are treated as our own,
    // not a deliberate user scroll-up. Mobile dispatches the scroll event
    // asynchronously, so a fixed-time guard is more reliable than a one-shot.
    programmaticUntil.current = Date.now() + 600;
    // In WebKit (Safari on macOS and all iOS browsers), active trackpad/touch
    // momentum scroll continues decelerating on the compositor thread and can
    // overwrite programmatic scrollTop on subsequent frames.
    // Toggling overflow:hidden cancels any active momentum scrolling in WebKit.
    const prevOverflow = el.style.overflow;
    el.style.overflow = "hidden";
    el.scrollTop = el.scrollHeight;
    el.style.overflow = prevOverflow;
    try {
      el.scrollTo({ top: el.scrollHeight, behavior: "instant" });
    } catch {
      el.scrollTop = el.scrollHeight;
    }
    gestureStartScrollTop.current = null;
  };

  // Re-snap to the true bottom across several frames until the scroll height
  // stops changing. A single scrollToBottom lands short whenever content is
  // still growing after the call — markdown/mermaid layout, image decode, a
  // late-arriving failure banner — which is exactly the "Jump to latest then
  // drifts back to the middle" report. Each pass refreshes programmaticUntil,
  // so the trailing layout-driven scroll events during settling are never
  // mistaken for a user scroll-up. Cancellable so a real user scroll-up mid-
  // settle wins.
  const settleFrame = useRef(0);
  const settleToBottom = (el: HTMLElement | null) => {
    if (!el) return;
    if (settleFrame.current) cancelAnimationFrame(settleFrame.current);
    let lastHeight = -1;
    let stableFrames = 0;
    let frames = 0;
    const step = () => {
      settleFrame.current = 0;
      // A genuine user scroll-up during settling detaches stickToBottom;
      // honor it and stop fighting the user.
      if (!stickToBottom.current) return;
      scrollToBottom(el);
      if (el.scrollHeight === lastHeight) {
        stableFrames += 1;
      } else {
        stableFrames = 0;
        lastHeight = el.scrollHeight;
      }
      frames += 1;
      // Stop once height holds steady for a few frames, or after a generous
      // cap so a pathological page can't poll forever.
      if (stableFrames >= 4 || frames >= 60) return;
      settleFrame.current = requestAnimationFrame(step);
    };
    settleFrame.current = requestAnimationFrame(step);
  };

  useEffect(
    () => () => {
      if (settleFrame.current) cancelAnimationFrame(settleFrame.current);
    },
    [],
  );

  const lastUserText = useMemo(() => {
    for (let i = messages.length - 1; i >= 0; i--) {
      if (messages[i].role === "user" && messages[i].text?.trim()) {
        return messages[i].text;
      }
    }
    return "";
  }, [messages]);

  const lastIndex = messages.length - 1;
  const latestComputerMessageIndex = useMemo(() => {
    for (let i = messages.length - 1; i >= 0; i--) {
      if ((messages[i].computerFrames || []).length > 0) return i;
    }
    return -1;
  }, [messages]);
  const lastMsg = lastIndex >= 0 ? messages[lastIndex] : null;
  const canRetry =
    Boolean(lastUserText) &&
    !sending &&
    Boolean(
      lastMsg &&
        (lastMsg.role === "assistant" || lastMsg.role === "system") &&
        (runStatus === "error" ||
          lastMsg.statusLabel === "Error" ||
          /error|fail/i.test(lastMsg.statusLabel || "")),
    );

  useEffect(() => {
    const el = getScroller();
    if (!el) return;
    // Real user intent requires BOTH a recent touch/wheel/pointer
    // interaction on the scroller AND the scroll position actually having
    // moved since that interaction started. A tap alone (e.g. expanding
    // "Thinking…", tapping a link) is not scrolling — only record where
    // scrollTop was when the gesture began, so a later scroll event can be
    // compared against it.
    const markUserGesture = () => {
      gestureActiveUntil.current = Date.now() + 600;
      gestureStartScrollTop.current = el.scrollTop;
    };
    let touchOrigin: { x: number; y: number } | null = null;
    const detach = () => {
      stickToBottom.current = false;
      setShowJump(messages.length > 0);
    };
    const onTouchStart = (event: TouchEvent) => {
      markUserGesture();
      const touch = event.touches?.[0];
      touchOrigin = touch ? { x: touch.clientX, y: touch.clientY } : null;
    };
    const onTouchMove = (event: TouchEvent) => {
      const touch = event.touches?.[0];
      if (!touch || !touchOrigin) return;
      const dy = touch.clientY - touchOrigin.y;
      const dx = touch.clientX - touchOrigin.x;
      // A downward finger swipe reads earlier messages. Detect intent before
      // native scroll/ResizeObserver ordering can let another token snap down.
      // Horizontal code/table pans and simple taps keep following the reply.
      if (dy > 8 && dy > Math.abs(dx) && el.scrollHeight > el.clientHeight) detach();
    };
    let wheelTimer: ReturnType<typeof setTimeout> | null = null;
    const onWheel = (event: WheelEvent) => {
      markUserGesture();
      if (event.deltaY < 0 && Math.abs(event.deltaY) > Math.abs(event.deltaX)) detach();
      if (wheelTimer) clearTimeout(wheelTimer);
      wheelTimer = setTimeout(() => {
        gestureActiveUntil.current = 0;
        clearGestureStart();
      }, 300);
    };
    const onKeyDown = (event: KeyboardEvent) => {
      const target = event.target as HTMLElement | null;
      if (target?.closest("input, textarea, select, [contenteditable=true]")) return;
      if (["ArrowUp", "PageUp", "Home"].includes(event.key) || (event.key === " " && event.shiftKey)) {
        markUserGesture();
        detach();
      }
    };
    const onScroll = () => {
      const dist = el.scrollHeight - el.scrollTop - el.clientHeight;
      const stuck = dist <= 8;
      const gestureActive = Date.now() < gestureActiveUntil.current;
      const movedSinceGesture =
        gestureStartScrollTop.current !== null &&
        gestureStartScrollTop.current - el.scrollTop > 8;
      const realUserScroll = gestureActive && movedSinceGesture;
      // Content shrinking (a run's activity/transcript collapsing once it
      // finishes) can make the browser itself clamp an out-of-range
      // scrollTop toward 0 as part of that reflow, firing its own "scroll"
      // event with zero user interaction. That clamp can land well after a
      // real gesture (finger already lifted, no touchend fired yet — common
      // on mobile) and, worse, can look identical to "the user just swiped
      // up" since scrollTop moves down either way. A shrinking scrollHeight
      // is the one reliable signal that a scroll-position drop was
      // layout-driven, not a finger — never let it detach stickToBottom.
      const shrunk =
        lastScrollHeight.current !== null && el.scrollHeight < lastScrollHeight.current;
      lastScrollHeight.current = el.scrollHeight;
      if (!stuck && stickToBottom.current && (!realUserScroll || shrunk)) {
        // Native scroll restoration can undo Jump to latest after settling,
        // without a resize or new token (e.g. during a long reasoning call).
        // Recover here instead of waiting for more content to arrive.
        scrollToBottom(el);
        setShowJump(false);
        setShowScrollTop(el.scrollTop > 300);
        return;
      }
      if (Date.now() < programmaticUntil.current && !realUserScroll) {
        // Ignore scroll events we caused (or mobile jitter/layout-driven
        // shifts right after them): only an actual user scroll-up gesture
        // should be able to detach from the bottom while inside this
        // window.
        if (stuck) stickToBottom.current = true;
        if (stickToBottom.current) {
          setShowJump(false);
        } else {
          setShowJump(!stuck && messages.length > 0);
        }
        setShowScrollTop(el.scrollTop > 300);
        return;
      }
      if (stuck) {
        stickToBottom.current = true;
      } else if (!shrunk && realUserScroll) {
        stickToBottom.current = false;
      }
      setShowJump(!stuck && messages.length > 0);
      setShowScrollTop(el.scrollTop > 300);
    };
    const clearGestureStart = () => {
      touchOrigin = null;
      gestureStartScrollTop.current = null;
    };
    el.addEventListener("scroll", onScroll, { passive: true });
    el.addEventListener("touchstart", onTouchStart, { passive: true });
    el.addEventListener("touchmove", onTouchMove, { passive: true });
    el.addEventListener("touchcancel", clearGestureStart, { passive: true });
    el.addEventListener("keydown", onKeyDown);
    el.addEventListener("wheel", onWheel, { passive: true });
    el.addEventListener("pointerdown", markUserGesture, { passive: true });
    el.addEventListener("touchend", clearGestureStart, { passive: true });
    el.addEventListener("pointerup", clearGestureStart, { passive: true });
    return () => {
      if (wheelTimer) clearTimeout(wheelTimer);
      el.removeEventListener("scroll", onScroll);
      el.removeEventListener("touchstart", onTouchStart);
      el.removeEventListener("touchmove", onTouchMove);
      el.removeEventListener("touchcancel", clearGestureStart);
      el.removeEventListener("keydown", onKeyDown);
      el.removeEventListener("wheel", onWheel);
      el.removeEventListener("pointerdown", markUserGesture);
      el.removeEventListener("touchend", clearGestureStart);
      el.removeEventListener("pointerup", clearGestureStart);
    };
  }, [messages.length]);

  useEffect(() => {
    const el = getScroller();
    const content = document.getElementById("messages");
    if (!el || !content || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(() => {
      if (stickToBottom.current) scrollToBottom(el);
      setShowJump(el.scrollHeight - el.scrollTop - el.clientHeight > 8);
      setShowScrollTop(el.scrollTop > 300);
    });
    observer.observe(content);
    observer.observe(el);
    return () => observer.disconnect();
  }, [sessionId, sessionLoading, messages.length]);

  // Scroll to bottom on initial session load. A single requestAnimationFrame
  // is not enough on mobile: markdown/mermaid/image content can keep growing
  // #messages' height for several frames after the first paint (font load,
  // image decode, diagram layout), so a one-shot scroll lands short of the
  // true bottom. Poll scrollHeight for a short window and re-snap whenever it
  // grows, until it stops changing (or a hard cap elapses) so the initial
  // position always lands at the real latest message, not a stale one.
  useEffect(() => {
    if (!sessionLoading && messages.length > 0 && initialScrollDone.current !== sessionId) {
      initialScrollDone.current = sessionId;
      stickToBottom.current = true;
      const el = getScroller();
      if (!el) return;
      settleToBottom(el);
    }
  }, [messages.length, sessionId, sessionLoading]);

  useEffect(() => {
    if (!askHumanMessageId || !stickToBottom.current) return;
    const node = document.querySelector(
      `[data-ask-human-message="${askHumanMessageId}"]`,
    );
    node?.scrollIntoView({ behavior: "smooth", block: "nearest" });
  }, [askHumanMessageId]);

  // iOS Safari/Chrome (same WebKit engine) restores a scrollable element's
  // OLD native scrollTop on back-forward-cache (bfcache) restore / tab
  // resume — this fires AFTER our mount-time initial-scroll effect already
  // ran, silently overwriting it. That's the "always have to scroll down
  // myself" report: the page never actually failed to scroll on load, iOS
  // scrolled it back up again afterward. `pageshow` (checking `persisted`
  // for a true bfcache restore) and `visibilitychange` are the only
  // reliable signals for "the OS may have just clobbered scroll" on iOS;
  // re-settle to the bottom whenever either fires while stuck.
  useEffect(() => {
    const resettle = () => {
      if (!stickToBottom.current) return;
      const el = getScroller();
      if (el) settleToBottom(el);
    };
    const onPageShow = (e: PageTransitionEvent) => {
      if (e.persisted) resettle();
    };
    const onVisibility = () => {
      if (document.visibilityState === "visible") resettle();
    };
    window.addEventListener("pageshow", onPageShow);
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      window.removeEventListener("pageshow", onPageShow);
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, []);

  const streamTail = useMemo(() => {
    const last = messages[messages.length - 1];
    if (!last) return "";
    // Streaming assistant output arrives as narrative `blocks`, not `text`,
    // so keying only on text.length misses token-by-token growth. Include the
    // transcript reducer cursor/seq (which advance on every streamed chunk)
    // and the blocks count so the follow effect re-fires as content grows.
    return [
      last.id,
      last.text?.length ?? 0,
      last.transcriptCursor ?? 0,
      last.transcriptSeq ?? 0,
      last.blocks?.length ?? 0,
      last.streaming ? 1 : 0,
    ].join(":");
  }, [messages]);

  useEffect(() => {
    if (!stickToBottom.current) return;
    const el = getScroller();
    if (!el) return;
    const frame = requestAnimationFrame(() => {
      scrollToBottom(el);
    });
    return () => cancelAnimationFrame(frame);
  }, [streamTail, sending]);

  const jumpToLatest = () => {
    stickToBottom.current = true;
    setShowJump(false);
    gestureActiveUntil.current = 0;
    gestureStartScrollTop.current = null;
    programmaticUntil.current = Date.now() + 800;
    const el = getScroller();
    // Settle across frames: content below the fold (images, diagrams, a
    // freshly rendered failure banner) can keep growing #messages after the
    // first snap, which previously left the view stranded in the middle.
    if (el) {
      scrollToBottom(el);
      settleToBottom(el);
    }
  };

  const lastMessage = messages[lastIndex];

  const handleContinue = useCallback(() => {
    if (!lastMessage) return;
    if (isDeliveryUnconfirmed(lastMessage.resultStatus, lastMessage.statusLabel)) {
      const run = sessionId ? useAppStore.getState().runs[sessionId] : undefined;
      const check = run?.clientRequestId ? recoverLiveProgress(sessionId ?? undefined) : Promise.resolve();
      void check.then(() => {
        const current = useAppStore.getState();
        if (current.sessionId === sessionId && current.messages.some(message => message.id === lastMessage.id && !message.streaming && isDeliveryUnconfirmed(message.resultStatus, message.statusLabel))) {
          showToast("Delivery is still unconfirmed. Your reply is preserved.");
        }
      });
      return;
    }
    void resumeLastTask().catch((err: Error) => showToast(err.message || String(err)));
  }, [lastMessage, sessionId, recoverLiveProgress, showToast, resumeLastTask]);

  const handleRecover = useCallback((mode: Parameters<NonNullable<RecoverOutcome>>[0], outcomeIds?: string[]) => {
    void resumeLastTask({ mode, outcomeIds: outcomeIds || [], turnId: lastMessage?.turnId }).catch((err: Error) => showToast(err.message || String(err)));
  }, [resumeLastTask, lastMessage?.turnId, showToast]);

  const handleRetry = useCallback(() => {
    void retryLastTurn().catch((err: Error) => showToast(err.message || String(err)));
  }, [retryLastTurn, showToast]);

  if (sessionLoading) {
    return <MessageSkeletons />;
  }

  if (!messages.length) {
    return <EmptyState />;
  }

  return (
    <>
      <div className="pb-4 pt-2" id="messages">
        {visibleStart > 0 && <button type="button" className="mx-auto mb-3 block rounded-md px-3 py-1.5 text-xs text-muted hover:bg-line/40 hover:text-ink"
          onClick={() => {
            const el = getScroller();
            if (el) historyScroll.current = { top: el.scrollTop, height: el.scrollHeight };
            stickToBottom.current = false;
            setHistoryWindow({ sessionId, firstId: messages[Math.max(0, visibleStart - 20)].id });
          }}>Load earlier messages ({visibleStart})</button>}
        {messages.slice(visibleStart).map((m, offset) => (
          <MessageRow
            key={m.id}
            message={m}
            steeringActive={sending && Boolean(activeTurnId) && m.turnId === activeTurnId}
            index={sourceMessages.findIndex((source) => source.id === (m.sourceMessageId || m.id))}
            showRetry={canRetry && offset + visibleStart === lastIndex}
            showContinue={!sending && offset + visibleStart === lastIndex}
            resuming={Boolean(sessionId) && resumingSessionId === sessionId}
            onContinue={!sending && offset + visibleStart === lastIndex ? handleContinue : undefined}
            onRecover={!sending && offset + visibleStart === lastIndex ? handleRecover : undefined}
            showComputerLive={offset + visibleStart === latestComputerMessageIndex}
            onRetry={canRetry && offset + visibleStart === lastIndex ? handleRetry : undefined}
          />
        ))}
      </div>
      {document.getElementById("conversation-navigation") ? createPortal(<>
      {showJump ? (
        <button
          type="button"
          className={cn("ka-chat-jump inline-flex items-center gap-2", sending && "ka-chat-jump--live border-accent/40 shadow-accent/20 text-accent")}
          onPointerDown={(e) => e.stopPropagation()}
          onTouchStart={(e) => e.stopPropagation()}
          onClick={(e) => {
            e.preventDefault();
            e.stopPropagation();
            jumpToLatest();
          }}
        >
          {sending ? (
            <>
              <span className="relative flex h-2 w-2">
                <span className="animate-ping absolute inline-flex h-full w-full rounded-full bg-accent opacity-75" />
                <span className="relative inline-flex rounded-full h-2 w-2 bg-accent" />
              </span>
              <span>↓ Jump to latest</span>
            </>
          ) : (
            <span>↓ Jump to latest</span>
          )}
        </button>
      ) : null}
      {showScrollTop ? (
        <button
          type="button"
          className="ka-chat-jump"
          onPointerDown={(e) => e.stopPropagation()}
          onTouchStart={(e) => e.stopPropagation()}
          onClick={(e) => {
            e.preventDefault();
            e.stopPropagation();
            const el = getScroller();
            if (el) {
              stickToBottom.current = false;
              if (visibleStart > 0) setHistoryWindow({ sessionId, firstId: messages[0].id });
              el.scrollTo({ top: 0, behavior: "instant" });
            }
          }}
        >
          ↑ Go to top
        </button>
      ) : null}
      </>, document.getElementById("conversation-navigation")!) : null}
    </>
  );
}
