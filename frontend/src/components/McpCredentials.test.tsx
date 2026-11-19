import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { api } from "../api/client";
import { McpAssertionCredential } from "./McpAssertionCredential";
import { McpMtlsCredential } from "./McpMtlsCredential";

vi.mock("../api/client", () => ({ api: vi.fn(async () => ({ ok: true })) }));
afterEach(() => { cleanup(); vi.clearAllMocks(); });
const server = { name: "example", revision: 4, oauth: { client_id: "client", callback_url: "http://127.0.0.1:12799/callback" } };

it("saves JWT signing configuration and clears the entered secret", async () => {
  const refreshed=vi.fn(async () => {});
  render(<McpAssertionCredential server={server} onSaved={refreshed} />);
  fireEvent.click(screen.getByText("JWT client authentication"));
  fireEvent.change(screen.getByLabelText("JWT method for example"), { target: { value: "private_key_jwt" } });
  fireEvent.change(screen.getByLabelText("JWT algorithm for example"), { target: { value: "RS256" } });
  fireEvent.change(screen.getByLabelText("OAuth issuer for example"), { target: { value: "https://issuer.example" } });
  fireEvent.change(screen.getByLabelText("JWT credential for example"), { target: { value: "private-fixture" } });
  fireEvent.change(screen.getByLabelText("JWT key ID for example"), { target: { value: "key-1" } });
  fireEvent.click(screen.getByRole("button", { name: "Save JWT credentials" }));
  await waitFor(() => expect(api).toHaveBeenCalledWith("/api/mcp/oauth/assertion-credential", expect.objectContaining({ method: "POST", body: JSON.stringify({ name: "example", expected_revision: 4, method: "private_key_jwt", algorithm: "RS256", issuer: "https://issuer.example", assertion_audience: "token_endpoint", client_id: "client", callback_url: "http://127.0.0.1:12799/callback", credential: "private-fixture", key_id: "key-1" }) })));
  await waitFor(() => expect(screen.getByLabelText("JWT credential for example")).toHaveValue(""));
  expect(refreshed).toHaveBeenCalled();
});

it("stores an mTLS identity file without displaying its private contents", async () => {
  const refreshed=vi.fn(async () => {});
  render(<McpMtlsCredential server={server} onSaved={refreshed} />);
  fireEvent.click(screen.getByText("Mutual TLS authentication"));
  const file = new File(["private fixture"], "identity.pem", { type: "text/plain" });
  Object.defineProperty(file, "text", { value: async () => "private fixture" });
  fireEvent.change(screen.getByLabelText("mTLS identity for example"), { target: { files: [file] } });
  expect(await screen.findByText("Identity file ready to save.")).toBeInTheDocument();
  expect(screen.queryByText("private fixture")).not.toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Save mTLS credentials" }));
  await waitFor(() => expect(api).toHaveBeenCalledWith("/api/mcp/oauth/mtls-credential", expect.objectContaining({ method: "POST", body: JSON.stringify({ name: "example", expected_revision: 4, client_id: "client", callback_url: "http://127.0.0.1:12799/callback", auth_method: "tls_client_auth", identity_pem: "private fixture" }) })));
  await waitFor(() => expect(screen.queryByText("Identity file ready to save.")).not.toBeInTheDocument());
  expect(refreshed).toHaveBeenCalled();
});
