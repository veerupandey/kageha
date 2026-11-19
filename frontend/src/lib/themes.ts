/** Saved palette IDs stay stable; color values live in styles/themes.css. */
export const THEMES = [
  { id: "dark", label: "Dark", description: "Charcoal & sky blue", scheme: "dark" },
  { id: "light", label: "Light", description: "Soft white & forest green", scheme: "light" },
  { id: "midnight", label: "Midnight", description: "Deep navy & ocean blue", scheme: "dark" },
  { id: "nord", label: "Nord", description: "Slate & arctic lavender", scheme: "dark" },
  { id: "sepia", label: "Sepia", description: "Warm paper & amber", scheme: "light" },
  { id: "rose", label: "Rose", description: "Pale blush & berry", scheme: "light" },
  { id: "cloud", label: "Cloud", description: "Soft ivory & graphite", scheme: "light" },
  { id: "teal", label: "Teal", description: "Deep ocean & sea glass", scheme: "dark" },
  { id: "espresso", label: "Espresso", description: "Cocoa & warm copper", scheme: "dark" },
] as const;

export type ThemeMode = (typeof THEMES)[number]["id"];

export function isTheme(value: unknown): value is ThemeMode {
  return THEMES.some((theme) => theme.id === value);
}

export function themeScheme(theme: ThemeMode): "dark" | "light" {
  return (THEMES.find((entry) => entry.id === theme) || THEMES[0]).scheme;
}
