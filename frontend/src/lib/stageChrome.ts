export const STAGE_CHROME_HOLD_MS = 1600;

export function stageChromeVisible(input: {
  controlling: boolean;
  pointerInside: boolean;
  focusWithin: boolean;
  recentActionAt: number;
  now: number;
}): boolean {
  if (input.controlling) return true;
  if (input.pointerInside || input.focusWithin) return true;
  if (input.recentActionAt > 0 && input.now - input.recentActionAt < STAGE_CHROME_HOLD_MS) {
    return true;
  }
  return false;
}
