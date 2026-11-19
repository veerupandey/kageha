import { useCallback, useEffect, useState } from "react";
import type { FocusEvent } from "react";
import { STAGE_CHROME_HOLD_MS, stageChromeVisible } from "../../lib/stageChrome";

export function useStageChrome(controlling: boolean) {
  const [pointerInside, setPointerInside] = useState(false);
  const [focusWithin, setFocusWithin] = useState(false);
  const [recentActionAt, setRecentActionAt] = useState(0);
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    if (controlling || recentActionAt <= 0) return;
    const remain = STAGE_CHROME_HOLD_MS - (Date.now() - recentActionAt);
    if (remain <= 0) return;
    const timer = window.setTimeout(() => setNow(Date.now()), remain + 16);
    return () => window.clearTimeout(timer);
  }, [controlling, recentActionAt]);

  const visible = stageChromeVisible({
    controlling,
    pointerInside,
    focusWithin,
    recentActionAt,
    now,
  });

  const onBlurCapture = useCallback((event: FocusEvent<HTMLElement>) => {
    const next = event.relatedTarget;
    if (next instanceof Node && event.currentTarget.contains(next)) return;
    setFocusWithin(false);
  }, []);

  const noteAction = useCallback(() => {
    const stamped = Date.now();
    setRecentActionAt(stamped);
    setNow(stamped);
  }, []);

  return {
    visible,
    bind: {
      onPointerEnter: () => setPointerInside(true),
      onPointerLeave: () => setPointerInside(false),
      onFocusCapture: () => setFocusWithin(true),
      onBlurCapture,
    },
    noteAction,
  };
}
