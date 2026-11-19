import { cn } from "../../lib/cn";

interface AgentAvatarProps {
  avatar?: string | null;
  name: string;
  size?: "sm" | "md" | "lg";
  className?: string;
}

const SIZE = {
  sm: "h-5 w-5 text-[0.7rem] rounded-md shadow-2xs",
  md: "h-7 w-7 text-sm rounded-lg shadow-xs",
  lg: "h-10 w-10 text-lg rounded-xl shadow-sm",
} as const;

const PALETTES = [
  "bg-indigo-500/15 text-indigo-400 border-indigo-500/25",
  "bg-sky-500/15 text-sky-400 border-sky-500/25",
  "bg-emerald-500/15 text-emerald-400 border-emerald-500/25",
  "bg-amber-500/15 text-amber-400 border-amber-500/25",
  "bg-rose-500/15 text-rose-400 border-rose-500/25",
  "bg-purple-500/15 text-purple-400 border-purple-500/25",
  "bg-teal-500/15 text-teal-400 border-teal-500/25",
];

function pickPalette(name: string): string {
  let hash = 0;
  for (let i = 0; i < name.length; i++) hash = (hash << 5) - hash + name.charCodeAt(i);
  return PALETTES[Math.abs(hash) % PALETTES.length];
}

const EMOJI_REGEX = /\p{Extended_Pictographic}/u;

/** Consistent letter/emoji tile for agent lists and the command-center hero. */
export function AgentAvatar({ avatar, name, size = "sm", className }: AgentAvatarProps) {
  const glyph = (avatar || "").trim() || (name.trim().slice(0, 1).toUpperCase() || "A");
  const isEmoji = EMOJI_REGEX.test(glyph);
  const palette = isEmoji ? "bg-[var(--color-line)]/50 border-line/40 text-ink" : pickPalette(name || glyph);

  return (
    <span
      className={cn(
        "inline-flex shrink-0 items-center justify-center font-semibold leading-none border transition-transform duration-150",
        palette,
        SIZE[size],
        className,
      )}
      aria-hidden="true"
    >
      {glyph}
    </span>
  );
}
