import { isAttachableKnowledgeBase, readKnowledgeSelection, saveKnowledgeSelection } from "../../lib/knowledgeSelection";
import { useCallback, useEffect, useRef, useState, useTransition } from "react";
import { api } from "../../api/client";
import { Icon } from "../../lib/icons";
import type { KnowledgeBaseSummary } from "../../api/types";

type Memory = { id?: string; kind?: string; state?: string; content?: string; summary?: string };
type Knowledge = {
  enabled: boolean;
  memories: Memory[];
  settings?: { memory_enabled: boolean; learning_enabled: boolean };
  knowledge_bases?: KnowledgeBaseSummary[];
};

type SearchHit = {
  id: string;
  kb?: string;
  text: string;
  score: number;
  source?: string;
  heading?: string;
  citation?: string;
  parent_text?: string;
};

type ChunkItem = {
  id: string;
  source: string;
  text: string;
  heading?: string;
  parent_id?: string;
  parent_text?: string;
};

const ACCEPTED_FILE_EXTENSIONS =
  ".pdf,.docx,.pptx,.xlsx,.xlsm,.txt,.md,.markdown,.csv,.json,.jsonl,.yaml,.yml,.xml," +
  ".py,.js,.jsx,.ts,.tsx,.rs,.go,.java,.c,.cpp,.h,.sql,.sh,.bash,.zsh,.toml,.html,.htm," +
  ".png,.jpg,.jpeg,.webp,.gif,.svg,.wav,.mp3,.m4a,.ogg,.flac,.mp4,.webm,.mov";

function getFileCategory(name: string): "code" | "document" | "media" | "data" {
  const ext = name.split(".").pop()?.toLowerCase() || "";
  if (["py", "ts", "tsx", "js", "jsx", "rs", "go", "java", "c", "cpp", "h", "sh", "sql", "html"].includes(ext)) {
    return "code";
  }
  if (["png", "jpg", "jpeg", "webp", "gif", "svg", "mp3", "wav", "m4a", "mp4", "mov", "webm"].includes(ext)) {
    return "media";
  }
  if (["json", "jsonl", "yaml", "yml", "csv", "xml", "toml"].includes(ext)) {
    return "data";
  }
  return "document";
}

function isSystemKnowledgeBase(id: string): boolean {
  return (
    id === "agent_memory" ||
    id === "memory_v2_index" ||
    id.startsWith("agent-") ||
    id.startsWith("agent_") ||
    id.startsWith("memory_")
  );
}

