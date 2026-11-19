import type { CanvasItem } from "../../../lib/artifactMedia";
import { formatBytes, kindLabel } from "../../../lib/artifactMedia";
import { Icon } from "../../../lib/icons";

export interface FileFallbackProps {
  item: CanvasItem;
}

export function FileFallback({ item }: FileFallbackProps) {
  const name = item.caption || item.path.split("/").pop() || "artifact";
  const ext = item.path.split(".").pop()?.toUpperCase() || "FILE";

  return (
    <div className="flex h-full w-full min-h-0 items-center justify-center bg-[var(--color-surface)] p-6">
      <div className="mx-6 flex max-w-md flex-col items-center gap-4 rounded-3xl border border-line bg-[var(--color-canvas)] p-8 text-center shadow-2xl">
        <span className="flex h-20 w-20 items-center justify-center rounded-2xl bg-accent/15 text-lg font-black tracking-wider text-accent shadow-inner">
          {ext.slice(0, 5)}
        </span>

        <div>
          <span className="break-all text-base font-semibold text-ink">{name}</span>
          <p className="mt-1 text-xs text-muted">
            {kindLabel(item.kind)} {item.size ? `· ${formatBytes(item.size)}` : ""}
          </p>
        </div>

        <span className="text-xs leading-relaxed text-faint">
          This format cannot be rendered safely by the browser. Use Download in the toolbar to save it, then open it in its native application.
        </span>

        <div className="mt-2 flex items-center gap-3">
          <a
            href={item.url}
            target="_blank"
            rel="noreferrer"
            className="inline-flex items-center gap-1.5 rounded-xl bg-accent px-4 py-2 text-xs font-semibold text-white shadow-md transition-all hover:bg-accent/90"
          >
            <Icon.External size={14} />
            <span>Open file</span>
          </a>

          <a
            href={item.url}
            download={name}
            className="inline-flex items-center gap-1.5 rounded-xl border border-line bg-[var(--color-surface)] px-4 py-2 text-xs font-semibold text-ink transition-all hover:bg-line/40"
          >
            <Icon.Export size={14} />
            <span>Download</span>
          </a>
        </div>
      </div>
    </div>
  );
}
