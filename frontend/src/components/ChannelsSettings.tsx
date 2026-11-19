import { useEffect, useMemo, useState } from "react";
import { api } from "../api/client";
import { Icon } from "../lib/icons";
import { cn } from "../lib/cn";
import { integrationError } from "../lib/integrationError";

type ChannelField = {
  key: string;
  label: string;
  placeholder: string;
  secret: boolean;
  required: boolean;
  help_text: string;
};

type ChannelSummary = {
  id: string;
  name: string;
  tagline: string;
  fields: ChannelField[];
  supports_qr: boolean;
  supports_check: boolean;
  setup_hint: string;
  docs_url: string;
  status: Record<string, unknown>;
};

type CallRow = { call_ref: string; turns: number; preview: string };
type Turn = { role: string; text: string };
type QrState = { status: string; qr?: string; error?: string };

const button =
  "inline-flex items-center justify-center gap-2 rounded-lg border border-line px-3 py-2 text-xs font-medium text-ink transition-colors hover:bg-surface focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent disabled:cursor-not-allowed disabled:opacity-50";
const primary = button + " ka-github-primary min-h-10";
const input =
  "w-full rounded-lg border border-line bg-canvas p-2.5 text-sm text-ink outline-none focus:border-accent focus:ring-2 focus:ring-accent/15";

function truthy(row: Record<string, unknown>, key: string): boolean {
  return Boolean(row[key]);
}
function text(row: Record<string, unknown>, key: string): string {
  const v = row[key];
  return typeof v === "string" ? v : "";
}

function Pill({ ok, label }: { ok: boolean; label: string }) {
  return (
    <span
      className={cn(
        "inline-flex items-center gap-1.5 rounded-full px-2.5 py-1 text-[0.68rem] font-medium",
        ok ? "bg-emerald-500/10 text-emerald-500" : "bg-amber-500/10 text-amber-500",
      )}
    >
      <span className={cn("h-1.5 w-1.5 rounded-full", ok ? "bg-emerald-500" : "bg-amber-500")} />
      {label}
    </span>
  );
}

function channelIcon(id: string) {
  if (id === "phone") return Icon.Phone;
  return Icon.Message;
}

/** One channel card: status header + field-driven config form + actions.
 *
 * New channels (Slack, Teams, ...) render automatically here — nothing to
 * add to this component, they just need a ChannelDescriptor on the backend.
 */
