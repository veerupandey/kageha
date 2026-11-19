const selectionKey = "kageha.knowledgeBases";

// Matches the Rust knowledge contract. The server still resolves workspace access.
export function isAttachableKnowledgeBase(id: unknown): id is string {
  return typeof id === "string"
    && /^[a-zA-Z0-9][a-zA-Z0-9._-]{0,95}$/.test(id)
    && !/^(?:agent[-_]|memory_)/.test(id);
}

export function readKnowledgeSelection(): string[] {
  try {
    const raw: unknown = JSON.parse(localStorage.getItem(selectionKey) ?? "[]");
    return Array.isArray(raw)
      ? [...new Set(raw.filter(isAttachableKnowledgeBase))].slice(0, 64)
      : [];
  } catch {
    return [];
  }
}

export function saveKnowledgeSelection(ids: string[]) {
  localStorage.setItem(selectionKey, JSON.stringify(ids));
}
