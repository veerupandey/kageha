export type BrowserPageFit = "contain" | "cover";


export function isExternalBrowser(backend: string, owned?: boolean): boolean {
  return ["comet", "chrome", "cdp"].includes(backend) && owned !== true;
}

export function needsConsentChoice(state: {
  connected?: boolean;
  loginRequired?: boolean;
  decision?: { requiresConsent?: boolean };
  backend: string;
  owned?: boolean;
}): boolean {
  // A saved profile is only a selection, not a successful attachment. Keep
  // recovery choices visible even when the agent currently owns the panel.
  if (isExternalBrowser(state.backend, state.owned) && state.connected === false) return true;
  return (
    Boolean(state.loginRequired || state.decision?.requiresConsent) &&
    !isExternalBrowser(state.backend, state.owned)
  );
}

export function displayHost(url: string): string {
  const raw = url.trim();
  if (!raw) return "";
  try {
    const parsed = new URL(raw.includes("://") ? raw : `https://${raw}`);
    const host = parsed.hostname.replace(/^www\./, "");
    const path = parsed.pathname === "/" ? "" : parsed.pathname.replace(/\/$/, "");
    return `${host}${path}`;
  } catch {
    return raw.replace(/^https?:\/\//, "");
  }
}

export function getArtifactUrl(rawUrl: string, sessionId: string | null | undefined): string | null {
  if (!rawUrl.trim() || !sessionId) return null;
  let local = rawUrl.trim();
  const files = `/api/sessions/${encodeURIComponent(sessionId)}/files/`;
  // Only this app's own file routes may be taken from an HTTP URL. A remote
  // /artifacts/report.html is still a remote page, regardless of its filename.
  if (/^(https?:)?\/\//i.test(local)) {
    try {
      const url = new URL(local, typeof window === "undefined" ? undefined : window.location.origin);
      if (typeof window === "undefined" || url.origin !== window.location.origin) return null;
      if (!url.pathname.startsWith(files)) return null;
      local = url.pathname + url.search + url.hash;
    } catch { return null; }
  } else if (/^file:/i.test(local)) {
    try {
      const url = new URL(local);
      if (url.hostname && url.hostname !== "localhost") return null;
      local = decodeURI(url.pathname) + url.search + url.hash;
    } catch { return null; }
  } else if (/^[a-z][a-z\d+.-]*:/i.test(local)) return null;

  const suffixAt = local.search(/[?#]/);
  const path = suffixAt < 0 ? local : local.slice(0, suffixAt);
  const suffix = suffixAt < 0 ? "" : local.slice(suffixAt);
  if (!/\.(html?|svg|pdf)$/i.test(path)) return null;
  if (path.startsWith(files)) return path + suffix;

  let artifact: string;
  if (/^(?:\.\/|\/)?artifacts\//.test(path)) {
    artifact = path.replace(/^(?:\.\/|\/)?artifacts\//, "");
  } else {
    const sessionRoot = `/sessions/${sessionId}/artifacts/`;
    const rootAt = path.indexOf(sessionRoot);
    if (path.startsWith("/") && rootAt >= 0) artifact = path.slice(rootAt + sessionRoot.length);
    else if (!path.includes("/") && !path.includes("\\")) artifact = path;
    else return null;
  }
  return artifact ? `${files}artifacts/${artifact}${suffix}` : null;
}

export function formatTabTitle(title: string, url: string): string {
  const cleanTitle = (title || "").trim();
  if (cleanTitle) return cleanTitle;
  const raw = (url || "").trim();
  if (!raw) return "New Tab";
  if (raw.startsWith("/") || raw.startsWith("file:") || raw.includes("/artifacts/")) {
    const parts = raw.split(/[/\\]/);
    const last = parts.filter(Boolean).pop();
    if (last) return last;
  }
  return displayHost(raw) || "New Tab";
}

export function pageProtocolInfo(url: string): {
  kind: "https" | "http" | "localhost" | "file";
  label: string;
  isSecure: boolean;
} {
  const raw = (url || "").trim().toLowerCase();
  if (raw.includes("localhost") || raw.includes("127.0.0.1") || raw.includes("0.0.0.0")) {
    return { kind: "localhost", label: "Local", isSecure: true };
  }
  if (raw.startsWith("/") || raw.startsWith("file:") || raw.includes("/artifacts/")) {
    return { kind: "file", label: "File", isSecure: true };
  }
  if (raw.startsWith("https://")) {
    return { kind: "https", label: "Secure", isSecure: true };
  }
  return { kind: "http", label: "Web", isSecure: false };
}

export function stageAddressLabel(title: string, url: string): string {
  const host = displayHost(url);
  const clean = title.replace(/\s+/g, " ").trim();
  if (clean && host) {
    const hostOnly = host.split("/")[0] || host;
    if (!clean.toLowerCase().includes(hostOnly.toLowerCase())) {
      return `${clean} · ${host}`;
    }
  }
  return clean || host;
}

export function browserCanvasSize(
  viewport: { width: number; height: number },
  box: { width: number; height: number },
  zoom: "fit" | number,
): { width: number; height: number } {
  const width = Math.max(1, viewport.width);
  const height = Math.max(1, viewport.height);
  const scale = zoom === "fit"
    ? Math.min(1, Math.max(1, box.width) / width, Math.max(1, box.height) / height)
    : zoom / 100;
  return { width: width * scale, height: height * scale };
}
const VIEWPORT_MAX_HEIGHT = 2400;

/** Reflow the page at the panel's CSS size, preserving aspect at size limits. */
export function panelViewportFor(box: { width: number; height: number }) {
  const width = Math.round(Math.max(360, Math.min(2560, box.width)));
  return {
    width,
    height: Math.round(Math.max(400, Math.min(VIEWPORT_MAX_HEIGHT, box.height * width / Math.max(1, box.width)))),
  };
}

export function isEmbeddedHost(win: { kageha?: { embedded?: boolean } } | undefined = typeof window === "undefined" ? undefined : window): boolean {
  return Boolean(win?.kageha?.embedded);
}

export function showEmbeddedGuest(input: {
  embeddedHost: boolean;
  backend: string;
  selectedBackend?: string;
  connected?: boolean;
  novnc?: boolean | string;
}): boolean {
  if (!input.embeddedHost || input.novnc) return false;
  if (!input.connected) return !input.selectedBackend || input.selectedBackend === "embedded";
  return input.backend === "embedded";
}

const PAINTABLE_BACKENDS = [
  "chromium",
  "comet",
  "chrome",
  "cdp",
  "docker",
  "machine",
  "remote",
  "headless",
  "embedded",
  "rust",
];

/**
 * Identity of the conditions a live stream was requested under. A stream that
 * errored stays down only while this is unchanged; once the server attaches a
 * page (screencast becomes available, binding or backend changes), the
 * panel asks again instead of staying on a stale frame for the whole session.
 */
export function streamRetryKey(state: {
  bindingId?: string;
  backend?: string;
  connected?: boolean;
  screencast?: { available?: boolean };
}): string {
  return [
    state.backend || "",
    state.connected ? "1" : "0",
    state.screencast?.available ? "1" : "0",
    ...(state.bindingId ? [state.bindingId] : []),
  ].join("|");
}

export function canScreencast(input: {
  backend: string;
  connected?: boolean;
  novnc?: boolean | string;
  streamFailed?: boolean;
}): boolean {
  if (!input.connected) return false;
  if (input.novnc) return false;
  // A stream that already errored stays down until its streamRetryKey
  // changes; retrying in a render loop just flickers the panel.
  if (input.streamFailed) return false;
  return PAINTABLE_BACKENDS.includes(input.backend);
}

export function isBlankBrowserUrl(url: string): boolean {
  const raw = (url || "").trim().toLowerCase();
  return !raw || raw === "about:blank" || raw === "about:srcdoc";
}

/**
 * Normalizes user-entered or loosely formatted browser addresses into
 * full URLs that the CDP and browser backend can safely navigate to.
 * Prepend https:// for web hosts and http:// for local loopback.
 */
export function normalizeBrowserUrl(raw: string): string {
  const trimmed = (raw || "").trim();
  if (!trimmed) return "";
  if (
    /^[a-zA-Z][a-zA-Z\d+\-.]*:\/\//.test(trimmed) ||
    trimmed.startsWith("about:") ||
    trimmed.startsWith("data:") ||
    trimmed.startsWith("javascript:")
  ) {
    return trimmed;
  }
  if (/^(localhost|127\.0\.0\.1|0\.0\.0\.0|\[::1\])(:\d+)?(\/.*)?$/i.test(trimmed)) {
    return `http://${trimmed}`;
  }
  return `https://${trimmed}`;
}

/**
 * True only when the value is a real, absolute URL safe to load into an
 * <iframe src>. The noVNC state can carry the sentinel string "ready" (screen
 * is up but the URL is not known yet); dropping that into an iframe resolves it
 * relative to the app origin and loads the whole app into itself — the infinite
 * mirror. Guard the iframe with this so only http(s)/ws(s) URLs are embedded.
 */
export function isEmbeddableNovncUrl(url: string | undefined | null): boolean {
  const raw = (url || "").trim();
  if (!raw || raw === "ready") return false;
  return /^(https?|wss?):\/\//i.test(raw);
}

export function browserBootCopy(input: {
  kind: "idle" | "opening" | "failed";
  url?: string;
  title?: string;
  browserStage?: boolean;
}): { title: string; sub: string } {
  const host = displayHost(input.url || "");
  if (input.kind === "failed") {
    return {
      title: host || input.title || "This page",
      sub: "Couldn't show a live view. The agent can still keep working.",
    };
  }
  if (input.kind === "idle") {
    if (input.browserStage === false) {
      return {
        title: "This computer",
        sub: "Connecting to the live desktop…",
      };
    }
    return {
      title: "Browser",
      sub: "Ask to open a page, or type a URL",
    };
  }
  return {
    title: input.browserStage === false ? "This computer" : "Browser",
    sub: host
      ? `Opening ${host}…`
      : input.browserStage === false
        ? "Connecting to the live desktop…"
        : "Connecting to the live page…",
  };
}

export function browserStreamUrl(sessionId: string, attempt = 0, bindingId?: string): string {
  const base = `/api/sessions/${encodeURIComponent(sessionId)}/browser/stream`;
  const query = new URLSearchParams();
  if (attempt > 0) query.set("attempt", String(attempt));
  if (bindingId) query.set("binding", bindingId);
  return query.size ? `${base}?${query}` : base;
}

export function browserProfileLabel(backend: string): string {
  if (backend === "lightpanda") return "Headless browser";
  if (backend === "comet") return "Comet profile";
  if (backend === "chrome") return "Chrome profile";
  if (backend === "cdp") return "Connected profile";
  if (backend === "embedded") return "Kageha profile";
  if (backend === "remote") return "Remote browser";
  if (backend === "machine" || backend === "docker") return "Sandbox browser";
  return "Private browser";
}

/**
 * Delay before retrying a failed stream while its retry key is unchanged, or
 * null when the panel should not retry. Covers transient refusals such as
 * "busy" or a promote in flight. It backs off (1s, 2s, 4s ... 15s) so a
 * backend that is truly down costs almost nothing. DOM-only lanes do not
 * retry, since their failure means "show the semantic view" and a retry would
 * make it flicker.
 */
export function streamRetryDelayMs(input: {
  backend: string;
  connected?: boolean;
  novnc?: boolean | string;
  failures: number;
}): number | null {
  if (!input.connected || input.novnc || input.failures <= 0) return null;
  if (!PAINTABLE_BACKENDS.includes(input.backend)) return null;
  return Math.min(1000 * 2 ** (input.failures - 1), 15000);
}

export function screencastViewport(state: {
  layout?: { mode?: string };
  viewport?: { width: number; height: number };
  screencast?: { deviceWidth?: number; deviceHeight?: number };
}): { width: number; height: number } {
  // A confirmed responsive layout is CSS geometry. Stream metadata can still
  // describe the preceding JPEG until the new producer publishes a frame.
  if (state.layout?.mode === "panel" && state.viewport) return state.viewport;
  const width = Number(state.screencast?.deviceWidth || 0);
  const height = Number(state.screencast?.deviceHeight || 0);
  if (width > 0 && height > 0) return { width, height };
  return state.viewport || { width: 980, height: 1100 };
}

export function browserStatePollMs(input: {
  hasFrame: boolean;
  novnc?: boolean;
  streaming?: boolean;
  running?: boolean;
  machineActive?: boolean;
  agentDriving?: boolean;
}): number {
  if (input.novnc || input.streaming) return 2000;
  if (!input.hasFrame && (input.running || input.agentDriving)) return 400;
  if (input.agentDriving) return 700;
  if (input.running && input.machineActive) return 2500;
  if (input.running || input.machineActive) return 1000;
  return 2500;
}

export function previewFitLayout(input: {
  boxWidth: number;
  boxHeight: number;
  naturalWidth: number;
  naturalHeight: number;
  fit: BrowserPageFit;
}): { scale: number; offsetX: number; offsetY: number; displayW: number; displayH: number } {
  const natW = Math.max(1, input.naturalWidth);
  const natH = Math.max(1, input.naturalHeight);
  const scale = (input.fit === "contain" ? Math.min : Math.max)(
    input.boxWidth / natW,
    input.boxHeight / natH,
  );
  const displayW = natW * scale;
  const displayH = natH * scale;
  return {
    scale,
    displayW,
    displayH,
    offsetX: (input.boxWidth - displayW) / 2,
    offsetY: (input.boxHeight - displayH) / 2,
  };
}

export function previewPointFromFit(input: {
  clientX: number;
  clientY: number;
  rect: { left: number; top: number; width: number; height: number };
  naturalWidth: number;
  naturalHeight: number;
  viewport: { width: number; height: number };
  fit: BrowserPageFit;
  clamp?: boolean;
}): { x: number; y: number } {
  const layout = previewFitLayout({
    boxWidth: input.rect.width,
    boxHeight: input.rect.height,
    naturalWidth: input.naturalWidth,
    naturalHeight: input.naturalHeight,
    fit: input.fit,
  });
  const natW = Math.max(1, input.naturalWidth);
  const natH = Math.max(1, input.naturalHeight);
  const x =
    ((input.clientX - input.rect.left - layout.offsetX) / layout.scale) *
    (input.viewport.width / natW);
  const y =
    ((input.clientY - input.rect.top - layout.offsetY) / layout.scale) *
    (input.viewport.height / natH);
  if (input.clamp === false) return { x, y };
  return {
    x: Math.max(0, Math.min(input.viewport.width, x)),
    y: Math.max(0, Math.min(input.viewport.height, y)),
  };
}

export function highlightRectFromFit(input: {
  highlight: { x: number; y: number; width: number; height: number };
  boxWidth: number;
  boxHeight: number;
  naturalWidth: number;
  naturalHeight: number;
  viewport: { width: number; height: number };
  fit: BrowserPageFit;
}): { left: number; top: number; width: number; height: number } {
  const layout = previewFitLayout({
    boxWidth: input.boxWidth,
    boxHeight: input.boxHeight,
    naturalWidth: input.naturalWidth,
    naturalHeight: input.naturalHeight,
    fit: input.fit,
  });
  const vw = Math.max(1, input.viewport.width);
  const vh = Math.max(1, input.viewport.height);
  return {
    left: layout.offsetX + (input.highlight.x / vw) * layout.displayW,
    top: layout.offsetY + (input.highlight.y / vh) * layout.displayH,
    width: (input.highlight.width / vw) * layout.displayW,
    height: (input.highlight.height / vh) * layout.displayH,
  };
}

export type OpenOutsideKind = "comet" | "chrome" | "popup";

export function shouldSamplePointer(input: {
  now: number;
  lastAt: number;
  x: number;
  y: number;
  lastX: number;
  lastY: number;
  minMs?: number;
  minPx?: number;
}): boolean {
  const minMs = input.minMs ?? 32;
  const minPx = input.minPx ?? 6;
  const moved = Math.hypot(input.x - input.lastX, input.y - input.lastY);
  return input.now - input.lastAt >= minMs || moved >= minPx;
}

export function openOutsideKind(backend: string, owned?: boolean): OpenOutsideKind {
  if (!isExternalBrowser(backend, owned)) return "popup";
  if (backend === "comet") return "comet";
  if (backend === "chrome") return "chrome";
  return "popup";
}

export function openOutsideTitle(backend: string, owned?: boolean): string {
  const kind = openOutsideKind(backend, owned);
  if (kind === "comet") return "Open in Comet";
  if (kind === "chrome") return "Open in Chrome";
  return "Pop out panel";
}

/** The authoritative hold summary covers receipts omitted from the display list. */
export function browserActionNeedsReview(
  actions?: ReadonlyArray<{ blocksCurrentTab?: boolean }>,
  hold?: { blocksCurrentTab?: boolean; unknownTarget?: boolean },
): boolean {
  return Boolean(hold?.unknownTarget) || (hold?.blocksCurrentTab ?? actions?.some(action => action.blocksCurrentTab !== false) ?? false);
}
