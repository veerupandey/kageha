import type { CanvasItem } from "../../lib/artifactMedia";
import type { ArtifactFilter } from "./ThreadHeader";

export function matchesFilter(item: CanvasItem, filter: ArtifactFilter): boolean {
  if (filter === "all") return true;
  if (filter === "images") return item.kind === "image";
  if (filter === "video") return item.kind === "video";
  if (filter === "code") return item.kind === "code";
  if (filter === "documents")
    return (
      item.kind === "pdf" ||
      item.kind === "document" ||
      item.kind === "text" ||
      item.kind === "spreadsheet" ||
      item.kind === "presentation"
    );
  if (filter === "webpages") return item.kind === "markdown" || item.kind === "webpage";
  return true;
}

/** Keep list/preview selection stable when the header filter changes. */
export function resolveArtifactSelection(
  filtered: CanvasItem[],
  selectedPath: string | null,
  storePath: string | null,
  turnPaths: Set<string>,
): { path: string | null; syncStore: boolean } {
  if (filtered.length === 0) return { path: null, syncStore: false };
  if (storePath && filtered.some((item) => item.path === storePath)) {
    return { path: storePath, syncStore: false };
  }
  if (selectedPath && filtered.some((item) => item.path === selectedPath)) {
    return { path: selectedPath, syncStore: false };
  }
  const turnItem =
    filtered.find(
      (item) =>
        turnPaths.has(item.path) &&
        (item.kind === "webpage" || item.kind === "markdown" || item.kind === "image"),
    ) || filtered.find((item) => turnPaths.has(item.path));
  return { path: turnItem?.path || filtered[0].path, syncStore: true };
}
