import { useEffect, useState } from "react";
import { formatFileSize, isImageFile } from "../lib/attachments";

export function PendingAttachments({
  files,
  onRemove,
  disabled = false,
}: {
  files: File[];
  onRemove: (index: number) => void;
  disabled?: boolean;
}) {
  const [previews, setPreviews] = useState<string[]>([]);

  useEffect(() => {
    const urls = files.map((file) =>
      isImageFile(file) ? URL.createObjectURL(file) : "",
    );
    setPreviews(urls);

    return () => {
      for (const url of urls) {
        if (url) URL.revokeObjectURL(url);
      }
    };
  }, [files]);

  // Do not render a stale URL while a changed attachment list is settling.
  const visiblePreviews = previews.length === files.length ? previews : [];

  if (!files.length) return null;

  return (
    <div className="ka-pending-attachments mb-2 flex flex-wrap gap-1.5" data-testid="pending-attachments">
      {files.map((file, i) =>
        visiblePreviews[i] ? (
          <span
            key={`${file.name}-${i}`}
            className="relative inline-flex overflow-hidden rounded-lg border border-line bg-canvas"
          >
            <img
              src={visiblePreviews[i]}
              alt={file.name}
              className="h-14 w-14 object-cover"
            />
            <button
              type="button"
              disabled={disabled}
              className="absolute right-0.5 top-0.5 inline-flex h-4 w-4 items-center justify-center rounded-full bg-black/65 text-[0.6rem] text-white hover:bg-black/80"
              aria-label={`Remove ${file.name}`}
              onClick={() => onRemove(i)}
            >
              ✕
            </button>
          </span>
        ) : (
          <span
            key={`${file.name}-${i}`}
            className="inline-flex items-center gap-1 rounded-md bg-canvas px-2 py-0.5 text-xs text-muted"
          >
            <span className="max-w-[12rem] truncate">{file.name}</span>
            <span className="text-faint">{formatFileSize(file.size)}</span>
            <button
              type="button"
              disabled={disabled}
              className="text-faint hover:text-ink"
              aria-label={`Remove ${file.name}`}
              onClick={() => onRemove(i)}
            >
              ✕
            </button>
          </span>
        ),
      )}
    </div>
  );
}
