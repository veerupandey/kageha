import { RunChangeCard } from "./RunChangeCard";
import { GoalStatus } from "./GoalStatus";
import { PermissionMenu } from "./PermissionMenu";
import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import type { SlashCommand } from "../api/types";
import type { AgentPersona } from "../api/types";
import { filterSlashByCapabilities } from "../api/slashCatalog";
import { cn } from "../lib/cn";
import { Icon } from "../lib/icons";
import { ATTACHMENT_ACCEPT, filesFromClipboard } from "../lib/attachments";
import { PastChatAttachments } from "./PastChatAttachments";
import { PendingAttachments } from "./PendingAttachments";
import {
  applySlashCommand,
  filterSlashCommands,
  getAtContext,
  getSlashContext,
} from "../lib/slash";
import { useAppStore } from "../store";
import { stopSpokenReply } from "../lib/voiceClient";
import { ComposerModelMenu } from "./ComposerModelMenu";
import { ComposerSearchMenu } from "./ComposerSearchMenu";
import { ComposerMediaMenu } from "./ComposerMediaMenu";
import { ModelRoleControls } from "./ModelRoleControls";
import { AgentModeSwitcher } from "./AgentModeSwitcher";
import { ExecutionTargetChip } from "./ExecutionTargetChip";
import { TaskStatusChip } from "./TaskStatusChip";
import { SlashCommandPicker } from "./SlashCommandPicker";
import { useGeminiLiveVoice } from "./useGeminiLiveVoice";
import { useComposerController } from "./useComposerController";
import { QueuedMessages } from "./QueuedMessages";
import { ImmersiveVoiceOverlay } from "./voice/ImmersiveVoiceOverlay";

const QA_START =
  /^(what|who|when|where|why|how|is|are|can|could|would|should|do|does|did)\b/i;

function looksLikeGoalQA(text: string): boolean {
  const t = String(text || "").trim();
  if (!t) return false;
  if (t.endsWith("?")) return true;
  return QA_START.test(t);
}

interface FileHit {
  path: string;
  source?: string;
}

interface AgentHit {
  id: string;
  name: string;
  avatar?: string | null;
  description?: string;
}



