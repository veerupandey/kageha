import { CapabilityCatalog } from "./CapabilityCatalog";
import { FallbackSettings } from "./FallbackSettings";
import * as DropdownMenu from "@radix-ui/react-dropdown-menu";
import { useEffect, useMemo, useState } from "react";
import type { ModelInfo } from "../api/types";
import { api } from "../api/client";
import { cn } from "../lib/cn";
import { Icon } from "../lib/icons";
import { modelDisplayLabel, providerDisplayName } from "../lib/modelCatalog";
import { parseSearchSetup } from "../lib/searchSetup";
import { useAppStore } from "../store";

interface SetupModel {
  id: string;
  provider: string;
  model: string;
  ready?: boolean;
}

interface SetupConnection {
  id: string;
  label: string;
  kind: string;
  auth_flow?: string;
  ready?: boolean;
  detected?: boolean;
  env?: string;
  hint?: string;
}

interface SetupPayload {
  credentials?: { can_authorize: boolean; locked: boolean };
  connections: SetupConnection[];
  models: SetupModel[];
  default_model?: string;
  roles?: { default?: string; planner?: string; worker?: string; acknowledgment?: string };
  search?: unknown;
}

interface BrowserCredentialStatus {
  configured: boolean;
  can_authorize: boolean;
  state: "ready" | "locked" | "not_initialized" | "external" | "not_configured";
}

const ENDPOINT_PRESETS = [
  { id: "kiro", label: "Kiro", url: "http://127.0.0.1:8787/v1", hint: "Sign in with kiro-cli, then start kiro-proxy. No API key needed; discover models to connect." },
  { id: "ollama", label: "Ollama", url: "http://localhost:11434/v1", hint: "Usually no API key" },
  { id: "lmstudio", label: "LM Studio", url: "http://localhost:1234/v1", hint: "Local OpenAI server" },
  { id: "vllm", label: "vLLM", url: "http://localhost:8000/v1", hint: "Local OpenAI server" },
  { id: "openrouter", label: "OpenRouter", url: "https://openrouter.ai/api/v1", hint: "Needs an API key" },
  { id: "groq", label: "Groq", url: "https://api.groq.com/openai/v1", hint: "Needs an API key" },
  { id: "custom", label: "Custom", url: "https://api.example.com/v1", hint: "Any OpenAI-compatible /v1 URL" },
] as const;

const fieldClass =
  "w-full rounded-lg border border-line bg-canvas px-3 py-2 text-sm text-ink outline-none transition-colors placeholder:text-faint focus:border-accent focus:ring-2 focus:ring-accent/15";
const btnClass = "rounded-lg px-3 py-2 text-xs font-medium transition-colors disabled:opacity-50";

function providerInitials(label: string): string {
  const parts = label.trim().split(/\s+/).filter(Boolean);
  if (parts.length >= 2) return `${parts[0][0]}${parts[1][0]}`.toUpperCase();
  return label.slice(0, 2).toUpperCase();
}

