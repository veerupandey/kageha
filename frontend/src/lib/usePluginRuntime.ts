import { useEffect, useState } from "react";
import { api } from "../api/client";
import { useAppStore } from "../store";
import type { PluginPane } from "../components/Sidebar/PluginPanes";

export interface PluginRuntimeStatus {
  updated_at?: string;
  pending: boolean;
  plugins: { name: string; version: string; revision: string }[];
  reload: { status: string; error?: string };
  mcp: { name: string; enabled: boolean; ok: boolean; error: string; tools: number }[];
  hook_diagnostics: { plugin: string; event: string; tool: string; status: string; message: string; at: string }[];
  mods?: string[];
  mod_diagnostics?: { mod: string; callback: string; message: string; at: string }[];
  panes?: PluginPane[];
  workers?: {
    mod: string; pid: number | null; status: string; stop_reason: string; last_error: string;
    callbacks: Record<string, { count: number; errors: number; mean_ms: number; last_ms: number; max_ms: number }>;
  }[];
}

export function usePluginRuntime() {
  const sessionId = useAppStore((s) => s.sessionId);
  const [runtime, setRuntime] = useState<PluginRuntimeStatus | null>(null);
  const [runtimeError, setRuntimeError] = useState("");
  useEffect(() => {
    setRuntime(null); setRuntimeError("");
    if (!sessionId) return;
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout>;
    const poll = async () => {
      try {
        const data = await api<PluginRuntimeStatus>(`/api/sessions/${encodeURIComponent(sessionId)}/plugins`);
        if (!cancelled) { setRuntime(data); setRuntimeError(""); }
      } catch (e) { if (!cancelled) setRuntimeError(String(e)); }
      if (!cancelled) timer = setTimeout(() => void poll(), 3000);
    };
    void poll();
    return () => { cancelled = true; clearTimeout(timer); };
  }, [sessionId]);
  return { sessionId, runtime, runtimeError };
}
