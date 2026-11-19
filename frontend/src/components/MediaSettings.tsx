import { useEffect, useState } from "react";
import { cn } from "../lib/cn";
import { Icon } from "../lib/icons";
import { MEDIA_MODEL_OPTIONS, type MediaModelId } from "../lib/mediaModel";
import { api } from "../api/client";
import { useAppStore } from "../store";
import { VoiceCloningSettings } from "./VoiceCloningSettings";

type ProviderRow = { id: string; protocol: string; ready: boolean; voice_cloning?: boolean; default_model: string; models: string[] };
type MediaProviders = { image: ProviderRow[]; tts: ProviderRow[]; stt: ProviderRow[] };

const PROVIDER_LABELS: Record<string, string> = {
  cloudflare: "Cloudflare Workers AI",
  gemini: "Gemini",
  "gemini-stt": "Gemini",
  openai: "OpenAI",
  fal: "Fal",
  audio8: "Audio8 · local voice cloning",
};

function providerLabel(id: string): string {
  return PROVIDER_LABELS[id] || id;
}

export function MediaSettings() {
  const selected = useAppStore((state) => state.prefs.mediaModel || "auto");
  const tts = useAppStore((state) => state.prefs.tts || "auto");
  const stt = useAppStore((state) => state.prefs.stt || "auto");
  const setPrefs = useAppStore((state) => state.setPrefs);
  const [providers, setProviders] = useState<MediaProviders | null>(null);

  const [revision, setRevision] = useState(0);

  useEffect(() => {
    let alive = true;
    (async () => {
      try {
        const res = (await api("/api/media-providers")) as { ok?: boolean; providers?: MediaProviders };
        if (alive && res?.ok && res.providers) setProviders(res.providers);
      } catch {
        /* availability is best-effort; selectors still work */
      }
    })();
    return () => {
      alive = false;
    };
  }, [revision]);

  return (
    <div className="space-y-5">
      <section className="rounded-2xl border border-line bg-canvas/35 p-4">
        <h3 className="text-sm font-semibold text-ink">Default image provider</h3>
        <p className="mt-1 text-[0.7rem] leading-relaxed text-muted">
          This is the default for new chat turns. You can override it from the compact Media chip
          beside the composer; the chosen provider is saved with each session turn.
        </p>
        <div className="mt-3 space-y-2" role="radiogroup" aria-label="Default image provider">
          {MEDIA_MODEL_OPTIONS.map((option) => {
            const active = option.id === selected;
            return (
              <button
                key={option.id}
                type="button"
                role="radio"
                aria-checked={active}
                disabled={!option.available}
                title={option.available ? option.detail : `${option.detail} — not connected yet`}
                className={cn(
                  "flex w-full items-center gap-3 rounded-xl border px-3 py-2.5 text-left transition-colors",
                  active
                    ? "border-accent bg-accent-soft text-ink"
                    : "border-line bg-surface/60 text-muted hover:border-line-strong hover:text-ink",
                  !option.available && "cursor-not-allowed opacity-45",
                )}
                onClick={() => {
                  if (option.available) setPrefs({ mediaModel: option.id as MediaModelId });
                }}
              >
                <Icon.Logo size={15} className={cn("shrink-0", active ? "text-accent" : "text-muted")} />
                <span className="min-w-0 flex-1">
                  <span className="block text-sm font-medium">{option.label}</span>
                  <span className="mt-0.5 block text-[0.68rem] text-faint">{option.detail}</span>
                </span>
                <span className="hidden shrink-0 text-[0.58rem] uppercase tracking-wide text-faint sm:block">
                  {option.capabilities.slice(0, 2).join(" · ")}
                </span>
                {active ? <Icon.Check size={15} className="shrink-0 text-accent" /> : null}
              </button>
            );
          })}
        </div>
      </section>

      <VoiceProviderSection
        title="Text-to-speech provider"
        description="Used by the text_to_speech tool for voiceovers and narrated audio. Auto picks the first configured provider."
        ariaLabel="Text-to-speech provider"
        selected={tts}
        rows={providers?.tts ?? null}
        onSelect={(id) => setPrefs({ tts: id })}
      />

      <VoiceCloningSettings onRefresh={() => setRevision(r => r + 1)}
        canClone={!!providers?.tts.some(p => p.id === tts && p.ready && p.voice_cloning)} />

      <VoiceProviderSection
        title="Speech-to-text provider"
        description="Used to transcribe voice messages and audio inputs. Auto picks the first configured provider."
        ariaLabel="Speech-to-text provider"
        selected={stt}
        rows={providers?.stt ?? null}
        onSelect={(id) => setPrefs({ stt: id as never })}
      />

      <section className="rounded-2xl border border-line/80 bg-canvas/20 px-4 py-3">
        <p className="text-[0.7rem] leading-relaxed text-muted">
          <span className="font-medium text-ink">Connections</span> — Nano Banana uses your
          Gemini connection, Fal uses FAL_KEY, Antigravity uses the signed-in <code>agy</code>
          CLI, and Cloudflare uses CLOUDFLARE_API_TOKEN + CLOUDFLARE_ACCOUNT_ID. OpenAI images
          reuse your OpenAI connection; no separate key is needed.
        </p>
      </section>
    </div>
  );
}

