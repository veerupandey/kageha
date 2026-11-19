import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import {
  browserStatePollMs,
  browserCanvasSize,
  browserStreamUrl,
  browserBootCopy,
  canScreencast,
  panelViewportFor,
  isBlankBrowserUrl,
  isEmbeddableNovncUrl,
  isEmbeddedHost,
  showEmbeddedGuest,
  displayHost,
  needsConsentChoice,
  highlightRectFromFit,
  openOutsideKind,
  openOutsideTitle,
  previewFitLayout,
  previewPointFromFit,
  screencastViewport,
  shouldSamplePointer,
  stageAddressLabel,
  streamRetryDelayMs,
  streamRetryKey,
  formatTabTitle,
  pageProtocolInfo,
  getArtifactUrl,
  normalizeBrowserUrl,
} from "./browserPreview";

describe("browser canvas sizing", () => {
  const viewport = { width: 1280, height: 960 };
  it("fits a desktop viewport into a tall panel without stretching or enlarging it", () => {
    expect(browserCanvasSize(viewport, { width: 640, height: 1000 }, "fit"))
      .toEqual({ width: 640, height: 480 });
    expect(browserCanvasSize(viewport, { width: 2000, height: 1600 }, "fit"))
      .toEqual(viewport);
  });
  it("maps clicks correctly at explicit zoom levels with a Retina capture", () => {
    for (const zoom of [50, 75, 100, 125, 150]) {
      const canvas = browserCanvasSize(viewport, { width: 640, height: 1000 }, zoom);
      expect(canvas.width / canvas.height).toBeCloseTo(1280 / 960);
      expect(previewPointFromFit({
        clientX: 80 + canvas.width / 2, clientY: 60 + canvas.height / 2,
        rect: { left: 80, top: 60, ...canvas }, naturalWidth: 2560, naturalHeight: 1920,
        viewport, fit: "contain",
      })).toEqual({ x: 640, y: 480 });
    }
  });
});

it("keeps rendered screenshot alignment consistent with input and highlight coordinates", () => {
  const css = readFileSync("src/styles/app.css", "utf8");
  const rule = css.match(/\.browser-stage-shot\.is-contain\s*\{([^}]+)\}/)?.[1];
  expect(rule).toMatch(/object-position:\s*center\s*;/);
  const layout = previewFitLayout({ boxWidth: 600, boxHeight: 900, naturalWidth: 1200,
    naturalHeight: 600, fit: "contain" });
  expect(layout.offsetY).toBe(300);
  const point = previewPointFromFit({ clientX: 300, clientY: 450,
    rect: { left: 0, top: 0, width: 600, height: 900 }, naturalWidth: 1200,
    naturalHeight: 600, viewport: { width: 1200, height: 600 }, fit: "contain" });
  expect(point).toEqual({ x: 600, y: 300 });
});

it("does not show the Electron guest for a disconnected chosen external profile", () => {
  expect(showEmbeddedGuest({ embeddedHost: true, backend: "comet", selectedBackend: "comet", connected: false })).toBe(false);
});

it("scopes stream identity and retry state to the current binding", () => {
  expect(browserStreamUrl("session-1", 0, "instance:4")).toContain("binding=instance%3A4");
  expect(streamRetryKey({ backend: "comet", connected: true, bindingId: "instance:4" }))
    .not.toBe(streamRetryKey({ backend: "comet", connected: true, bindingId: "instance:5" }));
});

describe("streamRetryKey", () => {
  it("changes the retry key when the server attaches a real page", () => {
    // First poll: no page of ours, a guessed tab, stream 409s.
    const passive = streamRetryKey({
      backend: "chrome",
      connected: true,
      screencast: { available: false },
    });
    // Later poll: the agent attached its own tab; the stream must be retried.
    const owned = streamRetryKey({
      backend: "chrome",
      connected: true,
      screencast: { available: true },
    });
    expect(owned).not.toBe(passive);
  });

  it("keeps the key stable when nothing relevant changed, so failures stay down", () => {
    const state = { backend: "chromium", connected: true, screencast: { available: true } };
    expect(streamRetryKey(state)).toBe(streamRetryKey({ ...state }));
  });
});

