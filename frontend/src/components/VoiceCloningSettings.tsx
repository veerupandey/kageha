import { useEffect, useState } from "react";
import { VoiceReferenceRecorder } from "./VoiceReferenceRecorder";
import { Icon } from "../lib/icons";
import { VoiceAudioPlayer } from "./VoiceAudioPlayer";
import "./voiceStudio.css";
import { api } from "../api/client";
import { useAppStore } from "../store";

type Profile = { id: string; name: string; transcript: string; duration_seconds: number };
type LocalSettings = { enabled: boolean; installed: boolean; device: string; model: string };

export function VoiceCloningSettings({ onRefresh, canClone }: { onRefresh: () => void; canClone: boolean }) {
  const provider = useAppStore(s => s.prefs.tts);
  const voice = useAppStore(s => s.prefs.ttsVoice || "");
  const setPrefs = useAppStore(s => s.setPrefs);
  const [local, setLocal] = useState<LocalSettings | null>(null);
  const [profiles, setProfiles] = useState<Profile[]>([]);
  const [name, setName] = useState("");
  const [transcript, setTranscript] = useState("");
  const [file, setFile] = useState<File | null>(null);
  const [tab, setTab] = useState<"library" | "create">("library");
  const [notice, setNotice] = useState("");
  const [mode, setMode] = useState<"record" | "upload">("record");
  const [captureBusy, setCaptureBusy] = useState(false);
  const [recordingVersion, setRecordingVersion] = useState(0);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [preview, setPreview] = useState("");
  const field = "voice-field";
  const button = "voice-secondary";

  async function refresh() {
    const [settings, voices] = await Promise.all([
      api<LocalSettings>("/api/audio8-settings"),
      api<{ profiles: Profile[] }>("/api/voice-profiles"),
    ]);
    setLocal(settings); setProfiles(voices.profiles);
  }
  useEffect(() => { refresh().catch(err => setError(String(err))); }, []);
  useEffect(() => () => { if (preview) URL.revokeObjectURL(preview); }, [preview]);

  async function perform(action: () => Promise<void>) {
    setBusy(true); setError("");
    try { await action(); } catch (err) { setError(err instanceof Error ? err.message : String(err)); }
    finally { setBusy(false); }
  }
  async function saveLocal(enabled: boolean, device: string) {
    await perform(async () => {
      setLocal(await api<LocalSettings>("/api/audio8-settings", { method: "POST", body: JSON.stringify({ enabled, device }) }));
      onRefresh();
    });
  }
  async function addVoice() {
    await perform(async () => {
      if (!file) throw new Error("Choose a reference WAV recording.");
      if (file.size > 12 * 1024 * 1024) throw new Error("Reference WAV must be under 12 MB.");
      const audio_base64 = await new Promise<string>((resolve, reject) => {
        const reader = new FileReader();
        reader.onload = () => resolve(String(reader.result).split(",")[1]);
        reader.onerror = () => reject(new Error("Could not read recording."));
        reader.readAsDataURL(file);
      });
      const res = await api<{ profile: Profile }>("/api/voice-profiles", {
        method: "POST", body: JSON.stringify({ name, transcript, audio_base64 }),
      });
      setPrefs({ ttsVoice: res.profile.id }); setName(""); setTranscript(""); setFile(null); setRecordingVersion(v => v + 1);
      await refresh(); setTab("library"); setNotice(`“${name.trim()}” is ready to use.`); setPreview("");
    });
  }
  async function generatePreview() {
    await perform(async () => {
      const res = await fetch("/api/voice-preview", {
        method: "POST", headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ provider, voice }),
      });
      if (!res.ok) {
        const data = await res.json().catch(() => ({}));
        throw new Error(data.error || `Voice preview failed (${res.status})`);
      }
      setPreview(URL.createObjectURL(await res.blob()));
    });
  }

  const selectedProfile = profiles.find(p => p.id === voice);
  const locked = busy || captureBusy;
  function changeTab(next: "library" | "create") { if (!locked) { setTab(next); setError(""); setNotice(""); } }

  return <section className="voice-studio" aria-label="Voice studio">
    <header className="voice-studio-head">
      <div>
        <div className="voice-eyebrow"><Icon.LiveVoice size={13} /> Voice studio</div>
        <h3 className="mt-3 text-xl font-semibold tracking-tight text-ink">A voice that’s yours.</h3>
        <p className="mt-2 max-w-md text-xs leading-relaxed text-muted">Create a personal voice for narration and spoken replies. Record a short passage, then make it part of Kageha.</p>
      </div>
      <div className="voice-studio-mark"><Icon.LiveVoice size={25} /></div>
    </header>
    <div className="voice-studio-tabs" role="tablist" aria-label="Voice studio views" onKeyDown={event => {
      if (locked || !["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return;
      event.preventDefault();
      const next = event.key === "Home" ? "library" : event.key === "End" ? "create" : tab === "library" ? "create" : "library";
      changeTab(next); document.getElementById(next === "library" ? "voice-library-tab" : "voice-create-tab")?.focus();
    }}>
      <button type="button" id="voice-library-tab" role="tab" tabIndex={tab === "library" ? 0 : -1} aria-selected={tab === "library"} aria-controls="voice-library-panel" disabled={locked} onClick={() => changeTab("library")}>My voices <span className="ml-1.5 text-faint">{profiles.length}</span></button>
      <button type="button" id="voice-create-tab" role="tab" tabIndex={tab === "create" ? 0 : -1} aria-selected={tab === "create"} aria-controls="voice-create-panel" disabled={locked} onClick={() => changeTab("create")}>Create a voice</button>
    </div>

    {tab === "library" ? <div id="voice-library-panel" role="tabpanel" aria-labelledby="voice-library-tab" className="voice-studio-body space-y-5">
      {notice && <p role="status" className="flex items-center gap-2 text-xs text-accent"><Icon.Check size={15} />{notice}</p>}
      {profiles.length > 0 ? <>
        <div className="flex items-center gap-3">
          <div className="voice-studio-mark" style={{ width: 42, height: 42 }}><Icon.Mic size={19} /></div>
          <div className="min-w-0 flex-1"><p className="truncate text-sm font-medium text-ink">{selectedProfile?.name || "Choose your voice"}</p><p className="mt-1 text-[11px] text-muted">{selectedProfile ? `${selectedProfile.duration_seconds.toFixed(1)}s reference · Personal voice` : "Use a saved reference for your next creation"}</p></div>
          {selectedProfile && <span className="rounded-full border border-line px-2 py-1 text-[10px] text-muted">Selected</span>}
        </div>
        <label className="block"><span className="voice-label">Saved reference voice</span>
          <select aria-label="Saved reference voice" className={field} value={voice} disabled={locked}
            onChange={e => { setPrefs({ ttsVoice: e.target.value }); setPreview(""); setNotice(""); }}>
            <option value="">No reference voice</option>
            {profiles.map(p => <option key={p.id} value={p.id}>{p.name} · {p.duration_seconds.toFixed(1)}s</option>)}
          </select>
        </label>
        {voice && !selectedProfile && <p className="text-xs text-muted">This voice was deleted. Choose another saved voice.</p>}
        <div className="flex flex-wrap gap-2">
          <button className="voice-primary" disabled={locked || !canClone} onClick={generatePreview}><Icon.Play size={14} />Preview selected voice</button>
          <button className={button} onClick={() => changeTab("create")} disabled={locked}><Icon.Plus size={14} />Add a voice</button>
          <button className={`${button} ml-auto`} disabled={locked || !voice} onClick={() => perform(async () => {
            await api(`/api/voice-profiles/${encodeURIComponent(voice)}`, { method: "DELETE" });
            setPrefs({ ttsVoice: "" }); setPreview(""); await refresh();
          })}><Icon.Delete size={14} />Delete voice</button>
        </div>
        {preview && <VoiceAudioPlayer key={preview} src={preview} label="Voice preview" title="Generated preview" />}
      </> : <div className="flex flex-col items-center rounded-xl border border-dashed border-line px-5 py-8 text-center">
        <div className="voice-studio-mark"><Icon.Mic size={24} /></div>
        <p className="mt-4 text-sm font-medium text-ink">Your voice library starts here</p>
        <p className="mt-2 max-w-xs text-xs leading-relaxed text-muted">A short recording is all you need. Read a passage, listen back, and save your personal voice.</p>
        <button className="voice-primary mt-5" onClick={() => changeTab("create")}><Icon.Plus size={14} />Create your first voice</button>
      </div>}
      {!canClone && <p className="flex items-start gap-2 text-[11px] leading-relaxed text-muted"><Icon.Alert size={14} className="mt-0.5 shrink-0" />Select an available cloning provider above to generate a preview.</p>}
    </div> : <div id="voice-create-panel" role="tabpanel" aria-labelledby="voice-create-tab" className="voice-studio-body space-y-5">
      <div className="flex flex-wrap items-center gap-x-5 gap-y-2" aria-label="Voice creation progress">
        <div className="voice-step" data-current={!file}><span>1</span><span>Record your voice</span></div>
        <span aria-hidden="true" className="text-faint">/</span>
        <div className="voice-step" data-current={!!file}><span>2</span><span>Review & save</span></div>
      </div>
      <div className="voice-source" role="group" aria-label="Reference voice source">
        <button aria-pressed={mode === "record"} disabled={locked} onClick={() => { if (mode !== "record") { setMode("record"); setFile(null); setTranscript(""); } }}><Icon.Mic size={14} />Record my voice</button>
        <button aria-pressed={mode === "upload"} disabled={locked} onClick={() => { if (mode !== "upload") { setMode("upload"); setFile(null); setTranscript(""); } }}><Icon.Upload size={14} />Upload a recording</button>
      </div>
      <label className="block"><span className="voice-label">Voice name</span>
        <input aria-label="Voice name" placeholder="e.g. My narration voice" maxLength={100} value={name} disabled={locked} onChange={e => setName(e.target.value)} className={field} />
      </label>
      {mode === "record" ? <VoiceReferenceRecorder key={recordingVersion} disabled={busy} onBusy={setCaptureBusy}
        onReady={(recording, text) => { setFile(recording); setTranscript(text); }} /> : <div className="space-y-3 rounded-xl border border-dashed border-line bg-canvas p-5">
        <Icon.Upload size={23} className="text-muted" /><p className="text-sm font-medium text-ink">Bring an existing recording</p>
        <p className="text-xs leading-relaxed text-muted">Choose a clean 1–30 second WAV of a single speaker. Use a recording you have permission to clone.</p>
        <input aria-label="Reference recording" type="file" accept=".wav,audio/wav" disabled={busy}
          onChange={e => setFile(e.target.files?.[0] || null)} className="block w-full text-xs text-muted file:mr-3 file:rounded-lg file:border file:border-line file:bg-surface file:px-3 file:py-2 file:text-xs file:text-ink" />
        <p className="text-[10px] text-faint">WAV · 16-bit PCM · Up to 12 MB</p>
      </div>}
      {(file || mode === "upload") && <label className="block"><span className="voice-label">Reference transcript <span className="ml-1 font-normal text-muted">· Check every word</span></span>
        <textarea disabled={locked} aria-label="Reference transcript" placeholder="Enter exactly what was spoken in the recording…" rows={3} maxLength={2000} value={transcript} onChange={e => setTranscript(e.target.value)} className={`${field} leading-relaxed`} />
      </label>}
      <div className="flex flex-wrap items-center justify-between gap-3 border-t border-line pt-4">
        <p className="flex items-center gap-1.5 text-[10px] text-muted"><Icon.Lock size={12} />Reference saved on this computer</p>
        <button className="voice-primary" disabled={locked || !file || !name.trim() || !transcript.trim()} onClick={addVoice}><Icon.Check size={14} />Save voice</button>
      </div>
    </div>}

    {busy && <p role="status" className="mx-6 mb-5 flex items-center gap-2 text-xs text-muted"><Icon.Loader size={14} className="animate-spin motion-reduce:animate-none" />Working… Local speech generation can take a few minutes.</p>}
    {error && <p role="alert" className="mx-6 mb-5 rounded-lg border border-red-400/20 bg-red-400/5 px-3 py-2 text-xs text-red-400">{error}</p>}
    {local && <footer className="voice-runtime space-y-3">
      <div className="flex items-center justify-between gap-4">
        <div className="flex items-center gap-2.5"><Icon.Desktop size={16} className="text-muted" /><div><p className="text-xs font-medium text-ink">Local voice engine</p><p className="mt-0.5 text-[10px] text-muted">Audio8 Realistic · {local.enabled ? "Enabled" : "Off"} · Optional</p></div></div>
        <input className="voice-toggle" type="checkbox" aria-label="Enable local Audio8 Realistic 0.6B" checked={local.enabled} disabled={locked || !local.installed} onChange={e => saveLocal(e.target.checked, local.device)} />
      </div>
      <details className="group">
        <summary className="flex cursor-pointer list-none items-center gap-1 text-[10px] text-muted">Engine settings<Icon.Chevron size={12} className="transition-transform group-open:rotate-180" /></summary>
        <div className="mt-3 space-y-3">
          <p className="text-[11px] leading-relaxed text-muted">Runs when Audio8 is selected as your speech provider. Auto never selects it. The first preview loads the model; live calls use their own voices.</p>
          {!local.installed && <p className="text-xs text-muted">Install the optional runtime using <code>scripts/setup_audio8.sh</code>, then reopen Settings.</p>}
          <label className="block"><span className="voice-label">Compute device</span>
            <select aria-label="Audio8 compute device" className={field} value={local.device} disabled={locked} onChange={e => saveLocal(local.enabled, e.target.value)}>
              <option value="cpu">CPU · compatible default</option><option value="mps">Apple GPU · experimental</option><option value="cuda">NVIDIA GPU</option>
            </select>
          </label>
        </div>
      </details>
    </footer>}
  </section>;
}