function ChannelCard({
  summary,
  onChanged,
}: {
  summary: ChannelSummary;
  onChanged: (row: ChannelSummary) => void;
}) {
  const [values, setValues] = useState<Record<string, string>>({});
  const [allowAll, setAllowAll] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [message, setMessage] = useState("");
  const [qr, setQr] = useState<QrState | null>(null);

  const status = summary.status;
  const configured = truthy(status, "configured");
  const hasAllowAllField = "allow_all_users" in status;
  // Some channels report live state (a listener is running AND the session is
  // linked) via a "connected" field. When present, drive the badge off the
  // live QR poll first (freshest), then the saved "connected" flag, so the
  // header reflects reality rather than just saved config.
  const reportsLive = "connected" in status;
  const liveStatus = qr?.status || text(status, "live_status") || "not_started";
  const connected = reportsLive
    ? qr ? liveStatus === "ready" : truthy(status, "connected")
    : configured;
  const configuredNotLive = reportsLive && configured && !connected;
  const badgeLabel = connected ? "Connected"
    : !configured ? "Not connected"
    : liveStatus === "connecting" ? "Connecting"
    : liveStatus === "reconnecting" ? "Reconnecting"
    : liveStatus === "qr" ? "Scan QR code"
    : liveStatus === "error" ? "Connection error"
    : "Not running";
  const showStartHint = configuredNotLive && liveStatus === "not_started";

  useEffect(() => {
    setAllowAll(truthy(status, "allow_all_users"));
    const next: Record<string, string> = {};
    for (const field of summary.fields) {
      if (field.secret) continue;
      next[field.key] = text(status, field.key);
    }
    setValues(next);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [summary.id, JSON.stringify(status)]);

  // WhatsApp (and future QR-based channels) poll their QR endpoint while
  // not yet connected, so the code appears here without a terminal.
  useEffect(() => {
    if (!summary.supports_qr) return;
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout>;
    async function poll() {
      try {
        const row = await api<QrState>(`/api/channels/${summary.id}/qr`);
        if (!cancelled) setQr(row);
      } catch {
        /* transient; keep polling */
      }
      if (!cancelled) timer = setTimeout(poll, 3000);
    }
    timer = setTimeout(poll, 300);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [summary.id, summary.supports_qr]);

  async function run(action: () => Promise<void>) {
    setBusy(true);
    setError("");
    setMessage("");
    try {
      await action();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  function save() {
    void run(async () => {
      const payload: Record<string, unknown> = { ...values };
      if (hasAllowAllField) payload.allow_all_users = allowAll;
      const row = await api<Record<string, unknown>>(`/api/channels/${summary.id}`, {
        method: "PUT",
        body: JSON.stringify(payload),
      });
      onChanged({ ...summary, status: row });
      setValues((prev) => {
        const next = { ...prev };
        for (const field of summary.fields) if (field.secret) next[field.key] = "";
        return next;
      });
      setMessage("Saved.");
    });
  }

  function disconnect() {
    void run(async () => {
      const row = await api<Record<string, unknown>>(`/api/channels/${summary.id}`, {
        method: "DELETE",
      });
      onChanged({ ...summary, status: row });
      setMessage("Disconnected.");
    });
  }

  function check() {
    void run(async () => {
      const r = await api<{ ok: boolean; detail: string }>(`/api/channels/${summary.id}/check`, {
        method: "POST",
      });
      if (r.ok) setMessage(r.detail);
      else setError(r.detail);
    });
  }

  const missing = summary.fields.filter(
    (f) => f.required && !f.secret && !values[f.key]?.trim() && !(hasAllowAllField && allowAll),
  );
  const needsSecretFirstTime =
    summary.fields.some((f) => f.secret) &&
    !configured &&
    !summary.fields.some((f) => f.secret && values[f.key]?.trim());
  const canSave = missing.length === 0 && !needsSecretFirstTime;

  const Icn = channelIcon(summary.id);

  return (
    <section className="space-y-3 rounded-2xl border border-line bg-canvas/50 p-4 sm:p-5">
      <div className="flex items-start gap-3">
        <span className="flex h-10 w-10 shrink-0 items-center justify-center rounded-xl border border-line bg-surface text-ink">
          <Icn size={20} />
        </span>
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <h3 className="text-sm font-semibold text-ink">{summary.name}</h3>
            <Pill ok={connected} label={badgeLabel} />
          </div>
          <p className="mt-0.5 text-xs text-muted">{summary.tagline}</p>
        </div>
      </div>

      {!configured && summary.setup_hint && (
        <p className="rounded-xl border border-line bg-surface/60 px-3 py-2 text-xs leading-relaxed text-muted">
          {summary.setup_hint}{" "}
          {summary.docs_url && (
            <a className="underline" href={summary.docs_url} target="_blank" rel="noreferrer">
              Learn more
            </a>
          )}
        </p>
      )}
      {showStartHint && (
        <p className="rounded-xl border border-amber-400/20 bg-amber-400/5 px-3 py-2 text-xs text-amber-500">
          Saved, but the listener isn&apos;t running yet. Click{" "}
          <span className="font-medium">Save changes</span> to start it — a QR code
          appears here if the account needs re-linking.
        </p>
      )}
      {"dependencies_ready" in status && !truthy(status, "dependencies_ready") && (
        <p className="rounded-xl border border-amber-400/20 bg-amber-400/5 px-3 py-2 text-xs text-amber-500">
          Server dependencies are missing. Run{" "}
          <code className="text-ink">cd integrations/whatsapp-qr &amp;&amp; npm install</code> on
          the server, then reload this page.
        </p>
      )}

      {error && (
        <p role="alert" className="rounded-xl border border-red-400/20 bg-red-400/5 px-3 py-2 text-xs text-red-400">
          {error}
        </p>
      )}
      {message && (
        <p role="status" className="rounded-xl border border-line bg-accent-soft px-3 py-2 text-xs text-ink">
          {message}
        </p>
      )}

      <div className="space-y-3">
        {summary.fields.map((field) => (
          <div key={field.key} className="space-y-1">
            <label className="block text-xs font-medium text-muted">{field.label}</label>
            <input
              className={input}
              type={field.secret ? "password" : "text"}
              autoComplete={field.secret ? "new-password" : "off"}
              spellCheck={false}
              value={values[field.key] || ""}
              disabled={field.key === "allowed_users" && allowAll}
              onChange={(e) => setValues((prev) => ({ ...prev, [field.key]: e.target.value }))}
              placeholder={
                field.secret && configured
                  ? "•••••• saved — leave blank to keep"
                  : field.placeholder
              }
            />
            {field.help_text && <p className="text-[0.68rem] text-faint">{field.help_text}</p>}
          </div>
        ))}
        {hasAllowAllField && (
          <label className="flex items-center gap-2 text-xs text-muted">
            <input type="checkbox" checked={allowAll} onChange={(e) => setAllowAll(e.target.checked)} />
            Allow everyone (testing only — skips the allowlist)
          </label>
        )}
      </div>

      {summary.supports_qr && qr?.status === "qr" && qr.qr && (
        <div className="flex flex-col items-center gap-2 rounded-xl border border-line bg-surface/60 p-4">
          <p className="text-xs text-muted">
            Scan with WhatsApp → Settings → Linked devices → Link a device
          </p>
          {/* Rendered by the Kageha server, never sent to a third party — the
              QR payload is a WhatsApp session-linking secret. */}
          <img
            className="h-48 w-48 rounded-lg border border-line bg-white p-2"
            alt="WhatsApp QR code"
            src={`/api/channels/${summary.id}/qr.svg?v=${encodeURIComponent(qr.qr.slice(-12))}`}
          />
        </div>
      )}
      {summary.supports_qr && qr?.status === "connecting" && (
        <p className="flex items-center gap-2 text-xs text-muted">
          <Icon.Loader size={12} className="animate-spin" /> Starting the WhatsApp listener…
        </p>
      )}
      {summary.supports_qr && qr?.status === "reconnecting" && (
        <p className="flex items-center gap-2 text-xs text-muted">
          <Icon.Loader size={12} className="animate-spin" /> Reconnecting the WhatsApp listener…
        </p>
      )}
      {summary.supports_qr && qr?.status === "ready" && (
        <p className="text-xs text-emerald-500">WhatsApp is linked and connected.</p>
      )}
      {summary.supports_qr && qr?.status === "error" && qr.error && (
        <p className="text-xs text-amber-500">WhatsApp bridge error: {qr.error}</p>
      )}

      <div className="flex flex-wrap items-center gap-2 pt-1">
        <button className={primary} disabled={busy || !canSave} onClick={save}>
          {busy ? <Icon.Loader size={14} className="animate-spin" /> : null}
          {configured ? "Save changes" : "Save & connect"}
        </button>
        {configured && summary.supports_check && (
          <button className={button} disabled={busy} onClick={check}>
            <Icon.Check size={14} /> Test connection
          </button>
        )}
        {configured && (
          <button className={button} disabled={busy} onClick={disconnect}>
            Disconnect
          </button>
        )}
        {!canSave && missing.length > 0 && (
          <span className="text-[0.68rem] text-faint">
            Enter {missing.map((f) => f.label).join(", ")} to save.
          </span>
        )}
      </div>
    </section>
  );
}

function PlaceCallForm({ fromNumber }: { fromNumber: string }) {
  const [to, setTo] = useState("");
  const [greeting, setGreeting] = useState("");
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState("");
  const [error, setError] = useState("");

  async function placeCall() {
    const target = to.trim();
    if (!target) {
      setError("Enter a number to call.");
      return;
    }
    const confirmed = window.confirm(
      `Call ${target}${fromNumber ? ` from ${fromNumber}` : ""}? The agent will pick up the conversation once answered.`,
    );
    if (!confirmed) return;
    setBusy(true);
    setError("");
    setNotice("");
    try {
      const res = await api<{ ok: boolean; call_sid: string; to: string; detail: string }>(
        "/api/channels/phone/calls",
        { method: "POST", body: JSON.stringify({ to: target, greeting: greeting.trim() }) },
      );
      setNotice(res.detail || `Calling ${res.to}…`);
    } catch (e) {
      setError(integrationError(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="space-y-3 rounded-2xl border border-line bg-canvas/40 p-4 sm:p-5">
      <div className="flex items-center gap-2">
        <Icon.Phone size={16} className="text-muted" />
        <h3 className="text-sm font-semibold text-ink">Place a call</h3>
      </div>
      <p className="text-xs text-muted">
        Dial a number and your agent picks up and talks once the call connects.
      </p>
      <input
        className={input}
        placeholder="+12369780550"
        value={to}
        onChange={(e) => setTo(e.target.value)}
        disabled={busy}
      />
      <input
        className={input}
        placeholder="Optional opening line the agent says first"
        value={greeting}
        onChange={(e) => setGreeting(e.target.value)}
        disabled={busy}
      />
      <div className="flex items-center gap-3">
        <button className={primary} disabled={busy} onClick={() => void placeCall()}>
          {busy ? <Icon.Loader size={14} className="animate-spin" /> : <Icon.Phone size={14} />}
          {busy ? "Calling…" : "Call"}
        </button>
        {notice && <span className="text-[0.68rem] text-emerald-500">{notice}</span>}
        {error && <span className="text-[0.68rem] text-amber-500">{error}</span>}
      </div>
    </section>
  );
}

type VoiceSelection = { tts_provider: string; voice: string };
type VoiceRow = { name: string; gender: string };
type VoiceOptions = {
  providers: string[];
  gemini_voices: string[];
  gemini_voice_table?: VoiceRow[];
};

function PhoneVoiceSettings({
  selection,
  options,
  onSaved,
}: {
  selection: VoiceSelection;
  options: VoiceOptions;
  onSaved: (status: Record<string, unknown>) => void;
}) {
  const [provider, setProvider] = useState(selection.tts_provider || "auto");
  const [voice, setVoice] = useState(selection.voice || "");
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState("");
  const [error, setError] = useState("");

  // Gemini exposes named voices; other providers use their own default, so the
  // voice picker only applies when Gemini is the chosen (or auto-resolved) TTS.
  const showVoicePicker =
    provider === "gemini" || (provider === "auto" && options.providers.includes("gemini"));

  async function save() {
    setBusy(true);
    setError("");
    setNotice("");
    try {
      const row = await api<Record<string, unknown>>("/api/channels/phone", {
        method: "PUT",
        body: JSON.stringify({ tts_provider: provider, voice }),
      });
      onSaved(row);
      setNotice("Voice saved. It applies to your next call.");
    } catch (e) {
      setError(integrationError(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="space-y-3 rounded-2xl border border-line bg-canvas/40 p-4 sm:p-5">
      <div className="flex items-center gap-2">
        <Icon.Phone size={16} className="text-muted" />
        <h3 className="text-sm font-semibold text-ink">Voice</h3>
      </div>
      <p className="text-xs text-muted">Choose how your agent sounds on calls.</p>
      <div className="space-y-1">
        <label className="block text-xs font-medium text-muted">Speech engine</label>
        <select
          className={input}
          value={provider}
          onChange={(e) => setProvider(e.target.value)}
          disabled={busy}
        >
          <option value="auto">Automatic (first ready)</option>
          {options.providers.map((p) => (
            <option key={p} value={p}>
              {p}
            </option>
          ))}
        </select>
      </div>
      {showVoicePicker && (
        <div className="space-y-1">
          <label className="block text-xs font-medium text-muted">Voice</label>
          <select
            className={input}
            value={voice}
            onChange={(e) => setVoice(e.target.value)}
            disabled={busy}
          >
            <option value="">Default</option>
            {(options.gemini_voice_table && options.gemini_voice_table.length > 0
              ? options.gemini_voice_table
              : options.gemini_voices.map((n) => ({ name: n, gender: "" }))
            ).map((v) => (
              <option key={v.name} value={v.name}>
                {v.gender ? `${v.name} (${v.gender})` : v.name}
              </option>
            ))}
          </select>
          <p className="text-[0.68rem] text-faint">
            You can also just tell the agent “use a male/female voice”.
          </p>
        </div>
      )}
      <div className="flex items-center gap-3">
        <button className={button} disabled={busy} onClick={() => void save()}>
          {busy ? <Icon.Loader size={14} className="animate-spin" /> : null}
          Save voice
        </button>
        {notice && <span className="text-[0.68rem] text-emerald-500">{notice}</span>}
        {error && <span className="text-[0.68rem] text-amber-500">{error}</span>}
      </div>
    </section>
  );
}

function PhoneCallsPanel({ fromNumber }: { fromNumber: string }) {
  const [calls, setCalls] = useState<CallRow[]>([]);
  const [activeRef, setActiveRef] = useState("");
  const [transcript, setTranscript] = useState<Turn[]>([]);

  useEffect(() => {
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout>;
    async function poll() {
      try {
        const data = await api<{ calls: CallRow[] }>("/api/channels/phone/calls");
        if (cancelled) return;
        setCalls(data.calls);
        if (activeRef) {
          const t = await api<{ transcript: Turn[] }>(
            "/api/channels/phone/transcript?call_ref=" + encodeURIComponent(activeRef),
          );
          if (!cancelled) setTranscript(t.transcript);
        }
      } catch {
        /* transient; keep polling */
      }
      if (!cancelled) timer = setTimeout(poll, 3000);
    }
    timer = setTimeout(poll, 500);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [activeRef]);

  return (
    <>
    <PlaceCallForm fromNumber={fromNumber} />
    <section className="space-y-3 rounded-2xl border border-line bg-canvas/40 p-4 sm:p-5">
      <div className="flex items-center gap-2">
        <Icon.Message size={16} className="text-muted" />
        <h3 className="text-sm font-semibold text-ink">Live calls</h3>
      </div>
      {calls.length === 0 ? (
        <p className="text-xs text-muted">No calls yet. When a call comes in or goes out, its live transcript appears here.</p>
      ) : (
        <div className="flex flex-wrap gap-2">
          {calls.map((c) => (
            <button
              key={c.call_ref}
              className={cn(button, activeRef === c.call_ref && "border-accent bg-accent-soft")}
              onClick={() => {
                setActiveRef(c.call_ref);
                setTranscript([]);
              }}
            >
              <span className="font-mono">{c.call_ref.slice(0, 8)}</span> · {c.turns} turns
            </button>
          ))}
        </div>
      )}
      {activeRef && (
        <div className="max-h-64 space-y-2 overflow-y-auto rounded-xl border border-line bg-surface/50 p-3">
          {transcript.length === 0 ? (
            <p className="flex items-center gap-2 text-xs text-muted">
              <Icon.Loader size={12} className="animate-spin" /> Waiting for the conversation…
            </p>
          ) : (
            transcript.map((t, i) => (
              <div key={i} className="text-sm leading-relaxed">
                <span className={cn("font-medium", t.role === "caller" ? "text-accent" : "text-muted")}>
                  {t.role === "caller" ? "Caller" : "Agent"}
                </span>
                <span className="text-ink"> · {t.text}</span>
              </div>
            ))
          )}
        </div>
      )}
    </section>
    </>
  );
}

export function ChannelsSettings() {
  const [channels, setChannels] = useState<ChannelSummary[] | null>(null);
  const [error, setError] = useState("");
  const [reload, setReload] = useState(0);

  useEffect(() => {
    let cancelled = false;
    setError("");
    void api<{ channels: ChannelSummary[] }>("/api/channels")
      .then((data) => {
        if (!cancelled) setChannels(data.channels);
      })
      .catch((e) => {
        if (!cancelled) setError(integrationError(e));
      });
    return () => {
      cancelled = true;
    };
  }, [reload]);

  const anyConfigured = useMemo(
    () => (channels || []).some((c) => truthy(c.status, "configured")),
    [channels],
  );

  function replace(row: ChannelSummary) {
    setChannels((prev) => (prev || []).map((c) => (c.id === row.id ? row : c)));
  }

  return (
    <div className="space-y-4">
      <p className="text-xs leading-relaxed text-muted">
        Connect a messaging app and your agent answers from the same threads, memory, and
        approvals as this Web UI — a message from Telegram or WhatsApp continues right where
        you left off here.
      </p>

      {error && (
        <p role="alert" className="rounded-xl border border-red-400/20 bg-red-400/5 px-4 py-3 text-sm text-red-400">
          {error}
          <button className={button + " ml-3"} onClick={() => setReload((v) => v + 1)}>
            Retry
          </button>
        </p>
      )}
      {!channels && !error && (
        <p className="flex items-center gap-2 text-sm text-muted">
          <Icon.Loader size={14} className="animate-spin" /> Loading channels…
        </p>
      )}

      {channels?.map((summary) => (
        <div key={summary.id} className="space-y-3">
          <ChannelCard summary={summary} onChanged={replace} />
          {summary.id === "phone" && truthy(summary.status, "configured") && (
            <>
              <PhoneVoiceSettings
                selection={
                  (summary.status.voice_selection as VoiceSelection) || {
                    tts_provider: "auto",
                    voice: "",
                  }
                }
                options={
                  (summary.status.voice_options as VoiceOptions) || {
                    providers: [],
                    gemini_voices: [],
                  }
                }
                onSaved={(row) => replace({ ...summary, status: row })}
              />
              <PhoneCallsPanel fromNumber={text(summary.status, "from_number")} />
            </>
          )}
        </div>
      ))}

      {channels && !anyConfigured && (
        <p className="text-center text-[0.68rem] text-faint">
          Nothing connected yet — pick a channel above to get started.
        </p>
      )}
    </div>
  );
}
