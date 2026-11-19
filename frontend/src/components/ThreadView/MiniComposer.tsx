import { useCallback, useEffect, useRef, useState } from "react";
import { useAppStore } from "../../store";
import { ATTACHMENT_ACCEPT, filesFromClipboard } from "../../lib/attachments";
import { Icon } from "../../lib/icons";
import { cn } from "../../lib/cn";
import { useComposerController } from "../useComposerController";
import { RunChangeCard } from "../RunChangeCard";
import { PermissionMenu } from "../PermissionMenu";
import { ComposerModelMenu } from "../ComposerModelMenu";
import { ComposerTools } from "../ComposerTools";
import { VoiceStatus } from "../VoiceStatus";
import { AttachmentUploadStatus } from "../AttachmentUploadStatus";
import { PendingAttachments } from "../PendingAttachments";
import { QueuedMessages } from "../QueuedMessages";
import { useSharedGeminiLiveVoice } from "../GeminiLiveVoiceContext";
import { ImmersiveVoiceOverlay } from "../voice/ImmersiveVoiceOverlay";
import { useComposerAutocomplete } from "../useComposerAutocomplete";

export function MiniComposer({ home = false }: { home?: boolean }) {
  const localDraft = useAppStore((s) => s.draft);
  const setLocalDraft = useAppStore((s) => s.setDraft);
  const uploading = useAppStore(s => s.attachmentUpload?.status === "uploading" && s.attachmentUpload.sessionId === s.sessionId);
  const preparing = useAppStore(s => s.preparingSends.some(pending => pending.sessionId === s.sessionId));
  const cancelPreparingSend = useAppStore(s => s.cancelPreparingSend);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const enterImmersiveRef = useRef<HTMLButtonElement>(null);
  const [immersiveOpen, setImmersiveOpen] = useState(false);
  const {
    addPendingFiles,
    handleMic,
    cancelRecording,
    pendingFiles,
    recording,
    removePendingFile,
    sending,
    sessionId,
    stopGeneration,
    submit,
    transcribing,
  } = useComposerController({
    draft: localDraft,
    setDraft: setLocalDraft,
  });
  const liveVoice = useSharedGeminiLiveVoice();
  const autocomplete = useComposerAutocomplete(inputRef, fileInputRef, uploading || recording || transcribing);
  const ensureSession = useAppStore((s) => s.ensureSession);
  const showToast = useAppStore((s) => s.showToast);
  const [startingLiveVoice, setStartingLiveVoice] = useState(false);

  // Live voice needs a session to attach to. If none exists yet (e.g. the
  // welcome/home composer before any message has been sent), mint one first
  // — mirroring how Gemini/ChatGPT let you start talking straight from the
  // home screen instead of forcing a thread to exist first.
  const handleLiveVoiceClick = useCallback(() => {
    if (liveVoice.status === "active" || liveVoice.status === "connecting") {
      liveVoice.toggle();
      return;
    }
    if (sessionId) {
      liveVoice.toggle();
      return;
    }
    void (async () => {
      setStartingLiveVoice(true);
      try {
        const { sessionId: mintedSessionId } = await ensureSession();
        liveVoice.start(mintedSessionId);
      } catch (err) {
        showToast(err instanceof Error ? err.message : "Could not start a thread for live voice");
      } finally {
        setStartingLiveVoice(false);
      }
    })();
  }, [ensureSession, liveVoice, sessionId, showToast]);

  const autosize = useCallback(() => {
    const el = inputRef.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 160)}px`;
  }, []);

  useEffect(() => {
    autosize();
  }, [localDraft, autosize]);

  const attachClipboard = useCallback(
    (data: DataTransfer | null | undefined) => {
      const files = filesFromClipboard(data);
      if (!files.length) return false;
      addPendingFiles(files);
      return true;
    },
    [addPendingFiles],
  );

  const handleSend = () => {
    if (!preparing) submit(localDraft);
  };

  return (
    <div className={`ka-composer ka-unified-composer ${home ? "ka-home-composer" : ""} shrink-0 border-t border-line bg-canvas px-3 pb-3 pt-2`}>
      <AttachmentUploadStatus />
      {preparing && !uploading && <div className="mb-2 text-xs text-muted" role="status" aria-live="polite">Preparing request…</div>}
      {!home && sessionId && <RunChangeCard key={sessionId} sessionId={sessionId} />}
      <PendingAttachments files={pendingFiles} onRemove={removePendingFile} disabled={uploading} />
      <QueuedMessages />
      <VoiceStatus recording={recording} transcribing={transcribing} onFinish={handleMic} onCancel={cancelRecording} />
      <div
        className="ka-composer-surface relative rounded-xl border border-[var(--color-line)] bg-[var(--color-surface)] px-3 py-2 focus-within:border-[var(--color-line-strong)]"
        onPaste={(e) => {
          if (!uploading && attachClipboard(e.clipboardData)) e.preventDefault();
        }}
        onDragOver={(e) => {
          if (Array.from(e.dataTransfer?.types || []).includes("Files")) {
            e.preventDefault();
          }
        }}
        onDrop={(e) => {
          if (uploading || !e.dataTransfer?.files?.length) return;
          e.preventDefault();
          addPendingFiles(e.dataTransfer.files);
        }}
      >
        {autocomplete.picker}
        {autocomplete.badgeElement}
        <div className="ka-message-entry flex items-end gap-2">
          <button
            type="button"
            id="btn-attach"
            disabled={uploading}
            className="inline-flex h-6 w-6 shrink-0 items-center justify-center rounded-md text-faint hover:text-ink"
            aria-label="Attach files"
            title="Attach or paste images"
            onClick={() => fileInputRef.current?.click()}
          >
            <Icon.Attach size={16} />
          </button>
          <input
            ref={fileInputRef}
            type="file"
            disabled={uploading}
            multiple
            className="hidden"
            accept={ATTACHMENT_ACCEPT}
            onChange={(e) => {
              if (e.target.files?.length) addPendingFiles(e.target.files);
              e.target.value = "";
            }}
          />
          <textarea
            ref={inputRef}
            id="message-input"
            aria-label="Message Kageha"
            className="max-h-[160px] min-h-[24px] min-w-0 flex-1 resize-none bg-transparent py-0.5 text-sm leading-relaxed text-ink outline-none placeholder:text-faint focus-visible:!outline-none focus-visible:!ring-0"
            rows={1}
            placeholder={home ? "Ask anything or start a task… / commands · @ agents · & teams" : "Add a follow-up… / commands · @ agents · & teams"}
            readOnly={uploading || recording || transcribing}
            value={localDraft}
            aria-autocomplete="list"
            aria-controls={autocomplete.open ? autocomplete.listboxId : undefined}
            onChange={(e) => {
              setLocalDraft(e.target.value);
              autocomplete.setCaret(e.target.selectionStart ?? e.target.value.length);
            }}
            onClick={autocomplete.syncCaret}
            onFocus={autocomplete.syncCaret}
            onSelect={autocomplete.syncCaret}
            onKeyUp={autocomplete.syncCaret}
            onKeyDown={(e) => {
              if (autocomplete.onKeyDown(e)) return;
              if (e.key !== "Enter") return;
              if (window.matchMedia?.("(pointer: coarse)").matches && !e.ctrlKey && !e.metaKey) return;
              if (e.shiftKey) {
                e.preventDefault();
                const el = e.currentTarget;
                const start = el.selectionStart ?? localDraft.length;
                const end = el.selectionEnd ?? localDraft.length;
                const pos = start + 1;
                setLocalDraft(localDraft.slice(0, start) + "\n" + localDraft.slice(end));
                requestAnimationFrame(() => {
                  el.focus();
                  el.setSelectionRange(pos, pos);
                });
                return;
              }
              if (!e.nativeEvent.isComposing) {
                e.preventDefault();
                if (!preparing) submit(localDraft, sending && (e.ctrlKey || e.metaKey) ? { delivery: "steer" } : undefined);
              }
            }}
          />
          <button
            type="button"
            id="btn-mic"
            className="shrink-0 px-1 text-[0.7rem] text-faint hover:text-ink disabled:opacity-40"
            title={recording ? "Stop recording" : "Voice input"}
            aria-label={recording ? "Stop recording" : "Voice input"}
            aria-pressed={recording}
            disabled={sending || transcribing}
            onClick={handleMic}
          >
            {transcribing ? "…" : recording ? "■" : <Icon.Mic size={18} />}
          </button>
          <button
            ref={liveVoice.levelTargetRef}
            type="button"
            id="btn-live-voice"
            className={cn(
              "ka-live-voice-btn inline-flex h-7 shrink-0 items-center justify-center gap-1 rounded-full px-1 text-[0.7rem] font-medium transition-colors disabled:opacity-40",
              liveVoice.status === "active"
                ? "bg-accent px-2.5 text-white shadow-[0_0_10px_2px_var(--color-accent)]"
                : liveVoice.status === "connecting" || startingLiveVoice
                  ? "px-2.5 text-accent"
                  : "text-faint hover:text-ink",
            )}
            data-phase={
              liveVoice.status === "active"
                ? liveVoice.phase
                : liveVoice.status === "connecting" || startingLiveVoice
                  ? "connecting"
                  : undefined
            }
            title={
              liveVoice.status === "active" || liveVoice.status === "connecting"
                ? "Stop live voice conversation"
                : "Start a live voice conversation (Gemini)"
            }
            aria-label={
              liveVoice.status === "active" || liveVoice.status === "connecting"
                ? "Stop live voice"
                : "Start live voice"
            }
            aria-pressed={liveVoice.status === "active"}
            disabled={recording || startingLiveVoice}
            onClick={handleLiveVoiceClick}
          >
            <Icon.LiveVoice size={liveVoice.status === "active" ? 15 : 18} className={liveVoice.status === "active" ? "text-white" : undefined} />
            {liveVoice.status === "active" || liveVoice.status === "connecting" || startingLiveVoice ? (
              <span>
                {liveVoice.status === "active" ? "Stop" : "Connecting…"}
              </span>
            ) : null}
          </button>
          {liveVoice.status === "error" && liveVoice.error ? (
            <span role="alert" className="max-w-56 text-xs text-red-400" title={liveVoice.error}>
              {liveVoice.error}
            </span>
          ) : null}
          <button
            ref={enterImmersiveRef}
            type="button"
            id="btn-enter-immersive"
            className={cn(
              "inline-flex h-7 w-7 shrink-0 items-center justify-center rounded-md transition-colors disabled:opacity-40",
              liveVoice.status === "active" || liveVoice.status === "connecting"
                ? "text-accent hover:bg-accent/15"
                : "text-faint hover:text-ink",
            )}
            title="Open immersive live voice view"
            aria-label="Open immersive live voice view"
            aria-haspopup="dialog"
            aria-expanded={immersiveOpen}
            disabled={!sessionId && liveVoice.status === "idle"}
            onClick={() => setImmersiveOpen(true)}
          >
            <Icon.Maximize size={14} />
          </button>
          {sending || preparing ? (
            <button
              type="button"
              id="btn-stop"
              className="inline-flex h-7 items-center justify-center rounded-full bg-red-500 px-3 text-xs font-medium text-white hover:bg-red-600 transition-colors"
              aria-label={preparing ? "Cancel request preparation" : uploading ? "Cancel upload" : "Stop"}
              onClick={() => preparing ? cancelPreparingSend() : void stopGeneration()}
            >
              {preparing || uploading ? "Cancel" : "Stop"}
            </button>
          ) : (
            <button
              type="button"
              id="btn-send"
              className="inline-flex h-7 w-7 items-center justify-center rounded-full bg-accent text-xs text-white disabled:opacity-40"
              aria-label="Send"
              disabled={recording || transcribing || (!localDraft.trim() && !pendingFiles.length)}
              onClick={handleSend}
            >
              <Icon.ArrowUp size={20} />
            </button>
          )}
        </div>
        <div className="ka-composer-controls">
          <PermissionMenu sessionId={sessionId ?? null} ensureSession={ensureSession} />
          <ComposerModelMenu />
          <ComposerTools />
          {sending && !uploading && !preparing ? (
            <div className="ml-auto flex items-center gap-1.5">
              <button type="button" id="btn-queue" title="Send when this turn finishes (Enter)"
                className="rounded-md px-2 py-1 text-xs text-muted hover:bg-line/50 disabled:opacity-40"
                disabled={recording || transcribing || (!localDraft.trim() && !pendingFiles.length)}
                onClick={handleSend}>Queue</button>
              <button type="button" id="btn-steer" title="Apply after the current tool call finishes (Ctrl+Enter)"
                className="rounded-md bg-accent px-2 py-1 text-xs font-medium text-white disabled:opacity-40"
                disabled={recording || transcribing || (!localDraft.trim() && !pendingFiles.length)}
                onClick={() => submit(localDraft, { delivery: "steer" })}>Steer</button>
              <button type="button" id="btn-send-now" title="Stop the current turn and send this message now"
                className="rounded-md border border-line px-2 py-1 text-xs font-medium text-ink hover:bg-line/50 disabled:opacity-40"
                disabled={recording || transcribing || (!localDraft.trim() && !pendingFiles.length)}
                onClick={() => submit(localDraft, { delivery: "now" })}>Send now</button>
            </div>
          ) : null}
        </div>
      </div>

      <ImmersiveVoiceOverlay
        open={immersiveOpen}
        openerRef={enterImmersiveRef}
        voice={liveVoice}
        sessionId={liveVoice.activeSessionId ?? sessionId ?? null}
        onClose={() => setImmersiveOpen(false)}
      />
    </div>
  );
}