describe("panelViewportFor", () => {
  it("reflows a controlled page at the panel's real dimensions without scaling text down", () => {
    expect(panelViewportFor({ width: 862, height: 848 })).toEqual({ width: 862, height: 848 });
    expect(panelViewportFor({ width: 319, height: 375 })).toEqual({ width: 360, height: 423 });
    expect(panelViewportFor({ width: 300, height: 2800 })).toEqual({ width: 360, height: 2400 });
    const viewport = panelViewportFor({ width: 862, height: 848 });
    expect(browserCanvasSize(viewport, viewport, "fit")).toEqual(viewport);
    expect(previewPointFromFit({
      clientX: 431, clientY: 424, rect: { left: 0, top: 0, ...viewport },
      naturalWidth: 1724, naturalHeight: 1696, viewport, fit: "contain",
    })).toEqual({ x: 431, y: 424 });
  });
});

describe("previewPointFromFit", () => {
  const viewport = { width: 1000, height: 800 };

  it("subtracts horizontal letterboxing in contain mode", () => {
    const point = previewPointFromFit({
      clientX: 50,
      clientY: 40,
      rect: { left: 0, top: 0, width: 200, height: 80 },
      naturalWidth: 1000,
      naturalHeight: 800,
      viewport,
      fit: "contain",
    });
    expect(point.x).toBeCloseTo(0);
    expect(point.y).toBeCloseTo(400);
  });

  it("subtracts vertical letterboxing in contain mode", () => {
    const point = previewPointFromFit({
      clientX: 50,
      clientY: 60,
      rect: { left: 0, top: 0, width: 100, height: 120 },
      naturalWidth: 1000,
      naturalHeight: 800,
      viewport,
      fit: "contain",
    });
    expect(point.x).toBeCloseTo(500);
    expect(point.y).toBeCloseTo(400);
  });

  it("clamps points that land in the letterbox to the native viewport", () => {
    const point = previewPointFromFit({
      clientX: 10,
      clientY: 10,
      rect: { left: 0, top: 0, width: 200, height: 80 },
      naturalWidth: 1000,
      naturalHeight: 800,
      viewport,
      fit: "contain",
    });
    expect(point.x).toBe(0);
    expect(point.y).toBeCloseTo(100);
  });

  it("maps cover-mode clicks through the cropped overflow", () => {
    const point = previewPointFromFit({
      clientX: 50,
      clientY: 40,
      rect: { left: 0, top: 0, width: 100, height: 80 },
      naturalWidth: 500,
      naturalHeight: 800,
      viewport: { width: 500, height: 800 },
      fit: "cover",
    });
    expect(point.x).toBeCloseTo(250);
    expect(point.y).toBeCloseTo(400);
  });
});

describe("highlightRectFromFit", () => {
  it("offsets contain highlights through horizontal letterboxing", () => {
    const layout = previewFitLayout({
      boxWidth: 200,
      boxHeight: 80,
      naturalWidth: 1000,
      naturalHeight: 800,
      fit: "contain",
    });
    const box = highlightRectFromFit({
      highlight: { x: 0, y: 0, width: 1000, height: 800 },
      boxWidth: 200,
      boxHeight: 80,
      naturalWidth: 1000,
      naturalHeight: 800,
      viewport: { width: 1000, height: 800 },
      fit: "contain",
    });
    expect(layout.offsetX).toBeCloseTo(50);
    expect(box.left).toBeCloseTo(50);
    expect(box.top).toBeCloseTo(0);
    expect(box.width).toBeCloseTo(100);
    expect(box.height).toBeCloseTo(80);
  });

  it("offsets contain highlights through vertical letterboxing", () => {
    const box = highlightRectFromFit({
      highlight: { x: 250, y: 200, width: 500, height: 400 },
      boxWidth: 100,
      boxHeight: 120,
      naturalWidth: 1000,
      naturalHeight: 800,
      viewport: { width: 1000, height: 800 },
      fit: "contain",
    });
    expect(box.left).toBeCloseTo(25);
    expect(box.top).toBeCloseTo(40);
    expect(box.width).toBeCloseTo(50);
    expect(box.height).toBeCloseTo(40);
  });

  it("maps cover highlights through cropped overflow", () => {
    const box = highlightRectFromFit({
      highlight: { x: 0, y: 0, width: 500, height: 800 },
      boxWidth: 100,
      boxHeight: 80,
      naturalWidth: 500,
      naturalHeight: 800,
      viewport: { width: 500, height: 800 },
      fit: "cover",
    });
    expect(box.left).toBeCloseTo(0);
    expect(box.top).toBeCloseTo(-40);
    expect(box.width).toBeCloseTo(100);
    expect(box.height).toBeCloseTo(160);
  });
});

