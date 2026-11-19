import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "../api/client";
import { cn } from "../lib/cn";
import { usePluginRuntime } from "../lib/usePluginRuntime";
import { McpAssertionCredential } from "./McpAssertionCredential";
import { McpMtlsCredential } from "./McpMtlsCredential";

type Transport = "stdio" | "sse" | "http";

type Server = {
  name: string;
  transport: string;
  command?: string;
  args?: string[];
  url?: string;
  plugin: string;
  origin: "plugin" | "standalone";
  enabled: boolean;
  plugin_enabled?: boolean;
  editable: boolean;
  revision?: number;
  oauth?: { client_id?: string; callback_url?: string } | null;
};

const TRANSPORTS: { id: Transport; label: string; hint: string }[] = [
  { id: "stdio", label: "Local", hint: "stdio command" },
  { id: "sse", label: "SSE", hint: "remote URL" },
  { id: "http", label: "HTTP", hint: "streamable HTTP" },
];

const fieldClass =
  "w-full rounded-lg border border-line bg-canvas px-3 py-2 text-sm text-ink outline-none transition-colors placeholder:text-faint focus:border-accent focus:ring-2 focus:ring-accent/15";
const btnClass = "rounded-lg px-3 py-2 text-xs font-medium transition-colors disabled:opacity-50";

function initials(name: string): string {
  const parts = name.trim().split(/[\s._-]+/).filter(Boolean);
  if (parts.length >= 2) return `${parts[0][0]}${parts[1][0]}`.toUpperCase();
  return name.slice(0, 2).toUpperCase() || "MCP";
}

function serverDetail(server: Server): string {
  if (server.transport === "stdio") {
    return [server.command, ...(server.args || [])].filter(Boolean).join(" ") || "stdio";
  }
  return server.url || server.transport;
}

function transportLabel(transport: string): string {
  if (transport === "stdio") return "Local";
  if (transport === "sse") return "SSE";
  if (transport === "http" || transport === "streamable_http") return "HTTP";
  return transport;
}

