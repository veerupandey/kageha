import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { ModelsSettings } from "./ProviderSetupPanel";
import { api } from "../api/client";

vi.mock("./FallbackSettings", () => ({ FallbackSettings: () => null }));
vi.mock("../store", () => ({ useAppStore: (select: (state: unknown) => unknown) => select({ loadModels: async () => {} }) }));
vi.mock("../api/client", () => ({ api: vi.fn(async (path: string) => {
  if (path === "/api/provider-setup") return { connections: [], models: [] };
  if (path === "/api/provider-setup/discover") return { models: [{ id: "claude-test" }] };
  return { ok: true };
}) }));
afterEach(() => cleanup());

it("discovers and connects an Anthropic-compatible endpoint with the selected API format", async () => {
  render(<ModelsSettings />);
  await screen.findByLabelText("API format");
  fireEvent.change(screen.getByLabelText("API format"), { target: { value: "anthropic_compat" } });
  fireEvent.change(screen.getByLabelText("Base URL"), { target: { value: "https://provider.test" } });
  fireEvent.change(screen.getByLabelText("API key"), { target: { value: "test-key" } });
  fireEvent.click(screen.getByRole("button", { name: "Find models" }));
  await screen.findByText("claude-test");
  const discoverCall = vi.mocked(api).mock.calls.find(([path]) => path === "/api/provider-setup/discover");
  expect(JSON.parse(discoverCall?.[1]?.body as string)).toMatchObject({ protocol: "anthropic_compat", base_url: "https://provider.test" });
  fireEvent.click(screen.getByRole("button", { name: "Connect" }));
  await waitFor(() => expect(vi.mocked(api).mock.calls.some(([path]) => path === "/api/provider-setup/connect")).toBe(true));
  const connectCall = vi.mocked(api).mock.calls.find(([path]) => path === "/api/provider-setup/connect");
  expect(JSON.parse(connectCall?.[1]?.body as string)).toMatchObject({ protocol: "anthropic_compat", models: ["claude-test"] });
});

it("offers a sign-in link when native PKCE login needs a browser tab", async () => {
  const open = vi.spyOn(window, "open").mockReturnValue(null);
  vi.mocked(api).mockImplementation(async (url) => {
    if (url === "/api/provider-setup") return {
      connections: [{ id: "native-login", label: "Native login", kind: "oauth", auth_flow: "pkce", ready: false }], models: [],
    };
    if (url === "/api/provider-setup/oauth") return { ok: true, authorization_url: "https://issuer.test/authorize?state=fixture" };
    return { ok: true };
  });
  render(<ModelsSettings />);
  fireEvent.click(await screen.findByRole("button", { name: "Sign in" }));
  expect(await screen.findByRole("link", { name: "Continue sign-in" })).toHaveAttribute("href", "https://issuer.test/authorize?state=fixture");
  expect(open).toHaveBeenCalledWith("https://issuer.test/authorize?state=fixture", "_blank", "noopener,noreferrer");
  open.mockRestore();
});

it("authorizes Keychain only after an explicit click and reports denial without success", async () => {
  vi.mocked(api).mockImplementation(async (path) => {
    if (path === "/api/provider-setup") return { credentials: {can_authorize:true,locked:true}, connections:[],models:[] } as never;
    if (path === "/api/provider-setup/unlock") throw new Error("Keychain authorization was denied");
    return {ok:true} as never;
  });
  render(<ModelsSettings />);
  const unlock=await screen.findByRole("button",{name:"Unlock saved credentials"});
  expect(vi.mocked(api).mock.calls.filter(([path])=>path==="/api/provider-setup/unlock")).toHaveLength(0);
  fireEvent.click(unlock);
  await screen.findByText(/Keychain authorization was denied/);
  const calls=vi.mocked(api).mock.calls.filter(([path])=>path==="/api/provider-setup/unlock");
  expect(calls).toHaveLength(1);
  expect(calls[0][1]).toMatchObject({method:"POST",body:"{}"});
  expect(screen.queryByText("Saved credentials unlocked for this running Kageha instance.")).not.toBeInTheDocument();
});

it("unlocks the browser vault only on request and confirms its status", async () => {
  let authorized = false;
  vi.mocked(api).mockImplementation(async (path) => {
    if (path === "/api/provider-setup") return { connections: [], models: [] } as never;
    if (path === "/api/provider-setup/browser-credentials") return {
      configured: true, can_authorize: true, state: authorized ? "ready" : "locked",
    } as never;
    if (path === "/api/provider-setup/browser-credentials/authorize") {
      authorized = true;
      return { authorized: true } as never;
    }
    return { ok: true } as never;
  });
  render(<ModelsSettings />);
  const unlock = await screen.findByRole("button", { name: "Unlock browser credentials" });
  expect(vi.mocked(api).mock.calls.filter(([path]) => path === "/api/provider-setup/browser-credentials/authorize")).toHaveLength(0);
  fireEvent.click(unlock);
  await screen.findByText("Browser credentials are ready. You can retry the task.");
  expect(vi.mocked(api).mock.calls.filter(([path]) => path === "/api/provider-setup/browser-credentials/authorize")).toHaveLength(1);
  expect(screen.queryByRole("button", { name: "Unlock browser credentials" })).not.toBeInTheDocument();
});

it("keeps the browser unlock available when authorization is denied", async () => {
  vi.mocked(api).mockImplementation(async (path) => {
    if (path === "/api/provider-setup") return { connections: [], models: [] } as never;
    if (path === "/api/provider-setup/browser-credentials") return { configured: true, can_authorize: true, state: "locked" } as never;
    if (path === "/api/provider-setup/browser-credentials/authorize") throw new Error("Authorization denied");
    return { ok: true } as never;
  });
  render(<ModelsSettings />);
  fireEvent.click(await screen.findByRole("button", { name: "Unlock browser credentials" }));
  await screen.findByText(/Authorization denied/);
  expect(screen.getByRole("button", { name: "Unlock browser credentials" })).toBeEnabled();
});
