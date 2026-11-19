import "@testing-library/jest-dom/vitest";
import { afterEach, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import JSZip from "jszip";
import type { CanvasItem } from "../../lib/artifactMedia";
import { WordDocumentPreview } from "./WordDocumentPreview";

const item: CanvasItem = {
  path: "artifacts/report.docx", caption: "report.docx", kind: "document", url: "/report.docx",
};

async function documentBytes(text: string) {
  const zip = new JSZip();
  zip.file("[Content_Types].xml", `<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>`);
  zip.file("_rels/.rels", `<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>`);
  zip.file("word/document.xml", `<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:rPr><w:b/></w:rPr><w:t>${text}</w:t></w:r></w:p><w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="3000"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t>Table cell</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:sectPr><w:pgSz w:w="12240" w:h="15840"/></w:sectPr></w:body></w:document>`);
  return zip.generateAsync({ type: "arraybuffer" });
}

function documentRoot() {
  return screen.getByLabelText("Word document preview: report.docx")
    .querySelector("div")?.firstElementChild?.shadowRoot;
}

afterEach(() => { cleanup(); vi.unstubAllGlobals(); });

it("renders real DOCX text, formatting and tables without exposing its styles to the app", async () => {
  const bytes = await documentBytes("Complete Word report");
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue({ ok: true, arrayBuffer: async () => bytes }));
  render(<WordDocumentPreview item={item} compact />);
  expect(screen.getByRole("status")).toHaveTextContent("Opening document");
  await waitFor(() => expect(documentRoot()?.textContent).toContain("Complete Word report"));
  expect(documentRoot()?.querySelector("table")?.textContent).toContain("Table cell");
  expect(documentRoot()?.querySelector("span")?.style.fontWeight).toBe("bold");
  await waitFor(() => expect(screen.queryByRole("status")).not.toBeInTheDocument());
  expect(document.querySelector("style.word-document")).toBeNull();
});

it("keeps a download available when the file is invalid or cannot be fetched", async () => {
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue({ ok: false, status: 403 }));
  render(<WordDocumentPreview item={item} />);
  expect(await screen.findByRole("alert")).toHaveTextContent("Could not preview");
  expect(screen.getByRole("link", { name: "Download DOCX" })).toHaveAttribute("href", item.url);
});

it("rejects corrupt Word bytes with a readable fallback", async () => {
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue({ ok: true, arrayBuffer: async () => new ArrayBuffer(3) }));
  render(<WordDocumentPreview item={item} />);
  expect(await screen.findByRole("alert")).toHaveTextContent("Could not preview");
});

it("ignores an old request after another document is selected", async () => {
  const bytes = await documentBytes("Newest document");
  let finishOld: ((response: unknown) => void) | undefined;
  const fetchFile = vi.fn()
    .mockImplementationOnce(() => new Promise((resolve) => { finishOld = resolve; }))
    .mockResolvedValueOnce({ ok: true, arrayBuffer: async () => bytes });
  vi.stubGlobal("fetch", fetchFile);
  const { rerender } = render(<WordDocumentPreview item={item} />);
  rerender(<WordDocumentPreview item={{ ...item, url: "/new.docx", modifiedAt: 2 }} />);
  await waitFor(() => expect(documentRoot()?.textContent).toContain("Newest document"));
  finishOld?.({ ok: false, status: 404 });
  await waitFor(() => expect(fetchFile.mock.calls[0][1].signal.aborted).toBe(true));
  expect(documentRoot()?.textContent).toContain("Newest document");
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
});
