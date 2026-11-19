import { useEffect, useMemo, useRef, useState } from "react";
import { api } from "../api/client";
import { useAppStore } from "../store";

type MachineRuntime = {
  status?: string;
  error?: string;
  image?: string;
  networkEnabled?: boolean;
  containerId?: string;
  paused?: boolean;
  workRoot?: string;
  sessionRoot?: string;
  browserReady?: boolean;
  cdpEndpoint?: string;
  novncUrl?: string;
  novncReady?: boolean;
};

type MachineCommand = {
  id?: string;
  command?: string;
  stdout?: string;
  stderr?: string;
  exitCode?: number | null;
  running?: boolean;
};

type MachineTreeEntry = {
  path: string;
  name: string;
  dir?: boolean;
  mtime?: number;
};

type MachineWatch = {
  commands?: MachineCommand[];
  recentWrites?: string[];
  tree?: MachineTreeEntry[];
  sessionTree?: MachineTreeEntry[];
  workTree?: MachineTreeEntry[];
};

function MachineTreeList({
  sessionId,
  title,
  empty,
  entries,
  recent,
}: {
  sessionId?: string | null;
  title: string;
  empty: string;
  entries: MachineTreeEntry[];
  recent: Set<string>;
}) {
  return (
    <div className="flex min-h-0 flex-1 flex-col overflow-hidden border-b border-[var(--color-line)] last:border-b-0">
      <p className="shrink-0 px-2 py-1.5 text-[0.62rem] font-medium uppercase tracking-wide text-faint">
        {title}
      </p>
      <ul className="min-h-0 flex-1 overflow-auto px-1 pb-2 text-[0.7rem]">
        {entries.length === 0 ? (
          <li className="px-1.5 py-1 text-faint">{empty}</li>
        ) : (
          entries.map((entry) => {
            const hot = recent.has(entry.path);
            const depth = entry.path.split("/").filter(Boolean).length - 1;
            return (
              <li key={`${title}:${entry.path}`}>
                <span
                  className={
                    hot
                      ? "block truncate rounded px-1.5 py-0.5 text-accent"
                      : "block truncate rounded px-1.5 py-0.5 text-ink"
                  }
                  style={{ paddingLeft: `${0.375 + Math.max(0, depth) * 0.55}rem` }}
                  title={entry.path}
                >
                  {entry.dir ? `${entry.name}/` : sessionId
                    ? <a href={fileUrl(sessionId, entry.path)} download={entry.name}>{entry.name}</a>
                    : entry.name}
                </span>
              </li>
            );
          })
        )}
      </ul>
    </div>
  );
}

function fileUrl(sessionId: string, rel: string): string {
  const trimmed = rel.replace(/^\/+/, "");
  return `/api/sessions/${encodeURIComponent(sessionId)}/machine/file/${trimmed
    .split("/")
    .map(encodeURIComponent)
    .join("/")}`;
}

