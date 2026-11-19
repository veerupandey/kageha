import { memo } from "react";
import type { BrowserActivityTarget, TranscriptBlock } from "../../api/types";
import { friendlyActivityLabel } from "../../lib/activityUi";
import { browserToolBlocked, isSubagentSpawn } from "../../lib/transcript";
import { NarrativeBlock } from "./NarrativeBlock";
import { AskHumanBlock } from "./AskHumanBlock";
import { ReasoningBlock } from "./ReasoningBlock";
import { ShellBlock } from "./ShellBlock";
import { ToolCallPill } from "./ToolCallPill";
import { ActionSummaryCard } from "../ActionSummaryCard";

function isGenericLivePulse(label: string): boolean {
  const compact = String(label || "").replace(/\s+/g, " ").trim();
  if (!compact) return true;
  return /^(still\s+)?(thinking|working|connecting|starting|planning|preparing)[.…]*$/i.test(
    compact,
  );
}

/**
 * Join narrative fragments that were split only by invisible blocks.
 *
 * Heartbeats and status frames can land between two text deltas of the same
 * reply. Each fragment would then render as its own Markdown document, which
 * breaks lists mid-item and leaves unmatched `**` as literal asterisks. Text
 * separated by a real block (tool, shell, visible reasoning) stays separate.
 *
 * Adjacent narrative blocks are always one stream: `applyNarrative` appends to
 * a trailing narrative block, so two in a row only exist because an empty
 * "Thinking…" placeholder between them was closed and removed afterwards.
 */
export function coalesceNarrativeBlocks(blocks: TranscriptBlock[]): TranscriptBlock[] {
  const out: TranscriptBlock[] = [];
  // Empty "Thinking…" placeholders that trail a narrative fragment.
  let pending: TranscriptBlock[] = [];
  const flushPending = () => {
    out.push(...pending);
    pending = [];
  };
  for (const block of blocks) {
    if (block.type === "status") continue;
    const previous = out.at(-1);
    if (
      block.type === "reasoning" &&
      !block.text?.trim() &&
      previous?.type === "narrative"
    ) {
      pending.push(block);
      continue;
    }
    if (block.type === "narrative" && previous?.type === "narrative") {
      // Text deltas are one continuous stream: concatenate without a separator.
      out[out.length - 1] = { ...previous, text: previous.text + block.text };
      pending = [];
      continue;
    }
    flushPending();
    if (block.type === "reasoning" && out.at(-1)?.type === "reasoning") {
      const prior = out.at(-1) as Extract<TranscriptBlock, { type: "reasoning" }>;
      // Keep the first ID stable so an expanded group stays open during streaming.
      out[out.length - 1] = {
        ...prior,
        durationMs: (prior.durationMs || 0) + (block.durationMs || 0),
        text: [prior.text, block.text].filter(Boolean).join("\n\n"),
        streaming: Boolean(prior.streaming || block.streaming),
      };
    } else {
      out.push(block);
    }
  }
  flushPending();
  return out;
}

type ToolBlock = Extract<TranscriptBlock, { type: "tool" | "shell" }>;
type Reasoning = Extract<TranscriptBlock, { type: "reasoning" }>;
type ActivityGroup = { type: "activity_group"; blocks: (ToolBlock | Reasoning)[]; cards: ToolBlock[] };
function quietTool(block: TranscriptBlock): block is ToolBlock {
  return (block.type === "tool" || block.type === "shell") &&
    block.card.name !== "request_secure_login" &&
    !(block.type === "shell" && block.card.errorPreview) &&
    !block.card.connectorOutcome?.recovery &&
    !block.card.researchRecovery?.recovered &&
    !browserToolBlocked(block.card) &&
    !isSubagentSpawn(block.card.name) &&
    ["done", "success", "completed", "ok"].includes(block.card.status || "");
}

// Only summarize related actions together. A summary for mixed browser, mail,
// and file operations can otherwise imply that every action did the same thing.
function actionKind(block: ToolBlock): string {
  const card = block.card;
  if (card.connectorOutcome?.connector) return `connector:${card.connectorOutcome.connector}`;
  if (/browser|navigate|url|page/i.test(card.name) || card.browserTarget || card.observedSource?.url) return "browser";
  return `${block.type}:${card.name.toLowerCase()}`;
}

function groupActivity(blocks: TranscriptBlock[]): (TranscriptBlock | ActivityGroup)[] {
  const items: (TranscriptBlock | ActivityGroup)[] = [];
  let group: ActivityGroup | undefined;
  let thoughts: Reasoning[] = [];
  const flush = () => {
    if (group) items.push(group);
    group = undefined;
    items.push(...thoughts);
    thoughts = [];
  };
  for (const block of blocks) {
    if (block.type === "reasoning" && !block.streaming) {
      thoughts.push(block);
    } else if (quietTool(block)) {
      if (group && actionKind(group.cards[0]) !== actionKind(block)) {
        items.push(group);
        group = undefined;
      }
      if (!group) group = { type: "activity_group", blocks: [], cards: [] };
      group.blocks.push(...thoughts, block);
      group.cards.push(block);
      thoughts = [];
    } else {
      flush();
      items.push(block);
    }
  }
  flush();
  return items;
}

