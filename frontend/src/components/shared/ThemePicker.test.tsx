import "@testing-library/jest-dom/vitest";
import { useState } from "react";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { THEMES, type ThemeMode } from "../../lib/themes";
import { ThemePicker } from "./ThemePicker";

afterEach(cleanup);

it("previews all palettes and exposes one selected radio", () => {
  const onChange = vi.fn();
  render(<ThemePicker value="midnight" onChange={onChange} />);
  expect(screen.getAllByRole("radio")).toHaveLength(THEMES.length);
  expect(screen.getByRole("radio", { name: "Midnight" })).toBeChecked();
  expect(screen.getByRole("radio", { name: "Midnight" })).toHaveAttribute("tabindex", "0");
  for (const theme of THEMES) {
    const radio = screen.getByRole("radio", { name: theme.label });
    expect(radio.querySelector("[data-theme]")).toHaveAttribute("data-theme", theme.id);
    fireEvent.click(radio);
    expect(onChange).toHaveBeenLastCalledWith(theme.id);
  }
});

it("supports arrows, Home and End while moving focus with the selected palette", () => {
  function Harness() {
    const [value, setValue] = useState<ThemeMode>("dark");
    return <ThemePicker value={value} onChange={setValue} />;
  }
  render(<Harness />);
  const dark = screen.getByRole("radio", { name: "Dark" });
  dark.focus();
  fireEvent.keyDown(dark, { key: "ArrowRight" });
  expect(screen.getByRole("radio", { name: "Light" })).toBeChecked();
  expect(screen.getByRole("radio", { name: "Light" })).toHaveFocus();
  fireEvent.keyDown(document.activeElement!, { key: "End" });
  expect(screen.getByRole("radio", { name: "Espresso" })).toBeChecked();
  fireEvent.keyDown(document.activeElement!, { key: "ArrowRight" });
  expect(dark).toBeChecked();
  fireEvent.keyDown(dark, { key: "ArrowLeft" });
  expect(screen.getByRole("radio", { name: "Espresso" })).toHaveFocus();
  fireEvent.keyDown(document.activeElement!, { key: "Home" });
  expect(dark).toBeChecked();
  expect(dark).toHaveFocus();
});
