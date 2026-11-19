import { describe, expect, it } from "vitest";
import {
  ATTACHMENT_ACCEPT,
  MAX_ATTACHMENT_BYTES,
  formatFileSize,
  validateAttachments,
  filesFromClipboard,
} from "./attachments";

describe("attachments", () => {
  it("accepts every document, media, source and archive format", () => {
    expect(ATTACHMENT_ACCEPT).toBe("*/*");
  });

  it("rejects files over the upload limit and keeps valid files", () => {
    const valid = new File(["# notes"], "notes.md", { type: "text/markdown" });
    const tooLarge = new File([new Uint8Array(MAX_ATTACHMENT_BYTES + 1)], "movie.mp4", {
      type: "video/mp4",
    });
    const result = validateAttachments([valid, tooLarge]);
    expect(result.accepted).toEqual([valid]);
    expect(result.rejected[0]).toContain("movie.mp4");
    expect(result.rejected[0]).toContain("max 500 MB");
  });

  it("formats sizes for attachment chips", () => {
    expect(formatFileSize(512)).toBe("512 B");
    expect(formatFileSize(2048)).toBe("2 KB");
    expect(formatFileSize(2 * 1024 * 1024)).toBe("2.0 MB");
  });

  it("extracts pasted image items and gives unnamed blobs a useful filename", () => {
    const blob = new File([new Uint8Array([1, 2, 3])], "", { type: "image/png" });
    const data = {
      items: [{ kind: "file", type: "image/png", getAsFile: () => blob }],
      files: [],
    } as unknown as DataTransfer;
    const files = filesFromClipboard(data);
    expect(files).toHaveLength(1);
    expect(files[0].name).toMatch(/^clipboard-\d+-0\.png$/);
    expect(files[0].type).toBe("image/png");
  });

  it("uses the clipboard item mime when the File has no type or name", () => {
    const blob = new File([new Uint8Array([1, 2, 3])], "", { type: "" });
    const data = {
      items: [{ kind: "file", type: "image/png", getAsFile: () => blob }],
      files: [],
    } as unknown as DataTransfer;
    const files = filesFromClipboard(data);
    expect(files).toHaveLength(1);
    expect(files[0].type).toBe("image/png");
    expect(files[0].name).toMatch(/^clipboard-\d+-0\.png$/);
  });

  it("keeps a named paste and fills jpeg from the clipboard item", () => {
    const named = new File([new Uint8Array([1, 2, 3])], "shot.png", { type: "image/png" });
    const jpeg = new File([new Uint8Array([4, 5, 6])], "", { type: "" });
    expect(
      filesFromClipboard({
        items: [{ kind: "file", type: "image/png", getAsFile: () => named }],
        files: [],
      } as unknown as DataTransfer)[0].name,
    ).toBe("shot.png");
    const files = filesFromClipboard({
      items: [{ kind: "file", type: "image/jpeg", getAsFile: () => jpeg }],
      files: [],
    } as unknown as DataTransfer);
    expect(files[0].type).toBe("image/jpeg");
    expect(files[0].name).toMatch(/^clipboard-\d+-0\.jpg$/);
  });

  it("pulls a pasted screenshot out of HTML data URLs", () => {
    const html =
      '<img src="data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==">';
    const data = {
      items: [],
      files: [],
      getData: (type: string) => (type === "text/html" ? html : ""),
    } as unknown as DataTransfer;
    const files = filesFromClipboard(data);
    expect(files).toHaveLength(1);
    expect(files[0].type).toBe("image/png");
    expect(files[0].name).toMatch(/\.png$/);
  });
});
