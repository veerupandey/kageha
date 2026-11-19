import * as Dialog from "@radix-ui/react-dialog";
import { Icon } from "../lib/icons";
import { useAppStore } from "../store";
import { ComposerSearchMenu } from "./ComposerSearchMenu";
import { ComposerMediaMenu } from "./ComposerMediaMenu";
import { AgentModeSwitcher } from "./AgentModeSwitcher";
import { ExecutionTargetChip } from "./ExecutionTargetChip";
import { taskControlLabel } from "../lib/taskStatus";
import { stopSpokenReply, supportsLiveSpeechRecognition } from "../lib/voiceClient";
import { ModelRoleControls } from "./ModelRoleControls";

export function ComposerTools() {
  const voiceReply = useAppStore((s) => s.prefs.voiceReply);
  const voiceConversation = useAppStore((s) => s.prefs.voiceConversation);
  const setPrefs = useAppStore((s) => s.setPrefs);
  const control = useAppStore((s) => s.taskControl);
  const reason = useAppStore((s) => s.taskControlReason);
  const runStatus = useAppStore((s) => s.runStatus);
  const statusLabel = useAppStore((s) => s.statusLabel);
  const exceptional = !!control && !["continue", "advance", "stop_success"].includes(control);
  const taskLabel = exceptional ? taskControlLabel(control) : ({ running: "Working", success: "Completed", error: "Needs attention", waiting_approval: "Awaiting approval", cancelled: "Stopped", interrupted: "Interrupted", idle: "Ready" })[runStatus];
  const voiceMode = voiceConversation ? "conversation" : voiceReply ? "aloud" : "text";
  const modes = [
    { id: "text", label: "Text only", detail: "Keep replies on screen.", icon: Icon.Message },
    { id: "aloud", label: "Read replies aloud", detail: "Speak new replies using your selected voice.", icon: Icon.Volume },
    { id: "conversation", label: "Voice conversation", detail: "Send speech, hear the reply, then listen again.", icon: Icon.Mic },
  ];
  function selectVoice(mode: string) {
    setPrefs({ voiceReply: mode !== "text", voiceConversation: mode === "conversation" });
    if (mode === "text") stopSpokenReply();
  }
  return (
    <Dialog.Root>
      <Dialog.Trigger asChild>
        <button type="button" className="ka-tools-trigger"><Icon.Plus size={16} /> Tools</button>
      </Dialog.Trigger>
      <Dialog.Portal>
        <Dialog.Overlay className="ka-sheet-backdrop" />
        <Dialog.Content className="ka-tool-sheet ka-tools-refined">
          <div className="ka-sheet-heading">
            <div><Dialog.Title>Task preferences</Dialog.Title><Dialog.Description>Choose your tools and how you talk with Kageha.</Dialog.Description></div>
            <Dialog.Close className="ka-icon-btn" aria-label="Close tools"><Icon.Close size={20} /></Dialog.Close>
          </div>
          <div className="ka-tool-setting"><div><strong>Search</strong><p>Find information and sources on the web.</p></div><ComposerSearchMenu /></div>
          <div className="ka-tool-setting"><div><strong>Create media</strong><p>Choose your image and media tools.</p></div><ComposerMediaMenu /></div>
          <div className="ka-tool-setting"><div><strong>Working mode</strong><p>Choose the approach for this task.</p></div><AgentModeSwitcher compact /></div>
          <div className="ka-tool-setting"><div><strong>Runs on</strong><p>Choose this host, a fresh Linux box, or a remote SSH host.</p></div><ExecutionTargetChip compact /></div>
          <section className="ka-tools-status" aria-label="Current task status">
            <div className="flex items-center justify-between gap-3"><span className="flex items-center gap-2 text-xs font-medium text-ink"><Icon.Activity size={14} />Task status</span><span className="ka-tools-status-badge" data-attention={exceptional || runStatus === "error"}><span aria-hidden="true" />{taskLabel}</span></div>
            <p className="mt-2 text-xs leading-relaxed text-muted">{exceptional && reason ? reason : runStatus === "running" ? statusLabel || "Kageha is working on your request." : runStatus === "success" ? "The latest task has finished." : runStatus !== "idle" ? statusLabel || "Check the conversation for the next step." : "Recovery updates appear here when a task needs attention."}</p>
          </section>
          <section className="ka-tools-voice" aria-labelledby="tools-voice-heading">
            <div className="mb-3 flex items-center justify-between"><h3 id="tools-voice-heading" className="text-xs font-semibold text-ink">Voice & playback</h3><span className="text-[10px] text-faint">Saved for this browser</span></div>
            <div role="radiogroup" aria-label="Voice interaction mode" className="ka-voice-modes">
              {modes.map(mode => <button key={mode.id} type="button" role="radio" aria-label={mode.label} aria-checked={voiceMode === mode.id} tabIndex={voiceMode === mode.id ? 0 : -1} className="ka-voice-mode" onKeyDown={event => {
                  if (!["ArrowDown", "ArrowUp", "ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return;
                  event.preventDefault(); const index = modes.findIndex(item => item.id === mode.id);
                  const next = event.key === "Home" ? 0 : event.key === "End" ? 2 : (index + (["ArrowDown", "ArrowRight"].includes(event.key) ? 1 : 2)) % 3;
                  selectVoice(modes[next].id); (event.currentTarget.parentElement?.children[next] as HTMLElement)?.focus();
                }} onClick={() => selectVoice(mode.id)}>
                <mode.icon size={17} className="shrink-0" /><span className="min-w-0 flex-1"><strong>{mode.label}</strong><span>{mode.detail}</span></span><span className="ka-voice-radio" aria-hidden="true" />
              </button>)}
            </div>
            <p className="ka-voice-mode-note">{voiceMode === "conversation" ? supportsLiveSpeechRecognition() ? "Tap the composer microphone to begin. Pause after speaking to send. The mic reopens after playback ends." : "Tap the composer microphone to begin, then finish recording to send. The mic reopens after playback ends." : voiceMode === "aloud" ? "New replies are read aloud. Change your voice and speech provider in Settings → Media." : "Use the microphone anytime to dictate a message and review it before sending."}</p>
          </section>
          <details className="ka-advanced"><summary>Advanced model routing</summary><ModelRoleControls /></details>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
