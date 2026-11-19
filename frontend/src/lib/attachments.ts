/** Shared browser-side attachment policy for every composer. */
export const MAX_ATTACHMENT_BYTES = 500 * 1024 * 1024;

// Accept every file type. The server stores uploads as inert workspace input
// files; validation is size-based rather than extension-based so new document,
// image, video, audio, archive, and source formats work without UI changes.
export const ATTACHMENT_ACCEPT = "*/*";

export function formatFileSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(bytes >= 10 * 1024 * 1024 ? 0 : 1)} MB`;
}

export function validateAttachments(files: File[]): {
  accepted: File[];
  rejected: string[];
} {
  const accepted: File[] = [];
  const rejected: string[] = [];
  for (const file of files) {
    if (file.size > MAX_ATTACHMENT_BYTES) {
      rejected.push(`${file.name} (${formatFileSize(file.size)}; max 500 MB)`);
      continue;
    }
    accepted.push(file);
  }
  return { accepted, rejected };
}

export function isImageFile(file: File): boolean {
  return file.type.startsWith("image/") || /\.(png|jpe?g|gif|webp|svg)$/i.test(file.name);
}

function extFromMime(mime: string): string {
  const subtype = (mime.split("/")[1] || "").split(";")[0].replace("+xml", "");
  if (!subtype) return "bin";
  return subtype === "jpeg" ? "jpg" : subtype;
}

function namedClipboardFile(file: File, index: number, mimeHint = ""): File {
  const mime = (file.type || mimeHint || "application/octet-stream").split(";")[0].trim();
  const named = Boolean(file.name && /\.[a-z0-9]+$/i.test(file.name));
  if (named && file.type) return file;
  const ext = named
    ? file.name.split(".").pop() || "bin"
    : extFromMime(mime);
  const name = named ? file.name : `clipboard-${Date.now()}-${index}.${ext}`;
  if (name === file.name && mime === file.type) return file;
  return new File([file], name, {
    type: mime,
    lastModified: file.lastModified || Date.now(),
  });
}

function filesFromDataImages(html: string): File[] {
  const out: File[] = [];
  const re = /src=["'](data:(image\/[a-zA-Z0-9+.-]+);base64,([A-Za-z0-9+/]+=*))["']/gi;
  let match: RegExpExecArray | null;
  while ((match = re.exec(html))) {
    const mime = match[2] || "image/png";
    const b64 = match[3];
    try {
      const bytes = Uint8Array.from(atob(b64), (ch) => ch.charCodeAt(0));
      const ext = extFromMime(mime);
      out.push(
        new File([bytes], `clipboard-${Date.now()}-${out.length}.${ext}`, {
          type: mime,
          lastModified: Date.now(),
        }),
      );
    } catch {
      /* ignore malformed data URLs */
    }
  }
  return out;
}

/** Extract files from clipboard data, including screenshots copied as items. */
export function filesFromClipboard(
  data: DataTransfer | null | undefined,
): File[] {
  if (!data) return [];
  const files: File[] = [];
  const seen = new Set<string>();
  const push = (file: File, mimeHint = "") => {
    const named = namedClipboardFile(file, files.length, mimeHint);
    const key = `${named.name}:${named.size}:${named.type}`;
    if (seen.has(key)) return;
    seen.add(key);
    files.push(named);
  };

  for (const item of Array.from(data.items || [])) {
    if (item.kind !== "file" && !item.type.startsWith("image/")) continue;
    const file = item.getAsFile();
    if (file) push(file, item.type);
  }
  if (files.length) return files;
  for (const file of Array.from(data.files || [])) push(file);
  if (files.length) return files;
  const html = typeof data.getData === "function" ? data.getData("text/html") : "";
  if (html) {
    for (const file of filesFromDataImages(html)) push(file);
  }
  return files;
}
