import * as Dialog from "@radix-ui/react-dialog";
import { useEffect, useRef, useState, type FormEvent } from "react";
import { api, ApiError } from "../../api/client";
import type {
  AgentKnowledgeConfig,
  AgentPersona,
  KnowledgeRetriever,
  ModelChain,
  ModelPreferences,
  SlashCatalogResponse,
  ToolPolicyOverride,
} from "../../api/types";
import { Icon } from "../../lib/icons";
import { useAppStore } from "../../store";

export interface PersonaEditorDialogProps {
  mode: "create" | "edit";
  /** Populated when mode === "edit". */
  persona?: AgentPersona | null;
  onClose: () => void;
  /** Called after a successful create/update, so callers can refresh their list. */
  onSaved: (persona: AgentPersona) => void;
  /** Called after a successful delete, so callers can refresh their list. */
  onDeleted?: (id: string) => void;
}

interface SkillOption {
  id: string;
  label: string;
  description?: string;
}

function parseLines(text: string): string[] {
  return text
    .split("\n")
    .map((line) => line.trim())
    .filter(Boolean);
}

function parseRiskOverrides(text: string): Record<string, string> {
  const result: Record<string, string> = {};
  for (const line of parseLines(text)) {
    const idx = line.indexOf(":");
    if (idx === -1) continue;
    const tool = line.slice(0, idx).trim();
    const riskClass = line.slice(idx + 1).trim();
    if (tool && riskClass) result[tool] = riskClass;
  }
  return result;
}

function formatLines(items?: string[] | null): string {
  return (items || []).join("\n");
}

function formatRiskOverrides(overrides?: Record<string, string> | null): string {
  return Object.entries(overrides || {})
    .map(([tool, riskClass]) => `${tool}:${riskClass}`)
    .join("\n");
}

/** A model chain is edited as comma-separated names, tried in order. */
function formatChain(chain?: ModelChain | null): string {
  return Array.isArray(chain) ? chain.join(", ") : chain || "";
}

/** One name stays a string; several become a list. */
function parseChain(text: string): ModelChain | null {
  const names = text
    .split(",")
    .map((name) => name.trim())
    .filter(Boolean);
  if (names.length === 0) return null;
  return names.length === 1 ? names[0] : names;
}

function describeError(err: unknown): string {
  if (err instanceof ApiError) {
    const data = err.data as { error?: string; error_type?: string } | undefined;
    if (data?.error_type) {
      return `${data.error_type}: ${data.error || err.message}`;
    }
    if (data?.error) return data.error;
    return err.message;
  }
  return err instanceof Error ? err.message : String(err);
}

/** Slug a display name into a persona id (`matcha-expert`). */
export function slugifyPersonaId(name: string): string {
  return name
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 80);
}

