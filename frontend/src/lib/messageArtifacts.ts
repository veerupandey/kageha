/** Card-strip paths come from tool writes, never from narrative mentions. */

import type { ChatMessage, ToolCard, TranscriptBlock } from "../api/types";
import { isChatMediaArtifact } from "./artifactMedia";

function normalizeRel(raw: string): string {
  return String(raw || "").replace(/\\/g, "/").replace(/^\/+/, "");
}

function pushRef(out: string[], seen: Set<string>, raw: string): void {
  const path = normalizeRel(raw);
  if (!path || seen.has(path)) return;
  seen.add(path);
  out.push(path);
}

function refsFromCard(card: ToolCard | undefined, out: string[], seen: Set<string>): void {
  for (const ref of card?.artifactRefs || []) pushRef(out, seen, ref);
}

/** Live toolCards plus journal-replayed tool/shell blocks (reopen Timeline). */
export function collectMessageToolCards(
  message: Pick<ChatMessage, "toolCards" | "blocks">,
): ToolCard[] {
  const seen = new Set<string>();
  const out: ToolCard[] = [];
  const push = (card: ToolCard | undefined) => {
    if (!card) return;
    if (card.id) {
      if (seen.has(card.id)) return;
      seen.add(card.id);
    }
    out.push(card);
  };
  for (const card of message.toolCards || []) push(card);
  for (const block of message.blocks || []) {
    if (block.type === "tool" || block.type === "shell") push(block.card);
  }
  return out;
}

/** Paths this message's tools actually wrote (live cards + journal replay blocks). */
export function collectWrittenArtifactRefs(
  message: Pick<ChatMessage, "toolCards" | "blocks">,
): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const card of message.toolCards || []) refsFromCard(card, out, seen);
  for (const block of message.blocks || []) {
    if (block.type === "tool" || block.type === "shell") {
      refsFromCard((block as Extract<TranscriptBlock, { card: ToolCard }>).card, out, seen);
    }
  }
  return out;
}

/**
 * Showcase cards for a chat bubble: this-turn writes ∩ files that exist in
 * the session artifact listing. Narrative backticks/bare paths never qualify.
 */
export function messageArtifactCardPaths(
  message: Pick<ChatMessage, "toolCards" | "blocks">,
  sessionPaths: Iterable<string>,
): string[] {
  const existing = new Set([...sessionPaths].map(normalizeRel).filter(Boolean));
  return collectWrittenArtifactRefs(message).filter(
    (path) => existing.has(path) && isChatMediaArtifact(path),
  );
}

/**
 * Manus-style delivery: pin files on the message that produced them.
 * Finished rows use ``attachments`` only. While streaming, fall back to
 * this-turn tool writes so thumbs can appear before the turn lands.
 */
export function messageBubbleArtifactPaths(
  message: Pick<
    ChatMessage,
    "role" | "attachments" | "toolCards" | "blocks" | "streaming"
  >,
  sessionPaths: Iterable<string>,
): string[] {
  const pinned = (message.attachments || []).filter(isChatMediaArtifact);
  if (message.role === "user" || pinned.length || !message.streaming) {
    return pinned;
  }
  return messageArtifactCardPaths(message, sessionPaths);
}
