import { useSessionTruth } from "../lib/sessionTruth";
import { useEffect } from "react";
import { useAppStore } from "../store";

export function SessionStateIndicator({ sessionId }: { sessionId: string | null }) {
  const snapshot = useSessionTruth(sessionId);
  useEffect(() => {
    if (snapshot.status !== "current" || !snapshot.data?.selected_target || !sessionId ||
        typeof useAppStore.getState !== "function") return;
    const state = useAppStore.getState();
    if (state.sessionId !== sessionId || state.executionTargetPending) return;
    const target = snapshot.data.selected_target;
    const fields = { machineActive: target === "machine", remoteActive: target === "remote",
      remoteHostId: target === "remote" ? snapshot.data.remote_host_id || state.remoteHostId : "" };
    useAppStore.setState({ ...fields, runs: state.runs[sessionId] ? {
      ...state.runs, [sessionId]: { ...state.runs[sessionId], ...fields },
    } : state.runs });
  }, [snapshot, sessionId]);
  return null;
}
