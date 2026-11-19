import { useRef } from "react";
import { cn } from "../../lib/cn";
import { Icon } from "../../lib/icons";
import { THEMES, type ThemeMode } from "../../lib/themes";

export function ThemePicker({ value, onChange }: {
  value: ThemeMode;
  onChange: (theme: ThemeMode) => void;
}) {
  const groupRef = useRef<HTMLDivElement>(null);
  return (
    <section>
      <h3 className="text-sm font-semibold text-ink">Theme</h3>
      <p className="mt-0.5 text-[0.7rem] text-muted">Find your focus. Colors update everywhere, instantly.</p>
      <div ref={groupRef} role="radiogroup" aria-label="Color theme" className="mt-3 grid grid-cols-2 gap-3 sm:grid-cols-3">
        {THEMES.map((theme, index) => (
          <button
            key={theme.id} type="button" role="radio" aria-label={theme.label}
            aria-checked={value === theme.id} aria-describedby={`theme-${theme.id}-description`}
            tabIndex={value === theme.id ? 0 : -1}
            className={cn(
              "group min-w-0 rounded-xl border p-2 text-left transition-all duration-150 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent",
              value === theme.id
                ? "border-accent bg-accent-soft shadow-xs ring-1 ring-accent/30"
                : "border-line bg-surface hover:border-line-strong hover:bg-surface-hover hover:-translate-y-0.5",
            )}
            onClick={() => onChange(theme.id)}
            onKeyDown={(event) => {
              if (!["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown", "Home", "End"].includes(event.key)) return;
              event.preventDefault();
              const next = event.key === "Home" ? 0 : event.key === "End" ? THEMES.length - 1
                : (index + (["ArrowLeft", "ArrowUp"].includes(event.key) ? -1 : 1) + THEMES.length) % THEMES.length;
              onChange(THEMES[next].id);
              groupRef.current?.querySelectorAll<HTMLButtonElement>("[role=radio]")[next]?.focus();
            }}
          >
            <span data-theme={theme.id} data-color-scheme={theme.scheme} aria-hidden="true" className="flex h-20 overflow-hidden rounded-lg border border-line bg-canvas">
              <span className="flex w-[28%] flex-col gap-1.5 border-r border-line bg-sidebar px-2 py-3">
                <span className="mb-1 h-1 w-3/4 rounded-full bg-ink/75" />
                <span className="h-1 rounded-full bg-faint/40" />
                <span className="h-1 w-3/4 rounded-full bg-faint/40" />
                <span className="mt-1 h-3 rounded bg-accent-soft" />
              </span>
              <span className="flex min-w-0 flex-1 flex-col gap-2 p-2.5">
                <span className="ml-auto h-4 w-3/4 rounded-md bg-accent-soft" />
                <span className="h-1 w-5/6 rounded-full bg-ink/70" />
                <span className="h-1 w-2/3 rounded-full bg-muted/60" />
                <span className="mt-auto flex h-3 items-center justify-end rounded border border-line bg-surface px-1">
                  <span className="h-1.5 w-1.5 rounded-full bg-accent" />
                </span>
              </span>
            </span>
            <span className="mt-2 flex items-center justify-between gap-1 px-0.5">
              <span className="text-xs font-medium text-ink">{theme.label}</span>
              {value === theme.id && <Icon.Check size={14} className="shrink-0 text-accent" />}
            </span>
            <span id={`theme-${theme.id}-description`} className="mt-0.5 block px-0.5 text-[0.6rem] leading-snug text-muted">{theme.description}</span>
          </button>
        ))}
      </div>
    </section>
  );
}
