import type { ChatMessage } from "../api/types";
import { collectMessageToolCards, collectWrittenArtifactRefs } from "./messageArtifacts";

const completed = ["success", "complete", "completed", "done", "ok"];

function references(text: string): Set<string> {
  return new Set(text.match(/[^\s`"'<>()[\]]+/g)?.map(value => value.replace(/[.,;:!?]+$/, "")) ?? []);
}

export function deliveredPaths(message: ChatMessage, existingPaths: Iterable<string>): string[] {
  if (message.role !== "assistant" || message.streaming || message.failureCause ||
      (message.resultStatus && !completed.includes(message.resultStatus))) return [];
  const mentioned = references(message.text);
  const linked = new Set<string>();
  for (const match of message.text.matchAll(/\]\(<?((?:artifacts|outputs|diagrams|inputs|uploads)\/[^)\n>]+)>?\)/g)) {
    try { linked.add(decodeURIComponent(match[1])); } catch { /* Invalid link encoding. */ }
  }
  for (const match of message.text.matchAll(/`([^`\n]+)`/g)) mentioned.add(match[1]);
  const existing = new Set(existingPaths);
  const candidates = [...existing, ...(message.attachments ?? []), ...collectWrittenArtifactRefs(message)];
  const delivered = new Set<string>();
  for (const path of candidates) {
    if (path.split("/").includes("..")) continue;
    // Tools use workspace-relative names in their answers while the artifact
    // registry exposes those same files as workspace/<name>. Resolve only the
    // exact relative path; matching a basename can select the wrong file.
    const relative = path.startsWith("workspace/") ? path.slice("workspace/".length) : path;
    if (!((/^(artifacts|outputs|diagrams)\//.test(relative) &&
        (mentioned.has(path) || mentioned.has(relative))) || linked.has(path) || linked.has(relative))) continue;
    const resolved = existing.has(path) ? path
      : existing.has(`workspace/${relative}`) ? `workspace/${relative}` : path;
    delivered.add(resolved);
  }
  return [...delivered];
}

export function observedSources(message: ChatMessage) {
  if (message.streaming || message.role !== "assistant") return [];
  const cited = references(message.text);
  const sources = new Map<string, { url: string; title: string }>();
  for (const card of collectMessageToolCards(message)) {
    const source = card.observedSource;
    if (card.name !== "web_fetch" || !completed.includes(card.status ?? "") || !source || !cited.has(source.url)) continue;
    try {
      const url = new URL(source.url);
      if (["http:", "https:"].includes(url.protocol) && !url.username && !url.password) {
        sources.set(source.url, { url: source.url, title: source.title || url.hostname });
      }
    } catch { /* An invalid URL cannot become a source card. */ }
  }
  return [...sources.values()];
}
