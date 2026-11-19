import { useEffect, useState } from "react";
import { api } from "../api/client";
import { providerDisplayName } from "../lib/modelCatalog";

type Provider = { id: string; ready: boolean; capabilities: string[] };
const labels: Record<string, string> = {
  tool_calling: "Tools", web_search: "Grounded search", vision: "Vision",
  image_generation: "Image generation", image_edit: "Image editing",
  reasoning: "Reasoning", stt: "Transcription", tts: "Speech", video: "Video",
};

export function CapabilityCatalog() {
  const [providers, setProviders] = useState<Provider[]>([]);
  useEffect(() => {
    let alive = true;
    const refresh = () => void api<{ providers: Provider[] }>("/api/capabilities")
      .then((result) => { if (alive) setProviders(result.providers || []); })
      .catch(() => {});
    refresh();
    const timer = window.setInterval(refresh, 60_000);
    return () => { alive = false; window.clearInterval(timer); };
  }, []);
  if (!providers.length) return null;
  return (
    <section className="rounded-2xl border border-line bg-canvas/35 p-4">
      <h3 className="text-sm font-semibold text-ink">Connected capabilities</h3>
      <p className="mt-1 text-xs text-muted">Features available through your provider connections. Account access is checked when used.</p>
      <div className="mt-3 space-y-3">
        {providers.filter((provider) => provider.capabilities.length).map((provider) => (
          <div key={provider.id} className="flex flex-wrap items-center gap-2 text-xs">
            <span className="font-medium text-ink">{providerDisplayName(provider.id)}</span>
            {!provider.ready && <span className="text-muted">Needs connection</span>}
            {provider.capabilities.map((capability) => (
              <span key={capability} className="rounded-md bg-line/60 px-2 py-1 text-muted">{labels[capability] || capability.replaceAll("_", " ")}</span>
            ))}
          </div>
        ))}
      </div>
    </section>
  );
}
