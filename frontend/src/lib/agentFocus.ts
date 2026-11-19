export function shouldAgentFocus(input: {
  pinnedAt: number | null;
  controlling: boolean;
  now: number;
  firstFrameOfTurn: boolean;
}): boolean {
  if (input.controlling) return false;
  if (input.pinnedAt != null) return false;
  return true;
}
