import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { expect, it, vi } from "vitest";
import { GoogleOAuthSetup } from "./GoogleOAuthSetup";
import { api } from "../api/client";
vi.mock("../api/client", () => ({ api: vi.fn() }));

it("saves setup, clears the input secret, and refreshes connection state", async () => {
  vi.mocked(api).mockResolvedValueOnce({ client_id: "old.apps.googleusercontent.com", secret_configured: true, redirect_uri: "https://agent.example/api/integrations/gmail/callback" });
  vi.mocked(api).mockResolvedValueOnce({ client_id: "new.apps.googleusercontent.com", secret_configured: true, configured: true });
  const saved = vi.fn().mockResolvedValue(undefined);
  render(<GoogleOAuthSetup onSaved={saved} />);
  await screen.findByDisplayValue("old.apps.googleusercontent.com");
  fireEvent.change(screen.getByLabelText("Google Client ID"), { target: { value: "new.apps.googleusercontent.com" } });
  fireEvent.change(screen.getByLabelText("Google Client Secret"), { target: { value: "secret-value" } });
  fireEvent.click(screen.getByRole("button", { name: "Save Google setup" }));
  await screen.findByText(/no restart needed/);
  expect(saved).toHaveBeenCalledOnce();
  expect(screen.getByLabelText("Google Client Secret")).toHaveValue("");
  expect(api).toHaveBeenLastCalledWith("/api/integrations/gmail/setup", { method: "PUT", body: JSON.stringify({ client_id: "new.apps.googleusercontent.com", client_secret: "secret-value", redirect_uri: "https://agent.example/api/integrations/gmail/callback" }) });
});
