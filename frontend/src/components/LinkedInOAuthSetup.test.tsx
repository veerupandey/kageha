import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { expect, it, vi } from "vitest";
import { LinkedInOAuthSetup } from "./LinkedInOAuthSetup";
import { api } from "../api/client";
vi.mock("../api/client", () => ({ api: vi.fn() }));

it("saves setup, clears the input secret, and refreshes connection state", async () => {
  vi.mocked(api).mockResolvedValueOnce({ client_id: "oldclient", secret_configured: true, publish: false, api_version: "202609", redirect_uri: "https://agent.example/api/integrations/linkedin/callback" });
  vi.mocked(api).mockResolvedValueOnce({ client_id: "newclient", secret_configured: true, configured: true });
  const saved = vi.fn().mockResolvedValue(undefined);
  render(<LinkedInOAuthSetup onSaved={saved} />);
  await screen.findByDisplayValue("oldclient");
  fireEvent.change(screen.getByLabelText("LinkedIn Client ID"), { target: { value: "newclient" } });
  fireEvent.change(screen.getByLabelText("LinkedIn Client Secret"), { target: { value: "secret-value" } });
  fireEvent.click(screen.getByRole("button", { name: "Save LinkedIn setup" }));
  await screen.findByText(/no restart needed/);
  expect(saved).toHaveBeenCalledOnce();
  expect(screen.getByLabelText("LinkedIn Client Secret")).toHaveValue("");
  expect(api).toHaveBeenLastCalledWith("/api/integrations/linkedin/setup", { method: "PUT", body: JSON.stringify({ client_id: "newclient", client_secret: "secret-value", redirect_uri: "https://agent.example/api/integrations/linkedin/callback", publish: false, api_version: "202609" }) });
});
