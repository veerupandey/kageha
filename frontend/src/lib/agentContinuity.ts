import { useCallback, useRef, useState } from "react";

export function agentKey(session: string, name: string) {
  return `kageha.agent.v1:${encodeURIComponent(session)}:${encodeURIComponent(name)}`;
}

export function readAgentValue<T>(key: string, fallback: T): T {
  try {
    const value = localStorage.getItem(key);
    if (value !== null) return JSON.parse(value) as T;
  } catch { /* Restricted storage or a malformed saved value must not block chat. */ }
  return fallback;
}

export function useAgentValue<T>(key: string, fallback: T) {
  const [entry, setEntry] = useState(() => ({ key, value: readAgentValue(key, fallback) }));
  const value = entry.key === key ? entry.value : readAgentValue(key, fallback);
  const latest = useRef(entry);
  if (latest.current.key !== key) latest.current = { key, value };
  const update = useCallback((next: T | ((previous: T) => T)) => {
    const current = readAgentValue(key, latest.current.key === key ? latest.current.value : fallback);
    const value = typeof next === "function" ? (next as (previous: T) => T)(current) : next;
    try { localStorage.setItem(key, JSON.stringify(value)); } catch { /* Keep this mount usable. */ }
    latest.current = { key, value };
    setEntry({ key, value });
  }, [key, fallback]);
  return [value, update] as const;
}
