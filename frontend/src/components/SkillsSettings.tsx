import { useCallback, useEffect, useState } from "react";
import { api } from "../api/client";
import { cn } from "../lib/cn";
import { Icon } from "../lib/icons";

type Skill = {
  name: string;
  description?: string;
  origin: "builtin" | "standalone" | "plugin" | string;
  /** Registry source; `learned` for skills promoted from successful runs. */
  source?: string;
  plugin: string;
  plugin_enabled: boolean;
  editable: boolean;
  can_pin: boolean;
  loads?: number;
  pinned?: boolean;
  archived?: boolean;
  peer_shareable?: boolean;
  peer_shareable_opted_in?: boolean;
  peer_shareable_blocked_reason?: string;
  /** Recent uses mostly failed: no longer suggested to the model. */
  degraded?: boolean;
  /** The earlier passing revision a degradation made active again. */
  restored_from?: number | null;
};

const isLearned = (skill: Skill) => skill.source === "learned" || skill.origin === "learned";

const fieldClass =
  "w-full rounded-lg border border-line bg-canvas px-3 py-2 text-sm text-ink outline-none transition-colors placeholder:text-faint focus:border-accent focus:ring-2 focus:ring-accent/15";
const btnClass = "rounded-lg px-3 py-2 text-xs font-medium transition-colors disabled:opacity-50";

