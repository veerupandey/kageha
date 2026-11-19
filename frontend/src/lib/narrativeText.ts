/** Identify a complete draft or its partial streamed prefix by a source offset. */
export function answerRevisionSuffix(saved: string, original: string, offset?: number): number | null {
  if (!original) return null;
  if (saved.endsWith(original)) return original.length;
  if (offset !== undefined && Number.isInteger(offset) && offset >= 0 && offset <= saved.length
      && original.startsWith(saved.slice(offset))) return saved.length - offset;
  return null;
}

function matchesRedacted(masked: string, plain: string): boolean {
  if (!masked.includes("[REDACTED]")) return false;
  return new RegExp(`^${redactedPattern(masked.trim())}$`).test(plain.trim());
}

function redactedPattern(masked: string): string {
  return masked.split("[REDACTED]")
    .map((part) => part.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"))
    .join("[^\\n]*?");
}

/** Compare a known answer item across its journal and terminal projections. */
export function sameNarrativeSnapshot(left: string, right: string): boolean {
  return left === right || matchesRedacted(left, right) || matchesRedacted(right, left);
}

/** Reconcile delivered narration with a terminal snapshot without deleting it. */
export function mergeTerminalNarrative(saved: string, final: string, separator = "\n\n"): string {
  if (!final.trim()) return saved;
  if (!saved) return final;
  if (final.startsWith(saved)) return final;
  if (saved.includes(final.trim())) return saved;
  // Journal payloads are redacted before persistence; the closing snapshot
  // can contain the same answer with an absolute path instead. Keep the
  // redacted version when that is the only difference, without appending a
  // second copy or restoring anything that was masked.
  const withoutInspectionLabel = (text: string) => text.replace(/\n\nInspection: /g, "");
  if (matchesRedacted(withoutInspectionLabel(saved), withoutInspectionLabel(final))) return saved;
  if (matchesRedacted(withoutInspectionLabel(final), withoutInspectionLabel(saved))) return final;
  const overlap = terminalAnswerOverlap(saved, final);
  if (overlap) {
    const suffix = saved.slice(-overlap);
    // The journal and terminal snapshot can differ only at masked URLs or
    // paths. Retain the journal's masks while adding terminal-only footers.
    const match = suffix.includes("[REDACTED]")
      ? new RegExp(redactedPattern(suffix)).exec(final) : null;
    const snapshot = match
      ? final.slice(0, match.index) + suffix + final.slice(match.index + match[0].length)
      : final;
    return saved.slice(0, -overlap) + snapshot;
  }
  return saved + separator + final;
}

/** Exact shared answer suffix, allowing terminal artifact headers and citations. */
export function terminalAnswerOverlap(saved: string, final: string): number {
  if (!saved || !final || saved.includes(final.trim())) return 0;
  // The controller can add an inspection label between already-delivered
  // fragments. That formatting change must not duplicate the entire answer.
  const unlabelled = final.replace(/\n\nInspection: /g, "");
  if (unlabelled === saved) return saved.length;
  // Reconciliation must compare the entire inspection pair before looking for
  // a shared suffix. Otherwise a path mask difference in the inspection makes
  // a short shared suffix append the whole earlier answer a second time.
  if (saved.includes("[REDACTED]") || final.includes("[REDACTED]")) {
    const masked = saved.includes("[REDACTED]") ? saved : unlabelled;
    const plain = saved.includes("[REDACTED]") ? unlabelled : saved;
    if (matchesRedacted(masked.replace(/\n\nInspection: /g, ""), plain)) return 0;
  }
  const starts = new Set([0]);
  // Canonical paragraph starts provide anchors even when streamed messages
  // were concatenated without a separator. Require substantial exact text.
  for (const line of final.split("\n")) {
    // Short headings can start a long answer; validate the full suffix below.
    if (!line.trim()) continue;
    const index = saved.lastIndexOf(line);
    if (index >= 0) starts.add(index);
  }
  let overlap = 0;
  for (const start of starts) {
    const suffix = saved.slice(start);
    if (suffix.length >= 80 && suffix.length > overlap && (
      unlabelled.includes(suffix) || (suffix.includes("[REDACTED]")
        && new RegExp(redactedPattern(suffix)).test(unlabelled))
    )) overlap = suffix.length;
  }
  return overlap;
}
