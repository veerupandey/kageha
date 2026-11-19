import { Component, type ErrorInfo, type ReactNode } from "react";
import { useAppStore } from "../store";

interface Props {
  children: ReactNode;
}

interface State {
  error: Error | null;
  reloading: boolean;
}

function staleChunkStorageKey(error: Error): string | null {
  const msg = error.message || "";
  const match = msg.match(/Failed to fetch dynamically imported module:\s*(\S+)/i);
  if (match) return `kageha:chunk-reload:${match[1]}`;
  if (
    /dynamically imported module|importing a module script failed|error loading dynamically imported module/i.test(msg)
    || error.name === "ChunkLoadError"
  ) {
    return "kageha:chunk-reload:generic";
  }
  return null;
}

function shouldReloadStaleChunk(error: Error): boolean {
  const key = staleChunkStorageKey(error);
  if (!key) return false;
  try {
    return sessionStorage.getItem(key) !== "1";
  } catch {
    // Without a persistent guard, automatic reload can loop indefinitely.
    return false;
  }
}

function BoundaryFallback({
  error,
  onReload,
  onNewChat,
}: {
  error: Error;
  onReload: () => void;
  onNewChat: () => void;
}) {
  return (
    <div className="error-boundary" role="alert">
      <div className="error-boundary-card">
        <p className="eyebrow">Something went wrong</p>
        <h1 className="error-boundary-title">Kageha hit an unexpected error</h1>
        <p className="error-boundary-message muted">
          {error.message || "Unknown render error"}
        </p>
        <div className="error-boundary-actions">
          <button type="button" className="btn primary" onClick={onReload}>
            Reload
          </button>
          <button type="button" className="btn ghost" onClick={onNewChat}>
            New chat
          </button>
        </div>
      </div>
    </div>
  );
}

export class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null, reloading: false };

  static getDerivedStateFromError(error: Error): State {
    if (shouldReloadStaleChunk(error)) {
      return { error, reloading: true };
    }
    return { error, reloading: false };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    if (this.state.reloading) {
      const key = staleChunkStorageKey(error);
      try {
        if (key) sessionStorage.setItem(key, "1");
      } catch {
        /* private mode */
      }
      window.location.reload();
      return;
    }
    console.error("WebUI ErrorBoundary", error, info.componentStack);
  }

  private handleReload = () => {
    window.location.reload();
  };

  private handleNewChat = () => {
    this.setState({ error: null });
    void useAppStore
      .getState()
      .newChat()
      .catch(() => {
        window.location.reload();
      });
  };

  render() {
    if (this.state.reloading) {
      return null;
    }
    if (this.state.error) {
      return (
        <BoundaryFallback
          error={this.state.error}
          onReload={this.handleReload}
          onNewChat={this.handleNewChat}
        />
      );
    }
    return this.props.children;
  }
}
