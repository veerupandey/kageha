import { afterEach, expect, it, vi } from "vitest";
import { loadComposerDraft, saveComposerDraft } from "./composerDraft";

afterEach(() => { vi.unstubAllGlobals(); sessionStorage.clear(); });
it("restores unsent text and removes sent drafts", () => {
  saveComposerDraft("Photo notes\nPlease review");
  expect(loadComposerDraft()).toBe("Photo notes\nPlease review");
  saveComposerDraft("");
  expect(loadComposerDraft()).toBe("");
});
it("keeps working when browser storage is unavailable", () => {
  vi.stubGlobal("sessionStorage", {
    getItem() { throw new Error("unavailable"); },
    setItem() { throw new Error("unavailable"); },
    removeItem() { throw new Error("unavailable"); },
  });
  expect(loadComposerDraft()).toBe("");
  expect(() => saveComposerDraft("notes")).not.toThrow();
  expect(() => saveComposerDraft("")).not.toThrow();
});
