import { useEffect, useState } from "react";
import { Icon } from "../lib/icons";
import { api } from "../api/client";

type Capability = {
  capability_id: string;
  version: string;
  description?: string;
  required_permissions?: string[];
  limits?: Record<string, number>;
};
type Peer = {
  peer_id: string;
  name: string;
  status: string;
  trust_state: string;
  fingerprint?: string;
  endpoint?: string;
  source?: string;
  skills?: string[];
  permissions?: string[];
  capabilities?: Capability[];
};
type PairRequest = {
  request_id: string;
  verification_code: string;
  requester?: { name?: string; fingerprint?: string };
};
type Grant = {
  grant_id: string;
  capability_id: string;
  state: string;
  requester_id: string;
  provider_id: string;
  permissions?: string[];
  limits?: Record<string, number>;
};
type Task = { task_id: string; objective: string; state: string };
type PeerIndex = { peers: Peer[]; local_identity?: { agent_id?: string } };
type Lesson = { task: string; summary: string };
type AskKnowledgeResult = {
  status: "ok" | "denied" | "timeout" | "error" | "created" | "running" | "failed" | "canceled";
  peer_id?: string;
  query?: string;
  lessons?: Lesson[];
  error?: string;
};
type SkillPreview = {
  name: string;
  description: string;
  body: string;
  allowed_tools: string;
  resource_count: string;
};
type ExportSkillResult = {
  status: "ok" | "denied" | "timeout" | "error" | "created" | "running" | "failed" | "canceled";
  peer_id?: string;
  skill?: string;
  bundle_id?: string;
  grant_id?: string;
  preview?: SkillPreview;
  error?: string;
};

const SCOPES = ["discover", "delegate_with_approval", "delegate"];
const OPTIONAL_PERMISSIONS = [
  "project.read",
  "project.write",
  "test.run",
  "shell.execute",
  "network.access",
  "browser.use",
  "artifact.write",
];

function TrustBadge({ trustState }: { trustState: string }) {
  const config: Record<string, { label: string; className: string; Glyph: typeof Icon.Check }> = {
    trusted: { label: "Trusted", className: "border-emerald-500/30 bg-emerald-500/10 text-emerald-500", Glyph: Icon.Check },
    pending: { label: "Pending", className: "border-amber-500/30 bg-amber-500/10 text-amber-500", Glyph: Icon.Loader },
    blocked: { label: "Blocked", className: "border-red-500/30 bg-red-500/10 text-red-500", Glyph: Icon.Alert },
    revoked: { label: "Revoked", className: "border-red-500/30 bg-red-500/10 text-red-500", Glyph: Icon.Alert },
  };
  const entry = config[trustState] || {
    label: trustState || "Unknown",
    className: "border-line bg-surface text-muted",
    Glyph: Icon.Globe,
  };
  const Glyph = entry.Glyph;
  return (
    <span className={`inline-flex items-center gap-1 rounded-full border px-2 py-0.5 text-[0.68rem] font-medium ${entry.className}`}>
      <Glyph size={12} className={trustState === "pending" ? "animate-spin" : undefined} />
      {entry.label}
    </span>
  );
}

/** AirDrop concentric radiating wave glyph */
function AirDropGlyph({ size = 24, className = "text-blue-500" }: { size?: number; className?: string }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 32 32"
      fill="none"
      stroke="currentColor"
      strokeWidth="2.2"
      strokeLinecap="round"
      strokeLinejoin="round"
      className={className}
      aria-hidden="true"
    >
      <circle cx="16" cy="22" r="1.75" fill="currentColor" stroke="none" />
      <path d="M12 17.5a5.7 5.7 0 0 1 8 0" />
      <path d="M8.5 13.5a10.6 10.6 0 0 1 15 0" />
      <path d="M5 9.5a15.5 15.5 0 0 1 22 0" />
    </svg>
  );
}

/** Animated circular SVG progress ring tracing an active AirDrop transfer / request */
function SendingRing({ size = 52 }: { size?: number }) {
  const strokeWidth = 3;
  const radius = (size - strokeWidth) / 2;
  const circumference = 2 * Math.PI * radius;
  return (
    <svg
      className="absolute -inset-1 animate-spin text-blue-500 pointer-events-none"
      width={size + 8}
      height={size + 8}
      viewBox={`0 0 ${size + 8} ${size + 8}`}
      aria-hidden="true"
    >
      <circle
        cx={(size + 8) / 2}
        cy={(size + 8) / 2}
        r={radius + 3}
        fill="transparent"
        stroke="currentColor"
        strokeWidth={strokeWidth}
        strokeDasharray={circumference}
        strokeDashoffset={circumference * 0.35}
        strokeLinecap="round"
      />
    </svg>
  );
}

/** Tactile 6-digit AirDrop verification code tiles */
function AirDropDigitTiles({ code, size = "md" }: { code: string; size?: "sm" | "md" | "lg" }) {
  const digits = (code || "").slice(0, 6).padEnd(6, "·").split("");
  const sizeClasses =
    size === "sm"
      ? "h-7 w-6 text-xs"
      : size === "lg"
      ? "h-14 w-11 text-2xl"
      : "h-10 w-8 text-base";
  return (
    <div className="flex items-center justify-center gap-1.5" aria-hidden="true">
      {digits.map((digit, idx) => (
        <span
          key={idx}
          className={`flex items-center justify-center rounded-lg border border-line bg-surface/90 font-mono font-bold text-ink shadow-xs backdrop-blur-xs transition-transform duration-150 ${sizeClasses}`}
        >
          {digit}
        </span>
      ))}
    </div>
  );
}

function describeTaskObjective(objective: string): string {
  // Task objectives are stored as compact JSON, e.g. {"skill":"invoice-formatter"}
  // or {"query":"how did you fix..."}. Render the useful part, not raw JSON.
  try {
    const parsed = JSON.parse(objective) as Record<string, unknown>;
    if (typeof parsed.skill === "string") return `skill.export: ${parsed.skill}`;
    if (typeof parsed.query === "string") return `knowledge.query: ${parsed.query}`;
    if (typeof parsed.name === "string" && typeof parsed.content === "string") {
      return `artifact.create: ${parsed.name}`;
    }
  } catch {
    // Not JSON — show as-is.
  }
  return objective;
}

function formatPeerError(error: string, peerName: string): string {
  if (!error) return "The request failed.";
  if (error.includes("401") || error.includes("signed authentication required")) {
    return `${peerName} has not completed pairing with your agent yet (or the pairing expired). Mutual approval is required before asking questions or borrowing skills.`;
  }
  if (error.includes("403") || error.includes("Forbidden") || error.includes("permission")) {
    return `Access was denied by ${peerName}. Check permissions in Agent Network.`;
  }
  if (error.includes("Connection refused") || error.includes("not reachable") || error.includes("timeout")) {
    return `Could not reach ${peerName}. Make sure the remote agent is online and reachable.`;
  }
  return error;
}

/** Shared overlay for every dialog in this panel: Escape closes it, clicking
 * the backdrop closes it, and the dialog role is announced to screen readers.
 * Six modals in this file previously hand-rolled this each without any
 * keyboard support at all. */
function ModalOverlay({
  label,
  onClose,
  children,
  maxWidth = "max-w-md",
}: {
  label: string;
  onClose: () => void;
  children: React.ReactNode;
  maxWidth?: string;
}) {
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        onClose();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [onClose]);

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/40 p-4"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div
        role="dialog"
        aria-modal="true"
        aria-label={label}
        className={`w-full ${maxWidth} rounded-xl bg-surface p-5 shadow-xl`}
      >
        {children}
      </div>
    </div>
  );
}