describe("stageAddressLabel", () => {
  it("pairs a page title with the host", () => {
    expect(displayHost("https://www.bareandfair.com/products/oil")).toBe("bareandfair.com/products/oil");
    expect(stageAddressLabel("Triphala Navel Oil", "https://www.bareandfair.com/products/oil")).toBe(
      "Triphala Navel Oil · bareandfair.com/products/oil",
    );
  });

  it("does not repeat the host when the title already names it", () => {
    expect(stageAddressLabel("bareandfair.com", "https://bareandfair.com/")).toBe("bareandfair.com");
  });
});

describe("showEmbeddedGuest", () => {
  it("is only for the desktop host on the embedded backend", () => {
    expect(isEmbeddedHost({})).toBe(false);
    expect(isEmbeddedHost({ kageha: { embedded: true } })).toBe(true);
    expect(showEmbeddedGuest({ embeddedHost: false, backend: "embedded", connected: true })).toBe(false);
    expect(showEmbeddedGuest({ embeddedHost: true, backend: "embedded", connected: true })).toBe(true);
    expect(showEmbeddedGuest({ embeddedHost: true, backend: "chromium", connected: true })).toBe(false);
    expect(showEmbeddedGuest({ embeddedHost: true, backend: "lightpanda", connected: false })).toBe(true);
    expect(
      showEmbeddedGuest({
        embeddedHost: true,
        backend: "embedded",
        connected: true,
        novnc: "http://127.0.0.1/vnc",
      }),
    ).toBe(false);
  });
});

describe("canScreencast", () => {
  it("allows paint-capable backends only when connected and not on noVNC", () => {
    expect(canScreencast({ backend: "chromium", connected: true })).toBe(true);
    expect(canScreencast({ backend: "embedded", connected: true })).toBe(true);
    expect(canScreencast({ backend: "chrome", connected: true })).toBe(true);
    expect(canScreencast({ backend: "lightpanda", connected: true })).toBe(false);
    expect(canScreencast({ backend: "chromium", connected: false })).toBe(false);
    expect(canScreencast({ backend: "chromium", connected: true, novnc: "http://127.0.0.1/vnc" })).toBe(
      false,
    );
  });

  it("keeps DOM-only lanes unstreamable", () => {
    expect(canScreencast({ backend: "lightpanda", connected: true })).toBe(
      false,
    );
    expect(canScreencast({ backend: "http", connected: true })).toBe(false);
  });

  it("stays down once a stream has errored, whatever the backend", () => {
    for (const backend of ["chromium", "embedded", "lightpanda"]) {
      expect(
        canScreencast({ backend, connected: true, streamFailed: true }),
      ).toBe(false);
    }
  });
});

