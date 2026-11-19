import { useState } from "react";
import { Icon } from "../../../lib/icons";

export interface WebpageViewerProps {
  url: string;
  name: string;
}

export function WebpageViewer({ url, name }: WebpageViewerProps) {
  const [device, setDevice] = useState<"desktop" | "tablet" | "mobile">("desktop");
  const [key, setKey] = useState(0);

  const reload = () => setKey((k) => k + 1);

  return (
    <div className="flex h-full w-full min-h-0 flex-col bg-[#0f1117]">
      {/* Device switcher and controls */}
      <div className="flex shrink-0 items-center justify-between border-b border-line bg-[var(--color-surface)] px-4 py-2 text-xs">
        <div className="flex items-center gap-2">
          <div className="inline-flex rounded-lg border border-line bg-[var(--color-canvas)] p-0.5">
            <button
              type="button"
              className={`inline-flex items-center gap-1.5 rounded-md px-2.5 py-1 text-xs font-medium transition-colors ${
                device === "desktop"
                  ? "bg-accent/15 text-accent font-semibold"
                  : "text-muted hover:text-ink"
              }`}
              onClick={() => setDevice("desktop")}
              title="Desktop viewport (100%)"
            >
              <Icon.Desktop size={14} />
              <span>Desktop</span>
            </button>
            <button
              type="button"
              className={`inline-flex items-center gap-1.5 rounded-md px-2.5 py-1 text-xs font-medium transition-colors ${
                device === "tablet"
                  ? "bg-accent/15 text-accent font-semibold"
                  : "text-muted hover:text-ink"
              }`}
              onClick={() => setDevice("tablet")}
              title="Tablet viewport (768px)"
            >
              <Icon.Tablet size={14} />
              <span>Tablet</span>
            </button>
            <button
              type="button"
              className={`inline-flex items-center gap-1.5 rounded-md px-2.5 py-1 text-xs font-medium transition-colors ${
                device === "mobile"
                  ? "bg-accent/15 text-accent font-semibold"
                  : "text-muted hover:text-ink"
              }`}
              onClick={() => setDevice("mobile")}
              title="Mobile viewport (375px)"
            >
              <Icon.Smartphone size={14} />
              <span>Mobile</span>
            </button>
          </div>
        </div>

        <div className="flex items-center gap-2">
          <button
            type="button"
            onClick={reload}
            className="inline-flex items-center gap-1 rounded-lg border border-line bg-[var(--color-surface)] px-2.5 py-1 text-xs text-muted hover:bg-line/40 hover:text-ink transition-colors"
            title="Reload webpage"
          >
            <Icon.Reload size={13} />
            <span>Reload</span>
          </button>

          <a
            href={url}
            target="_blank"
            rel="noopener noreferrer"
            className="inline-flex items-center gap-1 rounded-lg border border-line bg-[var(--color-surface)] px-2.5 py-1 text-xs font-medium text-ink hover:bg-line/40 transition-colors"
            title="Open in new window"
          >
            <Icon.External size={13} />
            <span>Open in new tab</span>
          </a>
        </div>
      </div>

      {/* Frame Canvas */}
      <div className="flex min-h-0 flex-1 items-center justify-center overflow-auto p-4 bg-slate-900/40">
        <div
          className={`h-full transition-all duration-200 overflow-hidden shadow-2xl bg-white ${
            device === "mobile"
              ? "w-[375px] rounded-3xl border-4 border-slate-700 max-h-[812px]"
              : device === "tablet"
                ? "w-[768px] rounded-2xl border-4 border-slate-700 max-h-[1024px]"
                : "w-full rounded-xl border border-line/60"
          }`}
        >
          <iframe
            key={key}
            src={url}
            title={name}
            sandbox="allow-scripts allow-same-origin allow-forms allow-popups"
            className="h-full w-full border-0 bg-white"
          />
        </div>
      </div>
    </div>
  );
}