export function ModelsSettings() {
  const catalog = useAppStore((s) => s.modelCatalog);
  const loadModels = useAppStore((s) => s.loadModels);
  const setPrefs = useAppStore((s) => s.setPrefs);

  const [setup, setSetup] = useState<SetupPayload | null>(null);
  const [browserCredentials, setBrowserCredentials] = useState<BrowserCredentialStatus | null>(null);
  const [presetId, setPresetId] = useState<(typeof ENDPOINT_PRESETS)[number]["id"]>("ollama");
  const [providerName, setProviderName] = useState("Ollama");
  const [baseUrl, setBaseUrl] = useState("http://localhost:11434/v1");
  const [apiKey, setApiKey] = useState("");
  const [protocol, setProtocol] = useState("openai_compat");
  const [discovered, setDiscovered] = useState<Array<{ id: string }>>([]);
  const [chosen, setChosen] = useState<string[]>([]);
  const [defaultModel, setDefaultModel] = useState("");
  const [planner, setPlanner] = useState("");
  const [worker, setWorker] = useState("");
  const [acknowledgment, setAcknowledgment] = useState("");
  const [savedRoles, setSavedRoles] = useState({ default: "", planner: "", worker: "", acknowledgment: "" });
  const [busy, setBusy] = useState("");
  const [message, setMessage] = useState("");
  const [authorizationUrl, setAuthorizationUrl] = useState("");
  const [messageTone, setMessageTone] = useState<"ok" | "warn">("ok");
  const [keyDrafts, setKeyDrafts] = useState<Record<string, string>>({});
  const [addHost, setAddHost] = useState("");
  const [addModelId, setAddModelId] = useState("");

  const preset = ENDPOINT_PRESETS.find((row) => row.id === presetId) || ENDPOINT_PRESETS[0];

  const note = (text: string, tone: "ok" | "warn" = "ok") => {
    setMessage(text);
    setMessageTone(tone);
  };

  const refresh = async () => {
    const data = await api<SetupPayload>("/api/provider-setup");
    setSetup(data);
    const roles = data.roles || {};
    const firstReady = data.models.find((model) => model.ready)?.id || "";
    const configured =
      (roles.default && data.models.some((model) => model.id === roles.default && model.ready)
        ? roles.default
        : "") || firstReady;
    setDefaultModel(configured);
    setPlanner(roles.planner && data.models.some((m) => m.id === roles.planner) ? roles.planner : configured);
    setWorker(roles.worker && data.models.some((m) => m.id === roles.worker) ? roles.worker : configured);
    setAcknowledgment(roles.acknowledgment || "");
    setSavedRoles({
      acknowledgment: roles.acknowledgment || "",
      default: configured,
      planner: roles.planner && data.models.some((m) => m.id === roles.planner) ? roles.planner : configured,
      worker: roles.worker && data.models.some((m) => m.id === roles.worker) ? roles.worker : configured,
    });
    const search = parseSearchSetup(data.search);
    if (search) useAppStore.setState({ searchSetup: search });
    setAddHost((current) => {
      const hosts = data.connections.filter((row) => row.kind !== "oauth");
      if (current && hosts.some((row) => row.id === current)) return current;
      return hosts.find((row) => row.ready)?.id || hosts[0]?.id || "";
    });
    await loadModels();
  };

  useEffect(() => {
    void refresh().catch((error) => note(String(error), "warn"));
    void api<BrowserCredentialStatus>("/api/provider-setup/browser-credentials")
      .then(setBrowserCredentials)
      .catch(() => setBrowserCredentials(null));
  }, []);

  const models = useMemo(() => setup?.models.filter((model) => model.ready) || [], [setup]);
  const modelHosts = useMemo(
    () => (setup?.connections || []).filter((connection) => connection.kind !== "oauth"),
    [setup?.connections],
  );
  const roleGroups = useMemo(() => {
    const extras = setup?.connections || [];
    const order: string[] = [];
    const groups = new Map<string, SetupModel[]>();
    for (const model of models) {
      const source = providerDisplayName(model.provider, extras) || "Other";
      const list = groups.get(source);
      if (!list) {
        groups.set(source, [model]);
        order.push(source);
      } else {
        list.push(model);
      }
    }
    return order.map((source) => ({ source, models: groups.get(source) || [] }));
  }, [models, setup?.connections]);
  const rolesDirty =
    defaultModel !== savedRoles.default ||
    planner !== savedRoles.planner ||
    worker !== savedRoles.worker || acknowledgment !== savedRoles.acknowledgment;

  const applyPreset = (id: (typeof ENDPOINT_PRESETS)[number]["id"]) => {
    const next = ENDPOINT_PRESETS.find((row) => row.id === id) || ENDPOINT_PRESETS[0];
    setPresetId(next.id);
    setProtocol("openai_compat");
    setProviderName(next.label);
    setBaseUrl(next.url);
    setDiscovered([]);
    setChosen([]);
  };

  const discoverModels = async () => {
    setBusy("discover");
    note("");
    try {
      const data = await api<{ models: Array<{ id: string }> }>("/api/provider-setup/discover", {
        method: "POST",
        body: JSON.stringify({ base_url: baseUrl, api_key: apiKey, protocol }),
      });
      setDiscovered(data.models);
      setChosen(data.models.map((model) => model.id));
      note(`${data.models.length} models found. Pick the ones you want, then connect.`);
    } catch (error) {
      note(String(error), "warn");
    } finally {
      setBusy("");
    }
  };

  const connect = async () => {
    setBusy("connect");
    note("");
    try {
      await api("/api/provider-setup/connect", {
        method: "POST",
        body: JSON.stringify({
          provider_name: providerName || preset.label,
          protocol,
          base_url: baseUrl,
          api_key: apiKey,
          models: chosen,
        }),
      });
      setApiKey("");
      await refresh();
      note("Connected. Those models are now in the picker.");
    } catch (error) {
      note(String(error), "warn");
    } finally {
      setBusy("");
    }
  };

  const connectOAuth = async (provider: string, label?: string) => {
    const name = label || (provider === "codex" ? "Codex Runtime" : provider === "antigravity" ? "Gemini CLI" : provider);
    setBusy(provider);
    note("");
    try {
      const data = await api<{ ok: boolean; authorization_url?: string }>("/api/provider-setup/oauth", {
        method: "POST",
        body: JSON.stringify({ provider }),
      });
      if (data.authorization_url) {
        const destination = new URL(data.authorization_url);
        if (destination.protocol !== "https:") throw new Error("OAuth requires an HTTPS authorization URL");
        setAuthorizationUrl(destination.toString());
        window.open(destination.toString(), "_blank", "noopener,noreferrer");
        note("Complete sign-in in the opened tab, then refresh the model list.");
        return;
      }
      await refresh();
      note(
        data.ok ? `${name} login connected.` : `No ${name} login found on this machine.`,
        data.ok ? "ok" : "warn",
      );
    } catch (error) {
      note(String(error), "warn");
    } finally {
      setBusy("");
    }
  };

  const saveKey = async (provider: string) => {
    const secret = (keyDrafts[provider] || "").trim();
    if (!secret) {
      note("Paste an API key first.", "warn");
      return;
    }
    setBusy(`key-${provider}`);
    note("");
    try {
      await api("/api/provider-setup/key", {
        method: "POST",
        body: JSON.stringify({ provider, api_key: secret }),
      });
      setKeyDrafts((current) => ({ ...current, [provider]: "" }));
      await refresh();
      note("Key saved on the server. It is never shown back in the browser.");
    } catch (error) {
      note(String(error), "warn");
    } finally {
      setBusy("");
    }
  };

  const addCatalogModel = async () => {
    const model = addModelId.trim();
    if (!addHost) {
      note("Pick a host first.", "warn");
      return;
    }
    if (!model) {
      note("Paste the model id the API expects.", "warn");
      return;
    }
    setBusy("add-model");
    note("");
    try {
      const data = await api<{ id?: string; provider?: string }>("/api/provider-setup/model", {
        method: "POST",
        body: JSON.stringify({ provider: addHost, model }),
      });
      setAddModelId("");
      await refresh();
      note(`Added ${data.id || model}. It is selectable in the composer under that host.`);
    } catch (error) {
      note(String(error), "warn");
    } finally {
      setBusy("");
    }
  };

  const saveRoles = async () => {
    setBusy("roles");
    note("");
    try {
      await api("/api/provider-setup/roles", {
        method: "POST",
        body: JSON.stringify({ default: defaultModel, planner, worker, acknowledgment }),
      });
      setPrefs({
        mainModel: defaultModel,
        plannerModel: planner,
        workerModel: worker,
      });
      await loadModels();
      setSavedRoles({ default: defaultModel, planner, worker, acknowledgment });
      note("Model roles saved.");
    } catch (error) {
      note(String(error), "warn");
    } finally {
      setBusy("");
    }
  };

  return (
    <div className="space-y-6">
      {browserCredentials?.can_authorize && ["locked", "not_initialized"].includes(browserCredentials.state) && <section className="rounded-xl border border-line bg-surface p-4" aria-label="Browser credentials unavailable">
        <h3 className="text-sm font-semibold text-ink">Browser credentials need access</h3>
        <p className="mt-1 text-xs leading-relaxed text-muted">Allow Kageha to use macOS Keychain for passwords it creates for your browser accounts. Enter your Mac password only in the macOS dialog.</p>
        <button type="button" className={cn(btnClass,"mt-3 bg-accent text-black")} disabled={busy !== ""} onClick={async () => {
          setBusy("browser-unlock");
          note("");
          try {
            await api("/api/provider-setup/browser-credentials/authorize", { method: "POST", body: "{}" });
            const status = await api<BrowserCredentialStatus>("/api/provider-setup/browser-credentials");
            setBrowserCredentials(status);
            if (status.state === "ready") note("Browser credentials are ready. You can retry the task.");
            else note("Browser credentials are still unavailable. Check Keychain and try again.", "warn");
          } catch (error) { note(String(error), "warn"); }
          finally { setBusy(""); }
        }}>{busy === "browser-unlock" ? "Waiting for macOS…" : "Unlock browser credentials"}</button>
      </section>}
      {setup?.credentials?.can_authorize && setup.credentials.locked && <section className="rounded-xl border border-line bg-surface p-4" aria-label="Saved credentials locked">
        <h3 className="text-sm font-semibold text-ink">Saved credentials are locked</h3>
        <p className="mt-1 text-xs leading-relaxed text-muted">Authorize this Kageha build in macOS Keychain. Enter your password only in the macOS dialog. Your saved keys and chats stay intact.</p>
        <button type="button" className={cn(btnClass,"mt-3 bg-accent text-black")} disabled={busy !== ""} onClick={async () => {
          setBusy("unlock");
          try {
            await api("/api/provider-setup/unlock", { method: "POST", body: "{}" });
            await refresh(); await loadModels(); note("Saved credentials unlocked for this running Kageha instance.");
          } catch (error) { note(String(error),"warn"); }
          finally { setBusy(""); }
        }}>{busy === "unlock" ? "Waiting for macOS…" : "Unlock saved credentials"}</button>
      </section>}
      {models.length > 0 ? (
        <section className="rounded-2xl border border-line bg-canvas/35 p-4">
          <div className="flex items-start justify-between gap-3">
            <div>
              <h3 className="text-sm font-semibold text-ink">Who does what</h3>
              <p className="mt-1 max-w-xl text-[0.7rem] leading-relaxed text-muted">
                Planner thinks. Worker runs tools. Default fills in when Auto is selected. Same weights can live on more than one provider.
              </p>
            </div>
            <button
              type="button"
              className={cn(
                btnClass,
                "shrink-0",
                rolesDirty ? "bg-accent text-black hover:brightness-110" : "bg-line/60 text-muted",
              )}
              onClick={() => void saveRoles()}
              disabled={busy !== "" || !rolesDirty}
            >
              {busy === "roles" ? "Saving…" : "Save roles"}
            </button>
          </div>
          <div className="ka-role-grid mt-4">
            <RoleSlotCard
              label="Default"
              hint="Composer Auto and new chats"
              icon={Icon.Logo}
              value={defaultModel}
              onChange={setDefaultModel}
              catalog={catalog}
              connections={setup?.connections || []}
              groups={roleGroups}
            />
            <RoleSlotCard
              label="Planner"
              hint="Thinks through the approach"
              icon={Icon.Brain}
              value={planner}
              onChange={setPlanner}
              catalog={catalog}
              connections={setup?.connections || []}
              groups={roleGroups}
            />
            <RoleSlotCard
              label="Worker"
              hint="Runs tools and writes code"
              icon={Icon.Skills}
              value={worker}
              onChange={setWorker}
              catalog={catalog}
              connections={setup?.connections || []}
              groups={roleGroups}
            />
            <RoleSlotCard
              label="Acknowledgment"
              hint="Streams a brief reply while work starts"
              autoLabel="Chat model"
              icon={Icon.Brain}
              value={acknowledgment}
              onChange={setAcknowledgment}
              catalog={catalog}
              connections={setup?.connections || []}
              groups={roleGroups}
            />
          </div>
        </section>
      ) : (
        <p className="rounded-2xl border border-dashed border-line bg-canvas/30 px-4 py-3.5 text-sm text-muted">
          No models are ready yet. Connect a key or an OpenAI-compatible endpoint below.
        </p>
      )}

      <section className="rounded-2xl border border-line bg-canvas/35 p-4">
        <h3 className="text-sm font-semibold text-ink">Add a model</h3>
        <p className="mt-1 text-[0.7rem] leading-relaxed text-muted">
          Paste the id the host expects. It shows up under that provider in the composer and can be selected.
        </p>
        <div className="mt-3 grid gap-2 sm:grid-cols-[10rem_1fr_auto]">
          <select
            className={fieldClass}
            value={addHost}
            onChange={(event) => setAddHost(event.target.value)}
            aria-label="Host for new model"
          >
            {modelHosts.map((connection) => (
              <option key={connection.id} value={connection.id}>
                {connection.label}
              </option>
            ))}
          </select>
          <input
            className={fieldClass}
            value={addModelId}
            onChange={(event) => setAddModelId(event.target.value)}
            placeholder="e.g. moonshotai/Kimi-K2 or gpt-5.6-sol"
            aria-label="New model id"
            autoComplete="off"
            onKeyDown={(event) => {
              if (event.key === "Enter") {
                event.preventDefault();
                void addCatalogModel();
              }
            }}
          />
          <button
            type="button"
            className={`${btnClass} bg-accent text-black hover:brightness-110`}
            onClick={() => void addCatalogModel()}
            disabled={busy !== "" || !addHost}
          >
            {busy === "add-model" ? "Adding…" : "Add model"}
          </button>
        </div>
      </section>

      <CapabilityCatalog />
      <FallbackSettings />

      <section>
        <div className="mb-3">
          <h3 className="text-sm font-semibold text-ink">Providers</h3>
          <p className="mt-0.5 text-[0.7rem] text-muted">
            Paste an API key to unlock that family in the composer picker.
          </p>
        </div>
        <div className="grid gap-2.5 sm:grid-cols-2">
          {(setup?.connections || []).map((connection) => (
            <div
              key={connection.id}
              className="rounded-2xl border border-line bg-canvas/40 p-3.5"
            >
              <div className="flex items-start gap-3">
                <span className="flex h-9 w-9 shrink-0 items-center justify-center rounded-xl bg-accent-soft text-[0.68rem] font-semibold tracking-wide text-accent">
                  {providerInitials(connection.label)}
                </span>
                <div className="min-w-0 flex-1">
                  <div className="flex items-center justify-between gap-2">
                    <span className="truncate text-sm font-medium text-ink">{connection.label}</span>
                    <span
                      className={cn(
                        "shrink-0 rounded-full px-1.5 py-px text-[0.58rem] font-medium uppercase tracking-wide",
                        connection.ready
                          ? "bg-accent-soft text-accent"
                          : "bg-line/70 text-faint",
                      )}
                    >
                      {connection.ready ? "Ready" : "Not connected"}
                    </span>
                  </div>
                  {connection.hint ? (
                    <p className="mt-1 text-[0.65rem] leading-relaxed text-faint">{connection.hint}</p>
                  ) : null}
                </div>
              </div>
              {connection.kind === "proxy" || connection.id === "kiro" ? (
                <p className="mt-3 text-xs text-faint">Uses your Kiro CLI login through the local proxy. No API key required.</p>
              ) : connection.kind === "oauth" ? (
                <button
                  type="button"
                  className={`${btnClass} mt-3 w-full bg-line/70 text-ink hover:bg-line`}
                  onClick={() => void connectOAuth(connection.id, connection.label)}
                  disabled={busy !== ""}
                >
                  {busy === connection.id ? "Checking…" : connection.auth_flow === "pkce" ? "Sign in" : "Use existing login"}
                </button>
              ) : (
                <div className="mt-3 flex gap-2">
                  <input
                    className={fieldClass}
                    type="password"
                    value={keyDrafts[connection.id] || ""}
                    onChange={(event) =>
                      setKeyDrafts((current) => ({ ...current, [connection.id]: event.target.value }))
                    }
                    placeholder={connection.ready ? "Paste a new key to replace" : "Paste API key"}
                    aria-label={`${connection.label} API key`}
                    autoComplete="off"
                  />
                  <button
                    type="button"
                    className={`${btnClass} shrink-0 bg-accent text-black hover:brightness-110`}
                    onClick={() => void saveKey(connection.id)}
                    disabled={busy !== "" || !(keyDrafts[connection.id] || "").trim()}
                  >
                    {busy === `key-${connection.id}`
                      ? "Saving…"
                      : connection.ready
                        ? "Replace"
                        : "Save"}
                  </button>
                </div>
              )}
            </div>
          ))}
        </div>
      </section>

      <section className="rounded-2xl border border-line bg-canvas/35 p-4">
        <div>
          <h3 className="text-sm font-semibold text-ink">Add a compatible API endpoint</h3>
          <p className="mt-1 text-[0.7rem] leading-relaxed text-muted">
            OpenAI- or Anthropic-compatible APIs. Models refresh automatically every six hours, with cached fallback.
          </p>
        </div>
        <div className="mt-3 flex flex-wrap gap-1.5">
          {ENDPOINT_PRESETS.map((row) => (
            <button
              key={row.id}
              type="button"
              className={cn(
                "rounded-full border px-2.5 py-1 text-[0.65rem] font-medium transition-colors",
                presetId === row.id
                  ? "border-accent bg-accent-soft text-ink"
                  : "border-line text-muted hover:border-line-strong hover:text-ink",
              )}
              onClick={() => applyPreset(row.id)}
            >
              {row.label}
            </button>
          ))}
        </div>
        <p className="mt-2 text-[0.65rem] text-faint">{preset.hint}</p>
        <div className="mt-3 grid gap-2 sm:grid-cols-2">
          <select className={fieldClass} aria-label="API format" value={protocol}
            onChange={(event) => { setProtocol(event.target.value); setDiscovered([]); setChosen([]); }}>
            <option value="openai_compat">OpenAI-compatible</option>
            <option value="anthropic_compat">Anthropic-compatible</option>
          </select>
          <input
            className={fieldClass}
            value={providerName}
            onChange={(event) => setProviderName(event.target.value)}
            placeholder="Name, e.g. Ollama"
            aria-label="Provider name"
          />
          <input
            className={`sm:col-span-2 ${fieldClass}`}
            value={baseUrl}
            onChange={(event) => setBaseUrl(event.target.value)}
            placeholder="https://api.example.com/v1"
            aria-label="Base URL"
          />
          <input
            className={`sm:col-span-2 ${fieldClass}`}
            type="password"
            value={apiKey}
            onChange={(event) => setApiKey(event.target.value)}
            placeholder="API key (optional for local servers)"
            aria-label="API key"
            autoComplete="off"
          />
        </div>
        <div className="mt-3 flex flex-wrap gap-2">
          <button
            type="button"
            className={`${btnClass} bg-line/70 text-ink hover:bg-line`}
            onClick={() => void discoverModels()}
            disabled={busy !== ""}
          >
            {busy === "discover" ? "Looking…" : "Find models"}
          </button>
          <button
            type="button"
            className={`${btnClass} bg-accent text-black hover:brightness-110`}
            onClick={() => void connect()}
            disabled={busy !== "" || chosen.length === 0}
          >
            {busy === "connect" ? "Connecting…" : "Connect"}
          </button>
        </div>
        {discovered.length > 0 ? (
          <div className="mt-3 grid max-h-36 gap-1 overflow-auto sm:grid-cols-2">
            {discovered.map((model) => (
              <label key={model.id} className="flex items-center gap-2 text-xs text-muted">
                <input
                  type="checkbox"
                  checked={chosen.includes(model.id)}
                  onChange={(event) =>
                    setChosen((current) =>
                      event.target.checked
                        ? [...current, model.id]
                        : current.filter((id) => id !== model.id),
                    )
                  }
                />
                {model.id}
              </label>
            ))}
          </div>
        ) : null}
      </section>

      {message ? (
        <p
          className={cn(
            "rounded-xl px-3.5 py-2.5 text-xs leading-relaxed",
            messageTone === "warn" ? "bg-warn-soft text-warn" : "bg-accent-soft text-ink",
          )}
        >
          {message}
          {authorizationUrl && <a href={authorizationUrl} target="_blank" rel="noopener noreferrer" className="ml-2 underline">Continue sign-in</a>}
        </p>
      ) : null}
    </div>
  );
}