export function KnowledgePanel({ onClose }: { onClose: () => void }) {
  const [data, setData] = useState<Knowledge | null>(null);
  const [tab, setTab] = useState<"bases" | "search" | "memories">("bases");
  const [bases, setBases] = useState<KnowledgeBaseSummary[]>([]);
  const [selectedBases, setSelectedBases] = useState(readKnowledgeSelection);

  const [expandedBases, setExpandedBases] = useState<Record<string, boolean>>({});
  const [uploadingBase, setUploadingBase] = useState<string | null>(null);
  const [dragOverBase, setDragOverBase] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [showSystemKbs, setShowSystemKbs] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState<string | null>(null);

  // New base modal / form state
  const [isCreating, setIsCreating] = useState(false);
  const [newBaseId, setNewBaseId] = useState("");
  const [newBaseChunking, setNewBaseChunking] = useState<"hierarchical" | "fixed">("hierarchical");

  // Document Editor state
  const [editingDoc, setEditingDoc] = useState<{
    kbId: string;
    filename: string;
    content: string;
    originalContent: string;
  } | null>(null);
  const [isLoadingDoc, setIsLoadingDoc] = useState(false);
  const [isSavingDoc, setIsSavingDoc] = useState(false);

  // Chunk Inspector state
  const [inspectingChunksKb, setInspectingChunksKb] = useState<{
    kbId: string;
    chunks: ChunkItem[];
    filterSource: string;
  } | null>(null);
  const [isLoadingChunks, setIsLoadingChunks] = useState(false);

  // Interactive search state
  const [searchQuery, setSearchQuery] = useState("");
  const [searchTargetKb, setSearchTargetKb] = useState<string>("all");
  const [searchMode, setSearchMode] = useState<"auto" | "semantic" | "lexical" | "broad">("auto");
  const [searchHits, setSearchHits] = useState<SearchHit[]>([]);
  const [isSearching, startSearchTransition] = useTransition();
  const [qaAnswer, setQaAnswer] = useState<string | null>(null);
  const [isAnswering, setIsAnswering] = useState(false);
  const [hasCopiedAnswer, setHasCopiedAnswer] = useState(false);

  const fileInputRef = useRef<HTMLInputElement>(null);
  const [activeUploadBaseId, setActiveUploadBaseId] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      const [knowledge, baseData] = await Promise.all([
        api<Knowledge>("/api/knowledge?limit=100"),
        api<{ items?: KnowledgeBaseSummary[] }>("/api/knowledge-bases"),
      ]);
      setData(knowledge);
      setBases(baseData.items || knowledge.knowledge_bases || []);
      setError(null);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to load knowledge");
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const toggleBase = (id: string) => {
    if (!isAttachableKnowledgeBase(id) || (selectedBases.length >= 64 && !selectedBases.includes(id))) return;
    const next = selectedBases.includes(id)
      ? selectedBases.filter((item) => item !== id)
      : [...selectedBases, id];
    setSelectedBases(next);
    saveKnowledgeSelection(next);
  };

  const handleCreateBase = async (e: React.FormEvent) => {
    e.preventDefault();
    const cleanId = newBaseId.trim().toLowerCase().replace(/[^a-z0-9._-]+/g, "-").replace(/^-+|-+$/g, "");
    if (!cleanId) return;

    try {
      await api("/api/knowledge-bases", {
        method: "POST",
        body: JSON.stringify({ id: cleanId, chunking: newBaseChunking }),
      });
      setNewBaseId("");
      setIsCreating(false);
      setSuccess(`Created knowledge base "${cleanId}"`);
      setTimeout(() => setSuccess(null), 3000);
      await load();
      if (!selectedBases.includes(cleanId)) {
        toggleBase(cleanId);
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : "Could not create knowledge base");
    }
  };

  const handleDeleteBase = async (baseId: string) => {
    if (!window.confirm(`Permanently delete knowledge base "${baseId}" and all its indexed files?`)) {
      return;
    }
    try {
      await api(`/api/knowledge-bases/${encodeURIComponent(baseId)}`, { method: "DELETE" });
      setSelectedBases((prev) => prev.filter((id) => id !== baseId));
      setSuccess(`Deleted knowledge base "${baseId}"`);
      setTimeout(() => setSuccess(null), 3000);
      await load();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Could not delete knowledge base");
    }
  };

  const triggerUpload = (baseId: string) => {
    setActiveUploadBaseId(baseId);
    if (fileInputRef.current) {
      fileInputRef.current.value = "";
      fileInputRef.current.click();
    }
  };

  const uploadFileList = async (baseId: string, files: FileList | File[]) => {
    if (!files || files.length === 0 || !baseId) return;

    setUploadingBase(baseId);
    setError(null);
    let successCount = 0;

    try {
      for (let i = 0; i < files.length; i++) {
        const file = files[i];
        const form = new FormData();
        form.append("file", file, file.name);
        await api(`/api/knowledge-bases/${encodeURIComponent(baseId)}/upload`, {
          method: "POST",
          body: form,
        });
        successCount++;
      }
      setSuccess(`Indexed ${successCount} document${successCount > 1 ? "s" : ""} into "${baseId}"`);
      setTimeout(() => setSuccess(null), 3500);
      await load();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to index documents");
    } finally {
      setUploadingBase(null);
      setActiveUploadBaseId(null);
    }
  };

  const handleFilesSelected = async (e: React.ChangeEvent<HTMLInputElement>) => {
    const files = e.target.files;
    const baseId = activeUploadBaseId;
    if (files && baseId) {
      await uploadFileList(baseId, files);
    }
  };

  const handleDrop = async (e: React.DragEvent, baseId: string) => {
    e.preventDefault();
    e.stopPropagation();
    setDragOverBase(null);
    if (e.dataTransfer.files && e.dataTransfer.files.length > 0) {
      await uploadFileList(baseId, e.dataTransfer.files);
    }
  };

  const handleRemoveSource = async (baseId: string, sourceName: string) => {
    if (!window.confirm(`Remove document "${sourceName}" from "${baseId}"?`)) return;
    try {
      await api(
        `/api/knowledge-bases/${encodeURIComponent(baseId)}/sources/${encodeURIComponent(sourceName)}`,
        { method: "DELETE" }
      );
      setSuccess(`Removed "${sourceName}"`);
      setTimeout(() => setSuccess(null), 3000);
      await load();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Could not remove document");
    }
  };

  const openDocEditor = async (kbId: string, filename: string) => {
    setIsLoadingDoc(true);
    setError(null);
    try {
      const res = await api<{ content: string; filename: string }>(
        `/api/knowledge-bases/${encodeURIComponent(kbId)}/sources/${encodeURIComponent(filename)}`
      );
      setEditingDoc({
        kbId,
        filename,
        content: res.content || "",
        originalContent: res.content || "",
      });
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to load document content");
    } finally {
      setIsLoadingDoc(false);
    }
  };

  const handleSaveDoc = async () => {
    if (!editingDoc) return;
    setIsSavingDoc(true);
    setError(null);
    try {
      await api(
        `/api/knowledge-bases/${encodeURIComponent(editingDoc.kbId)}/sources/${encodeURIComponent(editingDoc.filename)}`,
        {
          method: "PUT",
          body: JSON.stringify({ content: editingDoc.content }),
        }
      );
      setSuccess(`Updated and re-indexed "${editingDoc.filename}" in "${editingDoc.kbId}"`);
      setTimeout(() => setSuccess(null), 3500);
      setEditingDoc(null);
      await load();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to update document");
    } finally {
      setIsSavingDoc(false);
    }
  };

  const openChunkInspector = async (kbId: string, filterSource = "all") => {
    setIsLoadingChunks(true);
    setError(null);
    try {
      const chunks: ChunkItem[] = [];
      let cursor: number | null = 0;
      let pages = 0;
      let snapshot: string | undefined;
      while (cursor !== null) {
        if (++pages > 4096) throw new Error("Chunk listing exceeded the supported page limit");
        const page: { items: ChunkItem[]; total: number; next_cursor?: number | null; snapshot?: string } = await api(
          `/api/knowledge-bases/${encodeURIComponent(kbId)}/chunks?cursor=${cursor}${snapshot ? `&snapshot=${encodeURIComponent(snapshot)}` : ""}`
        );
        if (snapshot && page.snapshot !== snapshot) throw new Error("Knowledge sources changed during chunk loading");
        snapshot = page.snapshot;
        chunks.push(...(page.items || []));
        const next = page.next_cursor ?? null;
        if (next !== null && (!Number.isSafeInteger(next) || next <= cursor)) {
          throw new Error("Knowledge server returned an invalid chunk cursor");
        }
        cursor = next;
      }
      setInspectingChunksKb({
        kbId,
        chunks,
        filterSource,
      });
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to load chunks");
    } finally {
      setIsLoadingChunks(false);
    }
  };

  const handleSearch = () => {
    if (!searchQuery.trim()) return;
    setQaAnswer(null);

    startSearchTransition(async () => {
      try {
        const targetBases = searchTargetKb === "all"
          ? (bases.length > 0 ? bases.map((b) => b.id) : selectedBases)
          : [searchTargetKb];

        const allHits: SearchHit[] = [];
        await Promise.all(
          targetBases.map(async (kbId) => {
            try {
              const res = await api<{ items?: SearchHit[] }>(
                `/api/knowledge-bases/${encodeURIComponent(kbId)}/search?q=${encodeURIComponent(searchQuery)}&mode=${encodeURIComponent(searchMode)}`
              );
              if (res.items) {
                res.items.forEach((item) => allHits.push({ ...item, kb: kbId }));
              }
            } catch {
              // Ignore single KB search errors
            }
          })
        );

        allHits.sort((a, b) => (b.score || 0) - (a.score || 0));
        setSearchHits(allHits);
      } catch (err) {
        setError(err instanceof Error ? err.message : "Search failed");
      }
    });
  };

  const handleAskQuestion = async () => {
    if (!searchQuery.trim() || bases.length === 0) return;
    setIsAnswering(true);
    setQaAnswer(null);

    const targetBaseId = searchTargetKb === "all" ? bases[0].id : searchTargetKb;
    try {
      const res = await api<{ answer?: string; passages?: SearchHit[] }>(
        `/api/knowledge-bases/${encodeURIComponent(targetBaseId)}/query`,
        {
          method: "POST",
          body: JSON.stringify({ query: searchQuery }),
        }
      );
      setQaAnswer(res.answer || "No response received.");
      if (res.passages && res.passages.length > 0) {
        setSearchHits(res.passages.map((p) => ({ ...p, kb: targetBaseId })));
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : "Question answering failed");
    } finally {
      setIsAnswering(false);
    }
  };

  const handleCopyAnswer = async () => {
    if (!qaAnswer) return;
    try {
      await navigator.clipboard.writeText(qaAnswer);
      setHasCopiedAnswer(true);
      setTimeout(() => setHasCopiedAnswer(false), 2000);
    } catch {
      // Fallback
    }
  };

  const toggleExpand = (baseId: string) => {
    setExpandedBases((prev) => ({ ...prev, [baseId]: !prev[baseId] }));
  };

  const setting = async (key: "enabled" | "learning_enabled", value: boolean) => {
    try {
      setData(
        await api<Knowledge>("/api/knowledge/settings", {
          method: "POST",
          body: JSON.stringify({ key, value }),
        })
      );
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to update setting");
    }
  };

  const memoryAction = async (memory: Memory, action: "correct" | "forget") => {
    const id = String(memory.id || "");
    if (!id) return;
    const content =
      action === "correct"
        ? window.prompt("Replace this memory with:", memory.content || memory.summary || "")
        : "";
    if (action === "correct" && content === null) return;
    try {
      await api("/api/knowledge/memory", {
        method: "POST",
        body: JSON.stringify({ action, id, content: content || "" }),
      });
      await load();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Memory action failed");
    }
  };

  // Separate curated user document libraries from internal system memory indexes
  const userBases = bases.filter((b) => !isSystemKnowledgeBase(b.id));
  const systemBases = bases.filter((b) => isSystemKnowledgeBase(b.id));
  const activeAttachedCount = userBases.filter((b) => selectedBases.includes(b.id)).length;
  const totalUserDocs = userBases.reduce((acc, b) => acc + (b.sources?.length || 0), 0);
  const totalUserChunks = userBases.reduce((acc, b) => acc + (b.stats?.chunks || 0), 0);

  return (
    <div className="flex h-full flex-col bg-canvas text-ink overflow-hidden relative font-sans">
      {/* Hidden file input for document indexing */}
      <input
        ref={fileInputRef}
        type="file"
        multiple
        className="hidden"
        accept={ACCEPTED_FILE_EXTENSIONS}
        onChange={(e) => void handleFilesSelected(e)}
      />

      {/* Top Header Bar - Linear / Raycast Style */}
      <div className="flex items-center justify-between border-b border-line px-6 py-4 bg-surface/90 backdrop-blur-md shrink-0 shadow-2xs">
        <div className="flex items-center gap-3.5">
          <div className="relative flex h-10 w-10 items-center justify-center rounded-2xl bg-gradient-to-br from-accent/25 via-accent/10 to-transparent text-accent ring-1 ring-accent/30 shadow-xs">
            <Icon.Library size={20} />
            <span className="absolute -top-1 -right-1 flex h-3 w-3">
              <span className="animate-ping absolute inline-flex h-full w-full rounded-full bg-accent opacity-60" />
              <span className="relative inline-flex rounded-full h-3 w-3 bg-accent" />
            </span>
          </div>
          <div>
            <div className="flex items-center gap-2.5">
              <h2 className="text-base font-bold text-ink tracking-tight font-sans">Knowledge & Documentation</h2>
              {activeAttachedCount > 0 ? (
                <span className="inline-flex items-center gap-1.5 rounded-full bg-emerald-500/15 border border-emerald-500/30 px-2.5 py-0.5 text-[0.68rem] font-semibold text-emerald-600 dark:text-emerald-400">
                  <span className="h-1.5 w-1.5 rounded-full bg-emerald-500 animate-pulse" />
                  {activeAttachedCount} Active in Chat
                </span>
              ) : (
                <span className="rounded-full bg-line/80 px-2.5 py-0.5 text-[0.68rem] font-medium text-muted">
                  0 Active in Chat
                </span>
              )}
            </div>
            <p className="text-[0.72rem] text-muted font-normal mt-0.5">
              Reference documentation, codebases, and manuals grounded with exact file citations
            </p>
          </div>
        </div>

        <div className="flex items-center gap-1.5">
          <button
            type="button"
            className="ka-icon-btn h-8 w-8 text-muted hover:text-ink hover:bg-canvas rounded-xl transition-all flex items-center justify-center border border-transparent hover:border-line"
            onClick={() => void load()}
            title="Reload Knowledge"
            aria-label="Reload"
          >
            <Icon.Reload size={15} />
          </button>
          <button
            type="button"
            className="ka-icon-btn h-8 w-8 text-muted hover:text-ink hover:bg-canvas rounded-xl transition-all flex items-center justify-center border border-transparent hover:border-line"
            onClick={onClose}
            aria-label="Close"
          >
            <Icon.Close size={16} />
          </button>
        </div>
      </div>

      {/* Global Alerts */}
      {error && (
        <div className="flex items-center justify-between border-b border-danger/20 bg-danger/10 px-6 py-2.5 text-xs text-danger animate-in fade-in duration-150">
          <span className="truncate pr-2 font-medium">{error}</span>
          <button type="button" onClick={() => setError(null)} className="text-danger hover:opacity-80 flex-shrink-0">
            <Icon.Close size={13} />
          </button>
        </div>
      )}
      {success && (
        <div className="flex items-center justify-between border-b border-emerald-500/20 bg-emerald-500/10 px-6 py-2.5 text-xs text-emerald-700 dark:text-emerald-300 animate-in fade-in duration-150">
          <span className="flex items-center gap-1.5 font-medium truncate pr-2">
            <Icon.Check size={14} className="text-emerald-600 dark:text-emerald-400 flex-shrink-0" />
            <span className="truncate">{success}</span>
          </span>
          <button type="button" onClick={() => setSuccess(null)} className="hover:opacity-80 flex-shrink-0 text-muted">
            <Icon.Close size={13} />
          </button>
        </div>
      )}

      {/* Floating Segmented Navigation Bar */}
      <div className="px-6 sm:px-8 pt-4 pb-1 shrink-0">
        <div className="mx-auto max-w-5xl w-full flex items-center gap-2 p-1 rounded-2xl bg-surface/80 border border-line shadow-2xs backdrop-blur-sm">
          <button
            type="button"
            className={`flex-1 flex items-center justify-center gap-2 py-2 px-3.5 text-xs font-semibold rounded-xl transition-all duration-200 ${
              tab === "bases"
                ? "bg-canvas text-ink shadow-xs border border-line"
                : "text-muted hover:text-ink hover:bg-canvas/50"
            }`}
            onClick={() => setTab("bases")}
          >
            <Icon.Library size={14} className={tab === "bases" ? "text-accent" : ""} />
            <span>Document Libraries</span>
            <span
              className={`rounded-full px-2 py-0.2 text-[0.65rem] font-bold ${
                tab === "bases" ? "bg-accent/15 text-accent" : "bg-line/70 text-muted"
              }`}
            >
              {userBases.length}
            </span>
          </button>

          <button
            type="button"
            className={`flex-1 flex items-center justify-center gap-2 py-2 px-3.5 text-xs font-semibold rounded-xl transition-all duration-200 ${
              tab === "search"
                ? "bg-canvas text-ink shadow-xs border border-line"
                : "text-muted hover:text-ink hover:bg-canvas/50"
            }`}
            onClick={() => setTab("search")}
          >
            <Icon.Search size={14} className={tab === "search" ? "text-accent" : ""} />
            <span>Live Retrieval & QA</span>
          </button>

          <button
            type="button"
            className={`flex-1 flex items-center justify-center gap-2 py-2 px-3.5 text-xs font-semibold rounded-xl transition-all duration-200 ${
              tab === "memories"
                ? "bg-canvas text-ink shadow-xs border border-line"
                : "text-muted hover:text-ink hover:bg-canvas/50"
            }`}
            onClick={() => setTab("memories")}
          >
            <Icon.Brain size={14} className={tab === "memories" ? "text-accent" : ""} />
            <span>Learned Memories</span>
            <span
              className={`rounded-full px-2 py-0.2 text-[0.65rem] font-bold ${
                tab === "memories" ? "bg-accent/15 text-accent" : "bg-line/70 text-muted"
              }`}
            >
              {data?.memories?.length || 0}
            </span>
          </button>
        </div>
      </div>

      {/* Main Centered Content Container */}
      <div className="min-h-0 flex-1 overflow-y-auto px-6 sm:px-8 py-5">
        <div className="mx-auto max-w-5xl w-full space-y-6">
          {/* TAB 1: DOCUMENT LIBRARIES */}
          {tab === "bases" && (
            <div className="space-y-6 animate-in fade-in duration-150">
              {/* Search Toolbar & Create Button */}
              <div className="flex flex-col sm:flex-row items-stretch sm:items-center justify-between gap-3">
                <div className="relative flex-1 group">
                  <Icon.Search size={14} className="absolute left-3.5 top-3 text-faint group-focus-within:text-accent transition-colors pointer-events-none" />
                  <input
                    className="w-full rounded-2xl border border-line bg-surface/90 hover:bg-surface pl-10 pr-3.5 py-2.5 text-xs text-ink placeholder:text-faint outline-none focus:border-accent focus:ring-2 focus:ring-accent/20 transition-all shadow-2xs"
                    placeholder="Filter libraries, files, or symbols..."
                    value={query}
                    onChange={(e) => setQuery(e.target.value)}
                  />
                </div>
                <div className="flex items-center gap-2.5">
                  <div className="hidden sm:flex items-center gap-2 text-xs text-muted font-medium px-2.5 py-1.5 rounded-xl bg-surface/60 border border-line/60">
                    <span className="font-semibold text-ink">{totalUserDocs}</span>
                    <span>docs</span>
                    <span>•</span>
                    <span className="font-semibold text-ink">{totalUserChunks}</span>
                    <span>chunks</span>
                  </div>
                  <button
                    type="button"
                    className="inline-flex items-center justify-center gap-1.5 rounded-2xl bg-gradient-to-r from-accent to-accent/90 px-4 py-2.5 text-xs font-semibold text-white hover:opacity-95 shadow-sm hover:shadow-accent/20 transition-all active:scale-[0.98]"
                    onClick={() => setIsCreating(true)}
                  >
                    <Icon.Plus size={15} />
                    <span>New Library</span>
                  </button>
                </div>
              </div>

              {/* Create New Knowledge Base Modal / Card */}
              {isCreating && (
                <form
                  onSubmit={(e) => void handleCreateBase(e)}
                  className="rounded-3xl border border-accent/40 bg-surface p-6 space-y-4 shadow-lg transition-all animate-in fade-in slide-in-from-top-2 duration-200"
                >
                  <div className="flex items-center justify-between pb-3 border-b border-line">
                    <div className="flex items-center gap-3">
                      <div className="flex h-8 w-8 items-center justify-center rounded-xl bg-accent/10 text-accent ring-1 ring-accent/30">
                        <Icon.Library size={16} />
                      </div>
                      <div>
                        <h3 className="text-sm font-bold text-ink">Create Knowledge Library</h3>
                        <p className="text-[0.7rem] text-muted">
                          Organize documentation, source code, or guidelines for your AI assistant
                        </p>
                      </div>
                    </div>
                    <button
                      type="button"
                      onClick={() => setIsCreating(false)}
                      className="text-faint hover:text-ink rounded-lg p-1 transition-colors"
                    >
                      <Icon.Close size={15} />
                    </button>
                  </div>

                  <div className="grid grid-cols-1 md:grid-cols-2 gap-4 pt-1">
                    <div>
                      <label className="text-[0.72rem] text-ink font-semibold block mb-1.5">
                        Library Name / Identifier
                      </label>
                      <input
                        autoFocus
                        required
                        className="w-full rounded-xl border border-line bg-canvas/50 px-3.5 py-2 text-xs text-ink font-mono outline-none focus:border-accent focus:bg-surface focus:ring-2 focus:ring-accent/20 transition-all"
                        placeholder="e.g. backend-api-spec, company-policies"
                        value={newBaseId}
                        onChange={(e) => setNewBaseId(e.target.value)}
                      />
                      <p className="mt-1 text-[0.66rem] text-faint">
                        Used to cite sources in conversation (e.g. <code>[backend-api-spec:auth.py#verify]</code>).
                      </p>
                    </div>

                    <div>
                      <label className="text-[0.72rem] text-ink font-semibold block mb-1.5">
                        Structure Chunking Strategy
                      </label>
                      <select
                        className="w-full rounded-xl border border-line bg-canvas/50 px-3.5 py-2 text-xs text-ink outline-none focus:border-accent focus:bg-surface focus:ring-2 focus:ring-accent/20 transition-all"
                        value={newBaseChunking}
                        onChange={(e) => setNewBaseChunking(e.target.value as "hierarchical" | "fixed")}
                      >
                        <option value="hierarchical">Hierarchical (AST Code & Markdown aware - Recommended)</option>
                        <option value="fixed">Fixed Window (800-char blocks)</option>
                      </select>
                      <p className="mt-1 text-[0.66rem] text-faint">
                        Hierarchical intelligently parses functions, classes, and headers with parent context.
                      </p>
                    </div>
                  </div>

                  <div className="flex justify-end gap-2.5 pt-3 border-t border-line">
                    <button
                      type="button"
                      className="rounded-xl border border-line px-4 py-1.5 text-xs font-medium text-muted hover:bg-canvas hover:text-ink transition-colors"
                      onClick={() => setIsCreating(false)}
                    >
                      Cancel
                    </button>
                    <button
                      type="submit"
                      className="rounded-xl bg-accent px-5 py-1.5 text-xs font-semibold text-white hover:opacity-95 shadow-xs transition-opacity"
                    >
                      Create Library
                    </button>
                  </div>
                </form>
              )}

              {/* Empty State */}
              {userBases.length === 0 ? (
                <div className="rounded-3xl border border-dashed border-line p-12 text-center bg-surface shadow-xs">
                  <div className="mx-auto mb-3.5 flex h-16 w-16 items-center justify-center rounded-3xl bg-accent/10 text-accent ring-1 ring-accent/20">
                    <Icon.Library size={30} />
                  </div>
                  <h3 className="text-base font-bold text-ink">Give Your Agent Project Knowledge</h3>
                  <p className="mt-2 text-xs text-muted max-w-md mx-auto leading-relaxed">
                    Upload architectural documentation, PDF manuals, code repositories, or specifications.
                    Kageha will index them with hybrid vector search and cite exact line references during conversation.
                  </p>
                  <button
                    type="button"
                    className="mt-5 inline-flex items-center gap-1.5 rounded-2xl bg-accent px-5 py-2.5 text-xs font-semibold text-white hover:opacity-95 shadow-sm transition-all"
                    onClick={() => setIsCreating(true)}
                  >
                    <Icon.Plus size={15} /> Create First Library
                  </button>
                </div>
              ) : (
                /* Curated Document Libraries List */
                <div className="space-y-4">
                  {userBases
                    .filter((base) => `${base.id} ${base.engine || ""}`.toLowerCase().includes(query.toLowerCase()))
                    .map((base) => {
                      const isAttached = selectedBases.includes(base.id);
                      const isExpanded = !!expandedBases[base.id];
                      const docCount = base.sources?.length || 0;
                      const chunkCount = base.stats?.chunks || 0;
                      const isUploadingThis = uploadingBase === base.id;
                      const isDragActive = dragOverBase === base.id;

                      return (
                        <div
                          key={base.id}
                          onDragOver={(e) => {
                            e.preventDefault();
                            setDragOverBase(base.id);
                          }}
                          onDragLeave={() => setDragOverBase(null)}
                          onDrop={(e) => void handleDrop(e, base.id)}
                          className={`rounded-3xl border transition-all duration-300 bg-surface/90 hover:bg-surface shadow-xs hover:shadow-sm overflow-hidden relative ${
                            isDragActive
                              ? "border-accent ring-2 ring-accent/40 bg-accent-soft/30 scale-[1.01]"
                              : isAttached
                              ? "border-accent/40 ring-1 ring-accent/20 border-l-[4px] border-l-accent"
                              : "border-line hover:border-line-strong border-l-[4px] border-l-transparent"
                          }`}
                        >
                          {/* Library Card Header */}
                          <div className="p-5 sm:p-6">
                            <div className="flex flex-col sm:flex-row sm:items-center justify-between gap-4">
                              {/* Left: Identity & Badges */}
                              <div className="flex items-center gap-4 min-w-0">
                                <div
                                  className={`flex h-11 w-11 shrink-0 items-center justify-center rounded-2xl transition-all duration-300 ${
                                    isAttached
                                      ? "bg-accent text-white shadow-sm ring-2 ring-accent/30"
                                      : "bg-canvas text-muted border border-line"
                                  }`}
                                >
                                  <Icon.Library size={20} />
                                </div>
                                <div className="min-w-0">
                                  <div className="flex items-center gap-2.5 flex-wrap">
                                    <h4 className="text-sm font-bold text-ink truncate font-mono tracking-tight">
                                      {base.id}
                                    </h4>
                                    {isAttached && (
                                      <span className="inline-flex items-center gap-1.5 rounded-full bg-emerald-500/15 border border-emerald-500/30 px-2.5 py-0.5 text-[0.65rem] font-bold text-emerald-600 dark:text-emerald-400">
                                        <span className="h-1.5 w-1.5 rounded-full bg-emerald-500 animate-pulse" />
                                        Active in Chat
                                      </span>
                                    )}
                                  </div>
                                  <div className="mt-1.5 flex items-center gap-2 text-[0.72rem] text-muted flex-wrap font-medium">
                                    <span className="text-ink font-semibold">
                                      {docCount} {docCount === 1 ? "document" : "documents"}
                                    </span>
                                    <span>•</span>
                                    {/* Clickable Chunk Inspector Button */}
                                    <button
                                      type="button"
                                      onClick={() => void openChunkInspector(base.id)}
                                      className="rounded-lg bg-canvas hover:bg-surface px-2 py-0.5 border border-line/60 font-mono text-[0.65rem] text-muted hover:text-accent hover:border-accent transition-all flex items-center gap-1 cursor-pointer"
                                      title="Inspect actual chunks and AST boundaries"
                                    >
                                      <span>🧩 {chunkCount} {chunkCount === 1 ? "chunk" : "chunks"}</span>
                                      <span className="text-[0.6rem] text-accent font-semibold underline">Inspect</span>
                                    </button>
                                    <span>•</span>
                                    <span className="rounded-lg bg-canvas px-2 py-0.5 border border-line/60 font-mono text-[0.65rem] text-muted font-normal">
                                      ⚡ {base.chunking || "hierarchical"}
                                    </span>
                                  </div>
                                </div>
                              </div>

                              {/* Right: Tactile Switch & Add Files Action */}
                              <div className="flex items-center justify-between sm:justify-end gap-3.5 shrink-0 pt-3 sm:pt-0 border-t sm:border-t-0 border-line/50">
                                {isAttachableKnowledgeBase(base.id) && <div className="flex items-center gap-2.5 bg-canvas/60 px-3 py-1.5 rounded-2xl border border-line/60">
                                  <button
                                    type="button"
                                    role="switch"
                                    aria-checked={isAttached}
                                    onClick={() => toggleBase(base.id)}
                                    title="Toggle whether this knowledge base is included in active chat turns"
                                    className={`relative inline-flex h-5 w-9 shrink-0 cursor-pointer rounded-full border-2 border-transparent transition-colors duration-200 ease-in-out focus:outline-none ${
                                      isAttached ? "bg-accent" : "bg-line-strong"
                                    }`}
                                  >
                                    <span
                                      className={`pointer-events-none inline-block h-4 w-4 transform rounded-full bg-white shadow-md ring-0 transition duration-200 ease-in-out ${
                                        isAttached ? "translate-x-4" : "translate-x-0"
                                      }`}
                                    />
                                  </button>
                                  <span
                                    className={`text-[0.7rem] select-none cursor-pointer font-semibold ${
                                      isAttached ? "text-accent" : "text-muted font-medium"
                                    }`}
                                    onClick={() => toggleBase(base.id)}
                                  >
                                    {isAttached ? "Chat Active" : "Disabled"}
                                  </span>
                                </div>}

                                <div className="flex items-center gap-1.5">
                                  <button
                                    type="button"
                                    className="ka-icon-btn h-9 px-3.5 rounded-2xl border border-line bg-surface hover:bg-canvas text-ink text-xs font-semibold flex items-center gap-1.5 transition-all hover:border-accent shadow-2xs hover:shadow-xs active:scale-[0.98]"
                                    onClick={() => triggerUpload(base.id)}
                                    disabled={isUploadingThis}
                                    title="Upload documents or code"
                                  >
                                    {isUploadingThis ? (
                                      <Icon.Loader size={13} className="animate-spin text-accent" />
                                    ) : (
                                      <Icon.Upload size={14} className="text-accent" />
                                    )}
                                    <span>{isUploadingThis ? "Indexing..." : "Add Files"}</span>
                                  </button>

                                  <button
                                    type="button"
                                    className="ka-icon-btn h-9 w-9 text-muted hover:text-danger hover:bg-danger/10 rounded-2xl transition-colors flex items-center justify-center border border-transparent hover:border-danger/20"
                                    onClick={() => void handleDeleteBase(base.id)}
                                    title="Delete library"
                                    aria-label="Delete library"
                                  >
                                    <Icon.Delete size={15} />
                                  </button>
                                </div>
                              </div>
                            </div>

                            {/* Collapsible File List Toggle Row */}
                            <div className="mt-4 pt-3.5 border-t border-line/60 flex items-center justify-between text-xs">
                              <button
                                type="button"
                                onClick={() => toggleExpand(base.id)}
                                className="flex items-center gap-1.5 text-muted hover:text-ink font-semibold transition-colors group"
                              >
                                <span className="transition-transform group-hover:translate-x-0.5">
                                  {isExpanded ? <Icon.Chevron size={14} /> : <Icon.ChevronRight size={14} />}
                                </span>
                                <span>
                                  {isExpanded
                                    ? "Hide files"
                                    : docCount > 0
                                    ? `View ${docCount} indexed file${docCount !== 1 ? "s" : ""} (click to edit)`
                                    : "No files yet (click to upload)"}
                                </span>
                              </button>
                              <span className="text-faint text-[0.7rem] font-medium">
                                {isDragActive ? "✨ Drop files to index!" : "Drag & drop files onto this card"}
                              </span>
                            </div>
                          </div>

                          {/* Expanded File List & Drop Zone Area */}
                          {isExpanded && (
                            <div className="bg-canvas/50 border-t border-line/70 p-5 sm:p-6 space-y-4 animate-in fade-in duration-150">
                              {/* File List Table */}
                              {base.sources && base.sources.length > 0 ? (
                                <div className="space-y-2">
                                  <div className="flex items-center justify-between text-[0.68rem] text-faint uppercase tracking-wider font-bold px-1">
                                    <span>Indexed Files · Click any file to edit</span>
                                    <span>{base.sources.length} total</span>
                                  </div>
                                  <div className="grid grid-cols-1 sm:grid-cols-2 gap-2.5 max-h-60 overflow-y-auto pr-1">
                                    {base.sources.map((srcPath, idx) => {
                                      const filename = srcPath.replace(/\\/g, "/").split("/").pop() || srcPath;
                                      const cat = getFileCategory(filename);
                                      return (
                                        <div
                                          key={idx}
                                          className="flex items-center justify-between gap-2.5 rounded-2xl bg-surface px-3.5 py-2.5 text-xs text-ink border border-line/70 hover:border-accent hover:shadow-2xs transition-all group cursor-pointer"
                                        >
                                          {/* Clickable File Pill to open in Editor */}
                                          <div
                                            className="flex items-center gap-2.5 min-w-0 flex-1"
                                            onClick={() => void openDocEditor(base.id, filename)}
                                            title={`Click to view or edit ${filename}`}
                                          >
                                            <span className="shrink-0">
                                              {cat === "code" ? (
                                                <span className="rounded-lg bg-emerald-500/10 border border-emerald-500/20 px-2 py-0.5 font-mono text-[0.65rem] text-emerald-600 dark:text-emerald-400 font-bold">
                                                  &lt;/&gt;
                                                </span>
                                              ) : cat === "data" ? (
                                                <span className="rounded-lg bg-amber-500/10 border border-amber-500/20 px-2 py-0.5 font-mono text-[0.65rem] text-amber-600 dark:text-amber-400 font-bold">
                                                  data
                                                </span>
                                              ) : (
                                                <span className="rounded-lg bg-indigo-500/10 border border-indigo-500/20 px-2 py-0.5 font-mono text-[0.65rem] text-indigo-600 dark:text-indigo-400 font-semibold">
                                                  doc
                                                </span>
                                              )}
                                            </span>
                                            <span className="truncate text-[0.74rem] font-mono text-ink/90 font-semibold group-hover:text-accent transition-colors">
                                              {filename}
                                            </span>
                                          </div>

                                          <div className="flex items-center gap-1 shrink-0">
                                            <button
                                              type="button"
                                              onClick={() => void openDocEditor(base.id, filename)}
                                              className="text-faint hover:text-accent p-1.5 rounded-lg hover:bg-canvas transition-colors opacity-70 group-hover:opacity-100"
                                              title={`Edit ${filename}`}
                                            >
                                              <Icon.Compose size={13} />
                                            </button>
                                            <button
                                              type="button"
                                              onClick={(e) => {
                                                e.stopPropagation();
                                                void handleRemoveSource(base.id, filename);
                                              }}
                                              className="text-faint hover:text-danger p-1.5 rounded-lg hover:bg-danger/10 transition-colors opacity-70 group-hover:opacity-100"
                                              title={`Remove ${filename}`}
                                            >
                                              <Icon.Close size={13} />
                                            </button>
                                          </div>
                                        </div>
                                      );
                                    })}
                                  </div>
                                </div>
                              ) : null}

                              {/* Interactive Drop Zone Area */}
                              <div
                                onClick={() => triggerUpload(base.id)}
                                className="rounded-2xl border-2 border-dashed border-line hover:border-accent bg-surface/70 hover:bg-surface p-5 text-center cursor-pointer transition-all duration-200 group relative overflow-hidden"
                              >
                                <div className="mx-auto mb-2 flex h-9 w-9 items-center justify-center rounded-xl bg-accent/10 text-accent group-hover:scale-110 transition-transform">
                                  <Icon.Upload size={18} />
                                </div>
                                <p className="text-xs font-bold text-ink group-hover:text-accent transition-colors">
                                  Click to upload files or drop them here
                                </p>
                                <div className="mt-2 flex items-center justify-center gap-1.5 flex-wrap">
                                  {["Python", "TypeScript", "PDF", "Markdown", "JSON", "YAML"].map((ext) => (
                                    <span
                                      key={ext}
                                      className="rounded-md bg-canvas px-1.5 py-0.5 text-[0.62rem] font-mono text-faint border border-line/60"
                                    >
                                      {ext}
                                    </span>
                                  ))}
                                </div>
                              </div>
                            </div>
                          )}
                        </div>
                      );
                    })}
                </div>
              )}

              {/* Collapsible Internal Agent Storage Section */}
              {systemBases.length > 0 && (
                <div className="pt-6 border-t border-line/60">
                  <button
                    type="button"
                    onClick={() => setShowSystemKbs((prev) => !prev)}
                    className="flex items-center gap-2 text-xs text-muted hover:text-ink font-semibold transition-colors"
                  >
                    {showSystemKbs ? <Icon.Chevron size={14} /> : <Icon.ChevronRight size={14} />}
                    <span>Agent Internal Memory Stores ({systemBases.length})</span>
                  </button>
                  <p className="text-[0.7rem] text-faint mt-0.5">
                    Internal vector stores used automatically by background agents and episodic memory
                  </p>

                  {showSystemKbs && (
                    <div className="mt-3.5 space-y-2.5 opacity-85 animate-in fade-in duration-150">
                      {systemBases.map((sb) => (
                        <div
                          key={sb.id}
                          className="flex items-center justify-between gap-3 rounded-2xl border border-line bg-surface/60 px-4 py-2.5 text-xs shadow-2xs"
                        >
                          <div className="flex items-center gap-2.5">
                            <Icon.Brain size={15} className="text-muted" />
                            <span className="font-mono text-muted text-[0.74rem] font-medium">{sb.id}</span>
                            <span className="text-[0.68rem] text-faint">
                              ({sb.sources?.length || 0} episodes/facts)
                            </span>
                          </div>
                          <span className="rounded-md bg-canvas px-2 py-0.5 text-[0.64rem] text-faint border border-line font-mono">
                            system managed
                          </span>
                        </div>
                      ))}
                    </div>
                  )}
                </div>
              )}
            </div>
          )}

          {/* TAB 2: LIVE SEARCH & GROUNDED QA */}
          {tab === "search" && (
            <div className="space-y-6 animate-in fade-in duration-150">
              {/* Search Control Card */}
              <div className="rounded-3xl border border-line bg-surface p-6 space-y-4 shadow-sm">
                <div className="flex items-center justify-between pb-3 border-b border-line">
                  <div className="flex items-center gap-3">
                    <div className="flex h-8 w-8 items-center justify-center rounded-xl bg-accent/10 text-accent ring-1 ring-accent/30">
                      <Icon.Search size={16} />
                    </div>
                    <div>
                      <h3 className="text-sm font-bold text-ink">Live Knowledge Retrieval Playground</h3>
                      <p className="text-[0.7rem] text-muted">
                        Test dense semantic embeddings and lexical BM25 matching across your documents
                      </p>
                    </div>
                  </div>
                  <span className="text-[0.68rem] text-accent font-mono font-semibold bg-accent/10 px-2.5 py-0.5 rounded-full border border-accent/20">
                    Hybrid RRF Engine
                  </span>
                </div>

                {/* Target Library & Retrieval Mode Controls */}
                <div className="grid grid-cols-1 sm:grid-cols-2 gap-3.5 text-xs">
                  <div>
                    <label className="text-[0.72rem] text-ink font-semibold block mb-1.5">Target Library</label>
                    <select
                      className="w-full rounded-xl border border-line bg-canvas/40 px-3.5 py-2 text-xs text-ink outline-none focus:border-accent focus:bg-surface focus:ring-2 focus:ring-accent/20 transition-all font-medium"
                      value={searchTargetKb}
                      onChange={(e) => setSearchTargetKb(e.target.value)}
                    >
                      <option value="all">All Active Libraries ({userBases.length})</option>
                      {userBases.map((b) => (
                        <option key={b.id} value={b.id}>
                          {b.id} ({b.sources?.length || 0} docs)
                        </option>
                      ))}
                    </select>
                  </div>

                  <div>
                    <label className="text-[0.72rem] text-ink font-semibold block mb-1.5">Retrieval Mode</label>
                    <div className="grid grid-cols-4 gap-1 p-1 rounded-xl border border-line bg-canvas/50">
                      {(["auto", "semantic", "lexical", "broad"] as const).map((m) => (
                        <button
                          key={m}
                          type="button"
                          onClick={() => setSearchMode(m)}
                          className={`rounded-lg py-1.5 text-[0.68rem] font-semibold capitalize transition-all ${
                            searchMode === m
                              ? "bg-surface text-ink shadow-2xs border border-line"
                              : "text-muted hover:text-ink"
                          }`}
                        >
                          {m}
                        </button>
                      ))}
                    </div>
                  </div>
                </div>

                {/* Search Input Box */}
                <div className="space-y-2.5">
                  <div className="relative group">
                    <input
                      className="w-full rounded-2xl border border-line bg-canvas/40 px-4 py-3 text-xs text-ink placeholder:text-faint outline-none focus:border-accent focus:bg-surface focus:ring-2 focus:ring-accent/20 transition-all pr-9 font-medium shadow-2xs"
                      placeholder="Ask a question or enter keywords (e.g. JWT token expiration, uptime SLA, refund)..."
                      value={searchQuery}
                      onChange={(e) => setSearchQuery(e.target.value)}
                      onKeyDown={(e) => {
                        if (e.key === "Enter") handleSearch();
                      }}
                    />
                    {searchQuery && (
                      <button
                        type="button"
                        onClick={() => {
                          setSearchQuery("");
                          setSearchHits([]);
                          setQaAnswer(null);
                        }}
                        className="absolute right-3.5 top-3.5 text-faint hover:text-ink"
                      >
                        <Icon.Close size={14} />
                      </button>
                    )}
                  </div>

                  {/* Example prompt pills for instant testing */}
                  <div className="flex items-center gap-1.5 flex-wrap pt-0.5">
                    <span className="text-[0.68rem] text-faint font-medium">Quick prompts:</span>
                    {[
                      "JWT token expiration and SLA uptime",
                      "def verify_token",
                      "rate limiting policy",
                    ].map((example) => (
                      <button
                        key={example}
                        type="button"
                        onClick={() => {
                          setSearchQuery(example);
                          setSearchHits([]);
                          setQaAnswer(null);
                        }}
                        className="rounded-full bg-surface px-3 py-1 text-[0.66rem] font-medium text-muted hover:text-ink hover:border-accent border border-line transition-all shadow-2xs"
                      >
                        {example}
                      </button>
                    ))}
                  </div>

                  {/* Search Action Buttons */}
                  <div className="flex gap-2.5 pt-1.5">
                    <button
                      type="button"
                      onClick={handleSearch}
                      disabled={isSearching || !searchQuery.trim()}
                      className="flex-1 rounded-2xl bg-gradient-to-r from-accent to-accent/90 px-4 py-2.5 text-xs font-semibold text-white hover:opacity-95 disabled:opacity-50 transition-all flex items-center justify-center gap-2 shadow-sm active:scale-[0.98]"
                    >
                      {isSearching ? <Icon.Loader size={14} className="animate-spin" /> : <Icon.Search size={14} />}
                      <span>Search Passages</span>
                    </button>
                    <button
                      type="button"
                      onClick={() => void handleAskQuestion()}
                      disabled={isAnswering || !searchQuery.trim()}
                      className="rounded-2xl border border-line bg-surface hover:bg-canvas px-5 py-2.5 text-xs font-semibold text-ink disabled:opacity-50 transition-all flex items-center gap-2 shadow-2xs active:scale-[0.98]"
                    >
                      {isAnswering ? (
                        <Icon.Loader size={14} className="animate-spin text-accent" />
                      ) : (
                        <Icon.Brain size={14} className="text-accent" />
                      )}
                      <span>Grounded QA</span>
                    </button>
                  </div>
                </div>
              </div>

              {/* Grounded QA Answer Display */}
              {qaAnswer && (
                <div className="rounded-3xl border border-accent/40 bg-gradient-to-br from-accent/10 via-surface to-surface p-6 space-y-3.5 shadow-md animate-in fade-in duration-200">
                  <div className="flex items-center justify-between text-accent font-bold text-xs">
                    <span className="flex items-center gap-2">
                      <Icon.Logo size={18} /> Grounded AI Synthesis
                    </span>
                    <button
                      type="button"
                      onClick={() => void handleCopyAnswer()}
                      className="flex items-center gap-1.5 text-xs text-accent hover:underline font-bold bg-accent/10 px-3 py-1 rounded-full border border-accent/20 transition-all"
                    >
                      {hasCopiedAnswer ? (
                        <>
                          <Icon.Check size={13} />
                          <span>Copied!</span>
                        </>
                      ) : (
                        <>
                          <Icon.Copy size={13} />
                          <span>Copy Answer</span>
                        </>
                      )}
                    </button>
                  </div>
                  <div className="text-xs text-ink whitespace-pre-wrap leading-relaxed font-sans bg-surface/90 p-5 rounded-2xl border border-line shadow-2xs font-medium">
                    {qaAnswer}
                  </div>
                </div>
              )}

              {/* Search Hits List */}
              <div className="space-y-3">
                <div className="flex items-center justify-between text-xs text-faint px-1">
                  <span className="font-bold text-muted">
                    {searchHits.length} matching passage{searchHits.length !== 1 ? "s" : ""}
                  </span>
                  <span>Ranked by Reciprocal Rank Fusion</span>
                </div>

                {searchHits.length === 0 ? (
                  <div className="rounded-3xl border border-dashed border-line p-12 text-center text-xs text-faint bg-surface shadow-2xs">
                    <Icon.Search size={28} className="mx-auto mb-2 text-faint opacity-40" />
                    <p className="font-bold text-ink text-sm">No passages retrieved yet</p>
                    <p className="mt-1 text-[0.72rem]">
                      Type a query above to see how Kageha recalls and ranks passages from your documents.
                    </p>
                  </div>
                ) : (
                  searchHits.map((hit, idx) => {
                    const scorePercent = Math.min(100, Math.round((hit.score || 0) * 100));
                    const srcName = (hit.source || "").replace(/\\/g, "/").split("/").pop() || hit.kb || "doc";
                    const heading = hit.heading ? `#${hit.heading}` : "";
                    const citation = hit.citation || `[${hit.kb || "kb"}:${srcName}${heading}]`;

                    return (
                      <div
                        key={hit.id || idx}
                        className="rounded-3xl border border-line bg-surface p-5 text-xs space-y-2.5 hover:border-line-strong transition-all shadow-2xs"
                      >
                        <div className="flex items-center justify-between gap-2.5">
                          <span className="font-mono text-[0.72rem] font-bold text-accent bg-accent/10 px-3 py-1 rounded-xl border border-accent/25 truncate">
                            {citation}
                          </span>
                          <div className="flex items-center gap-2.5 text-xs text-faint shrink-0">
                            <div className="h-2 w-20 rounded-full bg-line overflow-hidden">
                              <div
                                className={`h-full rounded-full transition-all duration-300 ${
                                  scorePercent > 65
                                    ? "bg-gradient-to-r from-emerald-500 to-teal-400"
                                    : scorePercent > 35
                                    ? "bg-gradient-to-r from-accent to-sky-400"
                                    : "bg-faint"
                                }`}
                                style={{ width: `${Math.max(10, scorePercent)}%` }}
                              />
                            </div>
                            <span className="font-mono font-bold text-ink">{scorePercent}%</span>
                          </div>
                        </div>

                        <p className="text-xs text-ink/90 whitespace-pre-wrap leading-relaxed font-sans font-medium">
                          {hit.text}
                        </p>

                        {hit.parent_text && hit.parent_text !== hit.text && (
                          <details className="text-[0.7rem] text-faint pt-1.5 border-t border-line/40">
                            <summary className="cursor-pointer hover:text-ink font-semibold select-none">
                              View parent section context
                            </summary>
                            <div className="mt-2.5 p-3 rounded-xl bg-canvas/60 border border-line/60 text-muted whitespace-pre-wrap font-sans text-[0.72rem] leading-relaxed">
                              {hit.parent_text}
                            </div>
                          </details>
                        )}
                      </div>
                    );
                  })
                )}
              </div>
            </div>
          )}

          {/* TAB 3: LEARNED MEMORIES */}
          {tab === "memories" && (
            <div className="space-y-6 animate-in fade-in duration-150">
              {/* Memory Settings Banner */}
              <div className="rounded-3xl border border-line bg-surface p-6 space-y-3.5 shadow-sm">
                <div className="flex items-center justify-between pb-3 border-b border-line">
                  <div className="flex items-center gap-3">
                    <div className="flex h-8 w-8 items-center justify-center rounded-xl bg-accent/10 text-accent ring-1 ring-accent/30">
                      <Icon.Brain size={16} />
                    </div>
                    <div>
                      <h3 className="text-sm font-bold text-ink">Agent Memory & Learning Settings</h3>
                      <p className="text-[0.7rem] text-muted">
                        Preferences, persistent facts, and past task takeaways
                      </p>
                    </div>
                  </div>
                </div>

                <div className="grid grid-cols-1 sm:grid-cols-2 gap-4 pt-1">
                  <label className="flex items-start gap-3.5 p-3.5 rounded-2xl border border-line bg-canvas/40 hover:bg-surface cursor-pointer transition-colors shadow-2xs">
                    <input
                      type="checkbox"
                      className="accent-accent mt-0.5 h-4 w-4 rounded-md cursor-pointer shrink-0"
                      checked={!!data?.settings?.memory_enabled}
                      onChange={(e) => void setting("enabled", e.target.checked)}
                    />
                    <div>
                      <span className="text-xs font-bold text-ink block">Recall in Chat</span>
                      <span className="text-[0.7rem] text-muted block mt-0.5">
                        Automatically fetch relevant preferences and facts into agent context.
                      </span>
                    </div>
                  </label>

                  <label className="flex items-start gap-3.5 p-3.5 rounded-2xl border border-line bg-canvas/40 hover:bg-surface cursor-pointer transition-colors shadow-2xs">
                    <input
                      type="checkbox"
                      className="accent-accent mt-0.5 h-4 w-4 rounded-md cursor-pointer shrink-0"
                      checked={!!data?.settings?.learning_enabled}
                      onChange={(e) => void setting("learning_enabled", e.target.checked)}
                    />
                    <div>
                      <span className="text-xs font-bold text-ink block">Auto-learn from Turns</span>
                      <span className="text-[0.7rem] text-muted block mt-0.5">
                        Extract new facts and user corrections when tasks complete.
                      </span>
                    </div>
                  </label>
                </div>
              </div>

              {/* Memory Filter & List */}
              <div className="space-y-4">
                <div className="relative group">
                  <Icon.Search size={14} className="absolute left-3.5 top-3 text-faint group-focus-within:text-accent pointer-events-none transition-colors" />
                  <input
                    className="w-full rounded-2xl border border-line bg-surface/90 hover:bg-surface pl-10 pr-3.5 py-2.5 text-xs text-ink placeholder:text-faint outline-none focus:border-accent focus:ring-2 focus:ring-accent/20 transition-all shadow-2xs"
                    placeholder="Filter learned memories..."
                    value={query}
                    onChange={(e) => setQuery(e.target.value)}
                  />
                </div>

                {!data?.memories || data.memories.length === 0 ? (
                  <div className="rounded-3xl border border-dashed border-line p-12 text-center text-xs text-faint bg-surface shadow-2xs">
                    <div className="mx-auto mb-3 flex h-14 w-14 items-center justify-center rounded-2xl bg-accent/10 text-accent ring-1 ring-accent/20">
                      <Icon.Brain size={26} />
                    </div>
                    <p className="font-bold text-ink text-base">No Persistent Memories Yet</p>
                    <p className="mt-1.5 text-xs text-muted max-w-sm mx-auto">
                      Use <code>/memory remember …</code> in chat or let your agent automatically learn your preferences over time.
                    </p>
                  </div>
                ) : (
                  <div className="space-y-3">
                    {data.memories
                      .filter((m) =>
                        `${m.content || m.summary || ""} ${m.kind || ""}`
                          .toLowerCase()
                          .includes(query.toLowerCase())
                      )
                      .map((m) => (
                        <div
                          key={m.id}
                          className="rounded-3xl border border-line bg-surface p-5 text-xs space-y-2.5 hover:border-line-strong transition-all shadow-2xs"
                        >
                          <div className="flex items-center justify-between text-[0.7rem] text-faint">
                            <span className="rounded-lg bg-accent/10 text-accent px-2.5 py-0.5 border border-accent/20 font-bold uppercase tracking-wider text-[0.64rem]">
                              {m.kind || "memory"}
                            </span>
                            <span className="font-mono text-faint">{m.state || ""}</span>
                          </div>
                          <p className="text-xs text-ink leading-relaxed font-sans font-medium">
                            {m.content || m.summary || "(empty)"}
                          </p>
                          {m.id && (
                            <div className="flex items-center gap-3.5 pt-2.5 border-t border-line/50 text-[0.72rem]">
                              <button
                                type="button"
                                className="text-accent hover:underline font-bold"
                                onClick={() => void memoryAction(m, "correct")}
                              >
                                Correct
                              </button>
                              <button
                                type="button"
                                className="text-danger hover:underline font-medium"
                                onClick={() => void memoryAction(m, "forget")}
                              >
                                Forget
                              </button>
                            </div>
                          )}
                        </div>
                      ))}
                  </div>
                )}
              </div>
            </div>
          )}
        </div>
      </div>

      {/* Document View & Edit Modal Dialog */}
      {editingDoc && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-md p-4 sm:p-6 animate-in fade-in duration-200">
          <div className="flex h-[88vh] w-full max-w-4xl flex-col rounded-3xl border border-line bg-surface shadow-2xl overflow-hidden">
            {/* Modal Header */}
            <div className="flex items-center justify-between border-b border-line px-6 py-4 bg-surface shrink-0">
              <div className="flex items-center gap-3.5">
                <span className="rounded-xl bg-accent/15 px-2.5 py-1 font-mono text-[0.72rem] text-accent font-bold uppercase border border-accent/20">
                  {getFileCategory(editingDoc.filename)}
                </span>
                <div>
                  <h3 className="text-sm font-bold text-ink font-mono">{editingDoc.filename}</h3>
                  <p className="text-[0.7rem] text-muted">
                    Library: <span className="font-semibold text-ink font-mono">{editingDoc.kbId}</span>
                  </p>
                </div>
              </div>
              <div className="flex items-center gap-3">
                {editingDoc.content !== editingDoc.originalContent && (
                  <span className="text-[0.7rem] text-amber-500 font-bold px-2.5 py-0.5 rounded-lg bg-amber-500/10 border border-amber-500/20">
                    Unsaved changes
                  </span>
                )}
                <button
                  type="button"
                  onClick={() => setEditingDoc(null)}
                  className="ka-icon-btn h-8 w-8 text-muted hover:text-ink hover:bg-canvas rounded-xl transition-colors flex items-center justify-center border border-transparent hover:border-line"
                >
                  <Icon.Close size={16} />
                </button>
              </div>
            </div>

            {/* Modal Textarea Editor */}
            <div className="relative flex-1 p-5 bg-canvas/40 min-h-0">
              <textarea
                autoFocus
                value={editingDoc.content}
                onChange={(e) =>
                  setEditingDoc((prev) => (prev ? { ...prev, content: e.target.value } : null))
                }
                className="h-full w-full resize-none rounded-2xl border border-line bg-surface p-5 font-mono text-xs text-ink leading-relaxed outline-none focus:border-accent focus:ring-2 focus:ring-accent/20 shadow-inner transition-all font-medium"
                placeholder="Document content..."
                spellCheck={false}
              />
            </div>

            {/* Modal Footer */}
            <div className="flex items-center justify-between border-t border-line px-6 py-3.5 bg-surface shrink-0">
              <div className="text-[0.7rem] text-faint font-mono">
                {editingDoc.content.length.toLocaleString()} characters ·{" "}
                {editingDoc.content.split("\n").length.toLocaleString()} lines
              </div>
              <div className="flex items-center gap-2.5">
                <button
                  type="button"
                  onClick={() => setEditingDoc(null)}
                  className="rounded-xl border border-line px-4 py-2 text-xs text-muted hover:bg-canvas hover:text-ink transition-colors font-semibold"
                >
                  Cancel
                </button>
                <button
                  type="button"
                  onClick={() => void handleSaveDoc()}
                  disabled={isSavingDoc}
                  className="rounded-xl bg-accent px-5 py-2 text-xs font-semibold text-white hover:opacity-95 shadow-xs transition-all flex items-center gap-2 active:scale-[0.98] disabled:opacity-50"
                >
                  {isSavingDoc ? (
                    <Icon.Loader size={14} className="animate-spin" />
                  ) : (
                    <Icon.Check size={14} />
                  )}
                  <span>{isSavingDoc ? "Saving & Re-indexing..." : "Save & Re-index"}</span>
                </button>
              </div>
            </div>
          </div>
        </div>
      )}

      {/* Actual Chunks Inspector Modal Dialog */}
      {inspectingChunksKb && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-md p-4 sm:p-6 animate-in fade-in duration-200">
          <div className="flex h-[88vh] w-full max-w-4xl flex-col rounded-3xl border border-line bg-surface shadow-2xl overflow-hidden">
            {/* Header */}
            <div className="flex items-center justify-between border-b border-line px-6 py-4 bg-surface shrink-0">
              <div className="flex items-center gap-3.5">
                <div className="flex h-9 w-9 items-center justify-center rounded-xl bg-accent/15 text-accent ring-1 ring-accent/30 font-bold">
                  🧩
                </div>
                <div>
                  <div className="flex items-center gap-2">
                    <h3 className="text-sm font-bold text-ink">Actual Chunks & AST Boundaries</h3>
                    <span className="rounded-full bg-accent/10 px-2.5 py-0.5 text-[0.65rem] font-bold text-accent font-mono">
                      {inspectingChunksKb.chunks.length} total
                    </span>
                  </div>
                  <p className="text-[0.7rem] text-muted">
                    Library: <span className="font-semibold text-ink font-mono">{inspectingChunksKb.kbId}</span>
                  </p>
                </div>
              </div>

              <div className="flex items-center gap-3">
                {/* Source Filter Dropdown */}
                <select
                  className="rounded-xl border border-line bg-canvas/60 px-3 py-1.5 text-xs text-ink outline-none focus:border-accent font-mono text-[0.7rem]"
                  value={inspectingChunksKb.filterSource}
                  onChange={(e) =>
                    setInspectingChunksKb((prev) =>
                      prev ? { ...prev, filterSource: e.target.value } : null
                    )
                  }
                >
                  <option value="all">All Sources</option>
                  {Array.from(
                    new Set(
                      inspectingChunksKb.chunks.map(
                        (c) => c.source.replace(/\\/g, "/").split("/").pop() || c.source
                      )
                    )
                  ).map((src) => (
                    <option key={src} value={src}>
                      {src}
                    </option>
                  ))}
                </select>

                <button
                  type="button"
                  onClick={() => setInspectingChunksKb(null)}
                  className="ka-icon-btn h-8 w-8 text-muted hover:text-ink hover:bg-canvas rounded-xl transition-colors flex items-center justify-center border border-transparent hover:border-line"
                >
                  <Icon.Close size={16} />
                </button>
              </div>
            </div>

            {/* Chunks List */}
            <div className="flex-1 overflow-y-auto p-5 sm:p-6 space-y-3.5 bg-canvas/40 min-h-0">
              {inspectingChunksKb.chunks
                .filter((c) => {
                  if (inspectingChunksKb.filterSource === "all") return true;
                  const filename = c.source.replace(/\\/g, "/").split("/").pop();
                  return filename === inspectingChunksKb.filterSource;
                })
                .map((chunk, idx) => {
                  const filename = chunk.source.replace(/\\/g, "/").split("/").pop() || chunk.source;
                  return (
                    <div
                      key={`${chunk.source}:${chunk.id || idx}`}
                      className="rounded-2xl border border-line bg-surface p-4 text-xs space-y-2.5 shadow-2xs hover:border-line-strong transition-all"
                    >
                      <div className="flex items-center justify-between gap-2 flex-wrap">
                        <div className="flex items-center gap-2 flex-wrap">
                          <span className="font-mono text-[0.68rem] font-bold text-accent bg-accent/10 px-2 py-0.5 rounded-lg border border-accent/20">
                            #{chunk.id}
                          </span>
                          <span className="font-mono text-[0.72rem] font-semibold text-ink">
                            {filename}
                          </span>
                          {chunk.heading && (
                            <span className="rounded-md bg-canvas px-2 py-0.5 border border-line font-mono text-[0.65rem] text-accent font-semibold">
                              {chunk.heading}
                            </span>
                          )}
                        </div>
                        <span className="font-mono text-[0.68rem] text-faint">
                          {chunk.text.length} chars
                        </span>
                      </div>
                      <div className="p-3.5 rounded-xl bg-canvas/60 border border-line/60 font-mono text-[0.72rem] text-ink whitespace-pre-wrap leading-relaxed select-text">
                        {chunk.text}
                      </div>
                    </div>
                  );
                })}
            </div>

            {/* Footer */}
            <div className="flex items-center justify-between border-t border-line px-6 py-3.5 bg-surface shrink-0 text-xs text-muted">
              <span>
                AST chunks preserve function, class, and header boundaries for grounded citations
              </span>
              <button
                type="button"
                onClick={() => setInspectingChunksKb(null)}
                className="rounded-xl border border-line px-4 py-1.5 text-xs font-semibold text-ink hover:bg-canvas transition-colors"
              >
                Close
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Loading Overlay when fetching document or chunks */}
      {(isLoadingDoc || isLoadingChunks) && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/40 backdrop-blur-md">
          <div className="flex items-center gap-3 rounded-2xl border border-line bg-surface px-5 py-3.5 shadow-xl text-xs font-semibold text-ink">
            <Icon.Loader size={18} className="animate-spin text-accent" />
            <span>{isLoadingDoc ? "Opening document..." : "Loading chunks..."}</span>
          </div>
        </div>
      )}
    </div>
  );
}
