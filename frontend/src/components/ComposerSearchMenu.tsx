import * as DropdownMenu from "@radix-ui/react-dropdown-menu";
import { useEffect, useState } from "react";
import { cn } from "../lib/cn";
import { Icon } from "../lib/icons";
import {
  DEFAULT_SEARCH_OPTIONS,
  DEFAULT_VISION_OPTIONS,
  searchNeedsKeyHint,
  searchPillLabel,
  searchPillTitle,
  visionNeedsKeyHint,
  visionPillLabel,
  type SearchOption,
} from "../lib/searchSetup";
import {
  currentModelId,
  isCodexRuntimeModel,
  modelHasNativeSearch,
  modelNativeSearchLabel,
} from "../lib/modelCatalog";
import { useAppStore } from "../store";

type Panel = "root" | "search" | "vision";

const menuWidth = "w-[min(21rem,calc(100vw-2rem))]";
const menuContentClass =
  "ka-composer-menu z-50 flex max-h-[min(32rem,72vh)] flex-col overflow-hidden rounded-2xl border border-line bg-surface p-1.5 shadow-[0_18px_48px_rgba(0,0,0,0.28)]";

const itemClass =
  "flex cursor-pointer items-center gap-2 rounded-xl px-2.5 py-2 text-sm outline-none data-[highlighted]:bg-accent-soft data-[disabled]:cursor-not-allowed data-[disabled]:opacity-40";

function HintBadge({ hint }: { hint: string }) {
  const needsConnection = hint === "Needs connection";
  return (
    <span
      className={cn(
        "shrink-0 rounded-full px-1.5 py-px text-[0.62rem] font-medium uppercase tracking-wide",
        needsConnection ? "bg-warn-soft text-warn" : "bg-line/80 text-faint",
      )}
    >
      {hint}
    </span>
  );
}