export const Transcript = memo(function Transcript({
  blocks,
  streaming,
  liveLabel,
  liveDetail,
  messageId,
  onOpenBrowser,
  onOpenComputer,
  compactCompleted = true,
  openLiveReasoning = false,
}: {
  blocks: TranscriptBlock[];
  streaming?: boolean;
  liveLabel?: string;
  liveDetail?: string;
  messageId: string;
  onOpenBrowser?: (url?: string, target?: BrowserActivityTarget) => void;
  onOpenComputer?: () => void;
  compactCompleted?: boolean;
  openLiveReasoning?: boolean;
}) {
  const visibleBlocks = coalesceNarrativeBlocks(blocks).filter((block) =>
    block.type !== "tool" || block.card.name !== "ask_human" ||
      ["failed", "error", "interrupted"].includes(block.card.status || ""));
  const items = compactCompleted ? groupActivity(visibleBlocks) : visibleBlocks;
  const lastNarrativeId = [...visibleBlocks]
    .reverse()
    .find((block) => block.type === "narrative")?.id;
  const thinking = blocks.some((block) => block.type === "reasoning" && block.streaming);
  const friendly = friendlyActivityLabel(liveLabel || "") || liveLabel || "";
  // Keep an actionable phase visible separately from the expandable thought.
  // The thought chip already represents generic waiting/thinking pulses.
  const live = streaming && friendly && (!thinking || !isGenericLivePulse(friendly)) ? friendly : "";
  if (!visibleBlocks.length && !live) return null;
  // Narrative blocks can be fragments of one answer, not separate attempts.
  // Never hide the beginning of a reply based on its chunk boundaries.
  return (
    <div className="space-y-0.5">
      {items.map((block) => {
        if (block.type === "activity_group") {
          const first = block.cards[0];
          return (
            <ActionSummaryCard
              key={first.id}
              cards={block.cards.map((item) => item.card)}
              onOpenBrowser={onOpenBrowser}
            >
              {block.blocks.map((item) => item.type === "reasoning"
                ? <div key={item.id} className="px-3 py-1 text-xs text-faint"><span className="font-medium">Thought</span><p className="mt-1 whitespace-pre-wrap break-words">{item.text}</p></div>
                : item.type === "shell"
                ? <ShellBlock key={item.id} card={item.card} />
                : <ToolCallPill key={item.id} card={item.card} onOpenBrowser={onOpenBrowser} onOpenComputer={onOpenComputer} />)}
            </ActionSummaryCard>
          );
        }
        if (block.type === "reasoning") {
          return (
            <ReasoningBlock
              key={block.id}
              durationMs={block.durationMs}
              text={block.text}
              activityLabel={undefined}
              streaming={block.streaming}
              openLiveByDefault={openLiveReasoning}
              streamingDetail={block.streaming ? liveDetail : undefined}
            />
          );
        }
        if (block.type === "tool") {
          return (
            <ToolCallPill
              key={block.id}
              card={block.card}
              compact={compactCompleted && (["failed", "error", "denied", "blocked", "unconfirmed"].includes(block.card.status || "") || Boolean(block.card.connectorOutcome?.recovery) || browserToolBlocked(block.card))}
              onOpenBrowser={onOpenBrowser}
              onOpenComputer={onOpenComputer}
            />
          );
        }
        if (block.type === "shell") {
          return <ShellBlock key={block.id} card={block.card} compact={compactCompleted && (Boolean(block.card.errorPreview) || ["failed", "error", "denied"].includes(block.card.status || ""))} />;
        }
        if (block.type === "peer_result") {
          return (
            <div key={block.id} className="mb-3 rounded-lg border border-line bg-surface px-3 py-2">
              <div className="mb-1 text-[0.7rem] font-medium text-muted">
                {block.path ? `Peer result · ${block.path}` : "Peer result"}
              </div>
              <pre className="max-h-64 overflow-auto whitespace-pre-wrap break-words font-mono text-[0.8rem] text-ink">
                {block.text}
              </pre>
            </div>
          );
        }
        if (block.type === "progress") {
          return (
            <div key={block.id} className="my-2 text-sm leading-relaxed text-muted">
              <NarrativeBlock id={`${messageId}-${block.id}`} text={block.text} />
            </div>
          );
        }
        if (block.type === "narrative") {
          return (
            <div key={block.id} className="mb-2">
              <NarrativeBlock
                id={`${messageId}-${block.id}`}
                text={block.text}
                streaming={Boolean(streaming && block.id === lastNarrativeId)}
              />
            </div>
          );
        }
        if (block.type === "ask_human") {
          return <AskHumanBlock key={block.id} block={block} messageId={messageId} />;
        }
        return null;
      })}
      {live ? (
        <div className="mb-2 inline-flex items-center gap-2 rounded-full border border-line bg-surface/80 px-2.5 py-1 text-[0.72rem] text-muted shadow-sm backdrop-blur">
          <span className="ka-thinking-dots" aria-hidden="true">
            <span className="ka-thinking-dot" />
            <span className="ka-thinking-dot" />
            <span className="ka-thinking-dot" />
          </span>
          <span className="font-medium">{live}</span>
        </div>
      ) : null}
    </div>
  );
});
