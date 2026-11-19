import { useSyncExternalStore } from "react";
import type { BrowserActivityTarget } from "../api/types";

interface BrowserControls {
  sessionId: string;
  connected: boolean;
  controlling: boolean;
  busy: boolean;
  error: string;
  transfer: () => void;
}

let controls: BrowserControls | null = null;
let requested: { sessionId: string; target: BrowserActivityTarget | null } | null = null;
const listeners = new Set<() => void>();
const notify = () => listeners.forEach(listener => listener());
const subscribe = (listener: () => void) => {
  listeners.add(listener);
  return () => { listeners.delete(listener); };
};

export function publishBrowserControls(value: BrowserControls | null) {
  controls = value;
  notify();
}

export function requestBrowserTarget(sessionId: string, target?: BrowserActivityTarget) {
  requested = { sessionId, target: target ?? null };
  notify();
}

export function useBrowserControls(sessionId: string | null) {
  return useSyncExternalStore(subscribe, () => controls?.sessionId === sessionId ? controls : null);
}

export function useRequestedBrowserTarget(sessionId: string | null) {
  return useSyncExternalStore(subscribe, () => requested?.sessionId === sessionId ? requested : null);
}
