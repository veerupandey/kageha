import * as DropdownMenu from "@radix-ui/react-dropdown-menu";
import { useState } from "react";
import { cn } from "../lib/cn";
import { Icon } from "../lib/icons";
import {
  composerModelDisabled,
  composerModelHint,
  composerModelSelectable,
  currentModelId,
  effortLabel,
  EFFORT_OPTIONS,
  groupModelsByProvider,
  modelCapabilityTitle,
  modelDisplayLabel,
  modelProviderId,
} from "../lib/modelCatalog";
import { useAppStore } from "../store";

type Panel = "root" | "model" | "planner" | "effort";

const menuWidth = "w-[min(21rem,calc(100vw-2rem))]";
const menuContentClass =
  "ka-composer-menu z-50 flex max-h-[min(32rem,72vh)] flex-col overflow-hidden rounded-2xl border border-line bg-surface p-1.5 shadow-[0_18px_48px_rgba(0,0,0,0.28)]";

const itemClass =
  "flex cursor-pointer items-center gap-2 rounded-xl px-2.5 py-2 text-sm outline-none data-[highlighted]:bg-accent-soft data-[disabled]:cursor-not-allowed data-[disabled]:opacity-40";

type ModelRow = {
  id: string;
  free?: boolean;
  pricing_note?: string;
  label?: string;
  ready?: boolean;
  provider?: string;
  model?: string;
  web_search?: boolean;
  vision?: boolean;
  video?: boolean;
  reasoning?: boolean;
  latest?: boolean;
  capabilities?: string[];
};

function HintBadge({ hint }: { hint: string }) {
  const needsKey = hint === "Needs key" || hint === "Needs connection" || hint === "Key unavailable";
  return (
    <span
      className={cn(
        "shrink-0 rounded-full px-1.5 py-px text-[0.62rem] font-medium uppercase tracking-wide",
        needsKey ? "bg-warn-soft text-warn" : "bg-line/80 text-faint",
      )}
    >
      {hint}
    </span>
  );
}

function modelRowTitle(row: ModelRow, planner = false): string {
  if (row.ready === false) return "Add an API key in Settings to use this model";
  if (row.provider === "openai-codex-app" || row.id === "gpt-codex-app") {
    return "Codex Runtime — Kageha tools via your Codex login";
  }
  if (!planner && !composerModelSelectable(row)) return "Tool support is unavailable or unverified — use in Planner";
  const caps = modelCapabilityTitle(row);
  return [caps ? caps.charAt(0).toUpperCase() + caps.slice(1) : "", row.pricing_note]
    .filter(Boolean).join(" · ");
}

/** Shared model-selection panel: search + Auto + provider groups.
 *  Used for both the Executor (driving model) and Planner slots. */