describe("isEmbeddableNovncUrl", () => {
  it("only embeds real absolute URLs, never the 'ready' sentinel", () => {
    expect(isEmbeddableNovncUrl("")).toBe(false);
    expect(isEmbeddableNovncUrl(undefined)).toBe(false);
    expect(isEmbeddableNovncUrl(null)).toBe(false);
    // "ready" is a readiness sentinel — embedding it resolves relative to the
    // app origin and loads Kageha inside itself (the infinite mirror bug).
    expect(isEmbeddableNovncUrl("ready")).toBe(false);
    expect(isEmbeddableNovncUrl("vnc.html")).toBe(false);
    expect(isEmbeddableNovncUrl("http://127.0.0.1:9/vnc.html?autoconnect=1")).toBe(true);
    expect(isEmbeddableNovncUrl("https://host/vnc.html")).toBe(true);
    expect(isEmbeddableNovncUrl("wss://host/ws")).toBe(true);
  });
});

describe("isBlankBrowserUrl", () => {
  it("treats empty and about:blank as not a real page", () => {
    expect(isBlankBrowserUrl("")).toBe(true);
    expect(isBlankBrowserUrl("about:blank")).toBe(true);
    expect(isBlankBrowserUrl("https://kageha.ca")).toBe(false);
  });
});

