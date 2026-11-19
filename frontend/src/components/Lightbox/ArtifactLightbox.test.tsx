import "@testing-library/jest-dom/vitest";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import type { CanvasItem } from "../../lib/artifactMedia";
import { useAppStore } from "../../store";
import { ArtifactLightbox } from "./ArtifactLightbox";

const imageItem: CanvasItem = {
  path: "artifacts/image_01.png",
  url: "/api/sessions/s1/files/artifacts/image_01.png",
  kind: "image",
  caption: "image_01.png",
  size: 991_300,
};

const codeItem: CanvasItem = {
  path: "artifacts/demo.py",
  url: "/api/sessions/s1/files/artifacts/demo.py",
  kind: "code",
  caption: "demo.py",
};

describe("ArtifactLightbox mobile layout", () => {
  beforeEach(() => {
    // Code/text/markdown previews fetch their body asynchronously. Stub fetch
    // so the state update is deterministic and can be flushed inside act().
    vi.stubGlobal(
      "fetch",
      vi.fn(() =>
        Promise.resolve({ text: () => Promise.resolve("print('demo')") } as Response),
      ),
    );
    useAppStore.setState({
      canvasItems: [imageItem, codeItem],
      sessionId: "s1",
    });
  });

  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
    document.body.classList.remove("ka-lightbox-open");
    useAppStore.setState({ canvasItems: [], sessionId: null });
  });

  it("locks body scroll and shows the media layout with Download + Close only", () => {
    const onClose = vi.fn();
    render(
      <ArtifactLightbox
        itemPath={imageItem.path}
        onClose={onClose}
        onNavigate={() => {}}
      />,
    );

    expect(document.body).toHaveClass("ka-lightbox-open");
    expect(document.querySelector(".ka-lightbox--media")).toBeInTheDocument();
    expect(screen.getByAltText("image_01.png")).toHaveAttribute(
      "src",
      imageItem.url,
    );
    expect(screen.getByText("1 / 2")).toBeInTheDocument();

    // Only Download + Close actions remain (Remix/Copy removed).
    expect(screen.getByRole("button", { name: "Download" })).toBeInTheDocument();
    const close = screen.getByRole("button", { name: "Close" });
    expect(close).toBeInTheDocument();
    fireEvent.click(close);
    expect(onClose).toHaveBeenCalled();
  });

  it("triggers a download via an anchor click", () => {
    const clickSpy = vi
      .spyOn(HTMLAnchorElement.prototype, "click")
      .mockImplementation(() => {});
    render(
      <ArtifactLightbox
        itemPath={imageItem.path}
        onClose={() => {}}
        onNavigate={() => {}}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Download" }));
    expect(clickSpy).toHaveBeenCalled();
    clickSpy.mockRestore();
  });

  it("uses the code layout (solid toolbar) for code artifacts", async () => {
    render(
      <ArtifactLightbox
        itemPath={codeItem.path}
        onClose={() => {}}
        onNavigate={() => {}}
      />,
    );

    expect(document.querySelector(".ka-lightbox--media")).not.toBeInTheDocument();
    expect(document.querySelector(".ka-lightbox-toolbar")).toBeInTheDocument();

    // Flush the async code fetch so its setState runs inside act().
    await act(async () => {
      await Promise.resolve();
    });
  });

  it("supports swipe navigation on the preview area", () => {
    const onNavigate = vi.fn();
    render(
      <ArtifactLightbox
        itemPath={imageItem.path}
        onClose={() => {}}
        onNavigate={onNavigate}
      />,
    );

    const preview = document.querySelector(".ka-lightbox-preview");
    expect(preview).toBeTruthy();

    fireEvent.touchStart(preview!, { touches: [{ clientX: 180, clientY: 200 }] });
    fireEvent.touchEnd(preview!, { changedTouches: [{ clientX: 80, clientY: 205 }] });
    expect(onNavigate).toHaveBeenCalledWith("next");

    onNavigate.mockClear();
    fireEvent.touchStart(preview!, { touches: [{ clientX: 80, clientY: 200 }] });
    fireEvent.touchEnd(preview!, { changedTouches: [{ clientX: 180, clientY: 205 }] });
    expect(onNavigate).toHaveBeenCalledWith("prev");
  });

  it("removes body scroll lock on unmount", () => {
    const { unmount } = render(
      <ArtifactLightbox
        itemPath={imageItem.path}
        onClose={() => {}}
        onNavigate={() => {}}
      />,
    );
    unmount();
    expect(document.body).not.toHaveClass("ka-lightbox-open");
  });

  it("keeps cursor arrows in the CSV filter while preserving gallery shortcuts and Escape", () => {
    const table: CanvasItem = { path: "data.csv", url: "/data.csv", kind: "spreadsheet", caption: "data.csv", text: "Name\nAlice" };
    useAppStore.setState({ canvasItems: [table, imageItem] });
    const onNavigate = vi.fn(), onClose = vi.fn();
    render(<ArtifactLightbox itemPath={table.path} onClose={onClose} onNavigate={onNavigate} />);
    const filter = screen.getByPlaceholderText("Filter rows…");
    filter.focus();
    fireEvent.change(filter, { target: { value: "Ali" } });
    expect(fireEvent.keyDown(filter, { key: "ArrowLeft" })).toBe(true);
    expect(fireEvent.keyDown(filter, { key: "ArrowRight", shiftKey: true })).toBe(true);
    expect(onNavigate).not.toHaveBeenCalled();
    const close = screen.getByRole("button", { name: "Close" });
    close.focus();
    fireEvent.keyDown(close, { key: "ArrowRight" });
    expect(onNavigate).toHaveBeenCalledWith("next");
    fireEvent.keyDown(filter, { key: "Escape" });
    expect(onClose).toHaveBeenCalledOnce();
  });

  it("preserves slider and contenteditable keyboard controls", () => {
    const onNavigate = vi.fn();
    render(<ArtifactLightbox itemPath={imageItem.path} onClose={vi.fn()} onNavigate={onNavigate} />);
    const dialog = screen.getByRole("dialog");
    for (const tag of ["input", "textarea", "select", "audio", "video", "div"]) {
      const element = document.createElement(tag);
      if (tag === "div") element.setAttribute("contenteditable", "true");
      dialog.append(element);
      expect(fireEvent.keyDown(element, { key: "ArrowRight" })).toBe(true);
    }
    expect(onNavigate).not.toHaveBeenCalled();
  });
});
