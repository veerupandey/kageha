import { useEffect, useState } from "react";
import { api } from "../api/client";
import { cn } from "../lib/cn";
import { Icon } from "../lib/icons";
import {
  DEFAULT_SEARCH_KEYS,
  DEFAULT_SEARCH_OPTIONS,
  DEFAULT_VISION_OPTIONS,
  searchNeedsKeyHint,
  searchShortLabel,
  visionNeedsKeyHint,
  visionShortLabel,
  type SearchKeyCard,
  type SearchOption,
} from "../lib/searchSetup";
import { currentModelId, isCodexRuntimeModel, modelHasNativeSearch, modelNativeSearchLabel } from "../lib/modelCatalog";
import { useAppStore } from "../store";

function PickChip({
  option,
  selected,
  disabled,
  short,
  missingHint,
  onPick,
}: {
  option: SearchOption;
  selected: boolean;
  disabled?: boolean;
  short: string;
  missingHint: string;
  onPick: () => void;
}) {
  const missing = option.ready === false;
  return (
    <button
      type="button"
      role="radio"
      aria-checked={missing ? false : selected}
      aria-label={option.label}
      disabled={disabled}
      title={missing ? missingHint : option.label}
      className={cn("ka-pick-chip", selected && !missing && "is-on", missing && "is-missing")}
      onClick={onPick}
    >
      {selected && !missing ? <Icon.Check size={13} className="shrink-0 text-accent" /> : null}
      <span>{short}</span>
      {missing ? <span className="ka-pick-chip-key">Key</span> : null}
    </button>
  );
}

