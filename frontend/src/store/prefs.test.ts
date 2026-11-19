import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  DEFAULT_PREFS,
  loadPrefs,
  mergePrefs,
  PREFS_KEY,
  savePrefs,
  applyPrefsToDocument,
} from "./prefs";
import type { UserPrefs } from "../api/types";
import { THEMES } from "../lib/themes";

describe("prefs load/save", () => {
  const store = new Map<string, string>();

  beforeEach(() => {
    store.clear();
    vi.stubGlobal("localStorage", {
      getItem: (k: string) => store.get(k) ?? null,
      setItem: (k: string, v: string) => {
        store.set(k, String(v));
      },
      removeItem: (k: string) => {
        store.delete(k);
      },
      clear: () => store.clear(),
      key: () => null,
      get length() {
        return store.size;
      },
    });
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("returns defaults when empty", () => {
    expect(loadPrefs()).toEqual(DEFAULT_PREFS);
  });

  it.each(THEMES)("persists $label and applies its mode to the app and portal root", (theme) => {
    const app = document.createElement("div");
    app.id = "app";
    document.body.append(app);
    try {
      const prefs = mergePrefs(DEFAULT_PREFS, { theme: theme.id });
      savePrefs(prefs);
      expect(loadPrefs().theme).toBe(theme.id);
      applyPrefsToDocument(loadPrefs());
      for (const root of [app, document.documentElement]) {
        expect(root.getAttribute("data-theme")).toBe(theme.id);
        expect(root.getAttribute("data-color-scheme")).toBe(theme.scheme);
      }
    } finally {
      app.remove();
      applyPrefsToDocument(DEFAULT_PREFS);
    }
  });

  it("falls back for unknown saved themes and ignores invalid theme patches", () => {
    store.set(PREFS_KEY, JSON.stringify({ theme: "unknown" }));
    expect(loadPrefs().theme).toBe(DEFAULT_PREFS.theme);
    expect(mergePrefs(DEFAULT_PREFS, { theme: "unknown" as UserPrefs["theme"] }).theme).toBe(DEFAULT_PREFS.theme);
  });

  it("roundtrips prefs through localStorage", () => {
    const prefs: UserPrefs = {
      density: "compact",
      defaultAskMode: true,
      defaultAgentMode: "plan",
      reduceMotion: true,
      theme: "dark",
      voiceReply: true,
      voiceConversation: false,
      mainModel: "",
      plannerModel: "",
      workerModel: "",
      reasoningEffort: "medium",
      mediaModel: "auto",
      vision: "auto",
      webSearch: "auto",
      imageGeneration: "auto",
      tts: "auto",
      stt: "auto",
      sidebarCollapsed: true,
      rightPanelCollapsed: true,
    };
    savePrefs(prefs);
    expect(store.get(PREFS_KEY)).toBeTruthy();
    expect(loadPrefs()).toEqual(prefs);
  });

  it("migrates legacy ask mode when prefs missing", () => {
    store.set("kageha.askMode", "1");
    expect(loadPrefs().defaultAskMode).toBe(true);
  });

  it("persists a selected media model when preferences are patched", () => {
    const next = mergePrefs(DEFAULT_PREFS, { mediaModel: "antigravity" });
    expect(next.mediaModel).toBe("antigravity");
    expect(DEFAULT_PREFS.mediaModel).toBe("auto");
  });
});


it("persists future TTS provider IDs and saved voice profiles", () => {
  const prefs = mergePrefs(DEFAULT_PREFS, { tts: "future-voice-provider", ttsVoice: "abc123" });
  expect(prefs.tts).toBe("future-voice-provider");
  expect(prefs.ttsVoice).toBe("abc123");
  expect(mergePrefs(prefs, { tts: "../../invalid" }).tts).toBe("future-voice-provider");
});


it("keeps read-aloud and conversation preferences consistent", () => {
  const conversation = mergePrefs(DEFAULT_PREFS, { voiceConversation: true });
  expect(conversation.voiceReply).toBe(true);
  const silent = mergePrefs(conversation, { voiceReply: false });
  expect(silent.voiceReply).toBe(false);
  expect(silent.voiceConversation).toBe(false);
});