export function Composer() {
  const draft = useAppStore((s) => s.draft);
  const setDraft = useAppStore((s) => s.setDraft);
  const agentMode = useAppStore((s) => s.agentMode);
  const setAgentMode = useAppStore((s) => s.setAgentMode);



  const composerChip = useAppStore((s) => s.composerChip);
  const clearComposerChip = useAppStore((s) => s.clearComposerChip);
  const slashCatalog = useAppStore((s) => s.slashCatalog);
  const capabilities = useAppStore((s) => s.capabilities);
  const voiceReply = useAppStore((s) => s.prefs.voiceReply);
  const setPrefs = useAppStore((s) => s.setPrefs);

  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const enterImmersiveRef = useRef<HTMLButtonElement>(null);
  const [immersiveOpen, setImmersiveOpen] = useState(false);
  const [caret, setCaret] = useState(0);
  const [slashIndex, setSlashIndex] = useState(0);
  const [slashMenuOpen, setSlashMenuOpen] = useState(true);
  const [atIndex, setAtIndex] = useState(0);
  const [atHits, setAtHits] = useState<FileHit[]>([]);
  const [atLoading, setAtLoading] = useState(false);
  const personas = useAppStore((s) => s.personas);
  const setActivePersonaId = useAppStore((s) => s.setActivePersonaId);
  const activePersonaId = useAppStore((s) => s.activePersonaId);
  const [agentHits, setAgentHits] = useState<AgentHit[]>([]);
  // Warning shown when user types @name that doesn't match any known agent.
  const [atMentionWarning, setAtMentionWarning] = useState<string | null>(null);
  const {
    addPendingFiles,
    handleMic,
    pendingFiles,
    recording,
    removePendingFile,
    sending,
    sessionId,
    stopGeneration,
    submit,
    transcribing,
  } = useComposerController({ draft, setDraft });
  const liveVoice = useGeminiLiveVoice();


  const slashCommands = useMemo(
    () => filterSlashByCapabilities(slashCatalog, capabilities),
    [slashCatalog, capabilities],
  );

  const slashCtx = useMemo(
    () => getSlashContext(draft, caret, slashCommands),
    [draft, caret, slashCommands],
  );
  const atCtx = useMemo(() => {
    if (slashCtx) return null;
    return getAtContext(draft, caret);
  }, [draft, caret, slashCtx]);

  const slashItems = useMemo(() => {
    if (!slashCtx) return [] as SlashCommand[];
    return filterSlashCommands(slashCommands, slashCtx.query);
  }, [slashCtx, slashCommands]);

  const showGoalBanner = agentMode === "goal" && looksLikeGoalQA(draft);

  const autosize = useCallback(() => {
    const el = textareaRef.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 200)}px`;
  }, []);

  useEffect(() => {
    autosize();
  }, [draft, autosize]);

  useEffect(() => {
    setSlashIndex(0);
    setSlashMenuOpen(true);
  }, [slashCtx?.token]);

  useEffect(() => {
    setAtIndex(0);
  }, [atCtx?.query]);

  // Filter agents from the in-memory personas list — no network call needed.
  useEffect(() => {
    if (!atCtx) {
      setAgentHits([]);
      setAtMentionWarning(null);
      return;
    }
    const q = (atCtx.query || "").toLowerCase();
    const hits = personas
      .filter((p: AgentPersona) => {
        if (!q) return true;
        return (
          p.id.toLowerCase().includes(q) ||
          p.name.toLowerCase().includes(q) ||
          (p.description || "").toLowerCase().includes(q)
        );
      })
      .map((p: AgentPersona) => ({
        id: p.id,
        name: p.name,
        avatar: p.avatar,
        description: p.description,
      }));
    setAgentHits(hits);
    // Warn when the query looks like a complete slug (no space yet) and
    // matches nothing — so @typo shows feedback before the user hits Send.
    if (q.length >= 2 && hits.length === 0 && !q.includes(" ")) {
      setAtMentionWarning(
        `No agent "@${q}" — choose an existing agent or remove the mention.`,
      );
    } else {
      setAtMentionWarning(null);
    }
  }, [atCtx, personas]);

  const atOpen = Boolean(atCtx);
  const atQuery = atCtx?.query ?? "";

  useEffect(() => {
    if (!atOpen || capabilities.projectFiles === false) {
      setAtHits([]);
      setAtLoading(false);
      return;
    }
    let cancelled = false;
    setAtLoading(true);
    const q = encodeURIComponent(atQuery);
    const timer = window.setTimeout(() => {
      void fetch(`/api/project/files?q=${q}&limit=20`)
        .then((r) => r.json().catch(() => ({})))
        .then((data: { files?: FileHit[]; items?: FileHit[] }) => {
          if (cancelled) return;
          const list = data.files || data.items || [];
          setAtHits(
            list
              .map((f) =>
                typeof f === "string"
                  ? { path: f }
                  : { path: String(f.path || ""), source: f.source },
              )
              .filter((f) => f.path),
          );
          setAtLoading(false);
        })
        .catch(() => {
          if (!cancelled) {
            setAtHits([]);
            setAtLoading(false);
          }
        });
    }, 220);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [atOpen, atQuery, capabilities.projectFiles]);

  const syncCaret = () => {
    const el = textareaRef.current;
    if (el) setCaret(el.selectionStart ?? el.value.length);
  };

  const replaceRange = (start: number, end: number, insert: string) => {
    const head = draft.slice(0, start);
    const after = draft.slice(end);
    const next = head + insert + after;
    setDraft(next);
    requestAnimationFrame(() => {
      const el = textareaRef.current;
      if (!el) return;
      const pos = head.length + insert.length;
      el.focus();
      el.setSelectionRange(pos, pos);
      setCaret(pos);
    });
  };

  const pickSlash = (cmd: SlashCommand) => {
    if (!slashCtx) return;
    const result = applySlashCommand(cmd, {
      start: slashCtx.start,
      end: Math.max(slashCtx.end, caret),
    });
    if (result === "attach") fileInputRef.current?.click();
    if (result === "focus-model") {
      document.getElementById("composer-model-menu")?.click();
      return;
    }
    requestAnimationFrame(() => textareaRef.current?.focus());
  };

  const pickAt = (hit: FileHit) => {
    if (!atCtx) return;
    replaceRange(atCtx.start, Math.max(atCtx.end, caret), `@${hit.path} `);
  };

  // Selecting an agent: remove @token from the draft and switch the active
  // persona. The agent name appears in the composer chip (activePersonaId),
  // not as literal text in the message.
  const pickAgent = (hit: AgentHit) => {
    if (!atCtx) return;
    replaceRange(atCtx.start, Math.max(atCtx.end, caret), "");
    setActivePersonaId(hit.id);
  };

  const chipSlash =
    composerChip.kind === "multitask"
      ? "/new"
      : composerChip.kind === "mode" && composerChip.value
        ? `/${composerChip.value}`
        : null;

  return (
    <form
      className="ka-composer shrink-0 border-t border-line bg-canvas px-3 pb-3 pt-2 md:px-5"
      id="composer"
      onSubmit={(e) => {
        e.preventDefault();
        // Only send via button click — keyboard Enter is handled in onKeyDown
      }}
    >
      <div className="mx-auto max-w-3xl">
        {showGoalBanner ? (
          <div
            className="mb-2 flex items-center gap-2 rounded-md bg-warn-soft px-3 py-1.5 text-sm text-warn"
            role="status"
          >
            <span className="flex-1">This looks like Normal</span>
            <button
              type="button"
              className="font-medium underline-offset-2 hover:underline"
              onClick={() => setAgentMode("normal")}
            >
              Switch to Normal
            </button>
          </div>
        ) : null}

        {sessionId && <GoalStatus key={sessionId} sessionId={sessionId} />}
        {sessionId && <RunChangeCard key={sessionId} sessionId={sessionId} />}

        <PendingAttachments files={pendingFiles} onRemove={removePendingFile} />

        <QueuedMessages />

        <div className="ka-composer-surface relative rounded-2xl border border-line/80 bg-surface shadow-[0_2px_12px_rgba(0,0,0,0.08)] ring-1 ring-line/20 focus-within:border-accent/50 focus-within:ring-accent/20 transition-all">
          {!atCtx && personas.some(p => !p.builtin) && (
            <div className="flex min-w-0 items-center gap-2 border-b border-line/40 px-3 py-2">
              <label htmlFor="chat-agent-recipient" className="shrink-0 text-xs text-muted">To</label>
              <select id="chat-agent-recipient" aria-label="Reply from agent" value={activePersonaId || "main"}
                onChange={event => setActivePersonaId(event.target.value)}
                className="min-w-0 max-w-56 truncate rounded-lg border border-line bg-canvas px-2 py-1 text-xs text-ink focus-visible:outline-2 focus-visible:outline-accent">
                {personas.map(p => <option key={p.id} value={p.id}>{p.builtin ? "Kageha" : p.name}</option>)}
              </select>
              <span className="ml-auto hidden text-[11px] text-faint sm:block">Same chat · individual context</span>
            </div>
          )}

          {/* ── @mention warning ── */}
          {atMentionWarning && (
            <div
              className="flex items-center gap-2 border-b border-warn/30 bg-warn-soft/60 px-3 py-1 text-[0.72rem] text-warn"
              role="alert"
            >
              <span>⚠</span>
              <span>{atMentionWarning}</span>
            </div>
          )}
          {slashCtx && slashMenuOpen ? (
            <SlashCommandPicker
              items={slashItems}
              query={slashCtx.query}
              activeIndex={slashIndex}
              onHover={setSlashIndex}
              onPick={pickSlash}
            />
          ) : null}

          {atCtx ? (
            <div
              className={`absolute left-0 right-0 z-20 max-h-56 overflow-auto rounded-lg border border-line bg-surface p-1 shadow-lg ${
                textareaRef.current && textareaRef.current.getBoundingClientRect().top < 260
                  ? "top-full mt-1"
                  : "bottom-full mb-1"
              }`}
              id="at-picker"
              role="listbox"
              aria-label="Agents and file mentions"
            >
              {/* ── Agent matches ── */}
              {agentHits.length > 0 && (
                <>
                  <p className="px-2 pb-0.5 pt-1 text-[0.65rem] font-semibold uppercase tracking-wider text-faint">
                    Agents
                  </p>
                  {agentHits.map((hit, i) => (
                    <button
                      key={`agent:${hit.id}`}
                      type="button"
                      role="option"
                      aria-selected={i === atIndex}
                      className={cn(
                        "flex w-full items-center gap-2 rounded-md px-2.5 py-1.5 text-left text-sm",
                        i === atIndex && "bg-accent-soft",
                      )}
                      onMouseEnter={() => setAtIndex(i)}
                      onMouseDown={(e) => {
                        e.preventDefault();
                        pickAgent(hit);
                      }}
                    >
                      <span className="flex h-5 w-5 shrink-0 items-center justify-center rounded-full bg-accent/15 text-xs">
                        {hit.avatar || hit.name.charAt(0).toUpperCase()}
                      </span>
                      <span className="min-w-0 flex-1">
                        <span className="block truncate font-medium text-ink">
                          {hit.name}
                          {hit.id === activePersonaId && (
                            <span className="ml-1.5 text-[0.65rem] font-normal text-accent">active</span>
                          )}
                        </span>
                        {hit.description ? (
                          <span className="block truncate text-xs text-faint">
                            {hit.description}
                          </span>
                        ) : null}
                      </span>
                    </button>
                  ))}
                </>
              )}

              {/* ── File matches ── */}
              {(atHits.length > 0 || atLoading) && (
                <p className="px-2 pb-0.5 pt-1 text-[0.65rem] font-semibold uppercase tracking-wider text-faint">
                  Files
                </p>
              )}
              {atLoading ? (
                <p className="px-2.5 py-2 text-sm text-muted">Searching…</p>
              ) : !atHits.length && !agentHits.length ? (
                <p className="px-2.5 py-2 text-sm text-muted">No matches</p>
              ) : (
                atHits.map((hit, i) => {
                  const idx = agentHits.length + i;
                  return (
                    <button
                      key={hit.path}
                      type="button"
                      role="option"
                      aria-selected={idx === atIndex}
                      className={cn(
                        "flex w-full items-center gap-2 rounded-md px-2.5 py-1.5 text-left text-sm",
                        idx === atIndex && "bg-accent-soft",
                      )}
                      onMouseEnter={() => setAtIndex(idx)}
                      onMouseDown={(e) => {
                        e.preventDefault();
                        pickAt(hit);
                      }}
                    >
                      <span className="min-w-0 flex-1 truncate font-mono text-xs">
                        {hit.path}
                      </span>
                      {hit.source ? (
                        <span className="text-xs text-faint">{hit.source}</span>
                      ) : null}
                    </button>
                  );
                })
              )}
            </div>
          ) : null}

          <div className="flex items-end gap-1.5 px-3 pt-2.5">
            <button
              type="button"
              className="ka-icon-btn mb-1.5 h-8 w-8 shrink-0"
              id="btn-attach"
              title="Attach files (drag & drop also works)"
              aria-label="Attach files"
              onClick={() => fileInputRef.current?.click()}
            >
              <Icon.Attach size={18} />
            </button>
            {sessionId && <PastChatAttachments key={sessionId} sessionId={sessionId} />}
            <button
              type="button"
              className={cn(
                "mb-1.5 inline-flex h-8 shrink-0 items-center justify-center rounded-lg px-2.5 text-xs font-medium transition-colors",
                recording
                  ? "bg-danger/15 text-danger"
                  : "text-muted hover:bg-line/50 hover:text-ink",
              )}
              id="btn-mic"
              title={
                recording
                  ? "Stop recording"
                  : "Click to record, click again to send"
              }
              aria-label={recording ? "Stop recording" : "Voice input"}
              aria-pressed={recording}
              disabled={!sessionId || sending || transcribing}
              onClick={handleMic}
            >
              {transcribing ? "…" : <Icon.Mic size={15} className={recording ? "text-danger" : undefined} />}
            </button>
            <button
              ref={liveVoice.levelTargetRef}
              type="button"
              className={cn(
                "ka-live-voice-btn mb-1.5 inline-flex h-8 shrink-0 items-center justify-center gap-1.5 rounded-full px-3 text-xs font-medium transition-colors",
                liveVoice.status === "active"
                  ? "bg-accent text-white shadow-sm"
                  : liveVoice.status === "connecting"
                    ? "bg-accent/15 text-accent"
                    : "bg-accent-soft text-accent ring-1 ring-accent/30 hover:bg-accent/20",
              )}
              data-phase={
                liveVoice.status === "active"
                  ? liveVoice.phase
                  : liveVoice.status === "connecting"
                    ? "connecting"
                    : undefined
              }
              id="btn-live-voice"
              title={
                liveVoice.status === "active"
                  ? "Stop live voice conversation"
                  : "Start a live voice conversation (Gemini)"
              }
              aria-label={liveVoice.status === "active" ? "Stop live voice" : "Start live voice"}
              aria-pressed={liveVoice.status === "active"}
              disabled={!sessionId || recording || liveVoice.status === "connecting"}
              onClick={liveVoice.toggle}
            >
              <Icon.LiveVoice
                size={15}
                className={liveVoice.status === "active" ? "text-white" : undefined}
              />
              <span>
                {liveVoice.status === "active"
                  ? "Stop"
                  : liveVoice.status === "connecting"
                    ? "Connecting…"
                    : "Live"}
              </span>
            </button>
            <button
              ref={enterImmersiveRef}
              type="button"
              className={cn(
                "ka-enter-immersive-btn mb-1.5 inline-flex h-8 shrink-0 items-center justify-center rounded-lg px-2.5 text-xs font-medium transition-colors",
                liveVoice.status === "active" || liveVoice.status === "connecting"
                  ? "text-accent hover:bg-accent/15"
                  : "text-muted hover:bg-line/50 hover:text-ink",
              )}
              id="btn-enter-immersive"
              title="Open immersive live voice view"
              aria-label="Open immersive live voice view"
              aria-haspopup="dialog"
              aria-expanded={immersiveOpen}
              disabled={!sessionId}
              onClick={() => setImmersiveOpen(true)}
            >
              <Icon.Maximize size={15} />
            </button>
            <input
              ref={fileInputRef}
              type="file"
              id="file-input"
              multiple
              hidden
              accept={ATTACHMENT_ACCEPT}
              onChange={(e) => {
                addPendingFiles(e.target.files);
                e.target.value = "";
              }}
            />
            <div className="min-w-0 flex-1">
              <div className="flex items-start gap-1.5">
                {chipSlash ? (
                  <span className="ka-composer-chip" data-testid="composer-chip">
                    <span>{chipSlash}</span>
                    <button
                      type="button"
                      aria-label={`Remove ${chipSlash}`}
                      onClick={() =>
                        clearComposerChip({
                          resetMode: composerChip.kind === "mode",
                        })
                      }
                    >
                      <Icon.Close size={12} />
                    </button>
                  </span>
                ) : null}
                <div className="min-w-0 flex-1">
                  <label className="sr-only" htmlFor="message-input">
                    Message
                  </label>
                  <textarea
                    ref={textareaRef}
                    id="message-input"
                    className="max-h-[200px] min-h-[48px] w-full resize-none bg-transparent px-2 py-2.5 text-[0.92rem] leading-relaxed outline-none placeholder:text-faint/70"
                    rows={1}
                    placeholder={
                      chipSlash
                        ? `Continue after ${chipSlash}…`
                        : "Ask anything, or type / for commands"
                    }
                    autoComplete="off"
                    value={draft}
                    onChange={(e) => {
                      setDraft(e.target.value);
                      setCaret(e.target.selectionStart ?? e.target.value.length);
                    }}
                    onClick={syncCaret}
                    onKeyUp={syncCaret}
                    onSelect={syncCaret}
                    onPaste={(e) => {
                      const files = filesFromClipboard(e.clipboardData);
                      if (files.length) {
                        e.preventDefault();
                        addPendingFiles(files);
                      }
                    }}
                    onKeyDown={(e) => {
                      if (slashCtx && slashMenuOpen) {
                        if (e.key === "ArrowDown") {
                          e.preventDefault();
                          if (slashItems.length) {
                            setSlashIndex((i) =>
                              Math.min(i + 1, slashItems.length - 1),
                            );
                          }
                          return;
                        }
                        if (e.key === "ArrowUp") {
                          e.preventDefault();
                          setSlashIndex((i) => Math.max(0, i - 1));
                          return;
                        }
                        if (
                          (e.key === "Enter" || e.key === "Tab") &&
                          slashItems.length
                        ) {
                          e.preventDefault();
                          const cmd = slashItems[slashIndex];
                          if (cmd) pickSlash(cmd);
                          return;
                        }
                        if (e.key === "Escape") {
                          e.preventDefault();
                          setSlashMenuOpen(false);
                          return;
                        }
                      }
                      if (atCtx && (agentHits.length + atHits.length) > 0) {
                        if (e.key === "ArrowDown") {
                          e.preventDefault();
                          setAtIndex((i) =>
                            Math.min(i + 1, agentHits.length + atHits.length - 1),
                          );
                          return;
                        }
                        if (e.key === "ArrowUp") {
                          e.preventDefault();
                          setAtIndex((i) => Math.max(0, i - 1));
                          return;
                        }
                        if (e.key === "Enter" || e.key === "Tab") {
                          e.preventDefault();
                          if (atIndex < agentHits.length) {
                            const hit = agentHits[atIndex];
                            if (hit) pickAgent(hit);
                          } else {
                            const hit = atHits[atIndex - agentHits.length];
                            if (hit) pickAt(hit);
                          }
                          return;
                        }
                        if (e.key === "Escape") {
                          e.preventDefault();
                          replaceRange(atCtx.start, caret, "");
                          return;
                        }
                      }
                      if (e.key === "Enter") {
                        if (e.shiftKey || e.altKey) {
                          e.preventDefault();
                          const el = e.currentTarget;
                          const start = el.selectionStart ?? draft.length;
                          const end = el.selectionEnd ?? draft.length;
                          const pos = start + 1;
                          setDraft(
                            draft.slice(0, start) + "\n" + draft.slice(end),
                          );
                          requestAnimationFrame(() => {
                            el.focus();
                            el.setSelectionRange(pos, pos);
                            setCaret(pos);
                          });
                          return;
                        }
                        if (!e.nativeEvent.isComposing) {
                          e.preventDefault();
                          submit(undefined, sending && (e.ctrlKey || e.metaKey) ? { delivery: "steer" } : undefined);
                        }
                      }
                    }}
                  />
                </div>
              </div>
            </div>
            <div className="mb-1.5 flex shrink-0 items-center gap-1.5">
              {sending ? (
                <>
                  <button
                    type="button"
                    className="rounded-lg px-2.5 py-1.5 text-xs font-medium text-muted hover:bg-line/50 hover:text-ink transition-colors disabled:opacity-30"
                    id="btn-queue"
                    title="Send when this turn finishes (Enter)"
                    disabled={!draft.trim() && !pendingFiles.length}
                    onClick={() => submit(undefined, { allowWhileSending: true })}
                  >
                    Queue
                  </button>
                  <button
                    type="button"
                    className="rounded-lg bg-accent px-3 py-1.5 text-xs font-semibold text-white shadow-xs hover:opacity-90 active:scale-[0.98] transition-all disabled:opacity-40"
                    id="btn-steer"
                    title="Apply after the current tool call finishes (Ctrl+Enter)"
                    disabled={!draft.trim() && !pendingFiles.length}
                    onClick={() => submit(undefined, { delivery: "steer" })}
                  >
                    Steer
                  </button>
                  <button
                    type="button"
                    id="btn-send-now"
                    className="rounded-lg border border-line px-2.5 py-1.5 text-xs font-medium text-ink hover:bg-line/50 transition-colors disabled:opacity-30"
                    title="Stop the current turn and send this message now"
                    disabled={recording || transcribing || (!draft.trim() && !pendingFiles.length)}
                    onClick={() => submit(undefined, { delivery: "now" })}
                  >
                    Send now
                  </button>
                  <button
                    type="button"
                    className="inline-flex items-center gap-1.5 rounded-lg bg-rose-600/90 hover:bg-rose-600 text-white px-3 py-1.5 text-xs font-semibold shadow-xs transition-all active:scale-[0.98]"
                    id="btn-stop"
                    aria-label="Stop generation"
                    onClick={() => {
                      void stopGeneration();
                    }}
                  >
                    <span className="h-2 w-2 rounded-2xs bg-white" aria-hidden="true" />
                    <span>Stop</span>
                  </button>
                </>
              ) : (
                <button
                  type="button"
                  className={cn(
                    "rounded-lg bg-accent px-3.5 py-1.5 text-xs font-semibold text-white shadow-xs",
                    "hover:opacity-90 active:scale-[0.97] transition-all",
                    "disabled:opacity-40 disabled:cursor-not-allowed disabled:active:scale-100",
                  )}
                  id="btn-send"
                  disabled={!draft.trim() && !pendingFiles.length}
                  onClick={() => submit()}
                >
                  Send
                </button>
              )}
            </div>
          </div>

          <div className="flex flex-wrap items-center gap-2 border-t border-line/50 px-3 py-1.5">
            <div className="flex flex-wrap items-center gap-2">
              <ComposerModelMenu />
              <ComposerSearchMenu />
              <ComposerMediaMenu />
            </div>
            <AgentModeSwitcher />
            <ExecutionTargetChip />
            <TaskStatusChip />
            {activePersonaId && activePersonaId !== "main" && (() => {
              const p = personas.find((x: AgentPersona) => x.id === activePersonaId);
              return p ? (
                <button
                  type="button"
                  className="flex items-center gap-1 rounded-full border border-accent/40 bg-accent/10 px-2 py-0.5 text-[0.68rem] font-medium text-accent hover:bg-accent/20"
                  title={`Reply from ${p.name}. Click to return to Kageha.`}
                  onClick={() => setActivePersonaId("main")}
                >
                  {/* Use avatar emoji if set, otherwise name initial — consistent with chip row */}
                  <span>{(p.avatar || "").trim() || p.name.trim().charAt(0).toUpperCase()}</span>
                  <span>To {p.name}</span>
                  <span className="text-accent/60">×</span>
                </button>
              ) : null;
            })()}

            <button
              type="button"
              className={cn(
                "rounded-md px-2 py-1 text-xs font-medium",
                voiceReply
                  ? "bg-accent-soft text-accent"
                  : "text-muted hover:bg-line/70 hover:text-ink",
              )}
              title="Speak assistant replies using your selected voice provider"
              aria-pressed={voiceReply}
              onClick={() => {
                const next = !voiceReply;
                setPrefs({ voiceReply: next });
                if (!next) stopSpokenReply();
              }}
            >
              Speak
            </button>

            <PermissionMenu sessionId={sessionId ?? null} ensureSession={useAppStore.getState().ensureSession} />

            <ModelRoleControls compact />
          </div>
        </div>
      </div>

      <ImmersiveVoiceOverlay
        open={immersiveOpen}
        openerRef={enterImmersiveRef}
        voice={liveVoice}
        sessionId={sessionId ?? null}
        onClose={() => setImmersiveOpen(false)}
      />
    </form>
  );
}
