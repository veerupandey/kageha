import * as DropdownMenu from "@radix-ui/react-dropdown-menu";
import { useState } from "react";
import { api } from "../api/client";
import { cn } from "../lib/cn";
import { Icon } from "../lib/icons";
import { useAppStore } from "../store";
import { useSessionTruth } from "../lib/sessionTruth";

type RemoteHostRow = {
  id: string;
  name: string;
  host: string;
  port: number;
  risk_tier: string;
};

const menuContentClass =
  "ka-composer-menu z-50 flex max-h-[min(28rem,70vh)] w-[min(20rem,calc(100vw-2rem))] flex-col overflow-hidden rounded-2xl border border-line bg-surface p-1.5 shadow-[0_18px_48px_rgba(0,0,0,0.28)]";
const itemClass =
  "flex cursor-pointer items-center gap-2 rounded-xl px-2.5 py-2 text-sm outline-none data-[highlighted]:bg-accent-soft data-[disabled]:cursor-not-allowed data-[disabled]:opacity-40";

/**
 * Unified execution-target control: one answer to "where does this chat run?"
 * — This host (local), a fresh isolated Linux box (machine), or a saved remote
 * SSH host. The three surfaces are mutually exclusive (enforced in the store),
 * so this replaces the standalone Machine chip.
 */
