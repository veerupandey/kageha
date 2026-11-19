import { useCallback, useEffect, useState } from "react";
import { api } from "../api/client";
import { cn } from "../lib/cn";

type Caps = {
  ok?: boolean;
  bash?: boolean;
  workdir_writable?: boolean;
  browser?: boolean;
  desktop?: boolean;
};

type RemoteHost = {
  id: string;
  name: string;
  host: string;
  user: string;
  port: number;
  identity: string;
  host_fingerprint?: string;
  workdir: string;
  init: string;
  enable_browser: boolean;
  enable_desktop: boolean;
  enable_jobs?: boolean;
  risk_tier: string;
  caps?: Caps;
};

type Report = {
  ok: boolean;
  connect: string;
  hostname: string;
  uname: string;
  workdir: string;
  workdir_writable: boolean | null;
  workdir_state?: string;
  host_fingerprint?: string;
  browser: string;
  desktop: string;
  jobs?: string;
  error: string;
};

const empty = {
  id: "",
  name: "",
  host: "",
  user: "",
  port: "22",
  identity: "",
  host_public_key: "",
  workdir: "",
  init: "",
  enable_browser: false,
  enable_desktop: false,
  enable_jobs: false,
  risk_tier: "normal",
};

const fieldClass =
  "w-full rounded-lg border border-line bg-canvas px-3 py-2 text-sm text-ink outline-none transition-colors placeholder:text-faint focus:border-accent focus:ring-2 focus:ring-accent/15";
const btnClass =
  "rounded-lg px-3 py-2 text-xs font-medium transition-colors disabled:opacity-50";

