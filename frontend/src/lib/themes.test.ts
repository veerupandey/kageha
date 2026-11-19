import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { THEMES } from "./themes";

const css = readFileSync("src/styles/themes.css", "utf8");

function tokens(selector: string): Record<string, string> {
  const start = css.indexOf(selector);
  if (start < 0) throw new Error(`Missing theme selector ${selector}`);
  const body = css.slice(css.indexOf("{", start) + 1, css.indexOf("}", start));
  return Object.fromEntries([...body.matchAll(/--([\w-]+):\s*([^;]+);/g)].map((match) => [match[1], match[2].trim()]));
}

function luminance(hex: string): number {
  if (!/^#[\da-f]{6}$/i.test(hex)) throw new Error(`Expected a theme color, received ${hex}`);
  const values = [1, 3, 5].map((offset) => parseInt(hex.slice(offset, offset + 2), 16) / 255)
    .map((value) => value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4);
  return values[0] * 0.2126 + values[1] * 0.7152 + values[2] * 0.0722;
}

function contrast(a: string, b: string): number {
  const [low, high] = [luminance(a), luminance(b)].sort((x, y) => x - y);
  return (high + 0.05) / (low + 0.05);
}

describe.each(THEMES)("$label palette", (theme) => {
  const palette = tokens(`[data-theme="${theme.id}"]`);
  const mode = tokens(`[data-color-scheme="${theme.scheme}"] {`);

  it("keeps primary, secondary, and small text readable on workspace surfaces", () => {
    for (const foreground of ["ink", "muted", "faint"]) {
      for (const background of ["canvas", "surface", "sidebar", "surface-hover", "sidebar-hover", "accent-soft"]) {
        expect(contrast(palette[`color-${foreground}`], palette[`color-${background}`]), `${foreground} on ${background}`).toBeGreaterThanOrEqual(4.5);
      }
    }
    expect(contrast(palette["color-accent"], palette["color-accent-soft"])).toBeGreaterThanOrEqual(4.5);
  });

  it("keeps syntax highlighting and semantic status text readable", () => {
    for (const key of ["code-keyword", "code-string", "code-number", "code-comment", "code-title", "code-built-in"]) {
      for (const background of ["canvas", "surface", "surface-hover"]) {
        expect(contrast(mode[key], palette[`color-${background}`]), `${key} on ${background}`).toBeGreaterThanOrEqual(4.5);
      }
    }
    for (const key of ["danger", "warn", "success"]) {
      expect(contrast(mode[`color-${key}`], mode[`color-${key}-soft`]), key).toBeGreaterThanOrEqual(4.5);
    }
  });
});