function VoiceProviderSection({
  title,
  description,
  ariaLabel,
  selected,
  rows,
  onSelect,
}: {
  title: string;
  description: string;
  ariaLabel: string;
  selected: string;
  rows: ProviderRow[] | null;
  onSelect: (id: string) => void;
}) {
  // "Auto" is always available; concrete providers come from the backend
  // readiness probe. Until it resolves, only Auto renders.
  const options: { id: string; label: string; detail: string; models: string[]; ready: boolean }[] = [
    { id: "auto", label: "Auto", detail: "First configured provider", models: [], ready: true },
    ...(rows || []).map((r) => ({
      id: r.id,
      label: providerLabel(r.id),
      detail: r.ready ? r.default_model || r.protocol : "Not configured",
      models: r.models || [],
      ready: r.ready,
    })),
  ];
  return (
    <section className="rounded-2xl border border-line bg-canvas/35 p-4">
      <h3 className="text-sm font-semibold text-ink">{title}</h3>
      <p className="mt-1 text-[0.7rem] leading-relaxed text-muted">{description}</p>
      <div className="mt-3 space-y-2" role="radiogroup" aria-label={ariaLabel}>
        {options.map((option) => {
          const active = option.id === selected;
          return (
            <button
              key={option.id}
              type="button"
              role="radio"
              aria-checked={active}
              disabled={!option.ready}
              title={option.ready ? option.detail : `${option.label} — not configured`}
              className={cn(
                "flex w-full items-center gap-3 rounded-xl border px-3 py-2.5 text-left transition-colors",
                active
                  ? "border-accent bg-accent-soft text-ink"
                  : "border-line bg-surface/60 text-muted hover:border-line-strong hover:text-ink",
                !option.ready && "cursor-not-allowed opacity-45",
              )}
              onClick={() => {
                if (option.ready) onSelect(option.id);
              }}
            >
              <Icon.Logo size={15} className={cn("shrink-0", active ? "text-accent" : "text-muted")} />
              <span className="min-w-0 flex-1">
                <span className="block text-sm font-medium">{option.label}</span>
                <span className="mt-0.5 block text-[0.68rem] text-faint">{option.detail}</span>
                {option.models.length > 0 ? (
                  <span className="mt-0.5 block truncate text-[0.6rem] text-faint/80">
                    Models: {option.models.map((m) => m.split("/").pop()).join(" · ")}
                  </span>
                ) : null}
              </span>
              {!option.ready ? (
                <span className="shrink-0 text-[0.58rem] uppercase tracking-wide text-faint">Setup</span>
              ) : null}
              {active ? <Icon.Check size={15} className="shrink-0 text-accent" /> : null}
            </button>
          );
        })}
      </div>
    </section>
  );
}
