import type { ReactNode } from "react";
import { fileExt, type CanvasItem } from "../../lib/artifactMedia";
import { PresentationPreview } from "../shared/PresentationPreview";
import { WordDocumentPreview } from "../shared/WordDocumentPreview";
import { ImageViewer } from "./viewers/ImageViewer";
import { MarkdownViewer } from "./viewers/MarkdownViewer";
import { CodeDocViewer } from "./viewers/CodeDocViewer";
import { TableViewer } from "./viewers/TableViewer";
import { JsonDocViewer } from "./viewers/JsonDocViewer";
import { TextDocViewer } from "./viewers/TextDocViewer";
import { WebpageViewer } from "./viewers/WebpageViewer";
import { PdfViewer } from "./viewers/PdfViewer";
import { AudioViewer } from "./viewers/AudioViewer";
import { VideoViewer } from "./viewers/VideoViewer";
import { FileFallback } from "./viewers/FileFallback";

export interface LightboxPreviewProps {
  item: CanvasItem;
  children?: ReactNode;
  onTouchStart?: (e: React.TouchEvent) => void;
  onTouchEnd?: (e: React.TouchEvent) => void;
}

export function LightboxPreview({
  item,
  children,
  onTouchStart,
  onTouchEnd,
}: LightboxPreviewProps) {
  const name = item.caption || item.path.split("/").pop() || "artifact";
  const ext = fileExt(item.path);

  // 1. Image
  if (item.kind === "image" && item.url) {
    return (
      <div
        className="ka-lightbox-preview relative flex h-full w-full min-h-0 flex-1 flex-col overflow-hidden"
        onTouchStart={onTouchStart}
        onTouchEnd={onTouchEnd}
      >
        <ImageViewer
          url={item.url}
          name={name}
          onTouchStart={onTouchStart}
          onTouchEnd={onTouchEnd}
        >
          {children}
        </ImageViewer>
      </div>
    );
  }

  // 2. Video
  if (item.kind === "video" && item.url) {
    return (
      <div
        className="ka-lightbox-preview relative flex h-full w-full min-h-0 flex-1 flex-col overflow-hidden"
        onTouchStart={onTouchStart}
        onTouchEnd={onTouchEnd}
      >
        <VideoViewer
          url={item.url}
          name={name}
          onTouchStart={onTouchStart}
          onTouchEnd={onTouchEnd}
        >
          {children}
        </VideoViewer>
      </div>
    );
  }

  // 3. Audio
  if (item.kind === "audio" && item.url) {
    return (
      <div
        className="ka-lightbox-preview relative flex h-full w-full min-h-0 flex-1 flex-col overflow-hidden"
        onTouchStart={onTouchStart}
        onTouchEnd={onTouchEnd}
      >
        <AudioViewer url={item.url} name={name} />
        {children}
      </div>
    );
  }

  // 4. Presentation
  if (item.kind === "presentation") {
    return (
      <div className="ka-lightbox-preview relative flex h-full w-full min-h-0 flex-1 flex-col overflow-hidden p-0">
        <PresentationPreview item={item} />
        {children}
      </div>
    );
  }

  // 5. Word Document
  if (item.kind === "document" && ext === ".docx") {
    return (
      <div className="ka-lightbox-preview relative flex h-full w-full min-h-0 flex-1 flex-col overflow-hidden p-0">
        <WordDocumentPreview item={item} />
        {children}
      </div>
    );
  }

  // 6. PDF
  if (item.kind === "pdf" && item.url) {
    return (
      <div className="ka-lightbox-preview relative flex h-full w-full min-h-0 flex-1 flex-col overflow-hidden">
        <PdfViewer url={item.url} name={name} />
        {children}
      </div>
    );
  }

  // 7. Webpage (HTML)
  if (item.kind === "webpage" && item.url) {
    return (
      <div className="ka-lightbox-preview relative flex h-full w-full min-h-0 flex-1 flex-col overflow-hidden">
        <WebpageViewer url={item.url} name={name} />
        {children}
      </div>
    );
  }

  // 8. Markdown
  if (item.kind === "markdown") {
    return (
      <div className="ka-lightbox-preview relative flex h-full w-full min-h-0 flex-1 flex-col overflow-hidden">
        <MarkdownViewer url={item.url} initialText={item.text} name={name} />
        {children}
      </div>
    );
  }

  // 9. Tabular Data (CSV, TSV, or spreadsheet)
  if (
    ext === ".csv" ||
    ext === ".tsv" ||
    (item.kind === "spreadsheet" && (ext === ".csv" || ext === ".tsv"))
  ) {
    return (
      <div className="ka-lightbox-preview relative flex h-full w-full min-h-0 flex-1 flex-col overflow-hidden">
        <TableViewer url={item.url} initialText={item.text} name={name} />
        {children}
      </div>
    );
  }

  // 10. JSON
  if (ext === ".json" || ext === ".jsonl" || ext === ".ndjson") {
    return (
      <div className="ka-lightbox-preview relative flex h-full w-full min-h-0 flex-1 flex-col overflow-hidden">
        <JsonDocViewer url={item.url} initialText={item.text} name={name} />
        {children}
      </div>
    );
  }

  // 11. Code
  if (item.kind === "code") {
    return (
      <div className="ka-lightbox-preview relative flex h-full w-full min-h-0 flex-1 flex-col overflow-hidden">
        <CodeDocViewer url={item.url} initialText={item.text} name={name} />
        {children}
      </div>
    );
  }

  // 12. Plain Text / Logs
  if (item.kind === "text") {
    return (
      <div className="ka-lightbox-preview relative flex h-full w-full min-h-0 flex-1 flex-col overflow-hidden">
        <TextDocViewer url={item.url} initialText={item.text} name={name} />
        {children}
      </div>
    );
  }

  // 13. Fallback for other formats
  return (
    <div className="ka-lightbox-preview relative flex h-full w-full min-h-0 flex-1 flex-col overflow-hidden">
      <FileFallback item={item} />
      {children}
    </div>
  );
}
