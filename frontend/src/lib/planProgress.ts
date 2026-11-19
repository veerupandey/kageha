import type { TodoBoard } from "../api/types";

/** Use the complete backend board; display filtering cannot waive requirements. */
export function planOutcomeProgress(board: TodoBoard) {
  const required = (board.outcomes ?? []).filter(o => o.status !== "superseded");
  const verified = required.filter(o => o.done).length;
  return { verified, total: required.length,
    pending: Math.max(required.length - verified, board.complete === false ? 1 : 0) };
}