describe("browserBootCopy", () => {
  it("keeps idle, opening, and failed copy free of engine names", () => {
    expect(browserBootCopy({ kind: "idle" })).toEqual({
      title: "Browser",
      sub: "Ask to open a page, or type a URL",
    });
    expect(browserBootCopy({ kind: "idle", browserStage: false })).toEqual({
      title: "This computer",
      sub: "Connecting to the live desktop…",
    });
    expect(browserBootCopy({ kind: "opening", url: "https://amazon.ca/s?k=matcha" }).sub).toBe(
      "Opening amazon.ca/s…",
    );
    const failed = browserBootCopy({ kind: "failed", url: "https://example.com" });
    expect(failed.title).toBe("example.com");
    expect(failed.sub).toMatch(/Couldn't show a live view/);
    expect(JSON.stringify(failed)).not.toMatch(/lightpanda|screenshot|DOM/i);
  });
});

describe("browserStreamUrl", () => {
  it("points at the session MJPEG route", () => {
    expect(browserStreamUrl("session-1")).toBe("/api/sessions/session-1/browser/stream");
  });

  it("uses a fresh URL per retry so the image reconnects", () => {
    expect(browserStreamUrl("session-1", 2)).toBe(
      "/api/sessions/session-1/browser/stream?attempt=2",
    );
  });
});

describe("streamRetryDelayMs", () => {
  const base = { backend: "chrome", connected: true };

  it("backs off from 1s and caps at 15s", () => {
    expect(streamRetryDelayMs({ ...base, failures: 1 })).toBe(1000);
    expect(streamRetryDelayMs({ ...base, failures: 3 })).toBe(4000);
    expect(streamRetryDelayMs({ ...base, failures: 10 })).toBe(15000);
  });

  it("does not retry when there is nothing to retry or a retry would flicker", () => {
    expect(streamRetryDelayMs({ ...base, failures: 0 })).toBeNull();
    expect(streamRetryDelayMs({ ...base, connected: false, failures: 1 })).toBeNull();
    expect(streamRetryDelayMs({ ...base, novnc: "https://x/vnc", failures: 1 })).toBeNull();
    // Lightpanda's failure means "show the semantic view"; keep it steady.
    expect(streamRetryDelayMs({ backend: "lightpanda", connected: true, failures: 1 })).toBeNull();
  });
});

describe("screencastViewport", () => {
  it("uses applied responsive CSS dimensions while a previous frame is retiring", () => {
    expect(screencastViewport({layout: {mode: "panel"}, viewport: {width: 800, height: 700},
      screencast: {deviceWidth: 1280, deviceHeight: 960}})).toEqual({width: 800, height: 700});
  });
  it("prefers device dimensions from the live frame", () => {
    expect(
      screencastViewport({
        viewport: { width: 980, height: 1100 },
        screencast: { deviceWidth: 1280, deviceHeight: 720 },
      }),
    ).toEqual({ width: 1280, height: 720 });
  });
});

describe("browserStatePollMs", () => {
  it("polls quickly until the first live frame arrives", () => {
    expect(browserStatePollMs({ hasFrame: false, running: true })).toBe(400);
    expect(browserStatePollMs({ hasFrame: false, agentDriving: true })).toBe(400);
  });

  it("keeps a live cadence while the agent is driving", () => {
    expect(browserStatePollMs({ hasFrame: true, agentDriving: true })).toBe(700);
  });

  it("backs off when idle, streaming, or on noVNC", () => {
    expect(browserStatePollMs({ hasFrame: true, novnc: true, running: true })).toBe(2000);
    expect(browserStatePollMs({ hasFrame: true, streaming: true, running: true })).toBe(2000);
    expect(browserStatePollMs({ hasFrame: true })).toBe(2500);
  });
});

describe("shouldSamplePointer", () => {
  it("samples when enough time or distance has passed", () => {
    expect(
      shouldSamplePointer({ now: 40, lastAt: 0, x: 1, y: 1, lastX: 0, lastY: 0, minMs: 32, minPx: 6 }),
    ).toBe(true);
    expect(
      shouldSamplePointer({ now: 10, lastAt: 0, x: 1, y: 1, lastX: 0, lastY: 0, minMs: 32, minPx: 6 }),
    ).toBe(false);
    expect(
      shouldSamplePointer({ now: 10, lastAt: 0, x: 10, y: 0, lastX: 0, lastY: 0, minMs: 32, minPx: 6 }),
    ).toBe(true);
  });
});

describe("needsConsentChoice", () => {
  it("offers recovery for a disconnected saved user profile", () => {
    for (const backend of ["chrome", "comet", "cdp"]) {
      expect(needsConsentChoice({ backend, owned: false, connected: false })).toBe(true);
    }
    expect(needsConsentChoice({ backend: "chromium", owned: true, connected: false })).toBe(false);
  });

  it("asks for a choice on a private lane when login is required", () => {
    expect(
      needsConsentChoice({
        loginRequired: true,
        backend: "chromium",
        owned: true,
      }),
    ).toBe(true);
    expect(
      needsConsentChoice({
        decision: { requiresConsent: true },
        backend: "lightpanda",
      }),
    ).toBe(true);
  });

  it("does not ask again after the user browser is attached", () => {
    expect(
      needsConsentChoice({
        loginRequired: true,
        decision: { requiresConsent: true },
        backend: "comet",
        owned: false,
      }),
    ).toBe(false);
  });
});

describe("openOutsideKind", () => {
  it("routes Comet, Chrome, and generic CDP independently", () => {
    expect(openOutsideKind("comet", false)).toBe("comet");
    expect(openOutsideKind("chrome", false)).toBe("chrome");
    expect(openOutsideKind("cdp", false)).toBe("popup");
    expect(openOutsideKind("chromium")).toBe("popup");
    expect(openOutsideTitle("cdp", false)).toBe("Pop out panel");
    expect(openOutsideTitle("chrome", false)).toBe("Open in Chrome");
  });
});

describe("getArtifactUrl", () => {
  const sessionId = "910921f583ad";

  it.each([
    "https://example.org/artifacts/report.html",
    "https://example.org/artifacts/chart.svg?theme=dark#chart",
    "//example.org/artifacts/report.pdf",
    "https://example.org/910921f583ad/report.html",
    "https://example.org/api/sessions/910921f583ad/files/artifacts/report.html",
    "file:///Users/example/.kageha/sessions/other-chat/artifacts/report.html",
    "/api/sessions/other-chat/files/artifacts/report.html",
    "file://remote-server/sessions/910921f583ad/artifacts/report.html",
    ".artifacts/report.html",
  ])("does not substitute a local file for remote or foreign content: %s", url => {
    expect(getArtifactUrl(url, sessionId)).toBeNull();
  });

  it("recognizes this app's absolute file URL and preserves navigation suffixes", () => {
    const path = `/api/sessions/${sessionId}/files/artifacts/report.html?theme=dark#chapter`;
    expect(getArtifactUrl(window.location.origin + path, sessionId)).toBe(path);
    expect(getArtifactUrl(window.location.origin + "/artifacts/report.html", sessionId)).toBeNull();
    expect(getArtifactUrl(`./artifacts/report.html#chapter`, sessionId)).toBe(`/api/sessions/${sessionId}/files/artifacts/report.html#chapter`);
  });

  it("extracts session artifact URL preserving hashes and queries", () => {
    expect(
      getArtifactUrl(
        "/Users/rakesh.pandey/.kageha/sessions/910921f583ad/artifacts/decision-model-explainer.html#ch2",
        sessionId,
      ),
    ).toBe("/api/sessions/910921f583ad/files/artifacts/decision-model-explainer.html#ch2");

    expect(
      getArtifactUrl(
        "file:///Users/rakesh.pandey/.kageha/sessions/910921f583ad/artifacts/decision-model-explainer.html?theme=dark#ch2",
        sessionId,
      ),
    ).toBe("/api/sessions/910921f583ad/files/artifacts/decision-model-explainer.html?theme=dark#ch2");
  });

  it("handles relative artifact paths", () => {
    expect(getArtifactUrl("artifacts/chart.svg", sessionId)).toBe(
      "/api/sessions/910921f583ad/files/artifacts/chart.svg",
    );
    expect(getArtifactUrl("decision-model-explainer.html", sessionId)).toBe(
      "/api/sessions/910921f583ad/files/artifacts/decision-model-explainer.html",
    );
  });

  it("passes through existing session file URLs", () => {
    expect(
      getArtifactUrl(
        "/api/sessions/910921f583ad/files/artifacts/report.pdf#page=2",
        sessionId,
      ),
    ).toBe("/api/sessions/910921f583ad/files/artifacts/report.pdf#page=2");
  });

  it("returns null for non-artifact external URLs", () => {
    expect(getArtifactUrl("https://example.com/products", sessionId)).toBeNull();
    expect(getArtifactUrl("https://github.com", sessionId)).toBeNull();
    expect(getArtifactUrl("", sessionId)).toBeNull();
  });
});

describe("formatTabTitle and pageProtocolInfo", () => {
  it("formats tab title cleanly from URL when title is absent", () => {
    expect(
      formatTabTitle("", "/Users/rakesh.pandey/.../artifacts/decision-model-explainer.html#ch2"),
    ).toBe("decision-model-explainer.html#ch2");
    expect(formatTabTitle("Explainer", "/some/path")).toBe("Explainer");
  });

  it("identifies security protocol badge", () => {
    expect(pageProtocolInfo("https://example.com")).toEqual({ kind: "https", label: "Secure", isSecure: true });
    expect(pageProtocolInfo("http://localhost:3000")).toEqual({ kind: "localhost", label: "Local", isSecure: true });
    expect(pageProtocolInfo("/Users/rakesh.pandey/artifacts/foo.html")).toEqual({ kind: "file", label: "File", isSecure: true });
    expect(pageProtocolInfo("http://insecure.site")).toEqual({ kind: "http", label: "Web", isSecure: false });
  });

  it("normalizes user-entered URLs by prepending https or http", () => {
    expect(normalizeBrowserUrl("bareandfair.ca")).toBe("https://bareandfair.ca");
    expect(normalizeBrowserUrl("  bareandfair.ca/products  ")).toBe("https://bareandfair.ca/products");
    expect(normalizeBrowserUrl("http://example.com")).toBe("http://example.com");
    expect(normalizeBrowserUrl("https://example.com")).toBe("https://example.com");
    expect(normalizeBrowserUrl("localhost:5173")).toBe("http://localhost:5173");
    expect(normalizeBrowserUrl("127.0.0.1:8788")).toBe("http://127.0.0.1:8788");
    expect(normalizeBrowserUrl("about:blank")).toBe("about:blank");
    expect(normalizeBrowserUrl("")).toBe("");
  });
});