export function ExecutionTargetChip({ compact = false }: { compact?: boolean } = {}) {
  const machineActive = useAppStore((s) => s.machineActive);
  const machineBlocked = useAppStore((s) => s.machineBlocked);
  const setMachineActive = useAppStore((s) => s.setMachineActive);
  const remoteActive = useAppStore((s) => s.remoteActive);
  const remoteHostId = useAppStore((s) => s.remoteHostId);
  const remoteHostName = useAppStore((s) => s.remoteHostName);
  const remoteHealth = useAppStore((s) => s.remoteHealth);
  const remoteRiskTier = useAppStore((s) => s.remoteRiskTier);
  const setRemoteActive = useAppStore((s) => s.setRemoteActive);
  const openSettings = useAppStore((s) => s.openSettings);
  const running = useAppStore((s) => s.sending || s.runs[s.sessionId || ""]?.status === "running");
  const sessionId = useAppStore((s) => s.sessionId);
  const truth = useSessionTruth(sessionId);
  const pending = useAppStore((s) => s.executionTargetPending);
  const backendBusy = Boolean(truth.data?.turn && ["accepted", "planning", "executing", "waiting_approval", "waiting_dependencies", "verifying", "repairing"].includes(truth.data.turn.phase));
  const targetLocked = pending || running || backendBusy || Boolean(sessionId && truth.status !== "current");
  const [hosts, setHosts] = useState<RemoteHostRow[]>([]);
  const [upgradeChat, setUpgradeChat] = useState("");

  const target = machineActive ? "machine" : remoteActive ? "remote" : "local";
  const confirmed = !sessionId || (truth.status === "current" && truth.data?.selected_target === target && !truth.data.target_conflict);
  const prod = target === "remote" && remoteRiskTier === "production";
  const destination = confirmed ? truth.data?.selected_destination : undefined;
  const caps = destination?.capabilities;
  const features = caps ? [caps.shell && "Terminal", caps.files && "Files", caps.browser && "Browser", caps.desktop && "Desktop", caps.jobs && "Jobs"].filter(Boolean) : [];

  const loadHosts = () => {
    if (sessionId) {
      void api<{ upgrade_required?: boolean }>(`/api/sessions/${encodeURIComponent(sessionId)}/machine`)
        .then(state => setUpgradeChat(state.upgrade_required ? sessionId : ""))
        .catch(() => setUpgradeChat(""));
    }
    void api<{ hosts?: RemoteHostRow[] }>("/api/remote")
      .then((data) => setHosts(data.hosts || []))
      .catch(() => setHosts([]));
  };

  const label = !confirmed ? truth.status === "current" ? "Confirming location…" : "Location unconfirmed" : target === "machine"
      ? machineBlocked
        ? "Machine blocked"
        : "Fresh Linux box"
      : target === "remote"
        ? remoteHostName || "Remote"
        : "This host";

  const dotClass =
    target === "remote"
      ? remoteHealth === "blocked"
        ? "bg-rose-500"
        : remoteHealth === "connected"
          ? "bg-emerald-500"
          : "bg-amber-500"
      : target === "machine"
        ? machineBlocked
          ? "bg-rose-500"
          : "bg-accent"
        : "";

  const pillTone =
    prod
      ? "bg-rose-500/15 text-rose-600 dark:text-rose-300"
      : target !== "local"
        ? "bg-accent-soft text-accent"
        : "text-muted hover:bg-line/70 hover:text-ink";

  const chooseLocal = () => {
    if (remoteActive) void setRemoteActive(false);
    if (machineActive) void setMachineActive(false);
  };

  return (
    <DropdownMenu.Root onOpenChange={(open) => open && loadHosts()}>
      <DropdownMenu.Trigger asChild>
        <button
          type="button"
          disabled={targetLocked}
          className={cn(
            "inline-flex items-center gap-1.5 rounded-md px-2 py-1 text-xs font-medium transition-colors",
            pillTone,
          )}
          title={running || backendBusy ? "Stop the current run before changing where it runs" : targetLocked ? "Waiting for current backend state" : "Choose where this chat runs: this host, a fresh Linux box, or a remote SSH host"}
          aria-label="Execution target"
        >
          {dotClass && confirmed ? (
            <span className={cn("h-1.5 w-1.5 rounded-full", dotClass)} aria-hidden="true" />
          ) : (
            <Icon.Terminal size={12} className="shrink-0 text-faint" aria-hidden />
          )}
          <span className={compact ? "max-w-[8rem] truncate" : "max-w-[10rem] truncate"}>{label}</span>
          {prod ? (
            <span className="rounded-full bg-rose-500/20 px-1 text-[0.55rem] font-semibold uppercase tracking-wide">
              prod
            </span>
          ) : null}
          <Icon.Chevron size={11} className="shrink-0 text-faint" />
        </button>
      </DropdownMenu.Trigger>
      <DropdownMenu.Portal>
        <DropdownMenu.Content
          className={menuContentClass}
          side="top"
          align="start"
          sideOffset={8}
          collisionPadding={16}
        >
          <p className="px-2.5 py-1 text-[0.62rem] font-medium uppercase tracking-wide text-faint">
            Where this chat runs
          </p>
          {features.length > 0 && <p className="px-2.5 pb-1 text-[0.62rem] text-faint" title={destination?.workspace || undefined}>Configured · {features.join(" · ")}</p>}
          <DropdownMenu.Item className={itemClass} onSelect={() => chooseLocal()}>
            <span className="flex w-4 shrink-0 justify-center text-accent">
              {target === "local" ? <Icon.Check size={14} /> : null}
            </span>
            <Icon.Terminal size={15} className="shrink-0 text-muted" />
            <span className="min-w-0 flex-1">This host</span>
          </DropdownMenu.Item>
          <DropdownMenu.Item
            className={itemClass}
            onSelect={() => {
              const upgrade = Boolean(sessionId && upgradeChat === sessionId);
              if (upgrade && !window.confirm("Start a fresh Linux workstation for this chat? Your old desktop will be kept as a stopped backup.")) return;
              void setMachineActive(true, { upgrade });
            }}
          >
            <span className="flex w-4 shrink-0 justify-center text-accent">
              {target === "machine" ? <Icon.Check size={14} /> : null}
            </span>
            <Icon.Desktop size={15} className="shrink-0 text-muted" />
            <span className="min-w-0 flex-1">
              {sessionId && upgradeChat === sessionId ? "Upgrade Linux desktop" : "Fresh Linux box"}
              <span className="block text-[0.62rem] text-faint">Isolated Docker computer</span>
            </span>
          </DropdownMenu.Item>

          <DropdownMenu.Separator className="my-1.5 h-px bg-line" />
          <p className="px-2.5 py-1 text-[0.62rem] font-medium uppercase tracking-wide text-faint">
            Remote hosts (SSH)
          </p>
          {hosts.length === 0 ? (
            <DropdownMenu.Item
              className={itemClass}
              onSelect={() => openSettings("integrations")}
            >
              <span className="w-4 shrink-0" />
              <Icon.Plus size={15} className="shrink-0 text-muted" />
              <span className="min-w-0 flex-1 text-muted">Add a remote host…</span>
            </DropdownMenu.Item>
          ) : (
            hosts.map((h) => {
              const selected = remoteActive && remoteHostId === h.id;
              const isProd = h.risk_tier === "production";
              return (
                <DropdownMenu.Item
                  key={h.id}
                  className={itemClass}
                  onSelect={() => void setRemoteActive(true, h.id, h.name)}
                >
                  <span className="flex w-4 shrink-0 justify-center text-accent">
                    {selected ? <Icon.Check size={14} /> : null}
                  </span>
                  <Icon.Globe size={15} className="shrink-0 text-muted" />
                  <span className="min-w-0 flex-1 truncate">
                    {h.name}
                    <span className="block truncate text-[0.62rem] text-faint">
                      {h.host}
                      {h.port && h.port !== 22 ? `:${h.port}` : ""}
                    </span>
                  </span>
                  {isProd ? (
                    <span className="shrink-0 rounded-full bg-rose-500/15 px-1.5 py-px text-[0.55rem] font-semibold uppercase tracking-wide text-rose-600 dark:text-rose-300">
                      prod
                    </span>
                  ) : null}
                </DropdownMenu.Item>
              );
            })
          )}
          <DropdownMenu.Separator className="my-1.5 h-px bg-line" />
          <DropdownMenu.Item className={itemClass} onSelect={() => openSettings("integrations")}>
            <span className="w-4 shrink-0" />
            <Icon.Settings size={15} className="shrink-0 text-muted" />
            <span className="min-w-0 flex-1">Manage remote hosts…</span>
          </DropdownMenu.Item>
        </DropdownMenu.Content>
      </DropdownMenu.Portal>
    </DropdownMenu.Root>
  );
}
