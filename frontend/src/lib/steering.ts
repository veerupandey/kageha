import type { ChatMessage, TranscriptBlock } from "../api/types";

type Boundary = NonNullable<ChatMessage["steeringBoundary"]>;

export function captureSteeringBoundary(message?: ChatMessage): Boundary {
  const blocks = message?.blocks || [];
  if (!blocks.length && message?.text) return { blockCount: 1, tailTextLength: message.text.length };
  const tail = blocks.at(-1);
  return { blockCount: blocks.length, tailTextLength: tail?.type === "narrative" ? tail.text.length : 0 };
}

function position(blocks: TranscriptBlock[], boundary: Boundary): [number, number] {
  const count = Math.max(0, Math.min(boundary.blockCount, blocks.length));
  const tail = blocks[count - 1];
  return tail?.type === "narrative"
    ? [count - 1, Math.min(Math.max(0, boundary.tailTextLength), tail.text.length)]
    : [count, 0];
}

function sliceBlocks(blocks: TranscriptBlock[], from: [number, number], to: [number, number]): TranscriptBlock[] {
  const out: TranscriptBlock[] = [];
  blocks.forEach((block, i) => {
    if (i < from[0] || i > to[0] || (i === to[0] && to[1] === 0)) return;
    if (block.type === "narrative") {
      const text = block.text.slice(i === from[0] ? from[1] : 0, i === to[0] ? to[1] : undefined);
      if (text) out.push({ ...block, text });
    } else out.push(block);
  });
  return out;
}

/** Split display at each correction's send position; preserve stored messages. */
export function orderSteeringMessages(messages: ChatMessage[]): ChatMessage[] {
  if (messages.some((m) => m.sourceMessageId)) return messages;
  const anchored = messages.filter((m) => m.steering && m.turnId && m.steeringBoundary
    && messages.some((a) => a.role === "assistant" && a.turnId === m.turnId));
  const hidden = new Set(anchored.map((m) => m.id));
  return messages.flatMap((message) => {
    if (hidden.has(message.id)) return [];
    const corrections = message.role === "assistant" ? anchored.filter((m) => m.turnId === message.turnId) : [];
    if (!corrections.length) return [message];
    const blocks: TranscriptBlock[] = message.blocks?.length ? message.blocks
      : message.text ? [{ type: "narrative", id: `${message.id}-text`, text: message.text }] : [];
    const out: ChatMessage[] = [];
    let from: [number, number] = [0, 0];
    const segment = (part: TranscriptBlock[], id: string, final: boolean): ChatMessage => ({
      ...message, id, sourceMessageId: message.id,
      blocks: final ? part : part.map((b) => b.type === "reasoning" ? { ...b, streaming: false } : b),
      text: part.map((b) => b.type === "narrative" ? b.text : "").join(""),
      streaming: final && message.streaming, steps: [],
      toolCards: part.flatMap((b) => b.type === "tool" || b.type === "shell" ? [b.card] : []),
      computerFrames: final ? message.computerFrames : [],
      statusLabel: final ? message.statusLabel : undefined,
      failureCause: final ? message.failureCause : undefined,
      resultStatus: final ? message.resultStatus : undefined,
      attachments: final ? message.attachments : undefined,
      turnSteps: final ? message.turnSteps : undefined,
      turnCostUsd: final ? message.turnCostUsd : undefined,
    });
    for (const correction of corrections) {
      let to = position(blocks, correction.steeringBoundary!);
      if (to[0] < from[0] || (to[0] === from[0] && to[1] < from[1])) to = from;
      const part = sliceBlocks(blocks, from, to);
      if (part.length) out.push(segment(part, `${message.id}:before:${correction.id}`, false));
      out.push(correction);
      from = to;
    }
    const remaining = sliceBlocks(blocks, from, [blocks.length, 0]);
    if (remaining.length || message.streaming || message.failureCause || message.statusLabel || message.attachments?.length)
      out.push(segment(remaining, message.id, true));
    return out;
  });
}