export function SkillsSettings({ onOpenPlugin }: { onOpenPlugin?: (name: string) => void }) {
  const [skills, setSkills] = useState<Skill[]>([]);
  const [knowledgeOn, setKnowledgeOn] = useState(true);
  const [source, setSource] = useState("");
  const [query, setQuery] = useState("");
  const [busy, setBusy] = useState("");
  const [note, setNote] = useState("");
  const [tone, setTone] = useState<"ok" | "warn">("ok");

  const load = useCallback(async () => {
    const data = await api<{ enabled?: boolean; skills?: Skill[] }>("/api/extensions/skills");
    setKnowledgeOn(data.enabled !== false);
    setSkills(data.skills || []);
  }, []);

  useEffect(() => {
    void load().catch((error) => {
      setTone("warn");
      setNote(String(error));
    });
  }, [load]);

  // Match the app's toast-dismiss cadence so a stale success/warning message
  // doesn't sit indefinitely at the bottom of the panel.
  useEffect(() => {
    if (!note) return;
    const timer = window.setTimeout(() => setNote(""), 6000);
    return () => window.clearTimeout(timer);
  }, [note]);

  const install = async () => {
    setBusy("install");
    setNote("");
    try {
      const data = await api<{ installed: string[]; skipped: string[] }>("/api/skills/install", {
        method: "POST",
        body: JSON.stringify({ source: source.trim() }),
      });
      setSource("");
      setTone("ok");
      setNote(
        `Installed: ${data.installed.join(", ") || "none"}${
          data.skipped.length ? ` · skipped: ${data.skipped.join(", ")}` : ""
        }`,
      );
      await load().catch(() => {});
    } catch (error) {
      setTone("warn");
      setNote(error instanceof Error ? error.message : "Skill installation failed");
    } finally {
      setBusy("");
    }
  };

  const pin = async (skill: Skill) => {
    setBusy(`pin-${skill.name}`);
    setNote("");
    try {
      await api("/api/knowledge/skill", {
        method: "POST",
        body: JSON.stringify({ name: skill.name, action: skill.pinned ? "unpin" : "pin" }),
      });
      await load();
    } catch (error) {
      setTone("warn");
      setNote(error instanceof Error ? error.message : "Could not update pin");
    } finally {
      setBusy("");
    }
  };

  const togglePeerShare = async (skill: Skill) => {
    setBusy(`peer-${skill.name}`);
    setNote("");
    try {
      await api("/api/knowledge/skill", {
        method: "POST",
        body: JSON.stringify({
          name: skill.name,
          action: skill.peer_shareable ? "peer_unshare" : "peer_share",
        }),
      });
      setTone("ok");
      setNote(
        skill.peer_shareable
          ? `${skill.name} is no longer shareable with peers.`
          : `${skill.name} can now be exported to trusted peers who ask for it.`,
      );
      await load();
    } catch (error) {
      setTone("warn");
      setNote(error instanceof Error ? error.message : "Could not update peer sharing");
    } finally {
      setBusy("");
    }
  };

  /** Disables (`archive`), enables (`restore`) or deletes a skill. */
  const change = async (skill: Skill, action: "archive" | "restore" | "delete") => {
    if (action === "delete" && !window.confirm(
      `Delete the skill "${skill.name}"? It is removed from the library and no longer offered to agents.`,
    )) return;
    setBusy(`${action}-${skill.name}`);
    setNote("");
    try {
      await api("/api/knowledge/skill", {
        method: "POST",
        body: JSON.stringify({ name: skill.name, action }),
      });
      setTone("ok");
      setNote(action === "delete" ? `Deleted ${skill.name}.`
        : action === "archive" ? `Disabled ${skill.name}.` : `Enabled ${skill.name}.`);
      await load();
    } catch (error) {
      setTone("warn");
      setNote(error instanceof Error ? error.message : "Could not update the skill");
    } finally {
      setBusy("");
    }
  };

  const filtered = skills.filter((skill) => {
    const hay = `${skill.name} ${skill.description || ""}`.toLowerCase();
    return hay.includes(query.trim().toLowerCase());
  });

  return (
    <div className="space-y-6">
      <section className="rounded-2xl border border-line bg-canvas/35 p-4">
        <h3 className="text-sm font-semibold text-ink">Install a skill</h3>
        <p className="mt-1 text-[0.7rem] leading-relaxed text-muted">
          A local folder, GitHub or GitLab repo, raw <code>SKILL.md</code>, or a zip/tar of Anthropic-compatible skills.
        </p>
        <div className="mt-3 grid gap-2 sm:grid-cols-[1fr_auto]">
          <input
            className={fieldClass}
            placeholder="https://github.com/owner/repo or path to SKILL.md"
            value={source}
            onChange={(event) => setSource(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") {
                event.preventDefault();
                if (source.trim()) void install();
              }
            }}
            aria-label="Skill source"
            autoComplete="off"
          />
          <button
            type="button"
            className={`${btnClass} bg-accent text-black hover:brightness-110`}
            disabled={!source.trim() || busy !== ""}
            onClick={() => void install()}
          >
            {busy === "install" ? "Installing…" : "Install"}
          </button>
        </div>
        <p className="mt-2 text-[0.65rem] text-faint">
          Copied to <code>~/.kageha/skills</code>. Install-time code never runs.
        </p>
      </section>

      <section>
        <div className="mb-3 flex items-end justify-between gap-3">
          <div>
            <h3 className="text-sm font-semibold text-ink">Installed skills</h3>
            <p className="mt-0.5 text-[0.7rem] text-muted">
              Pin the ones this workspace should always load. Disable or delete any skill, including learned ones.
            </p>
          </div>
          {skills.length > 4 ? (
            <input
              className="w-44 rounded-lg border border-line bg-canvas px-2.5 py-1.5 text-xs text-ink outline-none placeholder:text-faint focus:border-accent"
              placeholder="Filter"
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              aria-label="Filter skills"
            />
          ) : null}
        </div>
        {filtered.length === 0 ? (
          <p className="rounded-2xl border border-dashed border-line bg-canvas/30 px-4 py-3.5 text-sm text-muted">
            {skills.length === 0 ? "No skills installed yet." : "No skills match that filter."}
          </p>
        ) : (
          <div className="grid gap-2.5 sm:grid-cols-2">
            {filtered.map((skill) => (
              <div key={skill.name} className={cn("rounded-2xl border border-line bg-canvas/40 p-3.5", skill.archived && "opacity-70")}>
                <div className="flex items-start gap-3">
                  <span className="flex h-9 w-9 shrink-0 items-center justify-center rounded-xl bg-accent-soft text-accent">
                    <Icon.Skills size={15} />
                  </span>
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center justify-between gap-2">
                      <span className="truncate text-sm font-medium text-ink">{skill.name}</span>
                      <span
                        className={cn(
                          "shrink-0 rounded-full px-1.5 py-px text-[0.58rem] font-medium uppercase tracking-wide",
                          skill.pinned
                            ? "bg-accent-soft text-accent"
                            : skill.origin === "builtin"
                              ? "bg-line/70 text-faint"
                              : "bg-line/70 text-faint",
                        )}
                      >
                        {skill.pinned ? "Pinned" : skill.origin === "builtin" ? "Built-in" : skill.origin === "plugin" ? "Plugin" : "Standalone"}
                      </span>
                    </div>
                    {(isLearned(skill) || skill.degraded || skill.archived) && (
                      <div className="mt-1 flex flex-wrap gap-1">
                        {isLearned(skill) && (
                          <span className="rounded-full bg-accent-soft px-1.5 py-px text-[0.58rem] font-medium uppercase tracking-wide text-accent"
                            title="Learned from a successful run">Learned</span>
                        )}
                        {skill.degraded && (
                          <span className="rounded-full bg-warn-soft px-1.5 py-px text-[0.58rem] font-medium uppercase tracking-wide text-warn"
                            title={`Most recent uses failed, so it is no longer suggested.${
                              skill.restored_from ? ` Revision ${skill.restored_from} is active again.` : ""}`}>Degraded</span>
                        )}
                        {skill.archived && (
                          <span className="rounded-full bg-line/70 px-1.5 py-px text-[0.58rem] font-medium uppercase tracking-wide text-faint">Disabled</span>
                        )}
                      </div>
                    )}
                    <p className="mt-1 text-[0.68rem] leading-relaxed text-muted">
                      {skill.description || "No description"}
                    </p>
                    <p className="mt-1 text-[0.62rem] text-faint">Used {skill.loads || 0} times</p>
                  </div>
                </div>
                {skill.plugin && <button type="button" className="mt-2 text-xs text-accent underline" onClick={() => onOpenPlugin?.(skill.plugin)}>Plugin: {skill.plugin} · {skill.plugin_enabled ? "Enabled" : "Disabled"}</button>}
                {knowledgeOn && skill.can_pin && !skill.archived ? (
                  <button
                    type="button"
                    className={cn(
                      btnClass,
                      "mt-3 w-full",
                      skill.pinned ? "bg-line/70 text-ink hover:bg-line" : "bg-accent text-black hover:brightness-110",
                    )}
                    disabled={busy !== ""}
                    onClick={() => void pin(skill)}
                  >
                    {busy === `pin-${skill.name}`
                      ? "Saving…"
                      : skill.pinned
                        ? "Unpin"
                        : "Pin for this workspace"}
                  </button>
                ) : null}
                {knowledgeOn && skill.editable ? (
                  skill.peer_shareable || skill.peer_shareable_opted_in ? (
                    <button
                      type="button"
                      className={cn(
                        btnClass,
                        "mt-2 w-full",
                        skill.peer_shareable
                          ? "bg-accent-soft text-accent hover:brightness-105"
                          : "bg-line/70 text-ink hover:bg-line",
                      )}
                      disabled={busy !== ""}
                      onClick={() => void togglePeerShare(skill)}
                      title={
                        skill.peer_shareable
                          ? "Trusted peers can request this skill's code"
                          : skill.peer_shareable_blocked_reason
                      }
                    >
                      {busy === `peer-${skill.name}`
                        ? "Saving…"
                        : skill.peer_shareable
                          ? "Shareable with peers"
                          : "Re-enable peer sharing"}
                    </button>
                  ) : (
                    <>
                      <p className="mt-1.5 text-[0.6rem] leading-relaxed text-faint">
                        {skill.peer_shareable_blocked_reason}
                      </p>
                      <button
                        type="button"
                        className={cn(btnClass, "mt-1.5 w-full bg-line/50 text-faint")}
                        disabled={busy !== "" || Boolean(skill.peer_shareable_blocked_reason)}
                        onClick={() => void togglePeerShare(skill)}
                        title={skill.peer_shareable_blocked_reason || "Allow trusted peers to request this skill's code"}
                      >
                        {busy === `peer-${skill.name}`
                          ? "Saving…"
                          : skill.peer_shareable_blocked_reason
                            ? "Blocked from peer sharing"
                            : "Allow peer sharing"}
                      </button>
                    </>
                  )
                ) : null}
                {skill.editable ? (
                  <div className="mt-2 grid grid-cols-2 gap-2">
                    <button
                      type="button"
                      className={cn(btnClass, "bg-line/50 text-ink hover:bg-line")}
                      disabled={busy !== ""}
                      onClick={() => void change(skill, skill.archived ? "restore" : "archive")}
                      aria-label={`${skill.archived ? "Enable" : "Disable"} ${skill.name}`}
                    >
                      {busy === `archive-${skill.name}` || busy === `restore-${skill.name}`
                        ? "Saving…" : skill.archived ? "Enable" : "Disable"}
                    </button>
                    <button
                      type="button"
                      className={cn(btnClass, "bg-danger-soft text-danger hover:brightness-105")}
                      disabled={busy !== ""}
                      onClick={() => void change(skill, "delete")}
                      aria-label={`Delete ${skill.name}`}
                    >
                      {busy === `delete-${skill.name}` ? "Deleting…" : "Delete"}
                    </button>
                  </div>
                ) : null}
              </div>
            ))}
          </div>
        )}
      </section>

      {note ? (
        <p className={cn("text-[0.7rem]", tone === "warn" ? "text-warn" : "text-muted")}>{note}</p>
      ) : null}
    </div>
  );
}