export function RemoteSettings() {
  const [hosts, setHosts] = useState<RemoteHost[]>([]);
  const [defaultId, setDefaultId] = useState("");
  const [form, setForm] = useState({ ...empty });
  const [busy, setBusy] = useState("");
  const [note, setNote] = useState("");
  const [tone, setTone] = useState<"ok" | "warn">("ok");
  const [report, setReport] = useState<Report | null>(null);

  const load = useCallback(async () => {
    const data = await api<{ hosts?: RemoteHost[]; default_host_id?: string }>("/api/remote");
    setHosts(data.hosts || []);
    setDefaultId(data.default_host_id || "");
  }, []);

  useEffect(() => {
    void load().catch((error) => {
      setTone("warn");
      setNote(String(error));
    });
  }, [load]);

  const set = (patch: Partial<typeof form>) => setForm((f) => ({ ...f, ...patch }));
  const editing = Boolean(form.id);

  const payload = () => ({
    id: form.id || undefined,
    name: form.name.trim(),
    host: form.host.trim(),
    user: form.user.trim(),
    port: Number(form.port) || 22,
    identity: form.identity.trim(),
    host_public_key: form.host_public_key.trim(),
    workdir: form.workdir.trim(),
    init: form.init.trim(),
    enable_browser: form.enable_browser,
    enable_desktop: form.enable_desktop,
    enable_jobs: form.enable_jobs,
    risk_tier: form.risk_tier,
  });

  const resetForm = () => {
    setForm({ ...empty });
    setReport(null);
  };

  const save = async () => {
    setBusy("save");
    setNote("");
    try {
      const data = await api<{ hosts?: RemoteHost[]; default_host_id?: string }>(
        "/api/remote/hosts",
        { method: "POST", body: JSON.stringify(payload()) },
      );
      setHosts(data.hosts || []);
      setDefaultId(data.default_host_id || "");
      resetForm();
      setTone("ok");
      setNote("Host saved. Enable it for a chat with the “Runs on” control.");
    } catch (error) {
      setTone("warn");
      setNote(error instanceof Error ? error.message : "Could not save host");
    } finally {
      setBusy("");
    }
  };

  const test = async () => {
    setBusy("test");
    setNote("");
    setReport(null);
    try {
      // Test the in-progress form values (inline), without needing to save first.
      const data = await api<{ ok: boolean; report: Report }>("/api/remote/test", {
        method: "POST",
        body: JSON.stringify(payload()),
      });
      setReport(data.report);
      setTone(data.ok ? "ok" : "warn");
      setNote(data.ok ? "Connection works." : `Failed: ${data.report?.error || "unreachable"}`);
    } catch (error) {
      setTone("warn");
      setNote(error instanceof Error ? error.message : "Connection test failed");
    } finally {
      setBusy("");
    }
  };

  const edit = (h: RemoteHost) => {
    setReport(null);
    setForm({
      id: h.id,
      name: h.name,
      host: h.host,
      user: h.user,
      port: String(h.port || 22),
      identity: h.identity,
      host_public_key: "",
      workdir: h.workdir,
      init: h.init,
      enable_browser: h.enable_browser,
      enable_desktop: h.enable_desktop,
      enable_jobs: h.enable_jobs === true,
      risk_tier: h.risk_tier || "normal",
    });
  };

  const remove = async (h: RemoteHost) => {
    setBusy(`rm-${h.id}`);
    try {
      const data = await api<{ hosts?: RemoteHost[]; default_host_id?: string }>(
        `/api/remote/hosts/${encodeURIComponent(h.id)}`,
        { method: "DELETE" },
      );
      setHosts(data.hosts || []);
      setDefaultId(data.default_host_id || "");
      if (form.id === h.id) resetForm();
    } catch (error) {
      setTone("warn");
      setNote(error instanceof Error ? error.message : "Could not remove host");
    } finally {
      setBusy("");
    }
  };

  const makeDefault = async (h: RemoteHost) => {
    setBusy(`def-${h.id}`);
    try {
      const data = await api<{ default_host_id?: string }>("/api/remote/default", {
        method: "POST",
        body: JSON.stringify({ host_id: h.id }),
      });
      setDefaultId(data.default_host_id || "");
    } catch (error) {
      setTone("warn");
      setNote(error instanceof Error ? error.message : "Could not set default");
    } finally {
      setBusy("");
    }
  };

  const canSave = Boolean(form.host.trim());

  return (
    <section className="rounded-2xl border border-line bg-canvas/35 p-4">
      <h3 className="text-sm font-semibold text-ink">Remote hosts (SSH)</h3>
      <p className="mt-1 text-[0.7rem] leading-relaxed text-muted">
        Run a chat’s shell and file work on a remote machine over SSH while Kageha
        stays local. Configure hosts here; pick one per chat with the “Runs on”
        control in the composer.
      </p>

      {/* Saved hosts */}
      {hosts.length > 0 ? (
        <div className="mt-3 grid gap-2">
          {hosts.map((h) => (
            <div
              key={h.id}
              className="flex items-center gap-2 rounded-xl border border-line bg-canvas/40 p-2.5"
            >
              <div className="min-w-0 flex-1">
                <div className="flex items-center gap-1.5">
                  <span className="truncate text-sm font-medium text-ink">{h.name}</span>
                  {h.id === defaultId ? (
                    <span className="shrink-0 rounded-full bg-accent-soft px-1.5 py-px text-[0.55rem] font-medium uppercase tracking-wide text-accent">
                      default
                    </span>
                  ) : null}
                  {h.risk_tier === "production" ? (
                    <span className="shrink-0 rounded-full bg-rose-500/15 px-1.5 py-px text-[0.55rem] font-semibold uppercase tracking-wide text-rose-600 dark:text-rose-300">
                      prod
                    </span>
                  ) : null}
                </div>
                <p className="truncate text-[0.65rem] text-faint">
                  {h.user ? `${h.user}@` : ""}
                  {h.host}
                  {h.port && h.port !== 22 ? `:${h.port}` : ""}
                  {h.caps?.ok ? " · reachable" : ""}
                  {h.enable_browser || h.enable_desktop ? " · browser" : ""}
                  {h.enable_desktop ? " · desktop" : ""}
                  {h.enable_jobs ? " · background jobs" : ""}
                </p>
              </div>
              {h.id !== defaultId ? (
                <button
                  type="button"
                  className={cn(btnClass, "bg-line/70 text-ink hover:bg-line")}
                  disabled={busy !== ""}
                  onClick={() => void makeDefault(h)}
                >
                  {busy === `def-${h.id}` ? "…" : "Default"}
                </button>
              ) : null}
              <button
                type="button"
                className={cn(btnClass, "bg-line/70 text-ink hover:bg-line")}
                disabled={busy !== ""}
                onClick={() => edit(h)}
              >
                Edit
              </button>
              <button
                type="button"
                className={cn(btnClass, "bg-danger-soft text-danger hover:brightness-95")}
                disabled={busy !== ""}
                onClick={() => void remove(h)}
              >
                {busy === `rm-${h.id}` ? "…" : "Remove"}
              </button>
            </div>
          ))}
        </div>
      ) : (
        <p className="mt-3 rounded-xl border border-dashed border-line bg-canvas/30 px-4 py-3 text-sm text-muted">
          No remote hosts yet. Add one below.
        </p>
      )}

      {/* Add / edit form */}
      <div className="mt-4 rounded-xl border border-line bg-canvas/50 p-3">
        <div className="flex items-center justify-between">
          <h4 className="text-xs font-semibold text-ink">
            {editing ? "Edit host" : "Add a host"}
          </h4>
          {editing ? (
            <button
              type="button"
              className="text-[0.66rem] text-muted hover:text-ink"
              onClick={resetForm}
            >
              Cancel edit
            </button>
          ) : null}
        </div>
        <div className="mt-2 grid gap-2">
          <div className="grid grid-cols-2 gap-2">
            <input
              className={fieldClass}
              placeholder="Name (e.g. gpu-box)"
              value={form.name}
              onChange={(e) => set({ name: e.target.value })}
              aria-label="Host name"
            />
            <select
              className={fieldClass}
              value={form.risk_tier}
              onChange={(e) => set({ risk_tier: e.target.value })}
              aria-label="Risk tier"
            >
              <option value="normal">Normal</option>
              <option value="production">Production (stricter)</option>
            </select>
          </div>
          <div className="grid grid-cols-[1fr_auto] gap-2">
            <input
              className={fieldClass}
              placeholder="Host or IP"
              value={form.host}
              onChange={(e) => set({ host: e.target.value })}
              aria-label="Remote host"
            />
            <input
              className={`${fieldClass} w-24`}
              placeholder="Port"
              value={form.port}
              onChange={(e) => set({ port: e.target.value.replace(/[^0-9]/g, "") })}
              aria-label="Remote port"
              inputMode="numeric"
            />
          </div>
          <input
            className={fieldClass}
            placeholder="SSH user (optional)"
            value={form.user}
            onChange={(e) => set({ user: e.target.value })}
            aria-label="Remote user"
          />
          <input
            className={fieldClass}
            placeholder="Identity / key path (optional; blank = ssh-agent)"
            value={form.identity}
            onChange={(e) => set({ identity: e.target.value })}
            aria-label="Identity file"
          />
          <input
            className={fieldClass}
            placeholder="Server host public key (ssh-ed25519 …)"
            value={form.host_public_key}
            onChange={(e) => set({ host_public_key: e.target.value })}
            aria-label="Trusted SSH host public key"
          />
          <p className="text-[0.68rem] text-muted">
            Get the host public key from the server administrator. Leave this blank to keep an existing trust pin.
          </p>
          <input
            className={fieldClass}
            placeholder="Remote working dir (optional; default ~/.kageha/work)"
            value={form.workdir}
            onChange={(e) => set({ workdir: e.target.value })}
            aria-label="Remote working directory"
          />
          <input
            className={fieldClass}
            placeholder="First-attach init command (optional)"
            value={form.init}
            onChange={(e) => set({ init: e.target.value })}
            aria-label="Init command"
          />
          <div className="flex flex-wrap gap-1.5">
            <button
              type="button"
              className={cn("ka-pick-chip", form.enable_browser && "is-on")}
              aria-pressed={form.enable_browser}
              onClick={() => set({ enable_browser: !form.enable_browser })}
            >
              Remote browser
            </button>
            <button
              type="button"
              className={cn("ka-pick-chip", form.enable_desktop && "is-on")}
              aria-pressed={form.enable_desktop}
              onClick={() => set({ enable_desktop: !form.enable_desktop })}
            >
              Remote desktop
            </button>
            <button
              type="button"
              className={cn("ka-pick-chip", form.enable_jobs && "is-on")}
              aria-pressed={form.enable_jobs}
              onClick={() => set({ enable_jobs: !form.enable_jobs })}
            >
              Background jobs
            </button>
          </div>
          {(form.enable_browser || form.enable_desktop || form.enable_jobs) && <p className="text-[0.68rem] text-muted">These features require the Kageha Linux session helper on the SSH host.</p>}
          <div className="flex gap-2">
            <button
              type="button"
              className={cn(btnClass, "flex-1 bg-line/70 text-ink hover:bg-line")}
              disabled={!canSave || busy !== ""}
              onClick={() => void test()}
            >
              {busy === "test" ? "Testing…" : "Test connection"}
            </button>
            <button
              type="button"
              className={cn(btnClass, "flex-1 bg-accent text-black hover:brightness-110")}
              disabled={!canSave || busy !== ""}
              onClick={() => void save()}
            >
              {busy === "save" ? "Saving…" : editing ? "Update host" : "Add host"}
            </button>
          </div>
        </div>

        {report ? (
          <dl className="mt-3 grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 rounded-lg border border-line bg-canvas/40 p-2.5 text-[0.68rem]">
            <dt className="text-muted">connect</dt>
            <dd className={report.connect === "Connected" ? "text-ink" : "text-warn"}>
              {report.connect === "Connected"
                ? `Connected to ${report.hostname || "?"} (${report.uname || "?"})`
                : report.error || "Unreachable"}
            </dd>
            {report.connect === "Connected" ? (
              <>
                {report.host_fingerprint && <><dt className="text-muted">Host key</dt><dd className="break-all">{report.host_fingerprint}</dd></>}
                <dt className="text-muted">workdir</dt>
                <dd className="text-ink">
                  {report.workdir} →{" "}
                  {report.workdir_state === "can-create" ? "created on first run" : report.workdir_writable === true
                    ? "writable"
                    : report.workdir_writable === false
                      ? "DENIED"
                      : "unknown"}
                </dd>
                <dt className="text-muted">browser</dt>
                <dd className="text-ink">{report.browser || "unavailable"}</dd>
                <dt className="text-muted">desktop</dt>
                <dd className="text-ink">
                  {report.desktop || "unavailable"}
                </dd>
                {report.jobs && <><dt className="text-muted">Background jobs</dt><dd>{report.jobs}</dd></>}
              </>
            ) : null}
          </dl>
        ) : null}
      </div>

      {note ? (
        <p className={cn("mt-3 text-[0.7rem]", tone === "warn" ? "text-warn" : "text-muted")}>
          {note}
        </p>
      ) : null}
    </section>
  );
}