function ModelChooser({
  title,
  role,
  autoHint,
  catalog,
  models,
  pinnedId,
  onChoose,
  onBack,
  onManageKeys,
  credentialsLocked,
}: {
  title: string;
  role: "executor" | "planner";
  autoHint: string;
  catalog: ModelRow[];
  models: string[];
  pinnedId: string;
  onChoose: (id: string) => void;
  onBack: () => void;
  onManageKeys: () => void;
  credentialsLocked: boolean;
}) {
  const [query, setQuery] = useState("");
  const [expandedModels, setExpandedModels] = useState<string[]>([]);
  const planner = role === "planner";
  const defaultModel = useAppStore((s) => s.defaultModel);
  const selected = catalog.find((row) => row.id === (pinnedId || defaultModel));
  const aliases = new Map<string, ModelRow>();
  const source: ModelRow[] = catalog.length ? catalog : models.map((id) => ({ id, label: id, ready: true }));
  for (const row of source) {
    const key = `${modelProviderId(row)}:${row.model || row.id}`;
    if (!aliases.has(key) || row.id === pinnedId) aliases.set(key, row);
  }
  const groups = groupModelsByProvider(
    [...aliases.values()],
    planner ? "planner" : "executor",
  );
  // Seed the open provider once on mount (the chooser remounts each time the
  // panel opens). Doing this in render would fight the user's collapse clicks.
  const [openProviders, setOpenProviders] = useState<string[]>(() => {
    const provider = selected ? modelProviderId(selected) : "";
    if (provider) return [provider];
    return groups.length === 1 ? [groups[0].id] : [];
  });
  const filteredGroups = groups
    .map((group) => ({
      ...group,
      models: group.models.filter((row) =>
        `${row.label || row.id} ${group.label} ${row.id}`
          .toLowerCase()
          .includes(query.toLowerCase().trim()),
      ),
    }))
    .filter((group) => group.models.length);

  const toggleProvider = (id: string) =>
    setOpenProviders((current) =>
      current.includes(id) ? current.filter((name) => name !== id) : [...current, id],
    );

  return (
    <>
      <DropdownMenu.Item
        className={itemClass}
        onSelect={(event) => {
          event.preventDefault();
          onBack();
        }}
      >
        <Icon.Back size={14} className="text-faint" />
        {title}
      </DropdownMenu.Item>
      <div className="min-h-0 flex-1 overflow-y-auto">
        <div className="ka-model-search">
          <Icon.Search size={16} />
          <input
            aria-label="Search models"
            placeholder="Search models or providers…"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            onKeyDown={(event) => {
              if (event.key !== "Escape" && event.key !== "Tab") event.stopPropagation();
            }}
          />
        </div>
        <DropdownMenu.Item className={itemClass} onSelect={() => onChoose("")}>
          <span className="flex w-4 shrink-0 justify-center text-accent">
            {!pinnedId ? <Icon.Check size={14} /> : null}
          </span>
          <span className="min-w-0 flex-1">
            <span className="block leading-tight">Auto</span>
            <span className="block text-[0.62rem] font-normal text-faint">{autoHint}</span>
          </span>
        </DropdownMenu.Item>
        {!filteredGroups.length && <p className="ka-menu-empty">No models match “{query}”.</p>}
        {filteredGroups.map((group) => {
          const open = Boolean(query.trim()) || openProviders.includes(group.id);
          const extraModels = group.models.filter((row) => !planner && composerModelDisabled(row) && row.id !== pinnedId);
          const showExtra = Boolean(query.trim()) || expandedModels.includes(group.id);
          const visibleModels = showExtra ? group.models : group.models.filter((row) => !extraModels.includes(row));
          const groupGrounded =
            group.id === "openai-codex-app" ||
            group.models.some(
              (row) =>
                row.provider === "openai-codex-app" ||
                row.web_search ||
                row.capabilities?.includes("web_search"),
            );
          return (
            <DropdownMenu.Group key={group.id}>
              <DropdownMenu.Item
                className={cn(itemClass, "justify-between")}
                onSelect={(event) => {
                  event.preventDefault();
                  toggleProvider(group.id);
                }}
                aria-expanded={open}
                title={
                  groupGrounded
                    ? "These models can search the live web without a separate search tool"
                    : undefined
                }
              >
                <span className="inline-flex min-w-0 items-center gap-1.5">
                  <Icon.Chevron
                    size={12}
                    className={cn(
                      "shrink-0 text-faint transition-transform",
                      open ? "rotate-0" : "-rotate-90",
                    )}
                  />
                  <span className="truncate text-[0.72rem] font-semibold text-muted">
                    {group.label}
                  </span>
                  {groupGrounded ? (
                    <span className="shrink-0 rounded-md bg-accent-soft/70 px-1 py-px text-[0.55rem] font-semibold uppercase tracking-wide text-accent">
                      Live web
                    </span>
                  ) : null}
                </span>
                <span className="text-[0.62rem] text-faint">
                  {group.models.filter((row) => row.ready !== false && (planner || composerModelSelectable(row))).length} ready
                </span>
              </DropdownMenu.Item>
              {open
                ? visibleModels.map((row) => {
                    const hint = row.ready === false && credentialsLocked ? "Key unavailable" : planner && row.ready !== false ? "" : composerModelHint(row);
                    const statusHint =
                      hint === "Needs key" || hint === "Needs connection" || hint === "Key unavailable" || hint === "Planning only" ? hint : "";
                    return (
                      <DropdownMenu.Item
                        key={row.id}
                        className={cn(itemClass, "pl-7", row.ready === false && "opacity-80")}
                        disabled={!planner && composerModelDisabled(row)}
                        title={hint === "Key unavailable" ? "Unlock saved credentials or check the provider connection in Models settings" : modelRowTitle(row, planner) || undefined}
                        onSelect={() => {
                          if (row.ready === false) {
                            onManageKeys();
                            return;
                          }
                          onChoose(row.id);
                        }}
                      >
                        <span className="flex w-4 shrink-0 justify-center text-accent">
                          {pinnedId === row.id ? <Icon.Check size={14} /> : null}
                        </span>
                        <span className="min-w-0 flex-1 whitespace-normal leading-tight">
                          {row.label || row.id}
                          {row.free && row.pricing_note ? (
                            <span className="mt-0.5 block text-[0.62rem] text-muted">{row.pricing_note}</span>
                          ) : null}
                        </span>
                        {row.free ? <HintBadge hint="Free" /> : null}
                        {statusHint ? (
                          <HintBadge hint={statusHint} />
                        ) : (
                          <>
                            {row.web_search ? (
                              <Icon.Globe size={13} className="shrink-0 text-faint" aria-hidden />
                            ) : null}
                            {row.vision ? (
                              <Icon.Eye size={13} className="shrink-0 text-faint" aria-hidden />
                            ) : null}
                            {row.latest ? (
                              <span className="rounded-md border border-line px-1 py-px text-[0.58rem] font-medium uppercase tracking-wide text-faint">
                                Latest
                              </span>
                            ) : null}
                          </>
                        )}
                      </DropdownMenu.Item>
                    );
                  })
                : null}
              {open && extraModels.length > 0 && !query.trim() ? (
                <DropdownMenu.Item
                  className={cn(itemClass, "pl-7 text-muted")}
                  onSelect={(event) => {
                    event.preventDefault();
                    setExpandedModels((ids) => ids.includes(group.id)
                      ? ids.filter((id) => id !== group.id) : [...ids, group.id]);
                  }}
                  aria-expanded={showExtra}
                >
                  {showExtra ? "Fewer models" : `More models (${extraModels.length})`}
                </DropdownMenu.Item>
              ) : null}
            </DropdownMenu.Group>
          );
        })}
      </div>
      <DropdownMenu.Separator className="my-1.5 h-px bg-line" />
      <DropdownMenu.Item
        className={cn(itemClass, "mt-0.5 border border-line/70 bg-canvas/40 hover:border-line-strong")}
        onSelect={onManageKeys}
      >
        <Icon.Plug size={14} className="shrink-0 text-muted" />
        <span className="min-w-0 flex-1">Manage models & keys</span>
        <span className="text-[0.62rem] text-faint">Settings</span>
      </DropdownMenu.Item>
    </>
  );
}

