export type DesktopBootInput = {
  phase?: string;
  detail?: string;
  adopted?: boolean;
  screenReady?: boolean;
  connecting?: boolean;
  browserReady?: boolean;
  blocked?: boolean;
  paused?: boolean;
};

export function desktopBootCopy(input: DesktopBootInput): { title: string; sub: string } {
  const phase = String(input.phase || "").trim().toLowerCase();
  const detail = String(input.detail || "").trim();
  if (input.paused && !input.blocked) {
    return { title: "Computer paused", sub: "Resume this computer to reconnect its screen." };
  }
  if (input.blocked) {
    return {
      title: "Computer unavailable",
      sub: detail || "The task machine could not start. Check Docker, then retry the task.",
    };
  }
  if (input.adopted && !input.screenReady) {
    return {
      title: "Reconnecting to this computer",
      sub: detail || "The box is still here — bringing the screen back.",
    };
  }
  if (input.adopted && input.connecting) {
    return {
      title: "Reconnected to this computer",
      sub: detail || "Attaching the live view.",
    };
  }
  if (!input.screenReady) {
    if (phase === "image") {
      return {
        title: "Pulling the computer image",
        sub: detail || "First boot downloads the XFCE box.",
      };
    }
    if (phase === "screen") {
      return {
        title: "Connecting the screen",
        sub: detail || "noVNC is coming up on this computer.",
      };
    }
    return {
      title: "Starting this computer",
      sub: detail || "Starting XFCE on the isolated box.",
    };
  }
  if (input.connecting) {
    return {
      title: "Connecting the screen",
      sub: detail || "Live view is attaching.",
    };
  }
  return {
    title: "This computer",
    sub:
      detail ||
      (input.browserReady === false
        ? "Screen is live — browser is still starting."
        : "Screen is live."),
  };
}

export function rfbViewerMode(input: {
  controlling: boolean;
  visible: boolean;
}): {
  viewOnly: boolean;
  qualityLevel: number;
  compressionLevel: number;
  scaleViewport: boolean;
  resizeSession: boolean;
} {
  const visible = Boolean(input.visible);
  const controlling = Boolean(input.controlling) && visible;
  return {
    viewOnly: !controlling,
    qualityLevel: !visible ? 2 : controlling ? 8 : 6,
    compressionLevel: !visible ? 8 : controlling ? 2 : 4,
    scaleViewport: true,
    resizeSession: false,
  };
}

export type DesktopBootStep = {
  id: string;
  label: string;
  state: "done" | "current" | "wait";
};

export function desktopBootSteps(input: {
  phase?: string;
  adopted?: boolean;
  screenReady?: boolean;
  connecting?: boolean;
}): DesktopBootStep[] {
  const phase = String(input.phase || "").trim().toLowerCase();
  const ready = Boolean(input.screenReady) && !input.connecting;
  const rank = ready
    ? 3
    : input.connecting || phase === "screen"
      ? 2
      : phase === "image"
        ? 0
        : 1;
  const labels = input.adopted
    ? ["Find the box", "Wake the desktop", "Attach the screen"]
    : ["Pull image", "Start XFCE", "Connect screen"];
  return labels.map((label, index) => ({
    id: ["image", "desktop", "screen"][index],
    label,
    state: index < rank ? "done" : index === rank ? "current" : "wait",
  }));
}

const ACTION_LABELS: Record<string, string> = {
  click: "Clicking",
  type: "Typing",
  key: "Pressing a key",
  scroll: "Scrolling",
  observe: "Looking at the screen",
  screenshot: "Looking at the screen",
  doctor: "Checking the desktop",
  batch: "Working on the desktop",
  computer_click: "Clicking",
  computer_type: "Typing",
  computer_key: "Pressing a key",
  computer_scroll: "Scrolling",
  computer_get_state: "Looking at the screen",
  computer_screenshot: "Looking at the screen",
  computer_batch: "Working on the desktop",
  browser_open: "Opening the browser",
  browser_navigate: "Opening a page",
  browser_click: "Clicking in the browser",
  browser_type: "Typing in the browser",
  browser_press: "Using the browser",
  bash: "Running a command",
};

function hostFromPreview(preview?: string): string {
  const raw = String(preview || "");
  const match = /https?:\/\/([^/\s"']+)/i.exec(raw);
  if (!match) return "";
  return match[1].replace(/^www\./, "");
}

export function desktopActionLabel(action?: string, caption?: string): string {
  const named = String(caption || "").trim();
  if (named) return named;
  const key = String(action || "").trim().toLowerCase();
  if (!key) return "";
  if (ACTION_LABELS[key]) return ACTION_LABELS[key];
  if (key.startsWith("computer_")) {
    return desktopActionLabel(key.slice("computer_".length));
  }
  return key.replace(/_/g, " ");
}

export function desktopToolNarration(name?: string, preview?: string): string {
  const tool = String(name || "").trim().toLowerCase();
  if (!tool) return "";
  const host = hostFromPreview(preview);
  if (host && tool.startsWith("browser_")) {
    return `Opening ${host}`;
  }
  return ACTION_LABELS[tool] || desktopActionLabel(tool);
}

export function desktopSessionNarration(input: {
  action?: string;
  caption?: string;
  toolName?: string;
  toolPreview?: string;
  shellLine?: string;
  running?: boolean;
}): string {
  const fromFrame = desktopActionLabel(input.action, input.caption);
  if (fromFrame) return fromFrame;
  const fromTool = desktopToolNarration(input.toolName, input.toolPreview);
  if (fromTool) return fromTool;
  const shell = String(input.shellLine || "").replace(/^\$\s*/, "").trim();
  if (shell) return `Running ${shell.split(/\s+/)[0]}`;
  if (input.running) return "Working on this computer";
  return "";
}

export function desktopWsUrl(sessionId: string, wsPath?: string): string {
  const path = (wsPath || `/api/sessions/${sessionId}/desktop/ws`).trim();
  const proto = window.location.protocol === "https:" ? "wss:" : "ws:";
  return `${proto}//${window.location.host}${path.startsWith("/") ? path : `/${path}`}`;
}
