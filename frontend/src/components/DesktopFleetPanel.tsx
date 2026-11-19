import { useEffect, useState } from "react";
import { api } from "../api/client";
import { cn } from "../lib/cn";

interface DesktopInventoryItem {
  sessionId: string;
  name: string;
  status: string;
  image?: string;
  title?: string;
  frame?: { path: string; version: number } | null;
  updatedAt?: string | number | null;
  frameCapturedAt?: number;
  connected?: boolean;
  error?: string | null;
  stats?: { cpu?: string; memory?: string; network?: string };
}

export function DesktopFleetPanel() {
  const [desktops, setDesktops] = useState<DesktopInventoryItem[]>([]);
  const [error, setError] = useState("");

  useEffect(() => {
    let cancelled = false;
    const refresh = async () => {
      try {
        const next = await api<{ desktops?: DesktopInventoryItem[] }>("/api/desktops");
        if (!cancelled) {
          setDesktops(next.desktops || []);
          setError("");
        }
      } catch (err) {
        if (!cancelled) setError(err instanceof Error ? err.message : String(err));
      }
    };
    void refresh();
    const timer = window.setInterval(() => void refresh(), 1500);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, []);

  return (
    <section className="min-h-0 flex-1 overflow-auto bg-[#0d1117] p-3" aria-label="Docker desktop fleet">
      <div className="mb-3 flex items-center justify-between">
        <div>
          <h2 className="text-sm font-semibold text-white/90">Docker desktops</h2>
          <p className="text-[0.65rem] text-white/45">Read-only view of already-running session containers</p>
        </div>
        <span className="rounded-full bg-white/10 px-2 py-1 text-[0.65rem] text-white/60">{desktops.length} running</span>
      </div>
      {error && <p className="mb-3 rounded border border-red-300/20 bg-red-950/40 p-2 text-xs text-red-200">{error}</p>}
      {!desktops.length && !error && <p className="rounded border border-white/10 p-4 text-center text-xs text-white/45">No running Kageha Docker desktops found.</p>}
      <div className="grid gap-3 sm:grid-cols-2">
        {desktops.map((desktop) => {
          const frameUrl = desktop.frame
            ? `/api/sessions/${encodeURIComponent(desktop.sessionId)}/files/${desktop.frame.path.split("/").map(encodeURIComponent).join("/")}?v=${desktop.frame.version}`
            : "";
          const running = /^up\b/i.test(desktop.status);
          return (
            <article key={desktop.sessionId} className="overflow-hidden rounded-xl border border-white/10 bg-white/[0.03]">
              <div className="flex items-center gap-2 border-b border-white/10 px-3 py-2">
                <span className={cn("h-2 w-2 rounded-full", running ? "bg-emerald-400" : "bg-amber-400")} />
                <span className="min-w-0 flex-1 truncate text-xs font-medium text-white/85">{desktop.title || desktop.sessionId}</span>
                <span className="text-[0.6rem] text-white/45">{running ? "running" : desktop.status}</span>
              </div>
              <div className="flex aspect-video items-center justify-center overflow-hidden bg-black/40">
                {frameUrl ? <img src={frameUrl} alt={`Live desktop for ${desktop.sessionId}`} className="h-full w-full object-contain" /> : <span className="px-4 text-center text-[0.65rem] text-white/40">Container is running, but no frame has been captured yet.</span>}
              </div>
              <div className="flex items-center justify-between gap-2 px-3 py-2 font-mono text-[0.58rem] text-white/40">
                <span className="truncate">{desktop.sessionId}</span>
                <span className="truncate text-right">
                  {desktop.stats?.cpu || "—"} · {desktop.stats?.memory || desktop.image || "Docker"}
                </span>
              </div>
              <div className="border-t border-white/10 px-3 py-1.5 text-[0.58rem] text-white/35">
                {desktop.connected
                  ? `Frame refreshed ${desktop.frameCapturedAt ? `${Math.max(0, Math.round((Date.now() * 1e-3 - desktop.frameCapturedAt) * 10) / 10)}s ago` : "now"}`
                  : desktop.error || "Container running; live bridge unavailable"}
              </div>
            </article>
          );
        })}
      </div>
    </section>
  );
}
