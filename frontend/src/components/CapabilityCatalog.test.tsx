import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { CapabilityCatalog } from "./CapabilityCatalog";
import { MediaSettings } from "./MediaSettings";

const fixture = vi.hoisted(() => ({ setPrefs: vi.fn() }));
vi.mock("./VoiceCloningSettings", () => ({ VoiceCloningSettings: () => null }));
vi.mock("../store", () => ({ useAppStore: (select: (value: unknown) => unknown) => select({ prefs: {}, setPrefs: fixture.setPrefs }) }));
vi.mock("../api/client", () => ({ api: vi.fn(async (path: string) => path === "/api/capabilities" ? {
  providers: [{ id: "openai", ready: true, capabilities: ["web_search", "image_generation", "image_edit"] }],
} : { ok: true, providers: { image: [{ id: "openai", protocol: "openai_images", ready: true, default_model: "gpt-image-2.5-flare", models: ["gpt-image-2.5-flare"] }], tts: [], stt: [] } }) }));
afterEach(() => cleanup());

it("shows shared capabilities from the connected provider catalog", async () => {
  render(<CapabilityCatalog />);
  expect(await screen.findByText("Connected capabilities")).toBeInTheDocument();
  expect(screen.getByText("Grounded search")).toBeInTheDocument();
  expect(screen.getByText("Image generation")).toBeInTheDocument();
  expect(screen.getByText("Image editing")).toBeInTheDocument();
});

it("selects OpenAI images using the existing connection", async () => {
  render(<MediaSettings />);
  fireEvent.click(await screen.findByRole("radio", { name: /OpenAI · GPT Image/ }));
  expect(fixture.setPrefs).toHaveBeenCalledWith({ mediaModel: "openai" });
  expect(screen.getByText(/reuse your OpenAI connection/)).toBeInTheDocument();
});