export function PeerNetwork({ onClose }: { onClose: () => void }) {
  const [peers, setPeers] = useState<Peer[]>([]);
  const [requests, setRequests] = useState<PairRequest[]>([]);
  const [capabilities, setCapabilities] = useState<Capability[]>([]);
  const [tasks, setTasks] = useState<Task[]>([]);
  const [grants, setGrants] = useState<Grant[]>([]);
  const [localAgentId, setLocalAgentId] = useState("");
  const [selected, setSelected] = useState<Peer | null>(null);
  const [permissions, setPermissions] = useState<string[]>(["discover"]);
  const [pairing, setPairing] = useState<PairRequest | null>(null);
  const [grantPeer, setGrantPeer] = useState<Peer | null>(null);
  const [reviewGrant, setReviewGrant] = useState<Grant | null>(null);
  const [grantCapability, setGrantCapability] = useState("");
  const [grantPermissions, setGrantPermissions] = useState<string[]>([]);
  const [grantDuration, setGrantDuration] = useState("3600");
  const [code, setCode] = useState("");
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [remoteSpec, setRemoteSpec] = useState("");
  const [inviteText, setInviteText] = useState("");
  const [askPeer, setAskPeer] = useState<Peer | null>(null);
  const [askQuery, setAskQuery] = useState("");
  const [askBusy, setAskBusy] = useState(false);
  const [askResult, setAskResult] = useState<AskKnowledgeResult | null>(null);
  const [exportPeer, setExportPeer] = useState<Peer | null>(null);
  const [exportSkillName, setExportSkillName] = useState("");
  const [exportBusy, setExportBusy] = useState(false);
  const [exportResult, setExportResult] = useState<ExportSkillResult | null>(null);
  const [installBusy, setInstallBusy] = useState(false);
  const [installMessage, setInstallMessage] = useState("");
  const [discoverNote, setDiscoverNote] = useState("");
  const [sendingPeerId, setSendingPeerId] = useState<string | null>(null);
  const [copiedInvite, setCopiedInvite] = useState(false);
  const [copiedFingerprint, setCopiedFingerprint] = useState<string | null>(null);

  const copyFingerprint = async (fingerprint: string) => {
    try {
      if (navigator.clipboard) {
        await navigator.clipboard.writeText(fingerprint);
        setCopiedFingerprint(fingerprint);
        window.setTimeout(() => setCopiedFingerprint(null), 2000);
      }
    } catch {
      // Clipboard access blocked
    }
  };

  const load = async () => {
    try {
      const [peerResult, requestResult, capabilityResult, taskResult, grantResult] =
        await Promise.all([
          api<PeerIndex>("/api/peers"),
          api<{ requests: PairRequest[] }>("/api/peers/pairing"),
          api<{ capabilities: Capability[] }>("/api/peers/capabilities"),
          api<{ tasks: Task[] }>("/api/peers/tasks"),
          api<{ grants: Grant[] }>("/api/peers/grants"),
        ]);
      setPeers(peerResult.peers || []);
      setLocalAgentId(peerResult.local_identity?.agent_id || "");
      setRequests(requestResult.requests || []);
      setCapabilities(capabilityResult.capabilities || []);
      setTasks(taskResult.tasks || []);
      setGrants(grantResult.grants || []);
    } catch (cause) {
      setError(String(cause));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    void load();
    const timer = window.setInterval(() => void load(), 5000);
    return () => window.clearInterval(timer);
  }, []);

  // A stale "Scan finished" note must not linger indefinitely once the user
  // has moved on to something else in the panel.
  useEffect(() => {
    if (!discoverNote) return;
    const timer = window.setTimeout(() => setDiscoverNote(""), 6000);
    return () => window.clearTimeout(timer);
  }, [discoverNote]);

  const run = async (action: () => Promise<unknown>) => {
    setBusy(true);
    setError("");
    try {
      await action();
      await load();
    } catch (cause) {
      setError(String(cause));
    } finally {
      setBusy(false);
    }
  };

  const startPairing = (peer: Peer) =>
    run(async () => {
      setSendingPeerId(peer.peer_id);
      try {
        const request = await api<PairRequest>("/api/peers/pair", {
          method: "POST",
          body: JSON.stringify({ peer_id: peer.peer_id, permissions }),
        });
        setSelected(null);
        setPairing(request);
        setCode(request.verification_code || "");
      } finally {
        setSendingPeerId(null);
      }
    });

  const approve = (request: PairRequest, accept: boolean) =>
    run(async () => {
      await api(`/api/peers/pair/${request.request_id}/${accept ? "approve" : "reject"}`, {
        method: "POST",
        body: JSON.stringify({ verification_code: code || request.verification_code }),
      });
      setPairing(null);
      setCode("");
    });

  const openGrantRequest = (peer: Peer) => {
    const available = peer.capabilities || capabilities;
    setGrantPeer(peer);
    const capability = available[0];
    setGrantCapability(capability?.capability_id || "");
    setGrantPermissions([...new Set([...(capability?.required_permissions || []), ...OPTIONAL_PERMISSIONS.slice(0, 3)])]);
  };

  const requestGrant = () =>
    run(async () => {
      if (!grantPeer || !grantCapability) return;
      const capability = (grantPeer.capabilities || capabilities).find(
        (item) => item.capability_id === grantCapability,
      );
      await api("/api/peers/grants/request-remote", {
        method: "POST",
        body: JSON.stringify({
          peer_id: grantPeer.peer_id,
          capability_id: grantCapability,
          permissions: grantPermissions.length ? grantPermissions : capability?.required_permissions || ["media.generate"],
          limits: {},
          expires_at: Math.floor(Date.now() / 1000) + Number(grantDuration),
        }),
      });
      setGrantPeer(null);
    });

  const openAskKnowledge = (peer: Peer) => {
    setAskPeer(peer);
    setAskQuery("");
    setAskResult(null);
  };

  const askKnowledge = async () => {
    if (!askPeer || !askQuery.trim()) return;
    setAskBusy(true);
    setAskResult(null);
    try {
      const result = await api<AskKnowledgeResult>("/api/peers/ask-knowledge", {
        method: "POST",
        body: JSON.stringify({
          peer_id: askPeer.peer_id,
          query: askQuery.trim(),
          grant_wait_seconds: 10,
          timeout_seconds: 30,
        }),
      });
      setAskResult(result);
      if (result.status === "ok") await load();
    } catch (cause) {
      setAskResult({ status: "error", error: String(cause) });
    } finally {
      setAskBusy(false);
    }
  };

  const togglePermission = (permission: string, checked: boolean) =>
    setGrantPermissions((old) => checked ? [...new Set([...old, permission])] : old.filter((item) => item !== permission));

  const openExportSkill = (peer: Peer, skillName?: string) => {
    setExportPeer(peer);
    setExportSkillName(skillName || "");
    setExportResult(null);
    setInstallMessage("");
  };

  const exportSkill = async () => {
    if (!exportPeer || !exportSkillName.trim()) return;
    setExportBusy(true);
    setExportResult(null);
    setInstallMessage("");
    try {
      const result = await api<ExportSkillResult>("/api/peers/export-skill", {
        method: "POST",
        body: JSON.stringify({
          peer_id: exportPeer.peer_id,
          skill: exportSkillName.trim(),
          grant_wait_seconds: 15,
          timeout_seconds: 30,
        }),
      });
      setExportResult(result);
    } catch (cause) {
      setExportResult({ status: "error", error: String(cause) });
    } finally {
      setExportBusy(false);
    }
  };

  // Retrying after a timeout must resume waiting on the same grant, not
  // request a brand-new one — otherwise an approval that lands just after
  // the wait window closes is permanently stranded, and every retry the
  // user makes burns another approval the owner has to grant for nothing.
  const retryExportSkill = async () => {
    if (!exportPeer || !exportSkillName.trim() || !exportResult?.grant_id) return;
    setExportBusy(true);
    try {
      const result = await api<ExportSkillResult>("/api/peers/export-skill", {
        method: "POST",
        body: JSON.stringify({
          peer_id: exportPeer.peer_id,
          skill: exportSkillName.trim(),
          grant_wait_seconds: 15,
          timeout_seconds: 30,
          pending_grant_id: exportResult.grant_id,
        }),
      });
      setExportResult(result);
    } catch (cause) {
      setExportResult({ status: "error", error: String(cause) });
    } finally {
      setExportBusy(false);
    }
  };

  const installExportedSkill = async () => {
    if (!exportResult?.bundle_id) return;
    setInstallBusy(true);
    try {
      const result = await api<{ ok?: boolean; installed?: string[]; error?: string }>(
        "/api/peers/install-skill-bundle",
        { method: "POST", body: JSON.stringify({ bundle_id: exportResult.bundle_id }) },
      );
      setInstallMessage(
        result.installed?.length
          ? `Installed: ${result.installed.join(", ")}`
          : result.error || "Nothing was installed.",
      );
      setExportResult(null);
    } catch (cause) {
      setInstallMessage(String(cause));
    } finally {
      setInstallBusy(false);
    }
  };

  const pendingGrantCount = grants.filter(
    (grant) => grant.state === "requested" && grant.provider_id === localAgentId,
  ).length;

  return (
    <main className="flex min-h-0 flex-1 flex-col overflow-y-auto p-6">
      <header className="mb-5 flex items-center justify-between">
        <div className="flex items-center gap-3">
          <span className="flex h-10 w-10 items-center justify-center rounded-xl border border-blue-500/20 bg-blue-500/10 text-blue-500 shadow-xs">
            <AirDropGlyph size={22} className="text-blue-500" />
          </span>
          <div>
            <div className="flex items-center gap-2">
              <h1 className="text-xl font-semibold">Agent Network</h1>
              <span className="inline-flex items-center gap-1 rounded-full border border-blue-500/30 bg-blue-500/10 px-2 py-0.5 text-[0.65rem] font-medium text-blue-600">
                AirDrop Mesh
              </span>
              {requests.length + pendingGrantCount > 0 ? (
                <span className="inline-flex h-5 min-w-5 items-center justify-center rounded-full bg-accent px-1.5 text-[0.65rem] font-semibold text-white">
                  {requests.length + pendingGrantCount}
                </span>
              ) : null}
            </div>
            <p className="text-sm text-muted">
              Connect to another Kageha agent to ask how it solved a problem or borrow one of its
              skills. Every request needs a one-time human approval before anything is shared.
            </p>
          </div>
        </div>
        <div className="flex gap-2">
          <button className="ka-sidebar-item inline-flex items-center gap-1.5" disabled={busy} onClick={() => run(async () => {
            const result = await api<{ peers?: Peer[] }>("/api/peers/discover", { method: "POST" });
            const found = result.peers?.length ?? 0;
            setDiscoverNote(
              found > 0
                ? `Scan finished — found ${found} peer${found === 1 ? "" : "s"}.`
                : "Scan finished — no peers found on this machine or the local network.",
            );
          })}>
            {busy ? <Icon.Loader size={13} className="animate-spin" /> : null}
            Discover
          </button>
          <button
            className="ka-sidebar-item inline-flex items-center gap-1.5"
            disabled={busy}
            onClick={() =>
              run(async () => {
                const issued = await api<{ invite?: string }>("/api/peers/invite");
                const token = issued.invite || "";
                setInviteText(token);
                try {
                  if (token && navigator.clipboard) {
                    await navigator.clipboard.writeText(token);
                    setCopiedInvite(true);
                    window.setTimeout(() => setCopiedInvite(false), 2500);
                  }
                } catch {
                  // Clipboard can be blocked when the window is not focused.
                }
              })
            }
          >
            {copiedInvite ? (
              <>
                <Icon.Check size={13} className="text-emerald-500" />
                <span className="text-emerald-500 font-medium">Copied invite!</span>
              </>
            ) : (
              <>
                <Icon.Copy size={13} />
                <span>Copy invite</span>
              </>
            )}
          </button>
          <button className="ka-sidebar-item" disabled={busy} onClick={() => void load()}>
            Refresh
          </button>
          <button className="ka-sidebar-item" onClick={onClose} title="Close panel">
            Close
          </button>
        </div>
      </header>

      {error && (
        <p className="mb-3 flex items-start justify-between gap-3 rounded border border-red-400/40 bg-red-500/10 p-3 text-sm text-red-500">
          <span>{error}</span>
          <button
            className="shrink-0 text-red-500 hover:underline"
            onClick={() => setError("")}
            aria-label="Dismiss error"
          >
            Dismiss
          </button>
        </p>
      )}

      {/* AirDrop Radar Proximity Deck */}
      <section className="mb-5 shrink-0 overflow-hidden rounded-2xl border border-[var(--color-line)] bg-gradient-to-b from-surface via-surface/90 to-canvas p-5 shadow-xs relative">
        <div className="flex flex-wrap items-center justify-between gap-2 mb-3">
          <div className="flex items-center gap-2">
            <span className="flex h-7 w-7 items-center justify-center rounded-lg bg-blue-500/10 text-blue-500">
              <AirDropGlyph size={18} className="text-blue-500" />
            </span>
            <h2 className="text-xs font-semibold tracking-wider uppercase text-muted">
              AirDrop Proximity Radar
            </h2>
          </div>
          <div className="flex items-center gap-2 text-xs text-muted">
            <span className="inline-flex items-center gap-1.5">
              <span className={`h-2 w-2 rounded-full ${loading ? "bg-amber-500 animate-ping" : peers.length ? "bg-emerald-500 animate-pulse" : "bg-muted"}`} />
              {loading ? "Scanning local mesh…" : `${peers.length} nearby agent${peers.length === 1 ? "" : "s"}`}
            </span>
          </div>
        </div>

        {/* Radar concentric circular sweep stage */}
        <div className="relative mx-auto my-2 flex h-64 w-full max-w-xl items-center justify-center overflow-hidden rounded-xl border border-line/60 bg-gradient-to-b from-canvas/40 via-surface/60 to-canvas/80 backdrop-blur-xs">
          {/* Concentric sonar wave rings */}
          <div className="pointer-events-none absolute inset-0 flex items-center justify-center" aria-hidden="true">
            {/* Outer ring */}
            <div className="h-56 w-56 rounded-full border border-blue-500/10" />
            {/* Mid ring */}
            <div className="absolute h-40 w-40 rounded-full border border-blue-500/15" />
            {/* Inner ring */}
            <div className="absolute h-24 w-24 rounded-full border border-blue-500/20" />

            {/* Animated pulsing sonar ripples */}
            <div className="absolute h-24 w-24 rounded-full border border-blue-400/40 airdrop-sonar-ring" />
            <div className="absolute h-24 w-24 rounded-full border border-blue-400/30 airdrop-sonar-ring-2" />
            <div className="absolute h-24 w-24 rounded-full border border-blue-400/20 airdrop-sonar-ring-3" />
          </div>

          {/* Center Node: "This Agent (You)" */}
          <div className="z-10 flex flex-col items-center select-none">
            <div className="relative flex h-13 w-13 items-center justify-center rounded-full bg-gradient-to-tr from-blue-600 to-indigo-500 text-white shadow-lg shadow-blue-500/25 airdrop-pulse-center">
              <AirDropGlyph size={24} className="text-white" />
              <span className="absolute -bottom-0.5 -right-0.5 flex h-4 w-4 items-center justify-center rounded-full bg-emerald-500 ring-2 ring-surface text-[9px] text-white font-bold">
                ✓
              </span>
            </div>
            <span className="mt-1.5 text-xs font-semibold text-ink">
              This Mac (You)
            </span>
            <span className="text-[10px] text-muted font-mono" title={localAgentId}>
              {localAgentId ? `${localAgentId.slice(0, 8)}…` : "Local device"}
            </span>
          </div>

          {/* Orbiting Discovered Peers */}
          {peers.map((peer, index) => {
            const total = peers.length;
            const angle = (index / total) * 2 * Math.PI - Math.PI / 2;
            const radiusX = 35; // % offset from center
            const radiusY = 30; // % offset from center
            const posX = 50 + radiusX * Math.cos(angle);
            const posY = 50 + radiusY * Math.sin(angle);

            const isSending = sendingPeerId === peer.peer_id;
            const isTrusted = peer.trust_state === "trusted";
            const isPending = peer.trust_state === "pending";
            const isBlocked = peer.trust_state === "blocked";

            return (
              <div
                key={`radar-${peer.peer_id}`}
                style={{ left: `${posX}%`, top: `${posY}%` }}
                className="group absolute -translate-x-1/2 -translate-y-1/2 z-20 flex flex-col items-center cursor-pointer transition-all duration-300 hover:scale-105 select-none"
                title={`${peer.name} (${peer.trust_state})`}
                onClick={() => {
                  if (isTrusted) {
                    openAskKnowledge(peer);
                  } else if (!isBlocked) {
                    setSelected(peer);
                    setPermissions(["discover", "delegate_with_approval"]);
                  }
                }}
              >
                <div className="relative flex items-center justify-center">
                  {isSending && <SendingRing size={44} />}
                  <div
                    className={`flex h-11 w-11 items-center justify-center rounded-full border-2 bg-surface shadow-sm transition-all ${
                      isTrusted
                        ? "border-emerald-500 text-emerald-600 shadow-emerald-500/20"
                        : isPending
                        ? "border-amber-500 text-amber-500 shadow-amber-500/20"
                        : isBlocked
                        ? "border-red-400 text-red-400"
                        : "border-blue-500 text-blue-600 shadow-blue-500/20"
                    }`}
                  >
                    <span className="text-xs font-bold">
                      {peer.name.slice(0, 2).toUpperCase()}
                    </span>
                  </div>
                  {isTrusted ? (
                    <span className="absolute -bottom-0.5 -right-0.5 h-3.5 w-3.5 rounded-full bg-emerald-500 ring-2 ring-surface flex items-center justify-center text-[8px] text-white font-bold">
                      ✓
                    </span>
                  ) : null}
                </div>
                <span className="mt-1 text-[11px] font-medium text-ink group-hover:text-accent transition-colors max-w-[80px] truncate text-center">
                  {peer.name} · nearby
                </span>
                <span className="text-[9px] text-muted">
                  {isSending ? "Sending…" : isTrusted ? "Query solutions" : isPending ? "Awaiting pair" : "Tap to send"}
                </span>
              </div>
            );
          })}
        </div>
      </section>

      <section className="mb-5 shrink-0 rounded-xl border border-[var(--color-line)] bg-surface p-4 shadow-2xs">
        <h2 className="mb-1.5 flex items-center gap-2 font-medium text-sm text-ink">
          <Icon.Globe size={15} className="text-muted" />
          Add remote peer
        </h2>
        <p className="mb-3 text-xs text-muted">
          Paste a public HTTPS Agent Card URL or a signed <code>kageha-peer:v1:</code> invite to connect.
        </p>
        <div className="flex flex-wrap gap-2">
          <input
            className="min-w-[16rem] flex-1 rounded-lg border border-[var(--color-line)] bg-canvas p-2 text-sm text-ink placeholder:text-muted/60"
            placeholder="https://host:8790 or kageha-peer:v1:..."
            value={remoteSpec}
            onChange={(event) => setRemoteSpec(event.target.value)}
          />
          <button
            className="ka-sidebar-item font-medium px-4"
            disabled={busy || !remoteSpec.trim()}
            onClick={() =>
              run(async () => {
                const spec = remoteSpec.trim();
                const body = spec.startsWith("kageha-peer:") ? { invite: spec } : { url: spec };
                await api("/api/peers/connect", { method: "POST", body: JSON.stringify(body) });
                setRemoteSpec("");
              })
            }
          >
            Connect
          </button>
        </div>
        {inviteText ? (
          <div className="mt-2.5 rounded-lg border border-line/60 bg-canvas/60 p-2 text-xs">
            <span className="text-[0.68rem] text-muted font-medium block mb-1">Your Agent's Invite Token:</span>
            <p className="break-all font-mono text-[0.68rem] text-ink select-all">{inviteText}</p>
          </div>
        ) : null}
      </section>

      {requests.length > 0 && (
        <section className="mb-5 shrink-0 rounded-2xl border border-blue-500/30 bg-gradient-to-b from-blue-500/10 via-surface/95 to-surface p-5 shadow-lg backdrop-blur-md airdrop-modal-pop">
          <div className="flex items-center justify-between mb-3">
            <h2 className="flex items-center gap-2 font-medium text-sm text-ink">
              <AirDropGlyph size={18} className="text-blue-500" />
              <Icon.Alert size={15} className="text-accent" />
              Pending pairing requests
            </h2>
            <span className="text-[0.68rem] font-medium text-blue-600 bg-blue-500/10 border border-blue-500/20 rounded-full px-2 py-0.5">
              AirDrop Pairing
            </span>
          </div>

          <div className="space-y-3">
            {requests.map((request) => (
              <div
                className="flex flex-col md:flex-row md:items-center justify-between gap-4 rounded-xl border border-line bg-canvas/70 p-4 shadow-2xs backdrop-blur-xs"
                key={request.request_id}
              >
                <div className="flex items-start gap-3">
                  <div className="flex h-10 w-10 shrink-0 items-center justify-center rounded-full bg-blue-500/10 text-blue-600 font-bold text-sm border border-blue-500/20">
                    {(request.requester?.name || "A").slice(0, 2).toUpperCase()}
                  </div>
                  <div className="text-sm">
                    <p className="font-semibold text-ink">
                      “{request.requester?.name || "Unknown agent"}” would like to pair with this agent
                    </p>
                    <div className="flex items-center gap-2 mt-1">
                      <span className="font-mono text-xs text-muted">{request.verification_code}</span>
                      <span className="text-xs text-muted">·</span>
                      <span className="text-xs text-muted font-mono truncate max-w-xs">{request.requester?.fingerprint}</span>
                    </div>
                  </div>
                </div>

                <div className="flex flex-wrap items-center gap-3">
                  <AirDropDigitTiles code={request.verification_code} size="sm" />
                  <div className="flex items-center gap-2">
                    <button
                      className="ka-sidebar-item rounded-full text-xs"
                      onClick={() => {
                        setPairing(request);
                        setCode(request.verification_code);
                      }}
                    >
                      Review
                    </button>
                    <button
                      className="ka-sidebar-item rounded-full text-xs text-red-500 hover:bg-red-500/10"
                      disabled={busy}
                      onClick={() => approve(request, false)}
                    >
                      Decline
                    </button>
                    <button
                      className="ka-sidebar-item rounded-full text-xs bg-blue-600 text-white hover:bg-blue-700 shadow-2xs"
                      disabled={busy}
                      onClick={() => approve(request, true)}
                    >
                      Accept
                    </button>
                  </div>
                </div>
              </div>
            ))}
          </div>
        </section>
      )}

      {discoverNote && (
        <p className="mb-3 shrink-0 text-sm text-muted">{discoverNote}</p>
      )}

      {loading ? (
        <div className="shrink-0 rounded-lg border border-dashed border-[var(--color-line)] p-6 text-center text-sm text-muted">
          <Icon.Loader size={22} className="mx-auto mb-2 animate-spin text-muted" />
          Loading peers…
        </div>
      ) : !peers.length ? (
        <div className="shrink-0 rounded-lg border border-dashed border-[var(--color-line)] p-6 text-center text-sm text-muted">
          <Icon.Network size={22} className="mx-auto mb-2 text-muted" />
          No peers discovered yet. Click Discover to scan this machine and the local network.
        </div>
      ) : (
        <div className="shrink-0 grid gap-3.5">
          {peers.map((peer) => {
            const isTrusted = peer.trust_state === "trusted";
            const isPending = peer.trust_state === "pending";
            const isBlocked = peer.trust_state === "blocked";
            const hasKnowledge = (peer.capabilities || []).some((item) => item.capability_id === "knowledge.query");
            const shortFingerprint = peer.fingerprint ? `${peer.fingerprint.slice(0, 8)}…${peer.fingerprint.slice(-6)}` : null;
            const isCopied = copiedFingerprint === peer.fingerprint;

            return (
              <article
                key={peer.peer_id}
                className={`rounded-xl border bg-surface p-4 shadow-xs transition-all hover:border-line-strong ${
                  isPending
                    ? "border-amber-500/40 border-l-4"
                    : isTrusted
                    ? "border-line"
                    : "border-line/70"
                }`}
              >
                <div className="flex flex-col gap-3.5 sm:flex-row sm:items-start sm:justify-between">
                  {/* Left: Avatar & Identity info */}
                  <div className="flex items-start gap-3 min-w-0">
                    <div
                      className={`flex h-10 w-10 shrink-0 items-center justify-center rounded-full border-2 text-xs font-bold transition-all ${
                        isTrusted
                          ? "border-emerald-500/80 bg-emerald-500/10 text-emerald-600 shadow-xs shadow-emerald-500/20"
                          : isPending
                          ? "border-amber-500 bg-amber-500/10 text-amber-500 shadow-xs shadow-amber-500/20"
                          : isBlocked
                          ? "border-red-400 bg-red-500/10 text-red-500"
                          : "border-blue-500/80 bg-blue-500/10 text-blue-600 shadow-xs shadow-blue-500/20"
                      }`}
                    >
                      {peer.name.slice(0, 2).toUpperCase()}
                    </div>

                    <div className="min-w-0 flex-1">
                      <div className="flex flex-wrap items-center gap-2">
                        <h2 className="font-semibold text-sm text-ink tracking-tight truncate">
                          {peer.name}
                        </h2>
                        <TrustBadge trustState={peer.trust_state} />
                        <span className="inline-flex items-center gap-1 text-[0.65rem] text-muted rounded-md bg-canvas px-1.5 py-0.5 border border-line/60">
                          {peer.source === "local" ? "local mesh" : peer.source === "invite" ? "invite" : peer.source || "peer"}
                        </span>
                      </div>

                      {/* Technical Info (Fingerprint & Endpoint) rendered compactly */}
                      <div className="mt-1 flex flex-wrap items-center gap-2 text-xs text-muted">
                        {peer.endpoint ? (
                          <span className="font-mono text-[0.68rem] text-muted truncate max-w-xs">
                            {peer.endpoint}
                          </span>
                        ) : null}
                        {shortFingerprint ? (
                          <>
                            <span className="text-muted/40">·</span>
                            <button
                              type="button"
                              onClick={() => peer.fingerprint && copyFingerprint(peer.fingerprint)}
                              className="inline-flex items-center gap-1 font-mono text-[0.68rem] text-muted hover:text-ink transition-colors"
                              title="Click to copy full fingerprint"
                            >
                              <span>{isCopied ? "Copied!" : `id: ${shortFingerprint}`}</span>
                              <Icon.Copy size={10} className={isCopied ? "text-emerald-500" : "text-muted"} />
                            </button>
                          </>
                        ) : null}
                      </div>
                    </div>
                  </div>

                  {/* Right: Action Buttons with clear hierarchy */}
                  <div className="flex flex-wrap items-center gap-2 shrink-0 sm:self-start">
                    {isTrusted ? (
                      <>
                        {hasKnowledge ? (
                          <button
                            className="ka-sidebar-item bg-accent/15 border-accent/40 text-accent font-semibold inline-flex items-center gap-1.5 shadow-2xs hover:bg-accent/25 transition-all text-xs"
                            onClick={() => openAskKnowledge(peer)}
                            title={`Query ${peer.name}'s verified problem-solving knowledge`}
                          >
                            <Icon.Brain size={13} className="text-accent" />
                            <span>Query Solutions</span>
                          </button>
                        ) : null}
                        <button
                          className="ka-sidebar-item text-xs"
                          onClick={() => openGrantRequest(peer)}
                        >
                          Request grant
                        </button>
                        <button
                          className="ka-sidebar-item text-xs"
                          onClick={() => { setSelected(peer); setPermissions(peer.permissions || ["discover"]); }}
                        >
                          Permissions
                        </button>
                        <button
                          className="ka-sidebar-item text-xs text-red-500/80 hover:text-red-500 hover:bg-red-500/10 transition-colors"
                          title="Remove trust permanently"
                          onClick={() => {
                            if (window.confirm(`Revoke trust with ${peer.name}? A new pairing will be required to trust it again.`)) {
                              void run(() => api(`/api/peers/${peer.peer_id}/revoke`, { method: "POST" }));
                            }
                          }}
                        >
                          Revoke trust
                        </button>
                      </>
                    ) : !isBlocked ? (
                      <button
                        className="ka-sidebar-item bg-accent text-white font-medium hover:bg-accent/90 shadow-2xs inline-flex items-center gap-1.5 px-3 text-xs"
                        onClick={() => { setSelected(peer); setPermissions(["discover", "delegate_with_approval"]); }}
                      >
                        <Icon.Network size={12} />
                        <span>Pair</span>
                      </button>
                    ) : (
                      <button
                        className="ka-sidebar-item border border-accent/40 text-accent font-medium text-xs"
                        onClick={() => run(() => api(`/api/peers/${peer.peer_id}/unblock`, { method: "POST" }))}
                      >
                        Unblock
                      </button>
                    )}

                    {!isBlocked && (
                      <button
                        className="ka-sidebar-item text-xs text-muted hover:text-red-500 hover:bg-red-500/10 transition-colors"
                        title="Block temporarily — can be unblocked anytime"
                        onClick={() => run(() => api(`/api/peers/${peer.peer_id}/block`, { method: "POST" }))}
                      >
                        Block (temporary)
                      </button>
                    )}
                  </div>
                </div>

                {/* Skills Section */}
                <div className="mt-3 pt-2.5 border-t border-line/60">
                  {(peer.skills || []).length ? (
                    <div className="flex flex-wrap items-center gap-1.5 text-xs">
                      <span className="text-muted font-medium text-[0.7rem] mr-1">Shareable skills:</span>
                      {(peer.skills || []).map((skillName) => (
                        <button
                          key={skillName}
                          className="inline-flex items-center gap-1 rounded-full border border-line bg-canvas px-2.5 py-0.5 text-[0.68rem] font-medium text-ink hover:border-accent hover:text-accent transition-colors shadow-2xs group"
                          disabled={!isTrusted}
                          onClick={() => openExportSkill(peer, skillName)}
                          title={isTrusted ? `Click to download & install ${skillName}` : "Pair with this agent first to borrow skills"}
                        >
                          <span aria-hidden="true" className="text-accent text-[0.65rem] group-hover:scale-110 transition-transform">⚡</span>
                          <span>{skillName}</span>
                        </button>
                      ))}
                      {isTrusted && (
                        <button
                          className="text-[0.68rem] text-muted underline hover:text-accent transition-colors ml-1"
                          onClick={() => openExportSkill(peer)}
                        >
                          Request a skill by name…
                        </button>
                      )}
                    </div>
                  ) : (
                    <div className="flex items-center justify-between text-xs text-muted">
                      <span className="text-[0.68rem]">Shareable skills: none advertised</span>
                      {isTrusted && (
                        <button
                          className="text-[0.68rem] text-muted underline hover:text-accent transition-colors"
                          onClick={() => openExportSkill(peer)}
                        >
                          Request a skill by name…
                        </button>
                      )}
                    </div>
                  )}
                </div>
              </article>
            );
          })}
        </div>
      )}

      <div className="mt-6 shrink-0 grid gap-4 md:grid-cols-3">
        <section className="rounded-lg border border-[var(--color-line)] bg-surface p-4">
          <h2 className="mb-2 flex items-center gap-2 font-medium">
            <Icon.Plug size={15} className="text-muted" />
            Published capabilities
          </h2>
          {capabilities.length ? capabilities.map((capability) => (
            <div className="border-b border-[var(--color-line)] py-2 last:border-0" key={capability.capability_id}>
              <div className="text-sm">{capability.capability_id} <span className="text-xs text-muted">v{capability.version}</span></div>
              <div className="text-xs text-muted">{capability.description || "No description"}</div>
            </div>
          )) : <p className="text-xs text-muted">No capabilities published.</p>}
        </section>

        <section className="rounded-lg border border-[var(--color-line)] bg-surface p-4">
          <h2 className="mb-2 flex items-center gap-2 font-medium">
            <Icon.Check size={15} className="text-muted" />
            Capability grants
            {pendingGrantCount > 0 ? (
              <span className="inline-flex h-5 min-w-5 items-center justify-center rounded-full bg-amber-500 px-1.5 text-[0.65rem] font-semibold text-white">
                {pendingGrantCount}
              </span>
            ) : null}
          </h2>
          {grants.length ? grants.slice(0, 8).map((grant) => {
            const needsMyApproval = grant.state === "requested" && grant.provider_id === localAgentId;
            return (
              <div
                className={`border-b border-[var(--color-line)] py-2 last:border-0 ${needsMyApproval ? "rounded-md bg-amber-500/5 px-2" : ""}`}
                key={grant.grant_id}
              >
                <div className="flex justify-between text-xs">
                  <span className="truncate pr-2">{grant.capability_id}</span>
                  <span className={needsMyApproval ? "font-medium text-amber-500" : "text-muted"}>{grant.state}</span>
                </div>
                {grant.permissions?.length ? <div className="mt-1 text-[0.68rem] text-muted">{grant.permissions.join(" · ")}</div> : null}
                {needsMyApproval && <button className="mt-1 ka-sidebar-item text-xs" onClick={() => { setReviewGrant(grant); setGrantPermissions(grant.permissions || []); }}>Review access</button>}
                {grant.state === "requested" && grant.requester_id === localAgentId && <button className="mt-1 ka-sidebar-item text-xs" onClick={() => run(() => api(`/api/peers/grants/${grant.grant_id}/sync`, { method: "POST", body: JSON.stringify({ peer_id: grant.provider_id }) }))}>Sync status</button>}
              </div>
            );
          }) : <p className="text-xs text-muted">No grants. Request one from a trusted peer.</p>}
        </section>

        <section className="rounded-lg border border-[var(--color-line)] bg-surface p-4">
          <h2 className="mb-2 flex items-center gap-2 font-medium">
            <Icon.Activity size={15} className="text-muted" />
            Collaboration tasks
          </h2>
          {tasks.length ? tasks.slice(0, 8).map((task) => (
            <div className="flex justify-between border-b border-[var(--color-line)] py-2 text-xs last:border-0" key={task.task_id}>
              <span className="truncate pr-2">{describeTaskObjective(task.objective)}</span><span className="text-muted">{task.state}</span>
            </div>
          )) : <p className="text-xs text-muted">No collaboration tasks.</p>}
          {tasks.length > 8 && (
            <p className="mt-1 text-[0.65rem] text-faint">+{tasks.length - 8} more not shown</p>
          )}
        </section>
      </div>

      {selected && (
        <ModalOverlay
          label={selected.trust_state === "trusted" ? "Peer permissions" : `Pair with ${selected.name}`}
          onClose={() => setSelected(null)}
        >
            <h2 className="text-lg font-semibold">{selected.trust_state === "trusted" ? "Peer permissions" : `Pair with ${selected.name}`}</h2>
            <p className="mt-2 break-all text-xs text-muted">Fingerprint: {selected.fingerprint}</p>
            {selected.trust_state !== "trusted" && (
              <p className="mt-2 text-xs text-muted">
                "delegate_with_approval" is what lets you Ask this agent a question or request one
                of its skills later — every individual request still needs its own approval on
                both sides. Leaving it unchecked limits this peer to discovery only.
              </p>
            )}
            <div className="mt-4 space-y-2">{SCOPES.map((scope) => (
              <label className="flex items-center gap-2 text-sm" key={scope}>
                <input type="checkbox" checked={permissions.includes(scope)} onChange={(event) => setPermissions((old) => event.target.checked ? [...new Set([...old, scope])] : old.filter((item) => item !== scope))} />
                {scope}
              </label>
            ))}</div>
            <div className="mt-5 flex justify-end gap-2">
              <button className="ka-sidebar-item" onClick={() => setSelected(null)}>Cancel</button>
              {selected.trust_state === "trusted" ? (
                <button className="ka-sidebar-item" disabled={busy || !permissions.length} onClick={() => run(async () => { await api(`/api/peers/${selected.peer_id}/permissions`, { method: "POST", body: JSON.stringify({ permissions }) }); setSelected(null); })}>Save</button>
              ) : (
                <button className="ka-sidebar-item" disabled={busy || !permissions.length} onClick={() => startPairing(selected)}>Start pairing</button>
              )}
            </div>
        </ModalOverlay>
      )}

      {pairing && (
        <ModalOverlay label="Verify pairing code" onClose={() => setPairing(null)}>
          <div className="flex flex-col items-center text-center">
            <div className="mb-2 flex h-12 w-12 items-center justify-center rounded-full bg-blue-500/10 text-blue-600 airdrop-pulse-center">
              <AirDropGlyph size={28} className="text-blue-500" />
            </div>
            <h2 className="text-lg font-semibold">Verify pairing code</h2>
            <p className="mt-1 text-xs text-muted">
              Confirm that this code matches on both agents before approving.
            </p>

            <p className="my-3 text-center font-mono text-3xl tracking-[0.3em]">
              {pairing.verification_code}
            </p>

            <AirDropDigitTiles code={pairing.verification_code} size="md" />

            <div className="mt-4 w-full">
              <input
                className="w-full rounded-lg border border-[var(--color-line)] bg-canvas p-2.5 font-mono text-center text-base tracking-widest placeholder:tracking-normal"
                placeholder="Enter matching code"
                value={code}
                onChange={(event) => setCode(event.target.value)}
                autoFocus
              />
            </div>

            <div className="mt-5 flex w-full justify-end gap-2">
              <button className="ka-sidebar-item rounded-full px-4" onClick={() => setPairing(null)}>
                Later
              </button>
              <button
                className="ka-sidebar-item rounded-full px-4 text-red-500 hover:bg-red-500/10"
                disabled={busy}
                onClick={() => approve(pairing, false)}
              >
                Reject
              </button>
              <button
                className="ka-sidebar-item rounded-full px-5 bg-blue-600 text-white hover:bg-blue-700 shadow-2xs disabled:opacity-50"
                disabled={busy || code.length !== 6}
                onClick={() => approve(pairing, true)}
              >
                Approve
              </button>
            </div>
          </div>
        </ModalOverlay>
      )}

      {askPeer && (
        <ModalOverlay
          label={`Query ${askPeer.name}'s knowledge base`}
          onClose={() => setAskPeer(null)}
          maxWidth="max-w-lg"
        >
          <div className="flex items-center gap-2.5 mb-1.5">
            <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-xl bg-accent/15 text-accent border border-accent/25">
              <Icon.Brain size={18} />
            </div>
            <div>
              <h2 className="text-base font-semibold text-ink">
                Query {askPeer.name}'s Solutions
              </h2>
              <p className="text-[0.68rem] text-muted">
                Search verified problem-solving lessons and task solutions
              </p>
            </div>
          </div>

          <p className="mt-1 text-xs text-muted leading-relaxed">
            Search how {askPeer.name} solved a problem in the past. Only completed, verified lessons are returned — not an open-ended conversational chat.
          </p>

          <textarea
            className="mt-3.5 h-20 w-full rounded-lg border border-[var(--color-line)] bg-canvas p-2.5 text-xs text-ink placeholder:text-muted/60 focus:border-accent outline-none transition-colors"
            placeholder="e.g., How did you fix flaky retry tests? or How did you structure the Instagram carousel?"
            value={askQuery}
            onChange={(event) => setAskQuery(event.target.value)}
            onKeyDown={(event) => {
              if ((event.metaKey || event.ctrlKey) && event.key === "Enter" && askQuery.trim() && !askBusy) {
                event.preventDefault();
                void askKnowledge();
              }
            }}
            disabled={askBusy}
            autoFocus
          />

          {/* Quick suggestion chips */}
          <div className="mt-2 flex flex-wrap items-center gap-1.5 text-xs">
            <span className="text-[0.68rem] text-muted font-medium">Suggestions:</span>
            {(askPeer.skills || []).map((skill) => (
              <button
                key={skill}
                type="button"
                className="rounded-md border border-line bg-surface px-2 py-0.5 text-[0.68rem] text-muted hover:border-accent hover:text-accent transition-colors"
                onClick={() => setAskQuery(`How did you implement ${skill}?`)}
              >
                How to implement {skill.split("-")[0]}?
              </button>
            ))}
            <button
              type="button"
              className="rounded-md border border-line bg-surface px-2 py-0.5 text-[0.68rem] text-muted hover:border-accent hover:text-accent transition-colors"
              onClick={() => setAskQuery("How did you fix flaky network tests?")}
            >
              Flaky tests?
            </button>
            <button
              type="button"
              className="rounded-md border border-line bg-surface px-2 py-0.5 text-[0.68rem] text-muted hover:border-accent hover:text-accent transition-colors"
              onClick={() => setAskQuery("What tasks have you completed successfully?")}
            >
              Completed tasks?
            </button>
          </div>

          {askResult && (
            <div className="mt-4 rounded-xl border border-[var(--color-line)] bg-canvas p-3.5 text-sm shadow-2xs">
              {askResult.status === "ok" ? (
                (askResult.lessons || []).length ? (
                  <ol className="list-decimal space-y-2 pl-4">
                    {(askResult.lessons || []).map((lesson, index) => (
                      <li key={index}>
                        <div className="font-medium text-xs text-ink">{lesson.task}</div>
                        <div className="text-[0.68rem] text-muted mt-0.5">{lesson.summary}</div>
                      </li>
                    ))}
                  </ol>
                ) : (
                  <div className="py-2.5 text-center space-y-2">
                    <p className="font-semibold text-ink text-xs">No verified solutions found</p>
                    <p className="text-xs text-muted max-w-sm mx-auto leading-relaxed">
                      {askPeer.name} hasn't documented any completed problem-solving lessons matching &ldquo;{askResult.query || askQuery}&rdquo;.
                    </p>
                    {(askPeer.skills || []).length > 0 && (
                      <div className="pt-2.5 border-t border-line/60 mt-2">
                        <p className="text-[0.7rem] text-muted mb-2 font-medium">You can borrow {askPeer.name.split(" ")[0]}'s skills directly:</p>
                        <div className="flex flex-wrap justify-center gap-1.5">
                          {(askPeer.skills || []).map((skill) => (
                            <button
                              key={skill}
                              type="button"
                              className="inline-flex items-center gap-1 rounded-full border border-line bg-surface px-2.5 py-1 text-xs text-ink hover:border-accent hover:text-accent shadow-2xs transition-colors"
                              onClick={() => {
                                const p = askPeer;
                                setAskPeer(null);
                                openExportSkill(p, skill);
                              }}
                            >
                              <span aria-hidden="true" className="text-accent text-[0.65rem]">⚡</span>
                              <span>{skill}</span>
                            </button>
                          ))}
                        </div>
                      </div>
                    )}
                  </div>
                )
              ) : askResult.status === "timeout" ? (
                <p className="text-amber-500 text-xs">
                  {askPeer.name} has not approved this request yet. Ask them to open Agent
                  Network → Capability grants and approve it, then try again.
                </p>
              ) : askResult.status === "denied" ? (
                <p className="text-red-500 text-xs">{askPeer.name} declined this request.</p>
              ) : (
                <div>
                  <p className="text-red-500 text-xs">{formatPeerError(askResult.error || "", askPeer.name)}</p>
                  {((askResult.error || "").includes("401") || (askResult.error || "").includes("signed authentication")) && (
                    <button
                      className="ka-sidebar-item mt-2 text-xs text-accent border border-accent/40 font-medium"
                      onClick={() => {
                        const p = askPeer;
                        setAskPeer(null);
                        setSelected(p);
                        setPermissions(["discover", "delegate_with_approval"]);
                      }}
                    >
                      Re-pair with {askPeer.name}
                    </button>
                  )}
                </div>
              )}
            </div>
          )}
          <div className="mt-5 flex justify-end gap-2">
            <button className="ka-sidebar-item" onClick={() => setAskPeer(null)}>Close</button>
            <button
              className="ka-sidebar-item bg-accent/15 border-accent/40 text-accent font-semibold px-4"
              disabled={askBusy || !askQuery.trim()}
              onClick={() => void askKnowledge()}
            >
              {askBusy ? "Searching…" : "Search Solutions"}
            </button>
          </div>
        </ModalOverlay>
      )}

      {exportPeer && (
        <ModalOverlay
          label={`Request a skill from ${exportPeer.name}`}
          onClose={() => setExportPeer(null)}
          maxWidth="max-w-lg"
        >
            <h2 className="text-lg font-semibold">Request a skill from {exportPeer.name}</h2>
            <p className="mt-1 text-xs text-muted">
              Requests a fresh approval every time — unlike Ask, this is never reused from a
              prior grant, because it transfers actual code. Nothing installs until you review
              the preview below and confirm.
            </p>
            <input
              className="mt-4 w-full rounded border border-[var(--color-line)] bg-canvas p-2 text-sm"
              placeholder="skill-name"
              value={exportSkillName}
              onChange={(event) => setExportSkillName(event.target.value)}
              onKeyDown={(event) => {
                if (
                  event.key === "Enter" &&
                  exportSkillName.trim() &&
                  !exportBusy &&
                  !exportResult?.bundle_id
                ) {
                  event.preventDefault();
                  void exportSkill();
                }
              }}
              disabled={exportBusy || Boolean(exportResult?.bundle_id) || exportResult?.status === "timeout"}
              autoFocus
            />
            {exportResult && (
              <div className="mt-4 rounded-lg border border-[var(--color-line)] bg-canvas p-3 text-sm">
                {exportResult.status === "ok" && exportResult.preview ? (
                  <div>
                    <div className="font-medium">{exportResult.preview.name}</div>
                    <p className="mt-1 text-xs text-muted">{exportResult.preview.description}</p>
                    <p className="mt-2 text-[0.68rem] text-muted">
                      Tools: {exportResult.preview.allowed_tools || "none"} · Resources:{" "}
                      {exportResult.preview.resource_count}
                    </p>
                    <pre className="mt-2 max-h-40 overflow-auto whitespace-pre-wrap rounded border border-[var(--color-line)] bg-surface p-2 text-[0.68rem]">
                      {exportResult.preview.body}
                    </pre>
                    <p className="mt-2 text-xs font-medium text-amber-500">
                      Review this before installing — it will run under your agent's own tool
                      access, not the peer's.
                    </p>
                  </div>
                ) : exportResult.status === "timeout" ? (
                  <div>
                    <p className="text-amber-500">
                      {exportPeer.name} has not approved this request yet.
                    </p>
                    <p className="mt-1 text-[0.68rem] text-muted">
                      Try again once they approve it — this checks the same request rather than
                      asking for a new one, so their approval isn't wasted.
                    </p>
                    <button
                      className="ka-sidebar-item mt-3"
                      disabled={exportBusy}
                      onClick={() => void retryExportSkill()}
                    >
                      {exportBusy ? "Checking…" : "Try again"}
                    </button>
                  </div>
                ) : exportResult.status === "denied" ? (
                  <p className="text-red-500">{exportPeer.name} declined this request.</p>
                ) : (
                  <div>
                    <p className="text-red-500">{formatPeerError(exportResult.error || "", exportPeer.name)}</p>
                    {((exportResult.error || "").includes("401") || (exportResult.error || "").includes("signed authentication")) && (
                      <button
                        className="ka-sidebar-item mt-2 text-xs text-accent border border-accent/40 font-medium"
                        onClick={() => {
                          const p = exportPeer;
                          setExportPeer(null);
                          setSelected(p);
                          setPermissions(["discover", "delegate_with_approval"]);
                        }}
                      >
                        Re-pair with {exportPeer.name}
                      </button>
                    )}
                  </div>
                )}
              </div>
            )}
            {installMessage && <p className="mt-3 text-xs text-muted">{installMessage}</p>}
            <div className="mt-5 flex justify-end gap-2">
              <button className="ka-sidebar-item" onClick={() => setExportPeer(null)}>Close</button>
              {exportResult?.status === "ok" && exportResult.bundle_id ? (
                <button
                  className="ka-sidebar-item"
                  disabled={installBusy}
                  onClick={() => void installExportedSkill()}
                >
                  {installBusy ? "Installing…" : "Install"}
                </button>
              ) : exportResult?.status === "timeout" ? null : (
                <button
                  className="ka-sidebar-item"
                  disabled={exportBusy || !exportSkillName.trim()}
                  onClick={() => void exportSkill()}
                >
                  {exportBusy ? "Requesting…" : "Request"}
                </button>
              )}
            </div>
        </ModalOverlay>
      )}

      {grantPeer && (
        <ModalOverlay label={`Request grant from ${grantPeer.name}`} onClose={() => setGrantPeer(null)}>
            <h2 className="text-lg font-semibold">Request grant from {grantPeer.name}</h2>
            <p className="mt-1 text-xs text-muted">The provider must approve this request before the agent can delegate work.</p>
            <label className="mt-4 block text-sm">Capability
              <select className="mt-1 w-full rounded border border-[var(--color-line)] bg-canvas p-2" value={grantCapability} onChange={(event) => setGrantCapability(event.target.value)} autoFocus>
                {(grantPeer.capabilities || capabilities).map((capability) => <option key={capability.capability_id} value={capability.capability_id}>{capability.capability_id}</option>)}
              </select>
            </label>
            <div className="mt-4 grid gap-2 sm:grid-cols-2">
              {Array.from(new Set([...(grantPeer.capabilities || capabilities).find((item) => item.capability_id === grantCapability)?.required_permissions || [], ...OPTIONAL_PERMISSIONS])).map((permission) => {
                const required = (grantPeer.capabilities || capabilities).find((item) => item.capability_id === grantCapability)?.required_permissions?.includes(permission);
                return <label className="flex items-center gap-2 text-xs" key={permission}>
                  <input type="checkbox" checked={grantPermissions.includes(permission)} disabled={required} onChange={(event) => togglePermission(permission, event.target.checked)} />
                  <span>{permission}{required ? " · required" : ""}</span>
                </label>;
              })}
            </div>
            <label className="mt-4 block text-xs text-muted">Access expires in
              <select className="mt-1 w-full rounded border border-[var(--color-line)] bg-canvas p-2 text-sm text-ink" value={grantDuration} onChange={(event) => setGrantDuration(event.target.value)}>
                <option value="900">15 minutes</option><option value="3600">1 hour</option><option value="86400">24 hours</option>
              </select>
            </label>
            <div className="mt-5 flex justify-end gap-2">
              <button className="ka-sidebar-item" onClick={() => setGrantPeer(null)}>Cancel</button>
              <button className="ka-sidebar-item" disabled={busy || !grantCapability} onClick={() => void requestGrant()}>Request grant</button>
            </div>
        </ModalOverlay>
      )}

      {reviewGrant && (
        <ModalOverlay label="Approve access" onClose={() => setReviewGrant(null)}>
            <h2 className="text-lg font-semibold">Approve access</h2>
            <p className="mt-1 text-xs text-muted">Grant only what this agent needs.</p>
            <div className="mt-4 grid gap-2 sm:grid-cols-2">
              {(reviewGrant.permissions || []).map((permission) => {
                const required = capabilities.find((item) => item.capability_id === reviewGrant.capability_id)?.required_permissions?.includes(permission);
                return (
                <label className="flex items-center gap-2 text-xs" key={permission}>
                  <input type="checkbox" checked={grantPermissions.includes(permission)} disabled={required} onChange={(event) => togglePermission(permission, event.target.checked)} />
                  {permission}{required ? " · required" : ""}
                </label>
                );
              })}
            </div>
            <div className="mt-5 flex justify-end gap-2">
              <button className="ka-sidebar-item" onClick={() => setReviewGrant(null)}>Later</button>
              <button className="ka-sidebar-item text-red-500" disabled={busy} onClick={() => run(async () => { await api(`/api/peers/grants/${reviewGrant.grant_id}/deny`, { method: "POST" }); setReviewGrant(null); })}>Reject</button>
              <button className="ka-sidebar-item" disabled={busy || !grantPermissions.length} onClick={() => run(async () => { await api(`/api/peers/grants/${reviewGrant.grant_id}/approve`, { method: "POST", body: JSON.stringify({ permissions: grantPermissions }) }); setReviewGrant(null); })}>Approve selected</button>
            </div>
        </ModalOverlay>
      )}
    </main>
  );
}
