import { useEffect, useLayoutEffect, useRef, useState } from "react";
import type { SlashCommand } from "../api/types";
import { cn } from "../lib/cn";
import {
  groupSlashItems,
  slashCommandTitle,
  slashCommandToken,
} from "../lib/slash";

function highlightMatch(text: string, query: string) {
  const q = query.trim();
  if (!q) return text;
  const i = text.toLowerCase().indexOf(q.toLowerCase());
  if (i < 0) return text;
  return (
    <>
      {text.slice(0, i)}
      <span className="ka-slash-hit">{text.slice(i, i + q.length)}</span>
      {text.slice(i + q.length)}
    </>
  );
}

export function SlashCommandPicker({
  items,
  query,
  activeIndex,
  onHover,
  onPick,
}: {
  items: SlashCommand[];
  query: string;
  activeIndex: number;
  onHover: (index: number) => void;
  onPick: (cmd: SlashCommand) => void;
}) {
  const groups = groupSlashItems(items);
  const activeRef = useRef<HTMLButtonElement>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const [placeBelow, setPlaceBelow] = useState(false);
  const [maxHeight, setMaxHeight] = useState<number | undefined>(undefined);

  useLayoutEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const parent = el.parentElement;
    if (!parent) return;

    const measure = () => {
      const parentRect = parent.getBoundingClientRect();
      const spaceAbove = parentRect.top;
      const spaceBelow = window.innerHeight - parentRect.bottom;

      // If headroom above is cramped (< 280px) and there is more room below, position downwards
      const shouldPlaceBelow = spaceAbove < 280 && spaceBelow > spaceAbove;
      setPlaceBelow(shouldPlaceBelow);

      const availableSpace = shouldPlaceBelow ? spaceBelow : spaceAbove;
      // Cap at 320px (20rem), keep 20px clearance from the viewport edge
      const clamped = Math.max(120, Math.min(320, availableSpace - 20));
      setMaxHeight(clamped);
    };

    measure();
    window.addEventListener("resize", measure);
    return () => window.removeEventListener("resize", measure);
  }, [items.length]);

  useEffect(() => {
    activeRef.current?.scrollIntoView({ block: "nearest" });
  }, [activeIndex]);

  return (
    <div
      ref={containerRef}
      className={cn("ka-slash-picker", placeBelow && "ka-slash-picker--below")}
      style={maxHeight ? { maxHeight: `${maxHeight}px` } : undefined}
      id="slash-picker"
      role="listbox"
      aria-label="Slash commands"
    >
      <div className="ka-slash-scroller">
        {items.length === 0 ? (
          <p className="ka-slash-empty">No commands match /{query}</p>
        ) : (
          groups.map((group) => {
            return (
              <div key={group.group} className="ka-slash-group">
                <p className="ka-slash-group-label">{group.group}</p>
                {group.items.map((cmd) => {
                  const index = items.indexOf(cmd);
                  const active = index === activeIndex;
                  return (
                    <button
                      key={cmd.id}
                      ref={active ? activeRef : undefined}
                      type="button"
                      role="option"
                      aria-selected={active}
                      className={cn("ka-slash-item", active && "is-active")}
                      onMouseEnter={() => onHover(index)}
                      onMouseDown={(event) => {
                        event.preventDefault();
                        onPick(cmd);
                      }}
                    >
                      <code className="ka-slash-token">
                        {highlightMatch(slashCommandToken(cmd), query)}
                      </code>
                      <span className="ka-slash-item-body">
                        <span className="ka-slash-item-title">
                          {highlightMatch(slashCommandTitle(cmd), query)}
                        </span>
                        {cmd.description ? (
                          <span className="ka-slash-item-desc">{cmd.description}</span>
                        ) : null}
                      </span>
                      {active ? <kbd className="ka-slash-kbd">↵</kbd> : null}
                    </button>
                  );
                })}
              </div>
            );
          })
        )}
      </div>
      <p className="ka-slash-hint">↑↓ to move · ↵ to select · esc to close</p>
    </div>
  );
}
