import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, expect, it, vi } from "vitest";
import { SocialIntegration } from "./SocialIntegration";
import { api } from "../api/client";

vi.mock("../api/client", () => ({ api: vi.fn() }));
let candidates: unknown[] = [];
beforeEach(() => {
  candidates = [];
  vi.mocked(api).mockReset();
  vi.mocked(api).mockImplementation(async path => {
    if (path === "/api/agents") return { agents: [] } as never;
    if (path.endsWith("/social")) return { accounts: [] } as never;
    if (path.endsWith("/setup")) return { configured: true, client_id: "123456", features: ["read"] } as never;
    if (path.endsWith("/connect")) return { authorization_url: "https://www.facebook.com/dialog/oauth?state=test" } as never;
    if (path.endsWith("/select")) { candidates = []; return {} as never; }
    return { configured: true, pending: false, candidates: path.endsWith("/meta") ? candidates : [], selection_id: "selection" } as never;
  });
});

it("recovers a blocked sign-in popup and permits cancellation", async () => {
  const popup = vi.spyOn(window, "open").mockReturnValue(null);
  render(<SocialIntegration />);
  fireEvent.click(screen.getByRole("button", { name: "Facebook & Instagram integration" }));
  const connect = screen.getByRole("button", { name: "Connect Facebook & Instagram" });
  await waitFor(() => expect(connect).toBeEnabled());
  fireEvent.click(connect);
  expect(await screen.findByRole("link", { name: "Open sign-in" })).toHaveAttribute("href", "https://www.facebook.com/dialog/oauth?state=test");
  fireEvent.click(screen.getByRole("button", { name: "Cancel sign-in" }));
  await waitFor(() => expect(screen.queryByText("Finish sign-in in the provider window.")).not.toBeInTheDocument());
  popup.mockRestore();
});

it("requires explicit selection of discovered accounts", async () => {
  candidates = [{ key: "facebook:page:123", username: "My Page", platform: "facebook" }];
  render(<SocialIntegration />);
  fireEvent.click(screen.getByRole("button", { name: "Facebook & Instagram integration" }));
  const selection = await screen.findByRole("button", { name: "Connect selected accounts" });
  expect(selection).toBeDisabled();
  fireEvent.click(screen.getByLabelText("My Page · Facebook Page"));
  fireEvent.click(selection);
  await waitFor(() => expect(api).toHaveBeenCalledWith("/api/integrations/meta/select", { method: "POST", body: JSON.stringify({ accounts: ["facebook:page:123"], selection_id: "selection" }) }));
});
