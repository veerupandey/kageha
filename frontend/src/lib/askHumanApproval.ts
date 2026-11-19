import type { ChatMessage, PendingApproval } from "../api/types";
import { isCredentialApproval, isQuestionApproval } from "../store/helpers";

export interface AskHumanApprovalView {
  question: string;
  options: string[];
  allowOther: boolean;
}

export function detailText(detail: string | string[] | undefined): string {
  if (!detail) return "";
  if (Array.isArray(detail)) return detail.map(String).join("\n");
  return String(detail);
}

// UI safety net mirroring credentials.credential_intent (Python). Even if the
// backend guard were bypassed, the plain-text ask_human field must never be
// used to collect a secret or a verification code.
const _SECRET_NOUN =
  /\b(pass\s?word|passcode|passphrase|api[\s_-]?key|secret[\s_-]?key|secret|token|credential|otp|one[\s-]?time[\s-]?(?:code|password)|(?:2fa|mfa|verification|security|auth(?:entication)?|sign[\s-]?in|login)\s+code|pin(?:\s+code)?|cvv|private[\s_-]?key|seed[\s_-]?phrase|recovery[\s_-]?code)\b/i;

const _VERIFICATION_INTENT =
  /\b(otp|one[\s-]?time|(?:2fa|mfa|verification|security|auth(?:entication)?|sign[\s-]?in|login)\s+code|code\s+(?:sent|from)\s+(?:the\s+)?(?:sms|text|app|email)|read\s+(?:me\s+)?the\s+code|enter\s+the\s+code)\b/i;

/** "" (safe) | "credential" | "verification" — see Python credential_intent. */
export function credentialIntent(question: string): "" | "credential" | "verification" {
  const text = String(question || "");
  if (_VERIFICATION_INTENT.test(text)) return "verification";
  if (_SECRET_NOUN.test(text)) return "credential";
  return "";
}

function dedupeOptions(items: string[]): string[] {
  const out: string[] = [];
  const seen = new Set<string>();
  for (const item of items) {
    const text = item.trim();
    if (!text) continue;
    const key = text.toLowerCase();
    if (seen.has(key)) continue;
    seen.add(key);
    out.push(text);
  }
  return out.slice(0, 8);
}

function parseOptionsFromDetail(detail: string): string[] {
  const numbered = [...detail.matchAll(/^\[(\d+)\]\s*(.+)$/gim)].map((match) =>
    String(match[2] || "").trim(),
  );
  if (numbered.length) return dedupeOptions(numbered);

  const yesMatch = detail.match(/\[Y(?:es)?]\s*(.+)/i);
  const noMatch = detail.match(/\[N(?:o)?]\s*(.+)/i);
  if (yesMatch || noMatch) {
    return dedupeOptions([
      yesMatch?.[1]?.trim() || "Yes",
      noMatch?.[1]?.trim() || "No",
    ]);
  }
  return [];
}

export function parseAskHumanApproval(
  pending: PendingApproval,
): AskHumanApprovalView {
  const rawDetail = detailText(pending.detail);
  const question =
    String(pending.question || "").trim() ||
    rawDetail.split(/\n\s*\[(?:Y|N|\d+)\]/i)[0]?.trim() ||
    rawDetail.trim();

  let options = Array.isArray(pending.options)
    ? dedupeOptions(pending.options.map((item) => String(item)))
    : [];

  if (!options.length) {
    const yes = String(pending.yes_label || "").trim();
    const no = String(pending.no_label || "").trim();
    if (yes || no) {
      options = dedupeOptions([yes || "Yes", no || "No"]);
    }
  }

  if (!options.length && rawDetail) {
    options = parseOptionsFromDetail(rawDetail);
  }

  return {
    question: question || "The agent has a question",
    options,
    allowOther: options.length > 0,
  };
}

export function pendingApprovalFieldsFromPayload(
  payload: Record<string, unknown>,
): Pick<
  PendingApproval,
  "question" | "yes_label" | "no_label" | "options"
> {
  const options = Array.isArray(payload.options)
    ? payload.options.map((item) => String(item).trim()).filter(Boolean).slice(0, 8)
    : undefined;
  return {
    question: payload.question ? String(payload.question) : undefined,
    yes_label: payload.yes_label ? String(payload.yes_label) : undefined,
    no_label: payload.no_label ? String(payload.no_label) : undefined,
    options,
  };
}

/** Pick the assistant message that should host the inline ask_human prompt. */
export function resolveAskHumanMessageId(
  messages: ChatMessage[],
  pending: PendingApproval | null | undefined,
  sessionId: string | null,
): string | null {
  if (!pending || !isQuestionApproval(pending)) return null;
  const pendingSession = pending.sessionId || pending.session_id;
  if (pendingSession && sessionId && pendingSession !== sessionId) return null;

  for (let i = messages.length - 1; i >= 0; i -= 1) {
    const message = messages[i];
    if (message.role !== "assistant" || !message.blocks?.length) continue;
    const hasPendingBlock = message.blocks.some(
      (block) =>
        block.type === "ask_human" &&
        block.approvalId === pending.approval_id &&
        block.status === "pending",
    );
    if (hasPendingBlock) return message.id;
  }

  for (let i = messages.length - 1; i >= 0; i -= 1) {
    const message = messages[i];
    if (
      message.role === "assistant" &&
      message.approval?.approval_id === pending.approval_id
    ) {
      return message.id;
    }
  }

  for (let i = messages.length - 1; i >= 0; i -= 1) {
    if (messages[i].role === "assistant") return messages[i].id;
  }

  return null;
}

export function pendingApprovalFromBlock(
  block: Extract<import("../api/types").TranscriptBlock, { type: "ask_human" }>,
  pending: PendingApproval | null | undefined,
): PendingApproval | null {
  if (
    pending &&
    pending.approval_id === block.approvalId &&
    (isQuestionApproval(pending) || isCredentialApproval(pending))
  ) {
    return pending;
  }
  return {
    approval_id: block.approvalId,
    question: block.question,
    options: block.options,
    action: "ask_human",
    risk_class: "hitl",
  };
}
