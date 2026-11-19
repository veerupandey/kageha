import "@testing-library/jest-dom/vitest";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
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

describe("Document Viewers for all artifact types", () => {
  beforeEach(() => {
    vi.stubGlobal(
      "fetch",
      vi.fn(() =>
        Promise.resolve({
          ok: true,
          status: 200,
          text: () => Promise.resolve("sample content"),
        } as Response),
      ),
    );
  });

  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  describe("ImageViewer", () => {
    it("renders image and provides zoom controls and modes", () => {
      render(<ImageViewer url="/diagram.png" name="diagram.png" />);
      const img = screen.getByAltText("diagram.png");
      expect(img).toBeInTheDocument();
      expect(img).toHaveAttribute("src", "/diagram.png");

      // Check mode buttons
      expect(screen.getByRole("button", { name: "Fit Screen" })).toBeInTheDocument();
      expect(screen.getByRole("button", { name: "Fit Width" })).toBeInTheDocument();
      expect(screen.getByRole("button", { name: "100%" })).toBeInTheDocument();

      // Zoom in and out
      const zoomInBtn = screen.getByRole("button", { name: "Zoom in" });
      const zoomOutBtn = screen.getByRole("button", { name: "Zoom out" });
      fireEvent.click(zoomInBtn);
      fireEvent.click(zoomOutBtn);

      // Switching to Fit Width
      fireEvent.click(screen.getByRole("button", { name: "Fit Width" }));
      expect(img.style.width).toBe("100%");

      // Switching to 100%
      fireEvent.click(screen.getByRole("button", { name: "100%" }));

      // Double click toggles mode
      fireEvent.doubleClick(img);
      expect(img.style.cursor).toBe("zoom-in");
    });

    it("auto-selects fit-width if the image is tall", () => {
      render(<ImageViewer url="/tall.png" name="tall.png" />);
      const img = screen.getByAltText("tall.png");
      Object.defineProperty(img, "naturalWidth", { value: 600, configurable: true });
      Object.defineProperty(img, "naturalHeight", { value: 1800, configurable: true });
      fireEvent.load(img);
      expect(img.style.width).toBe("100%");
    });
  });

  describe("MarkdownViewer", () => {
    it("renders formatted markdown and allows switching to raw source", () => {
      const md = "# Architecture\n\nThis is a test diagram.";
      render(<MarkdownViewer url="/doc.md" initialText={md} name="doc.md" />);

      expect(screen.getByText("Architecture")).toBeInTheDocument();
      expect(screen.getByText("Formatted")).toBeInTheDocument();
      expect(screen.getByText("Raw Source")).toBeInTheDocument();

      // Toggle to raw
      fireEvent.click(screen.getByText("Raw Source"));
      expect(screen.getByText(/Architecture/)).toBeInTheDocument();
      expect(screen.getByText(/This is a test diagram/)).toBeInTheDocument();

      // Copy button
      expect(screen.getByRole("button", { name: "Copy" })).toBeInTheDocument();
    });
  });

  describe("CodeDocViewer", () => {
    it("renders code with line numbers and allows toggling line wrap", () => {
      const code = "def hello():\n    return 'world'\n";
      render(<CodeDocViewer url="/app.py" initialText={code} name="app.py" />);

      expect(screen.getByText("def")).toBeInTheDocument();
      expect(screen.getByText("hello")).toBeInTheDocument();
      expect(screen.getByText("python")).toBeInTheDocument();
      expect(screen.getByText(/3 lines · \d+ characters/)).toBeInTheDocument();

      const wrapBtn = screen.getByRole("button", { name: /Wrap lines/i });
      fireEvent.click(wrapBtn);
      expect(wrapBtn).toBeInTheDocument();
    });
  });

  describe("TableViewer", () => {
    it("parses CSV data into interactive rows and columns", () => {
      const csv = "ID,Name,Role\n1,Alice,Engineer\n2,Bob,Designer\n";
      render(<TableViewer url="/data.csv" initialText={csv} name="data.csv" />);

      expect(screen.getByText("ID")).toBeInTheDocument();
      expect(screen.getByText("Alice")).toBeInTheDocument();
      expect(screen.getByText("Designer")).toBeInTheDocument();
      expect(screen.getByText("2 rows · 3 columns")).toBeInTheDocument();

      // Filter rows
      const searchInput = screen.getByPlaceholderText("Filter rows…");
      fireEvent.change(searchInput, { target: { value: "Alice" } });
      expect(screen.getByText("Alice")).toBeInTheDocument();
      expect(screen.queryByText("Bob")).not.toBeInTheDocument();

      // Toggle Raw CSV
      fireEvent.click(screen.getByText("Raw CSV"));
      expect(screen.getByText((content) => content.includes("Alice,Engineer"))).toBeInTheDocument();
    });
  });

  describe("JsonDocViewer", () => {
    it("pretty-prints JSON and allows raw toggle", () => {
      const json = '{"key":"value","count":42}';
      render(<JsonDocViewer url="/test.json" initialText={json} name="test.json" />);

      expect(screen.getByText('"key"')).toBeInTheDocument();
      expect(screen.getByText("Formatted")).toBeInTheDocument();
      expect(screen.getByText("Raw")).toBeInTheDocument();

      fireEvent.click(screen.getByText("Raw"));
      expect(screen.getByText('"value"')).toBeInTheDocument();
    });
  });

  describe("TextDocViewer", () => {
    it("renders text with line numbers and supports search filtering", () => {
      const text = "Server start\nListening on port 8080\nShutting down";
      render(<TextDocViewer url="/log.txt" initialText={text} name="log.txt" />);

      expect(screen.getByText("Listening on port 8080")).toBeInTheDocument();
      expect(screen.getByText(/3 lines · \d+ characters/)).toBeInTheDocument();

      // Filter
      const search = screen.getByPlaceholderText("Find in file…");
      fireEvent.change(search, { target: { value: "port" } });
      expect(screen.getByText("Listening on port 8080")).toBeInTheDocument();
      expect(screen.queryByText("Server start")).not.toBeInTheDocument();
    });
  });

  describe("WebpageViewer", () => {
    it("provides device switching for HTML artifacts", () => {
      render(<WebpageViewer url="/site.html" name="site.html" />);

      expect(screen.getByTitle("Desktop viewport (100%)")).toBeInTheDocument();
      expect(screen.getByTitle("Tablet viewport (768px)")).toBeInTheDocument();
      expect(screen.getByTitle("Mobile viewport (375px)")).toBeInTheDocument();

      fireEvent.click(screen.getByTitle("Mobile viewport (375px)"));
      const iframe = screen.getByTitle("site.html");
      expect(iframe).toHaveAttribute("src", "/site.html");
    });
  });

  describe("PdfViewer", () => {
    it("renders iframe for PDF documents", () => {
      render(<PdfViewer url="/doc.pdf" name="doc.pdf" />);
      const iframe = screen.getByTitle("doc.pdf");
      expect(iframe).toHaveAttribute("src", "/doc.pdf");
      expect(screen.getByRole("link", { name: /Open in new tab/i })).toHaveAttribute("href", "/doc.pdf");
    });
  });

  describe("AudioViewer", () => {
    it("resets playback and timing between files while keeping volume and speed preferences", () => {
      const play = vi.spyOn(HTMLMediaElement.prototype, "play").mockImplementation(function (this: HTMLMediaElement) {
        fireEvent.play(this);
        return Promise.resolve();
      });
      const view = render(<AudioViewer url="/first.mp3" name="first" />);
      const first = view.container.querySelector("audio")!;
      Object.defineProperty(first, "duration", { configurable: true, value: 120 });
      fireEvent.loadedMetadata(first);
      first.currentTime = 30;
      fireEvent.timeUpdate(first);
      fireEvent.click(screen.getByRole("button", { name: "Play" }));
      expect(screen.getByRole("button", { name: "Pause" })).toBeInTheDocument();
      fireEvent.click(screen.getByRole("button", { name: "1x" }));
      view.rerender(<AudioViewer url="/second.mp3" name="second" />);
      const second = view.container.querySelector("audio")!;
      expect(second).not.toBe(first);
      expect(screen.getByRole("button", { name: "Play" })).toBeInTheDocument();
      expect(screen.queryByText("0:30")).not.toBeInTheDocument();
      expect(screen.queryByText("2:00")).not.toBeInTheDocument();
      expect(second.playbackRate).toBe(1.25);
      fireEvent.click(screen.getByRole("button", { name: "Play" }));
      expect(play).toHaveBeenCalledTimes(2);
      expect(screen.getByRole("button", { name: "Pause" })).toBeInTheDocument();
      fireEvent.pause(second);
      expect(screen.getByRole("button", { name: "Play" })).toBeInTheDocument();
    });

    it("reports rejected playback without showing a false playing state", async () => {
      vi.spyOn(HTMLMediaElement.prototype, "play").mockRejectedValue(new Error("Playback denied"));
      render(<AudioViewer url="/audio.mp3" name="audio" />);
      await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Play" })); });
      expect(screen.getByRole("alert")).toHaveTextContent("Could not play this recording");
      expect(screen.getByRole("button", { name: "Play" })).toBeInTheDocument();
    });

    it("ignores an old recording's rejected play request after navigation", async () => {
      let reject!: (error: Error) => void;
      vi.spyOn(HTMLMediaElement.prototype, "play").mockImplementation(() => new Promise((_, fail) => { reject = fail; }));
      const view = render(<AudioViewer url="/old.mp3" name="old" />);
      fireEvent.click(screen.getByRole("button", { name: "Play" }));
      view.rerender(<AudioViewer url="/new.mp3" name="new" />);
      await act(async () => { reject(new Error("Old request failed")); });
      expect(screen.queryByRole("alert")).not.toBeInTheDocument();
      expect(screen.getByRole("button", { name: "Play" })).toBeInTheDocument();
    });
    it("renders audio player with controls and playback speed", () => {
      render(<AudioViewer url="/audio.mp3" name="recording.mp3" />);
      expect(screen.getByText("recording.mp3")).toBeInTheDocument();
      const playBtn = screen.getByRole("button", { name: "Play" });
      expect(playBtn).toBeInTheDocument();

      const speedBtn = screen.getByRole("button", { name: "1x" });
      fireEvent.click(speedBtn);
      expect(screen.getByRole("button", { name: "1.25x" })).toBeInTheDocument();
    });
  });

  describe("VideoViewer", () => {
    it("renders video element with speed and loop controls", () => {
      render(<VideoViewer url="/video.mp4" name="demo.mp4" />);
      expect(screen.getByRole("button", { name: "1x" })).toBeInTheDocument();
      const loopBtn = screen.getByRole("button", { name: "Loop" });
      expect(loopBtn).toBeInTheDocument();
      fireEvent.click(loopBtn);
    });
  });

  describe("FileFallback", () => {
    it("renders deliverable card with open and download links", () => {
      render(
        <FileFallback
          item={{
            path: "archive.zip",
            caption: "archive.zip",
            url: "/archive.zip",
            kind: "archive",
            size: 1048576,
          }}
        />,
      );
      expect(screen.getByText("archive.zip")).toBeInTheDocument();
      expect(screen.getByRole("link", { name: /Open file/i })).toHaveAttribute("href", "/archive.zip");
      expect(screen.getByRole("link", { name: /Download/i })).toHaveAttribute("href", "/archive.zip");
    });
  });
});