export function MachinePanel({
  compact = false,
  filesOpen = false,
}: {
  compact?: boolean;
  filesOpen?: boolean;
}) {
  const sessionId = useAppStore((s) => s.sessionId);
  const machineActive = useAppStore((s) => s.machineActive);
  const machineBlocked = useAppStore((s) => s.machineBlocked);
  const machineDetail = useAppStore((s) => s.machineDetail);
  const machinePaused = useAppStore((s) => s.machinePaused);
  const setMachineActive = useAppStore((s) => s.setMachineActive);
  const setMachinePaused = useAppStore((s) => s.setMachinePaused);
  const setMachineShellRunning = useAppStore((s) => s.setMachineShellRunning);
  const setMachineShellLine = useAppStore((s) => s.setMachineShellLine);
  const setMachineNovncUrl = useAppStore((s) => s.setMachineNovncUrl);
  const pauseMachine = useAppStore((s) => s.pauseMachine);
  const resumeMachine = useAppStore((s) => s.resumeMachine);
  const openCanvasItem = useAppStore((s) => s.openCanvasItem);
  const runStatus = useAppStore((s) => s.runStatus);
  const [runtime, setRuntime] = useState<MachineRuntime | null>(null);
  const [watch, setWatch] = useState<MachineWatch>({});
  const [showFiles, setShowFiles] = useState(filesOpen);
  const [draft, setDraft] = useState("");
  const [sending, setSending] = useState(false);
  const logRef = useRef<HTMLPreElement | null>(null);

  const commands = watch.commands || [];
  const shellRunning = commands.some((row) => row.running);

  useEffect(() => {
    if (!sessionId || !machineActive) {
      setRuntime(null);
      setWatch({});
      setMachineShellRunning(false);
      setMachineNovncUrl("");
      return;
    }
    let cancelled = false;
    let pulling = false;
    const pull = async () => {
      if (pulling) return;
      pulling = true;
      try {
        const data = await api<{ blocked?: boolean; error?: string; runtime?: MachineRuntime | null; watch?: MachineWatch }>(
          `/api/sessions/${encodeURIComponent(sessionId)}/machine`,
        );
        if (cancelled) return;
        setRuntime(data.runtime ?? null);
        setWatch(data.watch ?? {});
        const recovered = data.runtime?.status === "running" && !data.runtime.error;
        const blocked = !recovered && (data.blocked === true || data.runtime?.status === "error");
        useAppStore.setState((state) => state.sessionId === sessionId ? {
          machineBlocked: blocked,
          ...(blocked ? { machineDetail: data.error || data.runtime?.error || "The task machine could not start. Check Docker, then retry the task." }
            : state.machineBlocked ? { machineDetail: "" } : {}),
        } : {});
        setMachinePaused(Boolean(data.runtime?.paused || data.runtime?.status === "paused"));
        const novnc = String(data.runtime?.novncUrl || "").trim();
        const novncReady = data.runtime?.novncReady === true;
        if (blocked) {
          setMachineNovncUrl("");
        } else if (novncReady) {
          setMachineNovncUrl(novnc || "ready");
        } else {
          setMachineNovncUrl("");
        }
      } catch (error) {
        if (!cancelled) {
          setRuntime(null);
          useAppStore.setState((state) => state.sessionId === sessionId ? {
            machineBlocked: true,
            machineDetail: error instanceof Error ? error.message : String(error),
            machineNovncUrl: "",
          } : {});
        }
      } finally {
        pulling = false;
      }
    };
    void pull();
    const ms = shellRunning ? 350 : runStatus === "running" ? 600 : 1200;
    const timer = window.setInterval(() => void pull(), ms);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, [sessionId, machineActive, runStatus, shellRunning, setMachinePaused, setMachineShellRunning, setMachineNovncUrl]);

  useEffect(() => {
    setMachineShellRunning(Boolean(sessionId && machineActive && shellRunning));
    return () => setMachineShellRunning(false);
  }, [sessionId, machineActive, shellRunning, setMachineShellRunning]);

  useEffect(() => {
    const last = commands[commands.length - 1];
    setMachineShellLine(last?.command ? `$ ${String(last.command).trim()}` : "");
  }, [commands, setMachineShellLine]);

  useEffect(() => {
    const node = logRef.current;
    if (node) node.scrollTop = node.scrollHeight;
  }, [watch.commands]);

  const paused = machinePaused || runtime?.status === "paused" || Boolean(runtime?.paused);
  const status = machineBlocked
    ? "blocked"
    : paused
      ? "paused"
      : runStatus === "running" && machineActive
        ? "running"
        : machineActive
          ? "ready"
          : "off";

  const networkOn = Boolean(runtime?.networkEnabled);
  const image = (runtime?.image || "kageha-machine:local").replace(/^.*\//, "");
  const recent = new Set(watch.recentWrites || []);
  const sessionTree = watch.sessionTree || [];
  const workTree = watch.workTree?.length ? watch.workTree : watch.tree || [];
  const logText = useMemo(() => {
    if (!commands.length) return "";
    return commands
      .map((row) => {
        const head = `$ ${String(row.command || "").trim()}`;
        const out = `${row.stdout || ""}${row.stderr || ""}`.trimEnd();
        const tail =
          row.running
            ? "\n…"
            : row.exitCode === 0 || row.exitCode == null
              ? ""
              : `\n[exit ${row.exitCode}]`;
        return out ? `${head}\n${out}${tail}` : `${head}${tail}`;
      })
      .join("\n\n");
  }, [commands]);

  const previewRel = (watch.recentWrites || []).find((rel) =>
    /\.(html?|md|pdf|png|jpe?g|webp|svg)$/i.test(rel),
  );

  const runCommand = async () => {
    const command = draft.trim();
    if (!sessionId || !machineActive || !command || sending || paused) return;
    setSending(true);
    setDraft("");
    try {
      await api(`/api/sessions/${encodeURIComponent(sessionId)}/machine/exec`, {
        method: "POST",
        body: JSON.stringify({ command }),
      });
    } catch (error) {
      setDraft(command);
      useAppStore.getState().showToast(error instanceof Error ? error.message : String(error));
    } finally {
      setSending(false);
    }
  };

  const emptyLog = !machineActive
    ? "Toggle Machine or send /machine … to start."
    : paused
      ? "Paused · Wake or send a message to continue."
      : runtime?.containerId || runtime?.status === "running"
        ? compact
          ? "Computer is ready. Type below, or watch the agent."
          : "Computer is ready. Type below, or watch the agent."
        : "Starting the computer…";

  if (compact) {
    return (
      <div className="machine-terminal flex min-h-0 flex-1 overflow-hidden rounded-xl bg-surface">
        <div className="flex min-h-0 min-w-0 flex-1 flex-col">
          <pre
            ref={logRef}
            className="min-h-0 flex-1 overflow-auto whitespace-pre-wrap break-words px-3.5 py-2 font-mono text-[0.7rem] leading-[1.5] text-ink"
          >
            {logText || <span className="text-muted">{emptyLog}</span>}
          </pre>
          {machineDetail ? (
            <p className="shrink-0 px-3.5 pb-1.5 text-[0.65rem] text-warn">
              {machineDetail}
            </p>
          ) : null}
          {machineActive && !paused ? (
            <form
              className="flex shrink-0 items-center gap-1.5 border-t border-line px-2 py-1.5"
              onSubmit={(event) => {
                event.preventDefault();
                void runCommand();
              }}
            >
              <span className="font-mono text-[0.7rem] text-success">$</span>
              <input
                className="min-w-0 flex-1 bg-transparent font-mono text-[0.7rem] text-ink outline-none placeholder:text-muted"
                value={draft}
                onChange={(event) => setDraft(event.target.value)}
                placeholder="Type a command on this computer…"
                aria-label="Machine command"
                disabled={sending}
                autoComplete="off"
                spellCheck={false}
              />
              <button
                type="submit"
                className="rounded-full px-2 py-0.5 text-[0.62rem] text-muted hover:bg-surface-hover hover:text-ink disabled:opacity-40"
                disabled={sending || !draft.trim()}
              >
                Run
              </button>
            </form>
          ) : null}
          <div className="flex shrink-0 flex-wrap items-center gap-1 border-t border-line px-2 py-1.5">
            <span
              className={
                status === "blocked"
                  ? "rounded-full bg-danger-soft px-2 py-0.5 text-[0.58rem] font-medium uppercase tracking-[0.12em] text-danger"
                  : status === "paused"
                    ? "rounded-full bg-warn-soft px-2 py-0.5 text-[0.58rem] font-medium uppercase tracking-[0.12em] text-warn"
                    : "rounded-full bg-success-soft px-2 py-0.5 text-[0.58rem] font-medium uppercase tracking-[0.12em] text-success"
              }
            >
              {status}
            </span>
            {previewRel && sessionId ? (
              <button
                type="button"
                className="rounded-full px-2 py-0.5 text-[0.62rem] text-muted hover:bg-surface-hover hover:text-ink"
                onClick={() => {
                  if (/\.html?$/i.test(previewRel)) {
                    window.open(fileUrl(sessionId, previewRel), "_blank", "noopener,noreferrer");
                  } else {
                    openCanvasItem(previewRel, { expand: true });
                  }
                }}
              >
                Open {previewRel.split("/").pop()}
              </button>
            ) : null}
            <button
              type="button"
              className={
                showFiles
                  ? "rounded-full bg-surface-hover px-2 py-0.5 text-[0.62rem] text-ink"
                  : "rounded-full px-2 py-0.5 text-[0.62rem] text-muted hover:bg-surface-hover hover:text-ink"
              }
              onClick={() => setShowFiles((value) => !value)}
            >
              Files
            </button>
            {machineActive && paused ? (
              <button
                type="button"
                className="rounded-full px-2 py-0.5 text-[0.62rem] text-muted hover:bg-surface-hover hover:text-ink"
                onClick={() => void resumeMachine()}
              >
                Wake
              </button>
            ) : null}
            {machineActive && !paused ? (
              <button
                type="button"
                className="rounded-full px-2 py-0.5 text-[0.62rem] text-muted hover:bg-surface-hover hover:text-ink"
                onClick={() => void pauseMachine()}
              >
                Pause
              </button>
            ) : null}
            {machineActive ? (
              <button
                type="button"
                className="ml-auto rounded-full px-2 py-0.5 text-[0.62rem] text-muted hover:bg-surface-hover hover:text-ink"
                onClick={() => void setMachineActive(false)}
              >
                Stop
              </button>
            ) : null}
          </div>
        </div>
        {showFiles ? (
          <div className="flex w-56 shrink-0 flex-col overflow-hidden border-l border-line">
            <MachineTreeList
              title="/session"
              empty={machineActive ? "Session files appear here" : "Outputs land in /session"}
              entries={sessionTree}
              recent={recent}
            />
            <MachineTreeList
              sessionId={sessionId}
              title="/work"
              empty="Files created in the workstation appear here"
              entries={workTree}
              recent={recent}
            />
          </div>
        ) : null}
      </div>
    );
  }

  return (
    <div className="flex min-h-0 flex-1 flex-col bg-[var(--color-surface)]">
      <div className="flex shrink-0 items-center justify-between gap-3 border-b border-[var(--color-line)] px-3 py-2">
        <div>
          <p className="text-sm font-medium text-ink">Terminal</p>
          <p className="mt-0.5 text-[0.65rem] text-muted">
            Agent bash on this computer · Screen stays below
          </p>
        </div>
        <div className="flex items-center gap-2">
          {shellRunning ? (
            <span className="rounded-full bg-success-soft px-2 py-0.5 text-[0.65rem] font-medium uppercase tracking-wide text-success">
              bash
            </span>
          ) : null}
        <span
          className={
            status === "blocked"
              ? "rounded-full bg-danger-soft px-2 py-0.5 text-[0.65rem] font-medium uppercase tracking-wide text-danger"
              : status === "off"
                ? "rounded-full bg-[var(--color-line)] px-2 py-0.5 text-[0.65rem] font-medium uppercase tracking-wide text-faint"
                : status === "paused"
                  ? "rounded-full bg-warn-soft px-2 py-0.5 text-[0.65rem] font-medium uppercase tracking-wide text-warn"
                  : "rounded-full bg-success-soft px-2 py-0.5 text-[0.65rem] font-medium uppercase tracking-wide text-success"
          }
        >
          {status}
        </span>
        </div>
      </div>

      <div className="flex min-h-0 flex-1">
        <div className="flex min-h-0 min-w-0 flex-[3] flex-col border-r border-[var(--color-line)]">
          <pre
            ref={logRef}
            className="min-h-0 flex-1 overflow-auto whitespace-pre-wrap break-words px-3 py-2 font-mono text-[0.72rem] leading-5 text-ink"
          >
            {logText || (
              <span className="text-faint">
                {!machineActive
                  ? "Toggle Machine or send /machine … to start."
                  : paused
                    ? "Paused · Wake or send a message to continue."
                    : runtime?.containerId || runtime?.status === "running"
                      ? "Computer is ready. Agent bash streams here. The pane below is the screen — watch or take over."
                      : "Starting the computer…"}
              </span>
            )}
          </pre>
          <dl className="flex shrink-0 flex-wrap gap-x-4 gap-y-1 border-t border-[var(--color-line)] px-3 py-1.5 text-[0.62rem] text-muted">
            <div className="flex gap-1">
              <dt>Network</dt>
              <dd className="text-ink">
                {machineActive ? (networkOn ? "on" : "off") : "off"}
              </dd>
            </div>
            {machineActive && (
              <div className="flex min-w-0 gap-1">
                <dt>Image</dt>
                <dd className="truncate font-mono text-ink" title={runtime?.image || image}>
                  {image}
                </dd>
              </div>
            )}
            {machineActive && (
              <div className="flex gap-1">
                <dt>Screen</dt>
                <dd className="text-ink">{runtime?.novncUrl ? "live" : "starting"}</dd>
              </div>
            )}
          </dl>
        </div>
        <div className="flex w-[12.5rem] shrink-0 flex-col overflow-hidden">
          <MachineTreeList
            title="/session"
            empty={machineActive ? "Session files appear here" : "Outputs land in /session"}
            entries={sessionTree}
            recent={recent}
          />
          <MachineTreeList
            sessionId={sessionId}
            title="/work"
            empty="Files created in the workstation appear here"
            entries={workTree}
            recent={recent}
          />
        </div>
      </div>

      {machineDetail ? (
        <p className="shrink-0 border-t border-[var(--color-line)] px-3 py-2 text-xs text-ink">
          {machineDetail}
        </p>
      ) : null}

      <div className="flex shrink-0 flex-wrap items-center gap-2 border-t border-[var(--color-line)] px-3 py-2">
        {previewRel && sessionId ? (
          <button
            type="button"
            className="rounded-md border border-[var(--color-line)] px-2.5 py-1 text-[0.65rem] font-medium text-ink hover:bg-[var(--color-surface-hover)]"
            onClick={() => {
              if (/\.html?$/i.test(previewRel)) {
                window.open(fileUrl(sessionId, previewRel), "_blank", "noopener,noreferrer");
              } else {
                openCanvasItem(previewRel, { expand: true });
              }
            }}
          >
            Open {previewRel.split("/").pop()}
          </button>
        ) : null}
        {machineActive && paused && (
          <button
            type="button"
            className="rounded-md border border-[var(--color-line)] px-2.5 py-1 text-[0.65rem] font-medium text-ink hover:bg-[var(--color-surface-hover)]"
            onClick={() => void resumeMachine()}
          >
            Wake
          </button>
        )}
        {machineActive && !paused && (
          <button
            type="button"
            className="rounded-md border border-[var(--color-line)] px-2.5 py-1 text-[0.65rem] font-medium text-muted hover:bg-[var(--color-surface-hover)] hover:text-ink"
            onClick={() => void pauseMachine()}
          >
            Pause
          </button>
        )}
        {machineActive && (
          <button
            type="button"
            className="ml-auto rounded-md border border-[var(--color-line)] px-2.5 py-1 text-[0.65rem] font-medium text-muted hover:bg-[var(--color-surface-hover)] hover:text-ink"
            onClick={() => void setMachineActive(false)}
          >
            Stop machine · return to host bash
          </button>
        )}
      </div>
    </div>
  );
}
