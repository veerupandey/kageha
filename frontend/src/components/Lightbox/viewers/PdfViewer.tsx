import { useState } from "react";
import { Icon } from "../../../lib/icons";

export interface PdfViewerProps {
  url: string;
  name: string;
}

export function PdfViewer({ url, name }: PdfViewerProps) {
  const [loadError, setLoadError] = useState(false);

  return (
    <div className="flex h-full w-full min-h-0 flex-col bg-[#1e232a]">
      {/* Fallback bar */}
      <div className="flex shrink-0 items-center justify-between border-b border-line/40 bg-[var(--color-surface)] px-4 py-2 text-xs">
        <span className="text-muted">Viewing PDF document</span>
        <div className="flex items-center gap-2">
          <a
            href={url}
            target="_blank"
            rel="noopener noreferrer"
            className="inline-flex items-center gap-1.5 rounded-lg border border-line bg-[var(--color-surface)] px-2.5 py-1 text-xs font-medium text-ink hover:bg-line/40 transition-colors"
          >
            <Icon.External size={13} />
            <span>Open in new tab</span>
          </a>
        </div>
      </div>

      <div className="relative min-h-0 flex-1 overflow-hidden">
        {loadError ? (
          <div className="flex h-full w-full flex-col items-center justify-center gap-4 p-8 text-center text-white">
            <span className="flex h-16 w-16 items-center justify-center rounded-2xl bg-red-500/20 text-red-400 font-bold text-lg">
              PDF
            </span>
            <div>
              <p className="text-base font-semibold text-white">{name}</p>
              <p className="mt-1 text-xs text-slate-400 max-w-sm">
                Your browser could not embed this PDF preview directly. You can open it in a new window or download it.
              </p>
            </div>
            <a
              href={url}
              target="_blank"
              rel="noopener noreferrer"
              className="rounded-lg bg-accent px-4 py-2 text-xs font-semibold text-white hover:bg-accent/90 shadow-md"
            >
              Open PDF
            </a>
          </div>
        ) : (
          <iframe
            src={url}
            title={name}
            onError={() => setLoadError(true)}
            className="h-full w-full border-0 bg-white"
          />
        )}
      </div>
    </div>
  );
}