export function McpSettings({ onOpenPlugin }: { onOpenPlugin?: (name: string) => void }) {
  const { runtime, runtimeError } = usePluginRuntime();
  const [servers, setServers] = useState<Server[]>([]);
  const [name, setName] = useState("");
  const [transport, setTransport] = useState<Transport>("stdio");
  const [command, setCommand] = useState("");
  const [url, setUrl] = useState("");
  const [mailTool, setMailTool] = useState("");
  const [environment, setEnvironment] = useState("");
  const [json, setJson] = useState("");
  const [busy, setBusy] = useState("");
  const [note, setNote] = useState("");
  const [tone, setTone] = useState<"ok" | "warn">("ok");
  const [oauthPending, setOauthPending] = useState<{ name: string; url: string; epoch: number } | null>(null);
  const oauthEpoch = useRef(0);

  const load = useCallback(async () => {
    const data = await api<{ servers: Server[] }>("/api/mcp");
    setServers(data.servers || []);
  }, []);

  useEffect(() => {
    void load().catch((error) => {
      setTone("warn");
      setNote(String(error));
    });
  }, [load]);

  useEffect(() => {
    if (!oauthPending) return;
    let stopped = false;
    let timer: ReturnType<typeof setTimeout>;
    const poll = async () => {
      try {
        const state = await api<{ status: string; pending: boolean }>("/api/mcp/oauth/status", { method: "POST", body: JSON.stringify({ name: oauthPending.name }) });
        if (stopped || oauthEpoch.current !== oauthPending.epoch) return;
        if (state.pending) { timer = setTimeout(poll, 2500); return; }
        if (state.status !== "ready") throw new Error("Authorization expired or was cancelled. Please try again.");
        // The user selected Connect; discover tools after consent and preserve the server's stored configuration.
        await api("/api/mcp", { method: "POST", body: JSON.stringify({ name: oauthPending.name, enabled: true }) });
        if (stopped) return;
        setOauthPending(null); await load(); setTone("ok"); setNote("Connection authorized and enabled.");
      } catch (error) { if (!stopped) { setOauthPending(null); setTone("warn"); setNote(error instanceof Error ? error.message : "Connection failed"); } }
    };
    timer = setTimeout(poll, 2500);
    return () => { stopped = true; clearTimeout(timer); };
  }, [oauthPending, load]);

  const connectOAuth = (server: Server) => {
    const epoch = ++oauthEpoch.current;
    const popup = window.open("about:blank", "_blank");
    if (popup) popup.opener = null;
    setBusy(`oauth-${server.name}`); setNote("");
    void (async () => {
      try {
        const result = await api<{ authorization_url: string }>("/api/mcp/oauth/start", { method: "POST", body: JSON.stringify({ name: server.name }) });
        setOauthPending({ name: server.name, url: result.authorization_url, epoch });
        if (popup) popup.location.href = result.authorization_url;
      } catch (error) { popup?.close(); setTone("warn"); setNote(error instanceof Error ? error.message : "Could not start authorization"); }
      finally { setBusy(""); }
    })();
  };

  const enable = async (server: Server) => {
    setBusy(`enable-${server.name}`); setNote("");
    try {
      const data = await api<{ servers: Server[] }>("/api/mcp", { method: "POST", body: JSON.stringify({ name: server.name, enabled: true }) });
      setServers(data.servers || []); setTone("ok"); setNote("Connection enabled.");
    } catch (error) { setTone("warn"); setNote(error instanceof Error ? error.message : "Could not enable connection"); }
    finally { setBusy(""); }
  };

  const payload = () => {
    const parts = command.trim().split(/\s+/).filter(Boolean);
    const extra: { verification_mail_tool?: string; read_only_tools?: string[]; env?: Record<string, string> } = {};
    if (mailTool.trim()) {
      extra.verification_mail_tool = mailTool.trim();
      extra.read_only_tools = [mailTool.trim()];
    }
    if (environment.trim()) {
      let value: unknown;
      try { value = JSON.parse(environment); }
      catch { throw new Error("MCP environment must be valid JSON"); }
      if (!value || typeof value !== "object" || Array.isArray(value) || Object.values(value).some((v) => typeof v !== "string")) {
        throw new Error("MCP environment must be a JSON object with string values");
      }
      extra.env = value as Record<string, string>;
    }
    return {
      name: name.trim(),
      transport,
      command: parts[0] || "",
      args: parts.slice(1),
      url: url.trim(),
      ...extra,
    };
  };

  const saveServer = async () => {
    setBusy("save");
    setNote("");
    try {
      const data = await api<{ servers: Server[] }>("/api/mcp", {
        method: "POST",
        body: JSON.stringify(payload()),
      });
      setServers(data.servers || []);
      setName("");
      setCommand("");
      setUrl("");
      setMailTool("");
      setEnvironment("");
      setTone("ok");
      setNote("Saved. Use Plugins → Reload active sessions to expose new tools.");
    } catch (error) {
      setTone("warn");
      setNote(error instanceof Error ? error.message : "Could not save MCP server");
    } finally {
      setBusy("");
    }
  };

  const testServer = async (server?: Server) => {
    setBusy("test");
    setNote("");
    try {
      const data = await api<{ servers: Array<{ ok: boolean; error?: string; tools?: number }> }>(
        "/api/mcp/test",
        { method: "POST", body: JSON.stringify(server ? { saved_name: server.name } : payload()) },
      );
      const result = data.servers?.[0];
      if (result?.ok) {
        setTone("ok");
        setNote(`Connection works${result.tools ? ` · ${result.tools} tools` : ""}.`);
      } else {
        setTone("warn");
        setNote(`Connection failed: ${result?.error || "unknown error"}`);
      }
    } catch (error) {
      setTone("warn");
      setNote(error instanceof Error ? error.message : "Connection test failed");
    } finally {
      setBusy("");
    }
  };

  const saveJson = async () => {
    setBusy("json");
    setNote("");
    try {
      const data = await api<{ servers: Server[] }>("/api/mcp", {
        method: "POST",
        body: JSON.stringify({ json }),
      });
      setServers(data.servers || []);
      setJson("");
      setTone("ok");
      setNote("Imported. Use Plugins → Reload active sessions to expose new tools.");
    } catch (error) {
      setTone("warn");
      setNote(error instanceof Error ? error.message : "Invalid MCP JSON");
    } finally {
      setBusy("");
    }
  };

  const remove = async (server: Server) => {
    setBusy(`remove-${server.name}`);
    setNote("");
    try {
      await api(`/api/mcp/${encodeURIComponent(server.name)}`, { method: "DELETE" });
      await load();
    } catch (error) {
      setTone("warn");
      setNote(error instanceof Error ? error.message : "Could not remove server");
    } finally {
      setBusy("");
    }
  };

  const canSave = Boolean(name.trim() && (transport === "stdio" ? command.trim() : url.trim()));

  return (
    <div className="space-y-6">
      <section className="rounded-2xl border border-line bg-canvas/35 p-4">
        <h3 className="text-sm font-semibold text-ink">Add a server</h3>
        <p className="mt-1 text-[0.7rem] leading-relaxed text-muted">
          Local stdio processes or remote SSE / streamable HTTP.
        </p>
        <div className="mt-3 flex flex-wrap gap-1.5">
          {TRANSPORTS.map((row) => (
            <button
              key={row.id}
              type="button"
              className={cn("ka-pick-chip", transport === row.id && "is-on")}
              aria-pressed={transport === row.id}
              onClick={() => setTransport(row.id)}
            >
              {row.label}
              <span className="text-[0.62rem] font-normal text-faint">{row.hint}</span>
            </button>
          ))}
        </div>
        <div className="mt-3 grid gap-2">
          <input
            className={fieldClass}
            placeholder="Server name"
            value={name}
            onChange={(event) => setName(event.target.value)}
            aria-label="MCP server name"
            autoComplete="off"
          />
          {transport === "stdio" ? (
            <input
              className={fieldClass}
              placeholder="Command, e.g. npx -y @modelcontextprotocol/server-filesystem /tmp"
              value={command}
              onChange={(event) => setCommand(event.target.value)}
              aria-label="MCP command"
              autoComplete="off"
            />
          ) : (
            <input
              className={fieldClass}
              placeholder="https://example.com/mcp"
              value={url}
              onChange={(event) => setUrl(event.target.value)}
              aria-label="MCP server URL"
              autoComplete="off"
            />
          )}
          {transport === "stdio" && <input
            className={fieldClass}
            type="password"
            placeholder={'Environment JSON (optional), e.g. {"TOKEN":"…"}'}
            value={environment}
            onChange={(event) => setEnvironment(event.target.value)}
            aria-label="MCP environment JSON"
            autoComplete="off"
          />}
          <input
            className={fieldClass}
            placeholder="Protected mailbox tool (optional)"
            value={mailTool}
            onChange={(event) => setMailTool(event.target.value)}
            aria-label="Protected mailbox tool"
            autoComplete="off"
          />
          <p className="text-[0.7rem] leading-relaxed text-muted">Only mark a trusted, read-only tool that implements the protected verification mailbox contract. Environment values are saved as managed secrets.</p>
          <div className="flex gap-2">
            <button
              type="button"
              className={cn(btnClass, "flex-1 bg-line/70 text-ink hover:bg-line")}
              disabled={!canSave || busy !== ""}
              onClick={() => void testServer()}
            >
              {busy === "test" ? "Testing…" : "Test"}
            </button>
            <button
              type="button"
              className={cn(btnClass, "flex-1 bg-accent text-black hover:brightness-110")}
              disabled={!canSave || busy !== ""}
              onClick={() => void saveServer()}
            >
              {busy === "save" ? "Saving…" : "Save"}
            </button>
          </div>
        </div>
      </section>

      <section className="rounded-2xl border border-line bg-canvas/35 p-4">
        <h3 className="text-sm font-semibold text-ink">Import JSON</h3>
        <p className="mt-1 text-[0.7rem] leading-relaxed text-muted">
          Paste an <code>mcpServers</code> object.
        </p>
        <textarea
          className={`${fieldClass} mt-3 min-h-28 font-mono text-[0.68rem]`}
          placeholder={'{"mcpServers":{"filesystem":{"command":"npx","args":["-y","…"]}}}'}
          value={json}
          onChange={(event) => setJson(event.target.value)}
          aria-label="MCP JSON"
        />
        <button
          type="button"
          className={`${btnClass} mt-2 bg-line/70 text-ink hover:bg-line`}
          disabled={!json.trim() || busy !== ""}
          onClick={() => void saveJson()}
        >
          {busy === "json" ? "Importing…" : "Import configuration"}
        </button>
      </section>

      <section>
        <div className="mb-3">
          <h3 className="text-sm font-semibold text-ink">Configured servers</h3>
          <p className="mt-0.5 text-[0.7rem] text-muted">Standalone and plugin-provided servers. Configuration does not imply a live connection.</p>
        </div>
        {servers.length === 0 ? (
          <p className="rounded-2xl border border-dashed border-line bg-canvas/30 px-4 py-3.5 text-sm text-muted">
            No saved connections yet.
          </p>
        ) : (
          <div className="grid gap-2.5 sm:grid-cols-2">
            {servers.map((server) => {
              const connection = runtime?.mcp.find((row) => row.name === server.name);
              return (
              <div key={server.name} className="rounded-2xl border border-line bg-canvas/40 p-3.5">
                <div className="flex items-start gap-3">
                  <span className="flex h-9 w-9 shrink-0 items-center justify-center rounded-xl bg-accent-soft text-[0.68rem] font-semibold tracking-wide text-accent">
                    {initials(server.name)}
                  </span>
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center justify-between gap-2">
                      <span className="truncate text-sm font-medium text-ink">{server.name}</span>
                      <span className="shrink-0 rounded-full bg-accent-soft px-1.5 py-px text-[0.58rem] font-medium uppercase tracking-wide text-accent">
                        {transportLabel(server.transport)}
                      </span>
                    </div>
                    <p className="mt-1 truncate text-[0.65rem] leading-relaxed text-faint">
                      {serverDetail(server)}
                    </p>
                  </div>
                </div>
                <p className="mt-2 text-xs text-muted">{server.enabled ? "Enabled" : "Disabled"} · {server.origin === "plugin" ? "Provided by plugin" : "Standalone"}</p>
                {!server.enabled && <div className="flex flex-wrap gap-2">
                  {!server.oauth && <button type="button" className={`${btnClass} mt-2 bg-accent-soft text-ink`} disabled={busy !== "" || !!oauthPending} onClick={() => void enable(server)}>Enable {server.name}</button>}
                  {(server.oauth || (server.origin === "plugin" && (server.transport === "http" || server.transport === "streamable_http"))) && <button type="button" className={`${btnClass} mt-2 bg-accent-soft text-ink`} disabled={busy !== "" || !!oauthPending} onClick={() => connectOAuth(server)}>Connect {server.name}</button>}
                </div>}
                {oauthPending?.name === server.name && <div role="status" className="mt-2 space-y-2 text-xs"><p>Finish authorization in your browser.</p><a href={oauthPending.url} target="_blank" rel="noreferrer" className="text-accent underline">Open authorization</a><button type="button" className="ml-2 underline" onClick={() => { const name = oauthPending.name; ++oauthEpoch.current; setOauthPending(null); void api("/api/mcp/oauth/cancel", { method: "POST", body: JSON.stringify({ name }) }).catch(error => { setTone("warn"); setNote(error instanceof Error ? error.message : "Could not cancel authorization"); }); }}>Cancel authorization</button></div>}
                {(server.transport === "http" || server.transport === "streamable_http") && <><McpAssertionCredential server={server} onSaved={load} /><McpMtlsCredential server={server} onSaved={load} /></>}
                {connection && <p className="mt-1 text-xs text-muted">Last recorded in selected session: {connection.ok ? "Connected" : connection.enabled ? "Not connected" : "Disabled"}{connection.error ? ` · ${connection.error}` : ""}</p>}
                <button type="button" className={`${btnClass} mt-2 bg-line/70 text-ink`} disabled={!server.enabled || busy !== ""} onClick={() => void testServer(server)}>Test connection</button>
                {server.plugin && <button type="button" className="mt-2 text-xs text-accent underline" onClick={() => onOpenPlugin?.(server.plugin)}>Plugin: {server.plugin}</button>}
                {server.editable && <button
                  type="button"
                  className={`${btnClass} mt-3 w-full bg-danger-soft text-danger hover:brightness-95`}
                  disabled={busy !== ""}
                  onClick={() => void remove(server)}
                >
                  {busy === `remove-${server.name}` ? "Removing…" : "Remove"}
                </button>}
              </div>
              );
            })}
          </div>
        )}
      </section>

      {runtimeError && <p role="alert" className="text-sm text-warn">Could not read recorded session connections: {runtimeError}</p>}
      {note ? (
        <p className={cn("text-[0.7rem]", tone === "warn" ? "text-warn" : "text-muted")}>{note}</p>
      ) : null}
    </div>
  );
}