export function ComposerSearchMenu() {
  const setup = useAppStore((s) => s.searchSetup);
  const catalog = useAppStore((s) => s.modelCatalog);
  const modelOverride = useAppStore((s) => s.modelOverride);
  const prefs = useAppStore((s) => s.prefs);
  const defaultModel = useAppStore((s) => s.defaultModel);
  const loadSearch = useAppStore((s) => s.loadSearch);
  const setSearch = useAppStore((s) => s.setSearch);
  const openSettings = useAppStore((s) => s.openSettings);
  const showToast = useAppStore((s) => s.showToast);
  const [panel, setPanel] = useState<Panel>("root");

  useEffect(() => {
    if (!setup) void loadSearch();
  }, [setup, loadSearch]);

  const selectedModel = currentModelId(modelOverride, prefs.mainModel, defaultModel);
  const codexSearch = isCodexRuntimeModel(catalog, selectedModel) && modelHasNativeSearch(catalog, selectedModel);
  const grounded = modelHasNativeSearch(catalog, selectedModel);
  const groundedLabel = modelNativeSearchLabel(catalog, selectedModel);
  const toolPill = searchPillLabel(setup);
  const toolPillDisplay = toolPill === "Z.AI" ? "Z.AI search" : toolPill;
  const loading = !setup;
  const pillLabel = loading
    ? "Search"
    : codexSearch
      ? "Codex"
      : grounded
        ? `${toolPillDisplay} · live web`
        : toolPillDisplay;
  const visionShort = (() => {
    const selectedModelRow = catalog.find((row) => row.id === selectedModel);
    if (selectedModelRow?.vision || selectedModelRow?.video) return "Model vision";
    return visionPillLabel(setup);
  })();
  const hopActive =
    !codexSearch &&
    visionShort !== "Auto" &&
    visionShort !== "Model vision" &&
    visionShort !== "Native" &&
    Boolean(setup?.vision?.configured && setup.vision.configured !== "auto");

  const pillTitle = loading
    ? "Loading search settings…"
    : codexSearch
      ? "Web search: Codex hosted search (takes over while this model is selected)"
      : grounded
        ? `${groundedLabel} on this model · tool search ${searchPillTitle(setup).replace(/^Web search:\s*/i, "")}${hopActive ? ` · vision hop ${visionShort}` : ""}`
        : `${searchPillTitle(setup)}${hopActive ? ` · vision hop ${visionShort}` : ""}`;

  const searchOptions = setup?.options?.length ? setup.options : DEFAULT_SEARCH_OPTIONS;
  const visionOptions = setup?.vision?.options?.length
    ? setup.vision.options
    : DEFAULT_VISION_OPTIONS;
  const searchLocked = Boolean(setup?.source === "env" || codexSearch);
  const visionLocked = setup?.vision?.source === "env";
  const selectedSearch = codexSearch ? "codex" : setup?.configured || "auto";
  const selectedVision = setup?.vision?.configured || "auto";
  const selectedSearchReady =
    searchOptions.find((row) => row.id === selectedSearch)?.ready !== false ||
    selectedSearch === "auto" ||
    selectedSearch === "codex" ||
    selectedSearch === "ddg";
  const selectedSearchNeedsConnection = selectedSearch === "antigravity";
  const selectedModelRow = catalog.find((row) => row.id === selectedModel);
  const modelSeesMedia = Boolean(selectedModelRow?.vision || selectedModelRow?.video);

  const resolveKeyId = (optionId: string) =>
    optionId === "gemini_analyze" || optionId === "gemini_only" ? "gemini" : optionId;

  const chooseSearch = (option: SearchOption) => {
    if (option.ready === false) {
      showToast(searchNeedsKeyHint(option));
      const isCliProvider = option.id === "antigravity";
      openSettings("search", isCliProvider ? undefined : { searchKey: resolveKeyId(option.id) });
      return;
    }
    void setSearch({ backend: option.id }).catch((error) => {
      const message = error instanceof Error ? error.message : String(error);
      showToast(message || "Could not change search provider");
    });
  };

  const chooseVision = (option: SearchOption) => {
    if (option.ready === false) {
      showToast(visionNeedsKeyHint(option));
      openSettings("search", { searchKey: resolveKeyId(option.id) });
      return;
    }
    void setSearch({ vision: option.id }).catch((error) => {
      const message = error instanceof Error ? error.message : String(error);
      showToast(message || "Could not change vision hop");
    });
  };

  return (
    <DropdownMenu.Root
      onOpenChange={(open) => {
        if (!open) setPanel("root");
      }}
    >
      <DropdownMenu.Trigger asChild>
        <button
          type="button"
          id="composer-search-menu"
          className={cn(
            "inline-flex h-7 max-w-[13.5rem] items-center gap-1 rounded-full border bg-surface/80 px-2 text-xs font-medium text-ink shadow-sm hover:border-line-strong hover:bg-line/40",
            !selectedSearchReady && !codexSearch && !loading
              ? "border-warn/50 text-warn"
              : "border-line",
          )}
          title={
            !selectedSearchReady && !codexSearch && !loading
              ? `${pillTitle} — ${selectedSearchNeedsConnection ? "needs a signed-in CLI" : "needs an API key in Settings → Search"}`
              : pillTitle
          }
          aria-label="Web search and vision hop"
          disabled={loading}
        >
          <Icon.Globe size={13} className="shrink-0 text-muted" />
          <span className="min-w-0 truncate">{pillLabel}</span>
          {hopActive ? <Icon.Eye size={12} className="shrink-0 text-faint" aria-hidden /> : null}
          {!selectedSearchReady && !codexSearch && !loading ? (
            <span className="shrink-0 text-[0.58rem] font-medium uppercase tracking-wide text-warn">
              {selectedSearchNeedsConnection ? "Connect" : "Key"}
            </span>
          ) : null}
          <Icon.Chevron size={12} className="shrink-0 text-faint" />
        </button>
      </DropdownMenu.Trigger>
      <DropdownMenu.Portal>
        <DropdownMenu.Content
          className={cn(menuContentClass, menuWidth)}
          side="top"
          align="start"
          sideOffset={8}
          collisionPadding={16}
        >
          {panel === "root" ? (
            <>
              <DropdownMenu.Item
                className={cn(itemClass, "justify-between")}
                onSelect={(event) => {
                  event.preventDefault();
                  setPanel("search");
                }}
              >
                <span className="inline-flex min-w-0 items-center gap-2.5">
                  <Icon.Globe size={15} className="shrink-0 text-muted" />
                  <span>Search</span>
                </span>
                <span className="ml-3 inline-flex min-w-0 items-center gap-1.5 text-xs text-muted">
                  <span className="truncate">{pillLabel}</span>
                  <Icon.ChevronRight size={13} className="shrink-0 text-faint" />
                </span>
              </DropdownMenu.Item>
              <DropdownMenu.Item
                className={cn(itemClass, "justify-between")}
                onSelect={(event) => {
                  event.preventDefault();
                  setPanel("vision");
                }}
              >
                <span className="inline-flex items-center gap-2.5">
                  <Icon.Eye size={15} className="shrink-0 text-muted" />
                  <span>Vision hop</span>
                </span>
                <span className="ml-3 inline-flex items-center gap-1.5 text-xs text-muted">
                  {visionShort}
                  <Icon.ChevronRight size={13} className="text-faint" />
                </span>
              </DropdownMenu.Item>
              <DropdownMenu.Separator className="my-1.5 h-px bg-line" />
              <DropdownMenu.Item className={itemClass} onSelect={() => openSettings("search")}>
                <Icon.Settings size={15} className="shrink-0 text-muted" />
                <span className="flex-1">Search settings</span>
                <span className="text-[0.62rem] text-faint">Keys · providers</span>
              </DropdownMenu.Item>
            </>
          ) : null}

          {panel === "search" ? (
            <>
              <DropdownMenu.Item
                className={itemClass}
                onSelect={(event) => {
                  event.preventDefault();
                  setPanel("root");
                }}
              >
                <Icon.Back size={14} className="text-faint" />
                Search
              </DropdownMenu.Item>
              {codexSearch ? (
                <p className="px-2.5 py-1.5 text-[0.68rem] leading-relaxed text-muted">
                  Codex hosted search is active. Switch models to restore your tool search engine.
                </p>
              ) : searchLocked ? (
                <p className="px-2.5 py-1.5 text-[0.68rem] leading-relaxed text-warn">
                  Pinned by KAGEHA_WEB_SEARCH. Clear the env var to change it here.
                </p>
              ) : grounded ? (
                <p className="px-2.5 py-1.5 text-[0.68rem] leading-relaxed text-muted">
                  <span className="font-medium text-ink">{groundedLabel}</span> is on for this
                  model. Pick a tool engine below for multi-source research and citations.
                </p>
              ) : (
                <p className="px-2.5 py-1.5 text-[0.68rem] leading-relaxed text-muted">
                  Auto picks the first engine with a key. Needs key opens Search settings.
                </p>
              )}
              {grounded && !codexSearch ? (
                <div className="mx-1 mb-1 flex items-center gap-2 rounded-xl bg-accent-soft/40 px-2.5 py-1.5 text-[0.7rem] text-ink">
                  <Icon.Globe size={13} className="shrink-0 text-accent" aria-hidden />
                  <span className="min-w-0 flex-1 truncate">{groundedLabel}</span>
                  <span className="shrink-0 text-[0.58rem] font-medium uppercase tracking-wide text-accent">
                    On
                  </span>
                </div>
              ) : null}
              <div className="min-h-0 flex-1 overflow-y-auto">
                {(codexSearch
                  ? [{ id: "codex", label: "Codex hosted search", ready: true }]
                  : searchOptions
                ).map((option) => (
                  <DropdownMenu.Item
                    key={option.id}
                    className={cn(itemClass, option.ready === false && "opacity-80")}
                    disabled={searchLocked}
                    title={option.ready === false ? searchNeedsKeyHint(option) : undefined}
                    onSelect={() => chooseSearch(option)}
                  >
                    <span className="flex w-4 shrink-0 justify-center text-accent">
                      {selectedSearch === option.id ? <Icon.Check size={14} /> : null}
                    </span>
                    <span className="min-w-0 flex-1 whitespace-normal leading-tight">
                      {option.label}
                    </span>
                    {option.ready === false ? (
                      <HintBadge
                        hint={option.id === "antigravity" ? "Needs connection" : "Needs key"}
                      />
                    ) : null}
                  </DropdownMenu.Item>
                ))}
              </div>
            </>
          ) : null}

          {panel === "vision" ? (
            <>
              <DropdownMenu.Item
                className={itemClass}
                onSelect={(event) => {
                  event.preventDefault();
                  setPanel("root");
                }}
              >
                <Icon.Back size={14} className="text-faint" />
                Vision hop
              </DropdownMenu.Item>
              {visionLocked ? (
                <p className="px-2.5 py-1.5 text-[0.68rem] leading-relaxed text-warn">
                  Pinned by KAGEHA_VISION_HOP. Clear the env var to change it here.
                </p>
              ) : (
                <p className="px-2.5 py-1.5 text-[0.68rem] leading-relaxed text-muted">
                  {modelSeesMedia
                    ? "This model sees images and video natively. Hop only for text-only planners."
                    : "Native vision when the model supports it; hop only when it cannot see."}
                </p>
              )}
              <div className="min-h-0 flex-1 overflow-y-auto">
                {visionOptions.map((option) => (
                  <DropdownMenu.Item
                    key={option.id}
                    className={cn(itemClass, option.ready === false && "opacity-80")}
                    disabled={visionLocked}
                    title={option.ready === false ? visionNeedsKeyHint(option) : undefined}
                    onSelect={() => chooseVision(option)}
                  >
                    <span className="flex w-4 shrink-0 justify-center text-accent">
                      {selectedVision === option.id ? <Icon.Check size={14} /> : null}
                    </span>
                    <span className="min-w-0 flex-1">{option.label}</span>
                    {option.ready === false ? <HintBadge hint="Needs key" /> : null}
                  </DropdownMenu.Item>
                ))}
              </div>
            </>
          ) : null}
        </DropdownMenu.Content>
      </DropdownMenu.Portal>
    </DropdownMenu.Root>
  );
}