export function SearchSetupPanel() {
  const setup = useAppStore((s) => s.searchSetup);
  const hasCodexRuntime = useAppStore((s) =>
    s.modelCatalog.some(
      (model) => model.ready !== false && model.provider === "openai-codex-app",
    ),
  );
  const catalog = useAppStore((s) => s.modelCatalog);
  const modelOverride = useAppStore((s) => s.modelOverride);
  const prefs = useAppStore((s) => s.prefs);
  const defaultModel = useAppStore((s) => s.defaultModel);
  const loadSearch = useAppStore((s) => s.loadSearch);
  const loadModels = useAppStore((s) => s.loadModels);
  const setSearch = useAppStore((s) => s.setSearch);
  const showToast = useAppStore((s) => s.showToast);
  const searchKeyFocus = useAppStore((s) => s.searchKeyFocus);
  const clearSearchKeyFocus = useAppStore((s) => s.clearSearchKeyFocus);
  const [busy, setBusy] = useState("");
  const [note, setNote] = useState("");
  const [tone, setTone] = useState<"ok" | "warn">("ok");
  const [keyDrafts, setKeyDrafts] = useState<Record<string, string>>({});
  const [loadFailed, setLoadFailed] = useState(false);
  const [highlightKeyId, setHighlightKeyId] = useState("");

  useEffect(() => {
    if (!setup) {
      setLoadFailed(false);
      void loadSearch().then((ok) => {
        if (!ok && !useAppStore.getState().searchSetup) setLoadFailed(true);
      });
    }
  }, [setup, loadSearch]);

  useEffect(() => {
    if (!highlightKeyId) return;
    const timer = window.setTimeout(() => setHighlightKeyId(""), 3200);
    return () => window.clearTimeout(timer);
  }, [highlightKeyId]);

  useEffect(() => {
    if (!searchKeyFocus || !setup) return;
    const keyId = searchKeyFocus;
    const card = (setup.keys?.length ? setup.keys : DEFAULT_SEARCH_KEYS).find((row) => row.id === keyId);
    setTone("warn");
    setNote(
      card
        ? `${card.label} needs an API key. Paste it in the highlighted card below.`
        : "Paste an API key in the highlighted card below.",
    );
    setHighlightKeyId(keyId);
    clearSearchKeyFocus();
    const timer = window.setTimeout(() => {
      const input = document.getElementById(`search-key-${keyId}`) as HTMLInputElement | null;
      if (!input) return;
      input.focus();
      input.scrollIntoView({ behavior: "smooth", block: "center" });
    }, 0);
    return () => window.clearTimeout(timer);
  }, [searchKeyFocus, setup, clearSearchKeyFocus]);

  if (!setup) {
    return (
      <section className="rounded-2xl border border-dashed border-line bg-canvas/30 px-4 py-3.5">
        <p className="text-sm text-muted">
          {loadFailed
            ? "Could not load search settings. Check that the server is running, then reopen Settings."
            : "Loading search…"}
        </p>
      </section>
    );
  }

  const selectedModel = currentModelId(modelOverride, prefs.mainModel, defaultModel);
  const codexSearch = isCodexRuntimeModel(catalog, selectedModel) && modelHasNativeSearch(catalog, selectedModel);
  const nativeSearch = modelHasNativeSearch(catalog, selectedModel);
  const nativeSearchLabel = modelNativeSearchLabel(catalog, selectedModel);
  const grounded = nativeSearch;
  const groundedLabel = nativeSearchLabel;
  const selectedModelRow = catalog.find((row) => row.id === selectedModel);
  const modelSeesMedia = Boolean(selectedModelRow?.vision || selectedModelRow?.video);
  const searchOptions = codexSearch
    ? [{ id: "codex", label: "Codex hosted search", ready: true }]
    : setup.options.length
      ? setup.options
      : DEFAULT_SEARCH_OPTIONS;
  const visionOptions = setup.vision?.options.length
    ? setup.vision.options
    : DEFAULT_VISION_OPTIONS;
  const keyCards: SearchKeyCard[] = setup.keys?.length ? setup.keys : DEFAULT_SEARCH_KEYS;
  const searchLocked = setup.source === "env" || codexSearch;
  const visionLocked = setup.vision?.source === "env";
  const selectedSearch = codexSearch ? "codex" : setup.configured || "auto";
  const selectedVision = setup.vision?.configured || "auto";
  const readySearchCount = searchOptions.filter((row) => row.ready !== false && row.id !== "auto").length;

  const resolveKeyId = (optionId: string) =>
    optionId === "gemini_analyze" || optionId === "gemini_only" ? "gemini" : optionId;

  const pick = async (patch: { backend?: string; vision?: string }, option?: SearchOption) => {
    if (option && option.ready === false) {
      const hint = patch.backend
        ? searchNeedsKeyHint(option) + (option.id === "antigravity" ? "." : ". Paste a key in the cards below.")
        : visionNeedsKeyHint(option) + ". Paste a key in the cards below.";
      const keyId = resolveKeyId(option.id);
      setTone("warn");
      setNote(hint);
      const isCliProvider = option.id === "antigravity";
      setHighlightKeyId(isCliProvider ? "" : keyId);
      showToast(patch.backend ? searchNeedsKeyHint(option) : visionNeedsKeyHint(option));
      // Defer past React commit so the highlight/sticky banner exist before focusing.
      if (isCliProvider) return;
      window.setTimeout(() => {
        const input = document.getElementById(`search-key-${keyId}`) as HTMLInputElement | null;
        if (!input) return;
        input.focus();
        input.scrollIntoView({ behavior: "smooth", block: "center" });
      }, 0);
      return;
    }
    setBusy(patch.backend ? "search" : "vision");
    setNote("");
    setHighlightKeyId("");
    try {
      await setSearch(patch);
      setTone("ok");
      const label = option
        ? patch.backend
          ? searchShortLabel(option.id, option.label)
          : visionShortLabel(option.id, option.label)
        : "Preference";
      setNote(`${label} saved. Change it anytime from the chat bar, too.`);
      showToast(`${label} saved`);
    } catch (error) {
      setTone("warn");
      const message = error instanceof Error ? error.message : String(error);
      setNote(message || "Could not save search preference.");
      showToast(message || "Could not save search preference.");
    } finally {
      setBusy("");
    }
  };

  const saveKey = async (card: SearchKeyCard) => {
    const secret = (keyDrafts[card.id] || "").trim();
    if (!secret) {
      setTone("warn");
      setNote(`Paste a ${card.label} API key first.`);
      setHighlightKeyId(card.id);
      return;
    }
    setBusy(`key-${card.id}`);
    setNote("");
    try {
      await api("/api/provider-setup/key", {
        method: "POST",
        body: JSON.stringify({ provider: card.id, api_key: secret }),
      });
      setKeyDrafts((current) => ({ ...current, [card.id]: "" }));
      await Promise.all([loadSearch(), loadModels()]);
      setTone("ok");
      setHighlightKeyId("");
      setNote(`${card.label} key saved. It is never shown back in the browser.`);
      showToast(`${card.label} ready`);
    } catch (error) {
      setTone("warn");
      const message = error instanceof Error ? error.message : String(error);
      setNote(message || `Could not save ${card.label} key.`);
      showToast(message || `Could not save ${card.label} key.`);
    } finally {
      setBusy("");
    }
  };

  return (
    <div className="space-y-6">
      {note ? (
        <div
          role="status"
          className={cn(
            "sticky top-0 z-10 rounded-xl border px-3 py-2.5 text-[0.75rem] leading-relaxed shadow-sm",
            tone === "warn"
              ? "border-warn/40 border-l-[3px] border-l-warn bg-warn-soft text-warn"
              : "border-accent/25 border-l-[3px] border-l-accent bg-accent-soft/40 text-ink",
          )}
        >
          {note}
        </div>
      ) : null}

      <section className="rounded-2xl border border-line bg-canvas/35 p-4">
        <h3 className="text-sm font-semibold text-ink">Provider</h3>
        <p className="mt-1 text-[0.7rem] leading-relaxed text-muted">
          {codexSearch
            ? "Codex hosted search is active for the selected model. Switch models to restore the tool engine below."
            : grounded
              ? `${groundedLabel} is built into the selected model. The picker chooses the tool engine for multi-source research and citations.`
              : "Same picker as the chat bar. Paste keys below to unlock Brave, Tavily, Exa, Perplexity, Z.AI, or Gemini — DuckDuckGo works without a key."}
        </p>
        {grounded && !codexSearch ? (
          <div className="mt-3 flex items-start gap-2 rounded-xl border border-accent/25 bg-accent-soft/30 px-3 py-2 text-[0.72rem] leading-relaxed text-ink">
            <Icon.Globe size={14} className="mt-0.5 shrink-0 text-accent" aria-hidden />
            <span>
              <span className="font-medium">{groundedLabel}</span>
              {modelSeesMedia ? " · native vision" : ""} — answers can ground without a tool call.
            </span>
          </div>
        ) : null}
        <div
          className="ka-pick-grid mt-3"
          role="radiogroup"
          aria-label="Web search provider"
          aria-disabled={searchLocked || busy !== ""}
        >
          {searchOptions.map((option) => (
            <PickChip
              key={option.id}
              option={option}
              selected={selectedSearch === option.id}
              disabled={searchLocked || busy !== ""}
              short={searchShortLabel(option.id, option.label)}
              missingHint={searchNeedsKeyHint(option)}
              onPick={() => void pick({ backend: option.id }, option)}
            />
          ))}
        </div>
        <p className="mt-3 text-sm text-ink">
          Using {codexSearch ? "Codex hosted search" : setup.label}
        </p>
        {codexSearch ? (
          <p className="mt-0.5 text-[0.7rem] leading-relaxed text-muted">
            Switch to another model to restore {setup.label}.
          </p>
        ) : setup.fallbacks.length > 0 ? (
          <p className="mt-0.5 text-[0.7rem] leading-relaxed text-muted">
            Falls back to {setup.fallbacks.join(", ")}
          </p>
        ) : readySearchCount === 0 ? (
          <p className="mt-0.5 text-[0.7rem] leading-relaxed text-warn">
            No keyed search engines yet — DuckDuckGo still works, or paste a key below.
          </p>
        ) : null}
      </section>

      <section className="rounded-2xl border border-line bg-canvas/35 p-4">
        <h3 className="text-sm font-semibold text-ink">API keys</h3>
        <p className="mt-1 text-[0.7rem] leading-relaxed text-muted">
          Keys stay on the server and are never echoed back. Z.AI unlocks Coding Plan chat + search;
          Gemini unlocks live web search + chat. OpenCode Go is a separate chat host under Models.
        </p>
        <div className="mt-3 space-y-3">
          {keyCards.map((card) => (
            <div
              key={card.id}
              className={cn(
                "rounded-xl border bg-surface/60 px-3 py-2.5 transition",
                highlightKeyId === card.id
                  ? "border-warn/50 ring-2 ring-warn/25"
                  : "border-line/80",
              )}
            >
              <div className="flex flex-wrap items-center justify-between gap-2">
                <div className="min-w-0">
                  <p className="text-sm font-medium text-ink">{card.label}</p>
                  <p className="text-[0.65rem] text-faint">
                    {card.env}
                    {card.hint ? ` · ${card.hint}` : ""}
                  </p>
                </div>
                <span
                  className={cn(
                    "rounded-full px-2 py-0.5 text-[0.58rem] font-medium uppercase tracking-wide",
                    card.ready ? "bg-accent-soft text-accent" : "bg-warn-soft text-warn",
                  )}
                >
                  {card.ready ? "Ready" : "Needs key"}
                </span>
              </div>
              {card.ready ? null : (
                <div className="mt-2 flex flex-wrap gap-2">
                  <input
                    id={`search-key-${card.id}`}
                    type="password"
                    autoComplete="off"
                    spellCheck={false}
                    placeholder={`Paste ${card.env}`}
                    className="min-w-[12rem] flex-1 rounded-lg border border-line bg-canvas px-2.5 py-1.5 text-xs text-ink outline-none focus:border-accent"
                    value={keyDrafts[card.id] || ""}
                    onChange={(event) =>
                      setKeyDrafts((current) => ({
                        ...current,
                        [card.id]: event.target.value,
                      }))
                    }
                    disabled={busy !== ""}
                  />
                  <button
                    type="button"
                    className="rounded-lg bg-ink px-3 py-1.5 text-xs font-medium text-canvas disabled:opacity-50"
                    disabled={busy !== ""}
                    onClick={() => void saveKey(card)}
                  >
                    {busy === `key-${card.id}` ? "Saving…" : "Save"}
                  </button>
                </div>
              )}
            </div>
          ))}
        </div>
      </section>

      {hasCodexRuntime && !codexSearch ? (
        <section className="rounded-2xl border border-line/80 bg-canvas/20 px-4 py-3">
          <p className="text-[0.7rem] leading-relaxed text-muted">
            <span className="font-medium text-ink">Tip:</span> selecting a Codex Runtime model
            switches search to Codex hosted search automatically; your tool engine returns when you
            leave Codex.
          </p>
        </section>
      ) : null}

      <section className="rounded-2xl border border-line bg-canvas/35 p-4">
        <h3 className="flex items-center gap-1.5 text-sm font-semibold text-ink">
          <Icon.Eye size={14} className="text-accent" />
          Vision hop
        </h3>
        <p className="mt-1 text-[0.7rem] leading-relaxed text-muted">
          {modelSeesMedia
            ? "This model already sees media natively — hop only matters for text-only planners."
            : "Used when the selected model cannot see images or video. Multimodal models (GLM 5.3 Flash, Gemini) skip this hop."}
        </p>
        <div
          className="ka-pick-grid mt-3"
          role="radiogroup"
          aria-label="Vision hop"
          aria-disabled={visionLocked || busy !== ""}
        >
          {visionOptions.map((option) => (
            <PickChip
              key={option.id}
              option={option}
              selected={selectedVision === option.id}
              disabled={visionLocked || busy !== ""}
              short={visionShortLabel(option.id, option.label)}
              missingHint={visionNeedsKeyHint(option)}
              onPick={() => void pick({ vision: option.id }, option)}
            />
          ))}
        </div>
        {setup.vision ? (
          <p className="mt-3 text-[0.7rem] leading-relaxed text-muted">
            <span className="font-medium text-ink">{setup.vision.label}</span>
            {setup.vision.hint ? ` — ${setup.vision.hint}` : ""}
          </p>
        ) : null}
      </section>

      {searchLocked || visionLocked ? (
        <p className="text-[0.7rem] leading-relaxed text-warn">
          An environment variable is overriding the file. Clear KAGEHA_WEB_SEARCH /
          KAGEHA_VISION_HOP to use the pickers above.
        </p>
      ) : null}
    </div>
  );
}
