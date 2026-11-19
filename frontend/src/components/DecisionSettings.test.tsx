import "@testing-library/jest-dom/vitest";
import { afterEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { DecisionSettings } from "./DecisionSettings";

const mocks = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock("../api/client", () => ({ api: mocks.api }));
afterEach(() => { cleanup(); vi.clearAllMocks(); });
const inventory = () => ({
  browser_action_inheritance: true,
  policy: { mode: "shadow", model: "cloudflare/clef-flash", fallback_models: [], judge_fallback: true,
    threshold: 0.9, timeout_s: 3, overrides: {} },
  purposes: ["intake", "observation_intent"],
  models: [{ id: "cloudflare/clef-flash", label: "Clef-flash", ready: true, reason: "", decision_backend: true }, { id: "cloudflare/clef", label: "Clef", ready: true, reason: "", decision_backend: true }],
  codex: { supported: false, reason: "No dedicated Codex decision operation" },
});

it("switches models and saves per-purpose overrides and fallback controls", async () => {
  let saved: unknown;
  mocks.api.mockImplementation(async (_path: string, options?: { body: string }) => {
    if (!options) return inventory();
    saved = JSON.parse(options.body);
    return { policy: saved, message: "Saved for new runs" };
  });
  render(<DecisionSettings />);
  await screen.findByRole("radiogroup", { name: "Decision mode" });
  fireEvent.click(screen.getByRole("radio", { name: /Use decisions/ }));
  fireEvent.click(screen.getByRole("radio", { name: /^Clef Configured/ }));
  fireEvent.change(screen.getByLabelText("Decision fallback order"), { target: { value: "cloudflare/clef-flash" } });
  fireEvent.change(screen.getByLabelText("intake decision mode"), { target: { value: "shadow" } });
  fireEvent.change(screen.getByLabelText("observation_intent decision model"), { target: { value: "cloudflare/clef-flash" } });
  fireEvent.click(screen.getByRole("button", { name: "Save decision settings" }));
  expect(await screen.findByRole("status")).toHaveTextContent("Settings saved");
  expect(saved).toMatchObject({ mode: "active", model: "cloudflare/clef", fallback_models: ["cloudflare/clef-flash"],
    overrides: { intake: { mode: "shadow" }, observation_intent: { model: "cloudflare/clef-flash" } } });
});

it("disables models without credentials and shows the setup requirement", async () => {
  const value = inventory();
  value.models[1] = { id: "cloudflare/clef", label: "Clef", ready: false, reason: "CLOUDFLARE_API_TOKEN missing", decision_backend: true };
  mocks.api.mockResolvedValue(value);
  render(<DecisionSettings />);
  await screen.findByRole("radiogroup", { name: "Default decision model" });
  expect(screen.getByRole("radio", { name: /^Clef Setup required/ })).toBeDisabled();
  expect(screen.getByText(/CLOUDFLARE_API_TOKEN missing/)).toBeInTheDocument();
});

it("shows save errors without claiming success", async () => {
  mocks.api.mockResolvedValueOnce(inventory()).mockRejectedValueOnce(new Error("save failed"));
  render(<DecisionSettings />);
  await screen.findByRole("radiogroup", { name: "Decision mode" });
  fireEvent.click(screen.getByRole("radio", { name: /Use decisions/ }));
  fireEvent.click(screen.getByRole("button", { name: "Save decision settings" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("save failed");
});


it("explains each mode, tracks unsaved edits, and discards them", async () => {
  mocks.api.mockResolvedValue(inventory());
  render(<DecisionSettings />);
  await screen.findByRole("radiogroup", { name: "Decision mode" });
  expect(screen.getByText(/Results are logged for comparison/)).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Save decision settings" })).toBeDisabled();
  fireEvent.click(screen.getByRole("radio", { name: /Use decisions/ }));
  expect(screen.getByText(/Eligible browser actions can run directly/)).toBeInTheDocument();
  expect(screen.getByRole("status")).toHaveTextContent("Unsaved changes");
  fireEvent.click(screen.getByRole("button", { name: "Discard" }));
  expect(screen.getByRole("radio", { name: /Compare/ })).toHaveAttribute("aria-checked", "true");
  fireEvent.click(screen.getByRole("radio", { name: /Off/ }));
  expect(screen.getByText(/Tasks that inherit this mode/)).toBeInTheDocument();
});

it("prevents saving invalid reliability limits", async () => {
  mocks.api.mockResolvedValue(inventory());
  render(<DecisionSettings />);
  await screen.findByRole("radiogroup", { name: "Decision mode" });
  fireEvent.change(screen.getByLabelText("Decision probability threshold"), { target: { value: "0.2" } });
  expect(screen.getByRole("button", { name: "Save decision settings" })).toBeDisabled();
  fireEvent.change(screen.getByLabelText("Decision probability threshold"), { target: { value: "0.9" } });
  fireEvent.change(screen.getByLabelText("Decision timeout"), { target: { value: "20" } });
  expect(screen.getByRole("button", { name: "Save decision settings" })).toBeDisabled();
});

it("shows a load error without editable settings", async () => {
  mocks.api.mockRejectedValue(new Error("Cannot connect"));
  render(<DecisionSettings />);
  expect(await screen.findByRole("alert")).toHaveTextContent("Cannot connect");
  expect(screen.queryByRole("button", { name: "Save decision settings" })).not.toBeInTheDocument();
});

it("supports arrow-key navigation between mode cards", async () => {
  mocks.api.mockResolvedValue(inventory());
  render(<DecisionSettings />);
  const compare = await screen.findByRole("radio", { name: /Compare/ });
  compare.focus();
  fireEvent.keyDown(compare, { key: "ArrowRight" });
  expect(screen.getByRole("radio", { name: /Use decisions/ })).toHaveFocus();
  expect(screen.getByRole("radio", { name: /Use decisions/ })).toHaveAttribute("aria-checked", "true");
});

it("exposes all automation purposes with readable labels and independent overrides", async () => {
  const data = { ...inventory(), purposes: ["intake", "observation_intent", "skill_selection", "tool_selection", "memory_relevance", "context_retention", "model_routing", "recovery", "notification_triage", "resume_continuity", "verification", "browser_guidance", "desktop_guidance"] };
  let saved: unknown;
  mocks.api.mockImplementation(async (_path: string, options?: { body: string }) => {
    if (!options) return data;
    saved = JSON.parse(options.body);
    return { policy: saved };
  });
  render(<DecisionSettings />);
  await screen.findByRole("radiogroup", { name: "Decision mode" });
  fireEvent.click(screen.getByText(/Customize by task/));
  expect(screen.getByText("Skill selection")).toBeInTheDocument();
  expect(screen.getByText("Resume unfinished work")).toBeInTheDocument();
  expect(screen.getByText("Automatic model routing")).toBeInTheDocument();
  expect(screen.getByText("Verification needed?", { exact: true })).toBeInTheDocument();
  expect(screen.getByText("Browser routing")).toBeInTheDocument();
  expect(screen.getByText("Mac desktop routing")).toBeInTheDocument();
  expect(screen.getByText("Notification priority")).toBeInTheDocument();
  for (const purpose of data.purposes) {
    fireEvent.change(screen.getByLabelText(`${purpose} decision mode`), { target: { value: purpose === "model_routing" ? "off" : "active" } });
  }
  fireEvent.click(screen.getByRole("button", { name: "Save decision settings" }));
  await screen.findByText("Settings saved. Applies to new runs.");
  expect(saved).toMatchObject({ overrides: { model_routing: { mode: "off" }, notification_triage: { mode: "active" }, verification: { mode: "active" } } });
});

it("saves numeric per-task limits and restores inheritance when cleared", async () => {
  let saved: unknown;
  mocks.api.mockImplementation(async (_path: string, options?: { body: string }) => {
    if (!options) return inventory();
    saved = JSON.parse(options.body);
    return { policy: saved };
  });
  render(<DecisionSettings />);
  await screen.findByRole("radiogroup", { name: "Decision mode" });
  fireEvent.change(screen.getByLabelText("intake probability threshold"), { target: { value: "0.8" } });
  fireEvent.change(screen.getByLabelText("intake timeout"), { target: { value: "2.5" } });
  fireEvent.change(screen.getByLabelText("observation_intent probability threshold"), { target: { value: "0.2" } });
  expect(screen.getByRole("button", { name: "Save decision settings" })).toBeDisabled();
  fireEvent.change(screen.getByLabelText("observation_intent probability threshold"), { target: { value: "" } });
  fireEvent.click(screen.getByRole("button", { name: "Save decision settings" }));
  await screen.findByText("Settings saved. Applies to new runs.");
  expect(saved).toMatchObject({ threshold: .9, timeout_s: 3,
    overrides: { intake: { threshold: .8, timeout_s: 2.5 } } });
  expect((saved as { overrides: object }).overrides).not.toHaveProperty("observation_intent");
});

it("excludes chat models from default, task and backup decision pickers", async () => {
  const data = inventory();
  data.models.push(
    { id: "local/research", label: "Research chat", ready: true, reason: "", decision_backend: false },
    { id: "local/review", label: "Review chat", ready: true, reason: "", decision_backend: false },
    { id: "local/decision", label: "Custom decision", ready: true, reason: "", decision_backend: true },
  );
  mocks.api.mockResolvedValue(data);
  render(<DecisionSettings />);
  await screen.findByRole("radiogroup", { name: "Default decision model" });
  expect(screen.queryByText("Other configured models")).not.toBeInTheDocument();
  expect(screen.queryByLabelText("Other default model")).not.toBeInTheDocument();
  expect(screen.getByRole("radio", { name: "Custom decision Configured" })).toBeEnabled();
  for (const name of ["intake decision model", "observation_intent decision model", "Decision fallback order"]) {
    const picker = screen.getByLabelText(name) as HTMLSelectElement;
    const ids = Array.from(picker.options, option => option.value);
    expect(ids).not.toContain("local/research");
    expect(ids).not.toContain("local/review");
    expect(ids).toContain("local/decision");
  }
});

it("keeps browser review opt-in when guidance is active and saves its state budget", async () => {
  let saved: unknown;
  mocks.api.mockImplementation(async (_path: string, options?: { body: string }) => {
    if (!options) return { ...inventory(), policy: { ...inventory().policy, mode: "active" }, purposes: ["browser_guidance", "browser_review"] };
    saved = JSON.parse(options.body);
    return { policy: saved, message: "Saved for new runs" };
  });
  render(<DecisionSettings />);
  const review = await screen.findByLabelText("browser_review decision mode");
  expect(review).toHaveValue("off");
  expect(screen.queryByLabelText("browser_review probability threshold")).not.toBeInTheDocument();
  fireEvent.change(screen.getByLabelText("Browser decision state budget"), { target: { value: "5" } });
  fireEvent.click(screen.getByRole("button", { name: "Save decision settings" }));
  expect(await screen.findByRole("status")).toHaveTextContent("Settings saved");
  expect(saved).toMatchObject({ mode: "active", max_states: 5, overrides: {} });
});

it("requires explicit opt-in for decision-first actions and saves their budget", async () => {
  let saved: unknown;
  mocks.api.mockImplementation(async (_path: string, options?: { body: string }) => {
    if (!options) return { ...inventory(), policy: { ...inventory().policy, mode: "active" }, purposes: ["browser_action"] };
    saved = JSON.parse(options.body);
    return { policy: saved, message: "Saved" };
  });
  render(<DecisionSettings />);
  const mode = await screen.findByLabelText("browser_action decision mode");
  expect(mode).toHaveValue("off");
  fireEvent.change(mode, { target: { value: "active" } });
  fireEvent.change(screen.getByLabelText("Browser action decision budget"), { target: { value: "0" } });
  expect(screen.getByRole("button", { name: "Save decision settings" })).toBeDisabled();
  fireEvent.change(screen.getByLabelText("Browser action decision budget"), { target: { value: "32" } });
  fireEvent.click(screen.getByRole("button", { name: "Save decision settings" }));
  expect(await screen.findByRole("status")).toHaveTextContent("Settings saved");
  expect(saved).toMatchObject({max_action_decisions:32,overrides:{browser_action:{mode:"active"}}});
});

it("separates typed action backends from heavy chat models", async () => {
  mocks.api.mockResolvedValue({ ...inventory(), purposes: ["browser_action"], models: [
    { id: "cloudflare/clef-flash", label: "Clef-flash", ready: true, reason: "", decision_backend: true },
    { id: "openai/gpt-heavy", ready: true, reason: "", decision_backend: false },
  ] });
  render(<DecisionSettings />);
  const select = await screen.findByLabelText("browser_action decision model") as HTMLSelectElement;
  expect(Array.from(select.options).map(o=>o.value)).toContain("cloudflare/clef-flash");
  expect(Array.from(select.options).map(o=>o.value)).not.toContain("openai/gpt-heavy");
  expect(screen.getByLabelText("Browser decision batch size")).toHaveValue(8);
});

it("validates and saves a separate batch cutoff without changing the single action cutoff", async () => {
  let saved: unknown;
  mocks.api.mockImplementation(async (_path: string, options?: { body: string }) => {
    if (!options) return { ...inventory(), purposes: ["browser_action"] };
    saved = JSON.parse(options.body); return { policy: saved, message: "Saved" };
  });
  render(<DecisionSettings />);
  const batch = await screen.findByLabelText("Browser batch probability threshold");
  expect(batch).toHaveValue(0.99);
  fireEvent.change(batch, { target: { value: "0.49" } });
  expect(screen.getByRole("button", { name: "Save decision settings" })).toBeDisabled();
  fireEvent.change(batch, { target: { value: "0.995" } });
  fireEvent.change(screen.getByLabelText("browser_action probability threshold"), { target: { value: "0.95" } });
  fireEvent.click(screen.getByRole("button", { name: "Save decision settings" }));
  expect(await screen.findByRole("status")).toHaveTextContent("Settings saved");
  expect(saved).toMatchObject({threshold:0.9,batch_threshold:0.995,overrides:{browser_action:{threshold:0.95}}});
});

it("defaults new browser opt-ins to selected probability and accepts explicit confidence", async () => {
  let saved: unknown;
  mocks.api.mockImplementation(async (_path: string, options?: { body: string }) => {
    if (!options) return { ...inventory(), purposes: ["browser_action"], models: [{id:"openai-decisions/gpt-6-luna",ready:true,reason:"",decision_backend:true}] };
    saved=JSON.parse(options.body);return {policy:saved,message:"Saved"};
  });
  render(<DecisionSettings />);
  const mode=await screen.findByLabelText("browser_action decision mode");
  fireEvent.change(mode,{target:{value:"active"}});
  expect(screen.getByLabelText("Browser action confidence cutoff")).toHaveValue(null);
  expect(screen.getByLabelText("browser_action probability threshold")).toBeEnabled();
  expect(screen.getByLabelText("Browser batch probability threshold")).toHaveValue(0.99);
  fireEvent.change(screen.getByLabelText("Browser action confidence cutoff"),{target:{value:"1.1"}});
  expect(screen.getByRole("button",{name:"Save decision settings"})).toBeDisabled();
  fireEvent.change(screen.getByLabelText("Browser action confidence cutoff"),{target:{value:"0.93"}});
  fireEvent.change(screen.getByLabelText("browser_action decision model"),{target:{value:"openai-decisions/gpt-6-luna"}});
  fireEvent.click(screen.getByRole("button",{name:"Save decision settings"}));
  expect(await screen.findByRole("status")).toHaveTextContent("Settings saved");
  expect(saved).toMatchObject({overrides:{browser_action:{mode:"active",model:"openai-decisions/gpt-6-luna",confidence_cutoff:0.93}}});
});

 it("requires a connected typed model before enabling browser actions", async () => {
   mocks.api.mockResolvedValue({ ...inventory(), purposes: ["browser_action"],
     policy: { ...inventory().policy, model: "chat/heavy" },
     models: [{ id: "chat/heavy", ready: true, decision_backend: false }] });
   render(<DecisionSettings />);
   fireEvent.change(await screen.findByLabelText("browser_action decision mode"), { target: { value: "active" } });
   expect(screen.getByRole("button", { name: "Save decision settings" })).toBeDisabled();
   expect(screen.getByRole("status")).toHaveTextContent("Choose a connected typed model");
 });

it("shows the starter catalog and keeps the rolling alias and chat models out of browser choices", async () => {
  const labels = ["Clef", "Clef-flash", "TypeSafe Jev 1.14.0", "GPT-6 Luna Decisions", "Gemini Decision 1"];
  const typed = labels.map((label, i) => ({ id: `typed/${i}`, label, decision_backend: true, ready: true }));
  mocks.api.mockResolvedValue({ ...inventory(), purposes: ["browser_action"], models: [
    ...typed, { id: "rolling", label: "Rolling Jev", alias: true, decision_backend: true, ready: true },
    { id: "chat/luna", label: "Luna chat", decision_backend: false, ready: true }
  ] });
  render(<DecisionSettings />);
  const picker = await screen.findByRole("radiogroup", { name: "Default decision model" });
  expect(picker.querySelectorAll('[role="radio"]')).toHaveLength(5);
  const choices = screen.getByLabelText("browser_action decision model") as HTMLSelectElement;
  expect(Array.from(choices.options, o => o.text)).toEqual(["Inherit model", ...labels]);
  expect(screen.getByLabelText("browser_action decision mode")).toBeVisible();
});

it("keeps an existing advisory policy off for actions until an explicit opt-in", async () => {
  let saved: unknown;
  mocks.api.mockImplementation(async (_path: string, options?: { body: string }) => {
    if (!options) return { ...inventory(), policy: { ...inventory().policy, mode: "active" },
      purposes: ["browser_action", "browser_guidance", "recovery"] };
    saved = JSON.parse(options.body);
    return { policy: saved };
  });
  render(<DecisionSettings />);
  expect(await screen.findByText("Effective browser mode · Off")).toBeVisible();
  fireEvent.click(screen.getByRole("button", { name: "Use the default mode for browser actions" }));
  expect(screen.getByText("Effective browser mode · Direct execution")).toBeVisible();
  expect(screen.getByLabelText("browser_guidance decision mode")).toHaveDisplayValue("Default (Off)");
  fireEvent.change(screen.getByLabelText("browser_action decision mode"), { target: { value: "off" } });
  expect(screen.getByText("Effective browser mode · Off")).toBeVisible();
  fireEvent.click(screen.getByRole("button", { name: "Save decision settings" }));
  expect(await screen.findByRole("status")).toHaveTextContent("Settings saved");
  expect(saved).toMatchObject({ mode: "active", browser_actions_follow_mode: true, overrides: { browser_action: { mode: "off" } } });
});

it("tests the selected typed endpoint and explains missing confidence", async () => {
  mocks.api.mockImplementation(async (path: string) => path.endsWith("/test")
    ? { ok: false, reason: "missing_confidence", elapsed_ms: 500 }
    : { ...inventory(), purposes: ["browser_action"] });
  render(<DecisionSettings />);
  await screen.findByText("Effective browser mode · Off");
  fireEvent.change(screen.getByLabelText("Browser action confidence cutoff"), { target: { value: "0.9" } });
  fireEvent.click(screen.getByRole("button", { name: "Test decision model" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("did not return the selected confidence score");
  expect(mocks.api).toHaveBeenCalledWith("/api/models/decisions/test", {
    method: "POST", body: JSON.stringify({ model_id: "cloudflare/clef-flash", score_metric: "provider_confidence" }),
  });
});
