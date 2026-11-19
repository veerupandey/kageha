import { useEffect, useLayoutEffect, useRef } from "react";
import type { CanvasHTMLAttributes, RefCallback } from "react";
import { browserFrames } from "../lib/browserFrames";

interface Props extends CanvasHTMLAttributes<HTMLCanvasElement> {
  source: string;
  binding: string;
  surfaceRef: RefCallback<HTMLCanvasElement>;
  onFrame: () => void;
  onFailure: () => void;
  onPaint?: () => void;
}

/** Keep decoded pixels while reconnecting; discard them only for a new target. */
export function BrowserLiveSurface({ source, binding, surfaceRef, onFrame, onFailure, onPaint, ...props }: Props) {
  const canvas = useRef<HTMLCanvasElement | null>(null);
  const callbacks = useRef({ onFrame, onFailure, onPaint });
  const target = useRef({ source, binding });
  callbacks.current = { onFrame, onFailure, onPaint };
  target.current = { source, binding };
  // Source retries retain pixels; a different target must never inherit them.
  useLayoutEffect(() => {
    if (canvas.current) { canvas.current.width = 1; canvas.current.height = 1; }
  }, [binding]);

  useEffect(() => {
    if (!source) return;
    const controller = new AbortController();
    let active = true;
    void (async () => {
      try {
        const response = await fetch(source, { signal: controller.signal, cache: "no-store" });
        if (!response.ok || !response.body) throw new Error("Browser preview unavailable");
        let first = true;
        for await (const bytes of browserFrames(response.body)) {
          const bitmap = await createImageBitmap(new Blob([bytes], { type: "image/jpeg" }));
          try {
            if (!active || target.current.binding !== binding || target.current.source !== source || !canvas.current) return;
            const node = canvas.current;
            const resized = node.width !== bitmap.width || node.height !== bitmap.height;
            const context = node.getContext("2d", { alpha: false, desynchronized: true });
            if (!context) throw new Error("Browser preview drawing unavailable");
            // Decode first, then resize and draw in one task. A resize never
            // presents a cleared canvas between asynchronous frame decodes.
            if (resized) { node.width = bitmap.width; node.height = bitmap.height; }
            context.drawImage(bitmap, 0, 0);
            callbacks.current.onPaint?.();
            if (first || resized) callbacks.current.onFrame();
            first = false;
          } finally { bitmap.close(); }
        }
        if (active) callbacks.current.onFailure();
      } catch {
        if (active) callbacks.current.onFailure();
      }
    })();
    return () => { active = false; controller.abort(); };
  }, [source, binding]);
  return <canvas {...props} data-stream-source={source} ref={node => { canvas.current = node; surfaceRef(node); }} />;
}