export function ComposerModelMenu() {
  const catalog = useAppStore((s) => s.modelCatalog);
  const credentialsLocked = useAppStore((s) => s.modelCredentialsLocked);
  const models = useAppStore((s) => s.models);
  const defaultModel = useAppStore((s) => s.defaultModel);
  const modelOverride = useAppStore((s) => s.modelOverride);
  const setModelOverride = useAppStore((s) => s.setModelOverride);
  const prefs = useAppStore((s) => s.prefs);
  const setPrefs = useAppStore((s) => s.setPrefs);
  const openSettings = useAppStore((s) => s.openSettings);
  const [panel, setPanel] = useState<Panel>("root");

  const effort = prefs.reasoningEffort || "";
  // Executor / driving model — the composer pin.
  const executorPinId = (modelOverride || prefs.mainModel).trim();
  const selectedId = currentModelId(modelOverride, prefs.mainModel, defaultModel);
  const executorLabel = executorPinId ? modelDisplayLabel(catalog, selectedId) : "Auto";
  const selected = catalog.find((row) => row.id === selectedId);
  // Planner slot — independent of the executor pin.
  const plannerPinId = prefs.plannerModel.trim();
  const plannerLabel = plannerPinId ? modelDisplayLabel(catalog, plannerPinId) : "Auto";
  // Pill shows the effective driving model (or Auto).
  const pillLabel = executorPinId ? executorLabel : "Auto";

  const chooseExecutor = (id: string) => {
    setModelOverride(id);
    setPrefs({ mainModel: id });
  };
  const choosePlanner = (id: string) => {
    setPrefs({ plannerModel: id });
  };
  const goManageKeys = () => openSettings("models");

  return (
    <DropdownMenu.Root
      onOpenChange={(open) => {
        if (!open) setPanel("root");
      }}
    >
      <DropdownMenu.Trigger asChild>
        <button
          type="button"
          id="composer-model-menu"
          className="inline-flex h-7 max-w-[15rem] items-center gap-1 rounded-full border border-line bg-surface/80 px-2 text-xs font-medium text-ink shadow-sm hover:border-line-strong hover:bg-line/40"
          title="Model and reasoning effort"
          aria-label="Model and reasoning effort"
        >
          <Icon.Tune size={13} className="shrink-0 text-muted" />
          <span className="min-w-0 truncate">{pillLabel}</span>
          {selected?.web_search ? (
            <Icon.Globe size={12} className="shrink-0 text-faint" aria-hidden />
          ) : selected?.vision ? (
            <Icon.Eye size={12} className="shrink-0 text-faint" aria-hidden />
          ) : null}
          <Icon.Chevron size={12} className="shrink-0 text-faint" />
        </button>
      </DropdownMenu.Trigger>
      <DropdownMenu.Portal>
        <DropdownMenu.Content
          className={cn(menuContentClass, menuWidth, "ka-model-picker")}
          side="top"
          align="start"
          sideOffset={8}
          collisionPadding={16}
        >
          {credentialsLocked && <DropdownMenu.Item className={itemClass} onSelect={goManageKeys}>
            <span className="text-warn">Saved model credentials locked · Open Models settings</span>
          </DropdownMenu.Item>}
          {panel === "root" ? (
            <>
              <DropdownMenu.Label className="px-2.5 pb-1 pt-1.5 text-[0.62rem] font-semibold uppercase tracking-wide text-faint">
                Roles · Auto picks the best ready model per role
              </DropdownMenu.Label>
              <DropdownMenu.Item
                className={cn(itemClass, "justify-between")}
                onSelect={(event) => {
                  event.preventDefault();
                  setPanel("model");
                }}
              >
                <span className="inline-flex min-w-0 items-center gap-2.5">
                  <Icon.Brain size={15} className="shrink-0 text-muted" />
                  <span className="min-w-0">
                    <span className="block leading-tight">Executor</span>
                    <span className="block text-[0.6rem] font-normal text-faint">
                      Runs the task & tools
                    </span>
                  </span>
                </span>
                <span className="ml-3 inline-flex min-w-0 items-center gap-1.5 text-xs text-muted">
                  <span className="truncate">{executorLabel}</span>
                  <Icon.ChevronRight size={13} className="shrink-0 text-faint" />
                </span>
              </DropdownMenu.Item>
              <DropdownMenu.Item
                className={cn(itemClass, "justify-between")}
                onSelect={(event) => {
                  event.preventDefault();
                  setPanel("planner");
                }}
              >
                <span className="inline-flex min-w-0 items-center gap-2.5">
                  <Icon.Compose size={15} className="shrink-0 text-muted" />
                  <span className="min-w-0">
                    <span className="block leading-tight">Planner</span>
                    <span className="block text-[0.6rem] font-normal text-faint">
                      Designs the plan (Plan mode)
                    </span>
                  </span>
                </span>
                <span className="ml-3 inline-flex min-w-0 items-center gap-1.5 text-xs text-muted">
                  <span className="truncate">{plannerLabel}</span>
                  <Icon.ChevronRight size={13} className="shrink-0 text-faint" />
                </span>
              </DropdownMenu.Item>
              <DropdownMenu.Item
                className={cn(itemClass, "justify-between")}
                onSelect={(event) => {
                  event.preventDefault();
                  setPanel("effort");
                }}
              >
                <span className="inline-flex items-center gap-2.5">
                  <Icon.Gauge size={15} className="shrink-0 text-muted" />
                  <span>Reasoning effort</span>
                </span>
                <span className="ml-3 inline-flex items-center gap-1.5 text-xs text-muted">
                  {effortLabel(effort)}
                  <Icon.ChevronRight size={13} className="text-faint" />
                </span>
              </DropdownMenu.Item>
              <DropdownMenu.Separator className="my-1.5 h-px bg-line" />
              <DropdownMenu.Item className={itemClass} onSelect={goManageKeys}>
                <Icon.Settings size={15} className="shrink-0 text-muted" />
                <span className="flex-1">Models & keys</span>
                <span className="text-[0.62rem] text-faint">Connections</span>
              </DropdownMenu.Item>
            </>
          ) : null}

          {panel === "model" ? (
            <ModelChooser
              title="Executor"
              role="executor"
              autoHint="Picks the best ready model for the turn"
              catalog={catalog}
              models={models}
              pinnedId={executorPinId}
              onChoose={chooseExecutor}
              onBack={() => setPanel("root")}
              onManageKeys={goManageKeys}
              credentialsLocked={credentialsLocked}
            />
          ) : null}

          {panel === "planner" ? (
            <ModelChooser
              title="Planner"
              role="planner"
              autoHint="Auto — uses the best ready planning model"
              catalog={catalog}
              models={models}
              pinnedId={plannerPinId}
              onChoose={choosePlanner}
              onBack={() => setPanel("root")}
              onManageKeys={goManageKeys}
              credentialsLocked={credentialsLocked}
            />
          ) : null}

          {panel === "effort" ? (
            <>
              <DropdownMenu.Item
                className={itemClass}
                onSelect={(event) => {
                  event.preventDefault();
                  setPanel("root");
                }}
              >
                <Icon.Back size={14} className="text-faint" />
                Reasoning effort
              </DropdownMenu.Item>
              {EFFORT_OPTIONS.filter((opt) => {
                if (!selected?.reasoning_efforts || opt.value === "") return true;
                return selected.reasoning_efforts.includes(opt.value);
              }).map((opt) => (
                <DropdownMenu.Item
                  key={opt.value || "auto"}
                  className={itemClass}
                  onSelect={() => setPrefs({ reasoningEffort: opt.value })}
                >
                  <span className="flex w-4 shrink-0 justify-center text-accent">
                    {effort === opt.value ? <Icon.Check size={14} /> : null}
                  </span>
                  {opt.label}
                </DropdownMenu.Item>
              ))}
            </>
          ) : null}
        </DropdownMenu.Content>
      </DropdownMenu.Portal>
    </DropdownMenu.Root>
  );
}
