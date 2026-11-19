import * as DropdownMenu from "@radix-ui/react-dropdown-menu";
import { cn } from "../lib/cn";
import { Icon } from "../lib/icons";
import { MEDIA_MODEL_OPTIONS, mediaModelLabel, type MediaModelId } from "../lib/mediaModel";
import { useAppStore } from "../store";

const itemClass = "flex cursor-pointer items-center gap-2 rounded-xl px-2.5 py-2 text-sm outline-none data-[highlighted]:bg-accent-soft data-[disabled]:cursor-not-allowed data-[disabled]:opacity-45";

export function ComposerMediaMenu() {
  const selected = useAppStore((state) => state.prefs.mediaModel || "auto");
  const setPrefs = useAppStore((state) => state.setPrefs);
  const showToast = useAppStore((state) => state.showToast);
  const openSettings = useAppStore((state) => state.openSettings);
  const current = MEDIA_MODEL_OPTIONS.find((option) => option.id === selected) || MEDIA_MODEL_OPTIONS[0];

  return (
    <DropdownMenu.Root>
      <DropdownMenu.Trigger asChild>
        <button
          type="button"
          className="inline-flex h-7 max-w-[12rem] items-center gap-1 rounded-full border border-line bg-surface/80 px-2 text-xs font-medium text-ink shadow-sm hover:border-line-strong hover:bg-line/40"
          title={`Media model: ${current.label}`}
          aria-label="Media model"
        >
          <Icon.Logo size={13} className="shrink-0 text-muted" />
          <span className="min-w-0 truncate">{mediaModelLabel(selected)}</span>
          <Icon.Chevron size={12} className="shrink-0 text-faint" />
        </button>
      </DropdownMenu.Trigger>
      <DropdownMenu.Portal>
        <DropdownMenu.Content
          className="ka-composer-menu z-50 w-[min(22rem,calc(100vw-2rem))] rounded-2xl border border-line bg-surface p-1.5 shadow-[0_18px_48px_rgba(0,0,0,0.28)]"
          side="top"
          align="start"
          sideOffset={8}
          collisionPadding={16}
        >
          <div className="px-2.5 pb-1.5 pt-1 text-[0.65rem] font-semibold uppercase tracking-[0.12em] text-faint">
            Media model
          </div>
          {MEDIA_MODEL_OPTIONS.map((option) => {
            const isSelected = option.id === selected;
            return (
              <DropdownMenu.Item
                key={option.id}
                className={cn(itemClass, isSelected && "bg-accent-soft")}
                disabled={!option.available}
                title={option.available ? option.detail : `${option.detail} — not connected yet`}
                onSelect={() => {
                  if (!option.available) {
                    showToast(`${option.label} is not connected yet`);
                    return;
                  }
                  setPrefs({ mediaModel: option.id as MediaModelId });
                }}
              >
                <Icon.Logo size={14} className={cn("shrink-0", isSelected ? "text-accent" : "text-muted")} />
                <span className="min-w-0 flex-1">
                  <span className="block truncate font-medium">{option.label}</span>
                  <span className="block truncate text-[0.68rem] text-faint">{option.detail}</span>
                </span>
                <span className="hidden shrink-0 gap-1 text-[0.58rem] uppercase tracking-wide text-faint sm:flex">
                  {option.capabilities.slice(0, 2).map((capability) => <span key={capability}>{capability}</span>)}
                </span>
                {isSelected ? <Icon.Check size={14} className="shrink-0 text-accent" /> : null}
              </DropdownMenu.Item>
            );
          })}
          <DropdownMenu.Separator className="my-1.5 h-px bg-line" />
          <div className="px-2.5 pb-1 pt-0.5 text-[0.65rem] leading-relaxed text-faint">
            The selected provider is recorded with the session. Auto follows model capability.
          </div>
          <DropdownMenu.Separator className="my-1.5 h-px bg-line" />
          <DropdownMenu.Item className={itemClass} onSelect={() => openSettings("media")}>
            <Icon.Settings size={14} className="shrink-0 text-muted" />
            <span className="flex-1">Media settings</span>
            <span className="text-[0.62rem] text-faint">Defaults · connections</span>
          </DropdownMenu.Item>
        </DropdownMenu.Content>
      </DropdownMenu.Portal>
    </DropdownMenu.Root>
  );
}
