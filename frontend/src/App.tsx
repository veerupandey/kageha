import { useEffect, useState } from "react";
import { AppShell } from "./components/AppShell";
import { ConnectionBanner } from "./components/ConnectionBanner";
import { DropOverlay } from "./components/DropOverlay";
import { Toasts } from "./components/Toasts";
import { useAppStore } from "./store";
import { startAttachmentRecovery } from "./lib/attachmentRecovery";
import { applyUrlSurface, parseUrlState } from "./lib/urlSync";
import { watchSessionStatus } from "./lib/statusRefresh";

export default function App() {
  const boot = useAppStore((s) => s.boot);
  const addPendingFiles = useAppStore((s) => s.addPendingFiles);
  const setConnectionOnline = useAppStore((s) => s.setConnectionOnline);
  const [dropping, setDropping] = useState(false);

  useEffect(() => {
    let cancelled = false;
    let stopRecovery: (() => void) | undefined;
    (async () => {
      try {
        await boot();
      } catch (err) {
        console.warn("boot failed", err);
        return;
      }
      if (cancelled) return;
      applyUrlSurface(parseUrlState(), useAppStore.getState());
      stopRecovery = await startAttachmentRecovery(useAppStore);
      if (cancelled) { stopRecovery(); return; }
      if (!window.kageha?.embedded) return;
      const store = useAppStore.getState();
      if (!store.sessionId) {
        try {
          await store.newSession();
        } catch (err) {
          console.warn("desktop session failed", err);
        }
      }
      if (!cancelled) useAppStore.getState().pinLiveSurface(parseUrlState().surface || "browser");
    })();
    return () => {
      cancelled = true;
      stopRecovery?.();
    };
  }, [boot]);

  useEffect(() => {
    // Tab visible again or window focused: reload turn statuses from the server.
    const stopStatusWatch = watchSessionStatus(() => {
      void useAppStore.getState().refreshSessionStatus();
    });
    const onOnline = () => {
      setConnectionOnline(true);
      void useAppStore.getState().refreshSessionStatus();
    };
    const onOffline = () => setConnectionOnline(false);
    setConnectionOnline(navigator.onLine !== false);
    window.addEventListener("online", onOnline);
    window.addEventListener("offline", onOffline);
    let idleTicks = 0;
    let sidebarTicks = 0;
    const poll = window.setInterval(() => {
      if (document.visibilityState !== "visible" || navigator.onLine === false) return;
      const state = useAppStore.getState();
      if (++sidebarTicks >= 5) {
        sidebarTicks = 0;
        void state.refreshSessions({ background: true });
      }
      if (state.sessionLoading || state.sessionRefreshing) return;
      if (
        state.sending ||
        state.runStatus === "running" ||
        state.runStatus === "waiting_approval"
      ) {
        idleTicks = 0;
        void state.recoverLiveProgress(undefined, { lightweight: true });
        return;
      }
      // The UI can believe a turn is over (dropped SSE, lost poller, another
      // tab) while the backend is still working. Without this, the thread
      // looks frozen and the user types "go" into a turn that never stopped.
      // A cheap snapshot every ~10s re-attaches to any active backend turn.
      idleTicks += 1;
      if (idleTicks >= 5 && state.sessionId && document.visibilityState === "visible") {
        idleTicks = 0;
        void state.recoverLiveProgress(undefined, { lightweight: true });
      }
    }, 2000);
    return () => {
      window.removeEventListener("online", onOnline);
      window.removeEventListener("offline", onOffline);
      stopStatusWatch();
      window.clearInterval(poll);
    };
  }, [setConnectionOnline]);

  useEffect(() => {
    const onPopState = () => {
      const urlState = parseUrlState();
      const { sessionId } = urlState;
      const current = useAppStore.getState().sessionId;
      if (sessionId && sessionId !== current) {
        void useAppStore.getState().openSession(sessionId).then(() => {
          if (parseUrlState().sessionId === sessionId) applyUrlSurface(parseUrlState(), useAppStore.getState());
        });
      } else if (sessionId === current) {
        applyUrlSurface(urlState, useAppStore.getState());
      } else if (!sessionId && current) {
        void useAppStore.getState().newChat();
      }
    };
    window.addEventListener("popstate", onPopState);
    return () => window.removeEventListener("popstate", onPopState);
  }, []);

  useEffect(() => {
    let dragDepth = 0;
    const hasFiles = (e: DragEvent) =>
      Array.from(e.dataTransfer?.types || []).includes("Files");

    const onDragEnter = (e: DragEvent) => {
      if (!hasFiles(e)) return;
      e.preventDefault();
      dragDepth += 1;
      setDropping(true);
    };
    const onDragLeave = (e: DragEvent) => {
      if (!hasFiles(e)) return;
      e.preventDefault();
      dragDepth = Math.max(0, dragDepth - 1);
      if (dragDepth === 0) setDropping(false);
    };
    const onDragOver = (e: DragEvent) => {
      if (!hasFiles(e)) return;
      e.preventDefault();
    };
    const onDrop = (e: DragEvent) => {
      if (!hasFiles(e)) return;
      e.preventDefault();
      dragDepth = 0;
      setDropping(false);
      if (e.dataTransfer?.files?.length) {
        addPendingFiles(e.dataTransfer.files);
      }
    };

    window.addEventListener("dragenter", onDragEnter);
    window.addEventListener("dragleave", onDragLeave);
    window.addEventListener("dragover", onDragOver);
    window.addEventListener("drop", onDrop);
    return () => {
      window.removeEventListener("dragenter", onDragEnter);
      window.removeEventListener("dragleave", onDragLeave);
      window.removeEventListener("dragover", onDragOver);
      window.removeEventListener("drop", onDrop);
    };
  }, [addPendingFiles]);

  return (
    <>
      <div className="flex h-full min-h-0 flex-col overflow-hidden">
        <ConnectionBanner />

        <AppShell />
      </div>

      <DropOverlay visible={dropping} />
      <Toasts />
    </>
  );
}