function RoleSlotCard({
  label,
  autoLabel = "Auto",
  hint,
  icon: SlotIcon,
  value,
  onChange,
  catalog,
  connections,
  groups,
}: {
  label: string;
  autoLabel?: string;
  hint: string;
  icon: typeof Icon.Brain;
  value: string;
  onChange: (id: string) => void;
  catalog: ModelInfo[];
  connections: SetupConnection[];
  groups: { source: string; models: SetupModel[] }[];
}) {
  const selected = groups.flatMap((group) => group.models).find((model) => model.id === value);
  const name = value
    ? modelDisplayLabel(catalog, value) || selected?.model || value
    : autoLabel;
  const via = selected ? providerDisplayName(selected.provider, connections) : "";

  return (
    <DropdownMenu.Root>
      <DropdownMenu.Trigger asChild>
        <button type="button" className="ka-role-card" aria-label={`${label} model`}>
          <span className="ka-role-kicker">
            <SlotIcon size={13} />
            {label}
          </span>
          <span className="ka-role-name">{name}</span>
          <span className="ka-role-hint">{hint}</span>
          <span className="ka-role-meta">
            <span className={cn("ka-role-provider", !via && "is-auto")}>
              {via || `Follows ${autoLabel}`}
            </span>
            <Icon.Chevron size={14} className="shrink-0 text-faint" />
          </span>
        </button>
      </DropdownMenu.Trigger>
      <DropdownMenu.Portal>
        <DropdownMenu.Content className="ka-role-menu" align="start" sideOffset={8}>
          <DropdownMenu.Item className="ka-role-menu-item" onSelect={() => onChange("")}>
            <span className="flex w-4 shrink-0 justify-center text-accent">
              {!value ? <Icon.Check size={14} /> : null}
            </span>
            {autoLabel}
          </DropdownMenu.Item>
          {groups.map((group) => (
            <DropdownMenu.Group key={group.source}>
              <DropdownMenu.Label className="ka-role-menu-label">
                {group.source}
              </DropdownMenu.Label>
              {group.models.map((model) => (
                <DropdownMenu.Item
                  key={model.id}
                  className="ka-role-menu-item"
                  onSelect={() => onChange(model.id)}
                >
                  <span className="flex w-4 shrink-0 justify-center text-accent">
                    {value === model.id ? <Icon.Check size={14} /> : null}
                  </span>
                  <span className="min-w-0 flex-1 truncate">
                    {modelDisplayLabel(catalog, model.id) || model.model || model.id}
                  </span>
                </DropdownMenu.Item>
              ))}
            </DropdownMenu.Group>
          ))}
        </DropdownMenu.Content>
      </DropdownMenu.Portal>
    </DropdownMenu.Root>
  );
}
