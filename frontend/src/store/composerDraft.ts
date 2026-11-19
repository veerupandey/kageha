const KEY = "kageha.composerDraft";

export function loadComposerDraft(): string {
  try { return sessionStorage.getItem(KEY) || ""; }
  catch { return ""; }
}

export function saveComposerDraft(draft: string): void {
  try {
    if (draft) sessionStorage.setItem(KEY, draft);
    else sessionStorage.removeItem(KEY);
  } catch { /* Storage may be disabled or full; typing must still work. */ }
}