export function PersonaEditorDialog({
  mode,
  persona,
  onClose,
  onSaved,
  onDeleted,
}: PersonaEditorDialogProps) {
  const isEdit = mode === "edit";
  const isBuiltin = Boolean(persona?.builtin);
  const createPersona = useAppStore((s) => s.createPersona);
  const updatePersona = useAppStore((s) => s.updatePersona);
  const deletePersona = useAppStore((s) => s.deletePersona);

  const [id, setId] = useState(persona?.id || "");
  const [idTouched, setIdTouched] = useState(Boolean(persona?.id));
  const [name, setName] = useState(persona?.name || "");
  const [avatar, setAvatar] = useState(persona?.avatar || "");
  const [description, setDescription] = useState(persona?.description || "");
  const [systemPrompt, setSystemPrompt] = useState(persona?.system_prompt || "");
  const [generating, setGenerating] = useState(false);
  const [draftReady, setDraftReady] = useState(false);
  const [pinnedSkills, setPinnedSkills] = useState<string[]>(persona?.pinned_skills || []);
  const [restrictedToPinned, setRestrictedToPinned] = useState(
    Boolean(persona?.skills_restricted_to_pinned),
  );

  const [advancedOpen, setAdvancedOpen] = useState(false);
  const [allowText, setAllowText] = useState(formatLines(persona?.tool_policy?.allow));
  const [denyText, setDenyText] = useState(formatLines(persona?.tool_policy?.deny));
  const [riskText, setRiskText] = useState(
    formatRiskOverrides(persona?.tool_policy?.risk_class_overrides),
  );
  const [planner, setPlanner] = useState(formatChain(persona?.model_preferences?.planner));
  const [executor, setExecutor] = useState(formatChain(persona?.model_preferences?.executor));
  const [reviewer, setReviewer] = useState(formatChain(persona?.model_preferences?.reviewer));
  const [fastWorker, setFastWorker] = useState(persona?.model_preferences?.fast_worker || "");
  const [embedding, setEmbedding] = useState(persona?.model_preferences?.embedding || "");

  const [inheritWorkspace, setInheritWorkspace] = useState(
    persona?.id === "main" || persona?.knowledge?.inherit_workspace !== false,
  );
  const [extraKbText, setExtraKbText] = useState(formatLines(persona?.knowledge?.extra_kb_ids));
  const [retrieverKind, setRetrieverKind] = useState(persona?.knowledge?.retriever?.kind || "zvec");
  const [retrieverServer, setRetrieverServer] = useState(persona?.knowledge?.retriever?.server || "");
  const [retrieverTool, setRetrieverTool] = useState(persona?.knowledge?.retriever?.tool || "");
  const [retrieverUrl, setRetrieverUrl] = useState(persona?.knowledge?.retriever?.url || "");
  const [kbSources, setKbSources] = useState<string[]>([]);
  const [kbEngine, setKbEngine] = useState("zvec");
  const [uploading, setUploading] = useState(false);

  const [skillOptions, setSkillOptions] = useState<SkillOption[]>([]);
  const [skillsLoading, setSkillsLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const nameRef = useRef<HTMLInputElement>(null);
  const busy = generating || saving || deleting;

  useEffect(() => {
    if (!isEdit) nameRef.current?.focus();
  }, [isEdit]);

  useEffect(() => {
    let cancelled = false;
    api<SlashCatalogResponse>("/api/slash-catalog")
      .then((data) => {
        if (cancelled) return;
        const seen = new Map<string, SkillOption>();
        for (const cmd of data.commands || []) {
          if (cmd.kind !== "skill") continue;
          const raw = String(cmd.title || cmd.label || "").replace(/^\//, "").trim();
          if (!raw || seen.has(raw)) continue;
          seen.set(raw, { id: raw, label: raw, description: cmd.description });
        }
        setSkillOptions([...seen.values()]);
      })
      .catch(() => {
        if (!cancelled) setSkillOptions([]);
      })
      .finally(() => {
        if (!cancelled) setSkillsLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    if (!isEdit || !persona?.id) return;
    let cancelled = false;
    api<{ sources?: string[]; engine?: string }>(`/api/agents/${encodeURIComponent(persona.id)}/knowledge`)
      .then((data) => {
        if (cancelled) return;
        setKbSources(Array.isArray(data.sources) ? data.sources : []);
        if (data.engine) setKbEngine(data.engine);
      })
      .catch(() => {
        if (!cancelled) setKbSources([]);
      });
    return () => {
      cancelled = true;
    };
  }, [isEdit, persona?.id]);

  const toggleSkill = (skillId: string) => {
    setPinnedSkills((current) =>
      current.includes(skillId)
        ? current.filter((s) => s !== skillId)
        : [...current, skillId],
    );
  };

  const buildPayload = () => {
    const toolPolicy: ToolPolicyOverride = {
      allow: parseLines(allowText),
      deny: parseLines(denyText),
      risk_class_overrides: parseRiskOverrides(riskText),
    };
    const hasToolPolicy =
      (toolPolicy.allow?.length || 0) > 0 ||
      (toolPolicy.deny?.length || 0) > 0 ||
      Object.keys(toolPolicy.risk_class_overrides || {}).length > 0;

    const modelPreferences: ModelPreferences = {
      planner: parseChain(planner),
      executor: parseChain(executor),
      reviewer: parseChain(reviewer),
      fast_worker: fastWorker.trim() || null,
      embedding: embedding.trim() || null,
    };
    const hasModelPreferences = Boolean(
      modelPreferences.planner ||
        modelPreferences.executor ||
        modelPreferences.reviewer ||
        modelPreferences.fast_worker ||
        modelPreferences.embedding,
    );

    const retriever: KnowledgeRetriever = { kind: retrieverKind || "zvec" };
    if (retrieverKind === "mcp") {
      retriever.server = retrieverServer.trim() || null;
      retriever.tool = retrieverTool.trim() || null;
    }
    if (retrieverKind === "http") {
      retriever.url = retrieverUrl.trim() || null;
    }
    const knowledge: AgentKnowledgeConfig = {
      inherit_workspace: inheritWorkspace,
      extra_kb_ids: inheritWorkspace ? parseLines(extraKbText) : [],
      retriever,
    };

    return {
      id: id.trim(),
      name: name.trim(),
      avatar: avatar.trim() || null,
      description: description.trim(),
      system_prompt: systemPrompt,
      pinned_skills: pinnedSkills,
      skills_restricted_to_pinned: restrictedToPinned,
      tool_policy: hasToolPolicy ? toolPolicy : null,
      model_preferences: hasModelPreferences ? modelPreferences : null,
      knowledge,
    };
  };

  const handleGenerate = async () => {
    if (busy) return;
    if (!name.trim()) {
      setError("Enter a role or agent name first.");
      return;
    }
    setError(null);
    setGenerating(true);
    try {
      const result = await api<{ draft: { description: string; system_prompt: string } }>("/api/agents/draft", {
        method: "POST",
        body: JSON.stringify({ name: name.trim(), description, system_prompt: systemPrompt }),
      });
      setDescription(result.draft.description);
      setSystemPrompt(result.draft.system_prompt);
      setDraftReady(true);
    } catch (err) {
      setError(describeError(err));
    } finally {
      setGenerating(false);
    }
  };

  const handleSubmit = async (event: FormEvent) => {
    event.preventDefault();
    if (busy) return;
    setError(null);
    const trimmedId = id.trim();
    const trimmedName = name.trim();
    if (!trimmedId || !trimmedName) {
      setError("Id and name are required.");
      return;
    }
    setSaving(true);
    try {
      const payload = buildPayload();
      const agent = isEdit
        ? await updatePersona(trimmedId, payload)
        : await createPersona(payload);
      onSaved(agent);
    } catch (err) {
      setError(describeError(err));
    } finally {
      setSaving(false);
    }
  };

  const handleDelete = async () => {
    if (!persona) return;
    if (!window.confirm(
      `Delete “${persona.name}”? Existing threads keep the instructions they were created with. This cannot be undone.`,
    )) return;
    setError(null);
    setDeleting(true);
    try {
      await deletePersona(persona.id);
      onDeleted?.(persona.id);
      onClose();
    } catch (err) {
      setError(describeError(err));
    } finally {
      setDeleting(false);
    }
  };

  const handleUpload = async (file: File) => {
    if (!persona?.id) return;
    setError(null);
    setUploading(true);
    try {
      const form = new FormData();
      form.append("file", file, file.name);
      const result = await api<{ sources?: string[]; filename?: string }>(
        `/api/agents/${encodeURIComponent(persona.id)}/knowledge/upload`,
        { method: "POST", body: form },
      );
      setKbSources((current) => {
        const name = result.filename || file.name;
        return current.includes(name) ? current : [...current, name];
      });
    } catch (err) {
      setError(describeError(err));
    } finally {
      setUploading(false);
    }
  };

  return (
    <Dialog.Root open onOpenChange={(open) => { if (!open && !busy) onClose(); }}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 z-[70] bg-black/55" />
        <Dialog.Content
          className="ka-persona-dialog fixed left-1/2 top-1/2 z-[71] max-h-[90vh] w-full max-w-2xl -translate-x-1/2 -translate-y-1/2 overflow-auto rounded-2xl border border-line bg-surface p-5 shadow-2xl focus:outline-none"
        >
          <div className="flex items-start justify-between gap-4">
            <div>
              <p className="text-xs font-semibold uppercase tracking-wider text-accent">
                {isEdit ? "Configure agent" : "New agent"}
              </p>
              <Dialog.Title className="mt-1 text-lg font-semibold text-ink">
                {isEdit ? persona?.name || "Edit agent" : "Create a custom agent"}
              </Dialog.Title>
              <Dialog.Description className="mt-1 text-xs leading-relaxed text-muted">
                {isBuiltin
                  ? "Default automatically uses connected integrations, configured tools, skills, and workspace knowledge. You can add knowledge and change the retriever."
                  : isEdit
                    ? "Updates apply to new threads. Threads already started keep their original snapshot."
                    : "Give this agent a role, instructions, knowledge, and the skills it should always have."}
              </Dialog.Description>
            </div>
            <Dialog.Close
              className="rounded-md px-2 py-1 text-muted hover:bg-line/60"
              aria-label="Close"
              disabled={busy}
            >
              ×
            </Dialog.Close>
          </div>

          <form className="mt-5" onSubmit={handleSubmit}>
            <fieldset disabled={busy} className="space-y-4">
              <div className="grid gap-3 sm:grid-cols-[1fr_5.5rem]">
                <label className="text-xs text-muted">
                  Name
                  <input
                    ref={nameRef}
                    className="mt-1 w-full rounded-md border border-line bg-canvas px-2.5 py-2 text-sm text-ink"
                    value={name}
                    onChange={(event) => {
                      const next = event.target.value;
                      setName(next);
                      if (!isEdit && !idTouched) setId(slugifyPersonaId(next));
                    }}
                    placeholder="Matcha Expert"
                    aria-label="Agent name"
                    required
                    disabled={isBuiltin}
                  />
                </label>
                <label className="text-xs text-muted">
                  Avatar
                  <input
                    className="mt-1 w-full rounded-md border border-line bg-canvas px-2.5 py-2 text-center text-sm text-ink"
                    value={avatar}
                    onChange={(event) => setAvatar(event.target.value)}
                    maxLength={4}
                    placeholder="🍵"
                    aria-label="Avatar emoji"
                    disabled={isBuiltin}
                  />
                </label>
              </div>

              <label className="block text-xs text-muted">
                Description
                <input
                  className="mt-1 w-full rounded-md border border-line bg-canvas px-2.5 py-2 text-sm text-ink"
                  value={description}
                  onChange={(event) => setDescription(event.target.value)}
                  placeholder="Short summary shown in the sidebar and agent home"
                  aria-label="Agent description"
                  disabled={isBuiltin}
                />
              </label>

              <label className="block text-xs text-muted">
                Agent ID{!isEdit ? " · generated from name" : ""}
                <input
                  className="mt-1 w-full rounded-md border border-line bg-canvas px-2.5 py-2 font-mono text-xs text-ink disabled:opacity-60"
                  value={id}
                  onChange={(event) => {
                    setIdTouched(true);
                    setId(event.target.value);
                  }}
                  disabled={isEdit || isBuiltin}
                  placeholder="kageha-matcha-expert"
                  aria-label="Agent id"
                  required
                />
              </label>

              <label className="block text-xs text-muted">
                Instructions
                <textarea
                  className="mt-1 h-64 w-full resize-y rounded-md border border-line bg-canvas px-2.5 py-2 text-sm leading-relaxed text-ink"
                  value={systemPrompt}
                  onChange={(event) => setSystemPrompt(event.target.value)}
                  placeholder="Write your own persona instructions, or enter notes and ask AI for help."
                  aria-label="Instructions"
                  disabled={isBuiltin}
                />
              </label>

              {!isBuiltin && (
                <div className="text-xs text-muted">
                  <button
                    type="button"
                    onClick={() => void handleGenerate()}
                    className="rounded-md border border-line px-3 py-1.5 text-xs font-medium text-ink hover:bg-line/50 disabled:opacity-60"
                  >
                    {generating ? "Drafting instructions…" : "Help me write instructions"}
                  </button>
                  <p className="mt-1 text-[0.68rem] text-faint" role="status">
                    {draftReady
                      ? "AI draft ready. Review and edit the instructions above, then save when you are happy with them. Nothing has been saved yet."
                      : "Write your own instructions or ask AI to expand your role and notes. Review and edit before saving to your Kageha configuration."}
                  </p>
                </div>
              )}

              <div>
                <p className="text-xs text-muted">Pinned skills</p>
                <p className="mt-0.5 text-[0.68rem] text-faint">Always loaded for this agent. Leave empty to use the default catalog.</p>
                <div className="mt-1.5 max-h-36 overflow-y-auto rounded-md border border-line bg-canvas/50 p-1.5">
                  {skillsLoading ? (
                    <p className="px-1 py-1 text-[0.7rem] text-faint">Loading skills…</p>
                  ) : skillOptions.length === 0 ? (
                    <p className="px-1 py-1 text-[0.7rem] text-faint">No skills available.</p>
                  ) : (
                    skillOptions.map((skill) => (
                      <label key={skill.id} className="flex cursor-pointer items-center gap-2 rounded px-1.5 py-1 text-xs text-ink hover:bg-line/40">
                        <input
                          type="checkbox"
                          checked={pinnedSkills.includes(skill.id)}
                          onChange={() => toggleSkill(skill.id)}
                          disabled={isBuiltin}
                        />
                        <span className="truncate" title={skill.description}>
                          {skill.label}
                        </span>
                      </label>
                    ))
                  )}
                </div>
              </div>

              <label className="flex items-center gap-2 text-xs text-ink">
                <input
                  type="checkbox"
                  checked={restrictedToPinned}
                  onChange={(event) => setRestrictedToPinned(event.target.checked)}
                  disabled={isBuiltin}
                />
                Restrict skill loading to pinned skills only
              </label>

              <div className="rounded-lg border border-line p-3">
                <p className="text-xs font-semibold uppercase tracking-wide text-faint">Knowledge</p>
                <p className="mt-0.5 text-[0.68rem] text-faint">
                  Private {kbEngine} store for this agent. Images are OCR’d, audio is transcribed when STT is configured, and originals stay on disk.
                </p>
                <label className="mt-2 flex items-start gap-2 text-xs text-ink">
                  <input
                    type="checkbox"
                    className="mt-0.5"
                    checked={!inheritWorkspace}
                    disabled={persona?.id === "main"}
                    onChange={(event) => setInheritWorkspace(!event.target.checked)}
                  />
                  <span>
                    Use only this agent’s knowledge
                    <span className="mt-0.5 block text-[0.68rem] text-faint">
                      {persona?.id === "main" ? "Default automatically searches all current workspace knowledge bases." : "Off = also search workspace knowledge bases pinned to the turn."}
                    </span>
                  </span>
                </label>
                {inheritWorkspace && persona?.id !== "main" && (
                  <label className="mt-2 block text-xs text-muted">
                    Always-on workspace bases (one id per line)
                    <textarea
                      className="mt-1 h-16 w-full resize-y rounded-md border border-line bg-canvas px-2.5 py-2 text-xs text-ink"
                      value={extraKbText}
                      onChange={(event) => setExtraKbText(event.target.value)}
                      placeholder="product-docs"
                      aria-label="Extra knowledge base ids"
                    />
                  </label>
                )}
                <label className="mt-2 block text-xs text-muted">
                  Retriever
                  <select
                    className="mt-1 w-full rounded-md border border-line bg-canvas px-2.5 py-2 text-xs text-ink"
                    value={retrieverKind}
                    onChange={(event) => setRetrieverKind(event.target.value)}
                    aria-label="Retriever kind"
                  >
                    <option value="zvec">zvec (default vector store)</option>
                    <option value="mcp">MCP tool</option>
                    <option value="http">HTTP server</option>
                  </select>
                </label>
                {retrieverKind === "mcp" && (
                  <div className="mt-2 grid gap-2 sm:grid-cols-2">
                    <input
                      className="rounded-md border border-line bg-canvas px-2.5 py-2 text-xs text-ink"
                      value={retrieverServer}
                      onChange={(event) => setRetrieverServer(event.target.value)}
                      placeholder="MCP server"
                      aria-label="MCP retriever server"
                    />
                    <input
                      className="rounded-md border border-line bg-canvas px-2.5 py-2 text-xs text-ink"
                      value={retrieverTool}
                      onChange={(event) => setRetrieverTool(event.target.value)}
                      placeholder="Tool name"
                      aria-label="MCP retriever tool"
                    />
                  </div>
                )}
                {retrieverKind === "http" && (
                  <input
                    className="mt-2 w-full rounded-md border border-line bg-canvas px-2.5 py-2 text-xs text-ink"
                    value={retrieverUrl}
                    onChange={(event) => setRetrieverUrl(event.target.value)}
                    placeholder="https://retriever.example/search"
                    aria-label="HTTP retriever URL"
                  />
                )}
                {isEdit && (
                  <div className="mt-3 border-t border-line pt-2">
                    <p className="text-[0.68rem] text-faint">
                      {kbSources.length} document{kbSources.length === 1 ? "" : "s"} in this agent’s library
                    </p>
                    {kbSources.length > 0 && (
                      <ul className="mt-1 max-h-20 overflow-y-auto text-[0.68rem] text-ink">
                        {kbSources.map((source) => (
                          <li key={source} className="truncate">{source}</li>
                        ))}
                      </ul>
                    )}
                    <label className="mt-2 inline-flex cursor-pointer items-center text-xs font-medium text-accent">
                      {uploading ? "Indexing…" : "Add PDF, markdown, image, audio, or video"}
                      <input
                        type="file"
                        className="hidden"
                        accept=".pdf,.pptx,.docx,.xlsx,.xlsm,.txt,.md,.markdown,.csv,.json,.jsonl,.html,.htm,.yaml,.yml,.xml,.png,.jpg,.jpeg,.webp,.gif,.svg,.bmp,.tif,.tiff,.wav,.mp3,.m4a,.ogg,.flac,.aac,.mp4,.webm,.mov,.mkv,.m4v"
                        disabled={uploading}
                        onChange={(event) => {
                          const file = event.target.files?.[0];
                          if (file) void handleUpload(file);
                          event.currentTarget.value = "";
                        }}
                      />
                    </label>
                  </div>
                )}
              </div>

              {!isBuiltin && (
              <div className="rounded-lg border border-line">
                <button
                  type="button"
                  className="flex w-full items-center justify-between px-3 py-2"
                  onClick={() => setAdvancedOpen((open) => !open)}
                >
                  <span className="text-xs font-semibold uppercase tracking-wide text-faint">
                    Access &amp; model preferences
                  </span>
                  <span
                    className="text-faint transition-transform duration-150"
                    style={{ transform: advancedOpen ? undefined : "rotate(-90deg)" }}
                  >
                    <Icon.Chevron size={14} />
                  </span>
                </button>
                {advancedOpen && (
                  <div className="space-y-3 border-t border-line p-3">
                    <div className="grid gap-3 sm:grid-cols-2">
                      <label className="block text-xs text-muted">
                        Tool allow list (one per line)
                        <textarea
                          className="mt-1 h-20 w-full resize-y rounded-md border border-line bg-canvas px-2.5 py-2 text-xs text-ink"
                          value={allowText}
                          onChange={(event) => setAllowText(event.target.value)}
                          placeholder={"read_file\nweb_search"}
                          aria-label="Tool allow list"
                        />
                      </label>
                      <label className="block text-xs text-muted">
                        Tool deny list (one per line)
                        <textarea
                          className="mt-1 h-20 w-full resize-y rounded-md border border-line bg-canvas px-2.5 py-2 text-xs text-ink"
                          value={denyText}
                          onChange={(event) => setDenyText(event.target.value)}
                          placeholder={"shell_execute"}
                          aria-label="Tool deny list"
                        />
                      </label>
                    </div>
                    <label className="block text-xs text-muted">
                      Risk class overrides (one per line, <code>tool_name:risk_class</code>)
                      <textarea
                        className="mt-1 h-16 w-full resize-y rounded-md border border-line bg-canvas px-2.5 py-2 text-xs text-ink"
                        value={riskText}
                        onChange={(event) => setRiskText(event.target.value)}
                        placeholder={"shell_execute:high"}
                        aria-label="Risk class overrides"
                      />
                    </label>
                    <div>
                      <p className="text-xs text-muted">Model preferences</p>
                      <div className="mt-1 grid gap-2 sm:grid-cols-2">
                        <input
                          className="rounded-md border border-line bg-canvas px-2.5 py-2 text-xs text-ink"
                          value={planner}
                          onChange={(event) => setPlanner(event.target.value)}
                          placeholder="Planner models (comma-separated)"
                          aria-label="Planner model preference"
                        />
                        <input
                          className="rounded-md border border-line bg-canvas px-2.5 py-2 text-xs text-ink"
                          value={executor}
                          onChange={(event) => setExecutor(event.target.value)}
                          placeholder="Executor models (comma-separated)"
                          aria-label="Executor model preference"
                        />
                        <input
                          className="rounded-md border border-line bg-canvas px-2.5 py-2 text-xs text-ink"
                          value={reviewer}
                          onChange={(event) => setReviewer(event.target.value)}
                          placeholder="Reviewer models (comma-separated)"
                          aria-label="Reviewer model preference"
                        />
                        <input
                          className="rounded-md border border-line bg-canvas px-2.5 py-2 text-xs text-ink"
                          value={fastWorker}
                          onChange={(event) => setFastWorker(event.target.value)}
                          placeholder="Fast worker model"
                          aria-label="Fast worker model preference"
                        />
                        <input
                          className="rounded-md border border-line bg-canvas px-2.5 py-2 text-xs text-ink"
                          value={embedding}
                          onChange={(event) => setEmbedding(event.target.value)}
                          placeholder="Embedding model"
                          aria-label="Embedding model preference"
                        />
                      </div>
                    </div>
                  </div>
                )}
              </div>
              )}

              {error && (
                <p role="alert" className="rounded-md bg-[var(--color-danger-soft)] px-3 py-2 text-xs text-[var(--color-danger)]">
                  {error}
                </p>
              )}

              <div className="flex items-center justify-between gap-2 pt-1">
                {isEdit && persona && !persona.builtin ? (
                  <button
                    type="button"
                    className="rounded-md border border-[var(--color-danger)] px-3 py-1.5 text-xs font-medium text-[var(--color-danger)] hover:bg-[var(--color-danger-soft)] disabled:opacity-60"
                    onClick={() => void handleDelete()}
                  >
                    {deleting ? "Deleting…" : "Delete agent"}
                  </button>
                ) : (
                  <span />
                )}
                <div className="flex gap-2">
                  <button
                    type="button"
                    className="rounded-md border border-line px-3 py-1.5 text-xs font-medium text-ink hover:bg-line/50"
                    onClick={onClose}
                  >
                    Cancel
                  </button>
                  <button
                    type="submit"
                    className="rounded-md bg-accent px-3 py-1.5 text-xs font-medium text-black hover:brightness-110 disabled:opacity-60"
                  >
                    {saving ? "Saving…" : isEdit ? "Save changes" : "Create agent"}
                  </button>
                </div>
              </div>
            </fieldset>
          </form>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
