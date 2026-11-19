import { useEffect, useMemo, useRef, useState } from "react";
import type { KeyboardEvent, RefObject } from "react";
import { useAppStore } from "../store";
import { filterSlashByCapabilities } from "../api/slashCatalog";
import { applySlashCommand, filterSlashCommands, getAtContext, getSlashContext } from "../lib/slash";
import type { SlashCommand } from "../api/types";
import { SlashCommandPicker } from "./SlashCommandPicker";

/** Autocomplete for the home and conversation composers. */
export function useComposerAutocomplete(
  inputRef: RefObject<HTMLTextAreaElement | null>,
  fileInputRef: RefObject<HTMLInputElement | null>,
  disabled: boolean,
) {
  const draft = useAppStore(s => s.draft);
  const setDraft = useAppStore(s => s.setDraft);
  const catalog = useAppStore(s => s.slashCatalog);
  const capabilities = useAppStore(s => s.capabilities);
  const personas = useAppStore(s => s.personas);
  const teams = useAppStore(s => s.teams);
  const activePersonaId = useAppStore(s => s.activePersonaId);
  const activeTeamId = useAppStore(s => s.activeTeamId);
  const chip = useAppStore(s => s.composerChip);
  const [caret, setCaret] = useState(draft.length);
  const [index, setIndex] = useState(0);
  const [dismissed, setDismissed] = useState(false);
  const activeRef = useRef<HTMLButtonElement>(null);
  const commands = useMemo(() => filterSlashByCapabilities(catalog, capabilities), [catalog, capabilities]);
  const slash = getSlashContext(draft, caret, commands);
  const at = getAtContext(draft, caret);
  // Teams use &team on the wire; also let users discover them with @.
  const team = getAtContext(draft, caret, "&");
  const mention = slash ? null : at || team;
  const sigil = mention ? draft[mention.start] : null;
  const query = (mention?.query || "").toLowerCase();
  const slashItems = slash ? filterSlashCommands(commands, slash.query) : [];
  const mentionItems = [
    ...(sigil === "@" ? personas.filter(p => [p.id, p.name, p.description || ""].some(v => v.toLowerCase().includes(query))).map(p => ({ ...p, kind: "agent" as const })) : []),
    ...teams.filter(t => [t.id, t.name, t.description || ""].some(v => v.toLowerCase().includes(query))).map(t => ({ ...t, kind: "team" as const })),
  ];
  const count = slash ? slashItems.length : mentionItems.length;
  const activeIndex = Math.min(index, Math.max(0, count - 1));
  const open = !disabled && !dismissed && Boolean(slash || mention);
  const contextKey = `${slash?.start ?? mention?.start}:${slash?.token ?? mention?.token}`;

  useEffect(() => { setIndex(0); setDismissed(false); }, [contextKey]);
  useEffect(() => { activeRef.current?.scrollIntoView({ block: "nearest" }); }, [activeIndex]);

  const syncCaret = () => {
    if (inputRef.current) setCaret(inputRef.current.selectionStart);
  };
  const focusAt = (pos: number) => {
    setCaret(pos);
    requestAnimationFrame(() => {
      inputRef.current?.focus();
      inputRef.current?.setSelectionRange(pos, pos);
    });
  };
  const pickSlash = (cmd: SlashCommand) => {
    if (!slash) return;
    const result = applySlashCommand(cmd, { start: slash.start, end: slash.end });
    setDismissed(true);
    if (result === "attach") fileInputRef.current?.click();
    if (result === "focus-model") document.getElementById("composer-model-menu")?.click();
    else focusAt(useAppStore.getState().draft.length);
  };
  const pickMention = (item: (typeof mentionItems)[number]) => {
    if (!mention) return;
    const store = useAppStore.getState();
    if (item.kind === "agent") {
      store.setActivePersonaId(item.id);
      store.setActiveTeamId(null);
      const insert = `@${item.id} `;
      setDraft(draft.slice(0, mention.start) + insert + draft.slice(mention.end));
      focusAt(mention.start + insert.length);
    } else {
      // Keep the native prefix for backend team context/coordinator routing.
      store.setActivePersonaId("main");
      store.setActiveTeamId(item.id);
      const insert = `&${item.id} `;
      setDraft(draft.slice(0, mention.start) + insert + draft.slice(mention.end));
      focusAt(mention.start + insert.length);
    }
    setDismissed(true);
  };
  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (!open || event.nativeEvent.isComposing) return false;
    if (event.key === "Escape") {
      event.preventDefault();
      setDismissed(true);
      return true;
    }
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      setIndex(Math.max(0, Math.min(activeIndex + (event.key === "ArrowDown" ? 1 : -1), count - 1)));
      return true;
    }
    if (count && (event.key === "Tab" || event.key === "Enter") && !event.shiftKey && !event.altKey && !event.ctrlKey && !event.metaKey) {
      event.preventDefault();
      if (slash) pickSlash(slashItems[activeIndex]);
      else pickMention(mentionItems[activeIndex]);
      return true;
    }
    return false;
  };

  const selection = activeTeamId ? teams.find(t => t.id === activeTeamId) : personas.find(p => p.id === activePersonaId && !p.builtin);
  const badge = chip.kind === "mode" && chip.value ? `/${chip.value}` : chip.kind === "multitask" ? "/new" : selection?.name;
  const isTightAbove = Boolean(inputRef.current && inputRef.current.getBoundingClientRect().top < 260);
  const picker = open && slash ? (
    <SlashCommandPicker items={slashItems} query={slash.query} activeIndex={activeIndex} onHover={setIndex} onPick={pickSlash} />
  ) : open && mention ? (
    <div id="mention-picker" role="listbox" aria-label="Agents and teams" className={`absolute left-0 right-0 z-20 max-h-56 overflow-auto rounded-lg border border-line bg-surface p-1 shadow-lg ${isTightAbove ? "top-full mt-1" : "bottom-full mb-1"}`}>
      {!mentionItems.length && <p className="px-2 py-2 text-sm text-muted">No agents or teams match {sigil}{query}</p>}
      {mentionItems.map((item, i) => (
        <button key={`${item.kind}:${item.id}`} ref={i === activeIndex ? activeRef : undefined} type="button" role="option" aria-selected={i === activeIndex}
          className={`flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-sm ${i === activeIndex ? "bg-accent-soft" : ""}`}
          onMouseEnter={() => setIndex(i)} onMouseDown={e => { e.preventDefault(); pickMention(item); }}>
          <span className="min-w-0 flex-1"><span className="block font-medium">{item.name}</span><span className="block truncate text-xs text-muted">{item.description || `${item.kind === "team" ? "&" : "@"}${item.id}`}</span></span>
          <span className="text-xs text-muted">{item.kind === "team" ? "Team" : "Agent"}</span>
        </button>
      ))}
    </div>
  ) : null;
  const badgeElement = badge ? (
    <span className="ka-composer-chip mb-2" data-testid="composer-selection">
      {badge}<button type="button" aria-label={`Remove ${badge}`} onClick={() => {
        const store = useAppStore.getState();
        if (chip.kind) store.clearComposerChip({ resetMode: chip.kind === "mode" });
        else {
          if (activePersonaId && draft.startsWith(`@${activePersonaId} `)) setDraft(draft.slice(activePersonaId.length + 2));
          if (activeTeamId && draft.startsWith(`&${activeTeamId} `)) setDraft(draft.slice(activeTeamId.length + 2));
          store.setActiveTeamId(null);
          store.setActivePersonaId("main");
        }
        inputRef.current?.focus();
      }}>×</button>
    </span>
  ) : null;
  return { picker, badgeElement, onKeyDown, syncCaret, setCaret, open, listboxId: slash ? "slash-picker" : "mention-picker" };
}
