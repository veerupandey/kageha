import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, expect, it, vi } from "vitest";
import { ShopifyIntegration } from "./ShopifyIntegration";
import { api } from "../api/client";

vi.mock("../api/client", () => ({ api: vi.fn() }));
beforeEach(() => {
  vi.mocked(api).mockReset();
  vi.mocked(api).mockImplementation(async path => {
    if (path === "/api/agents") return { agents: [] } as never;
    if (path.endsWith("/setup")) return { configured: true, mode: "oauth", client_id: "client123", scopes: ["read_products"], secret_configured: true } as never;
    if (path.endsWith("/connect")) return { authorization_url: "https://shop.myshopify.com/admin/oauth/authorize?state=test" } as never;
    return { configured: true, pending: false, error: "", stores: [] } as never;
  });
});

it("requires a store and recovers from a blocked OAuth popup", async () => {
  const popup = vi.spyOn(window, "open").mockReturnValue(null);
  render(<ShopifyIntegration />);
  fireEvent.click(screen.getByRole("button", { name: "Shopify integration" }));
  const connect = screen.getByRole("button", { name: "Connect Shopify store" });
  expect(connect).toBeDisabled();
  fireEvent.change(screen.getByLabelText("Store domain"), { target: { value: "shop.myshopify.com" } });
  await waitFor(() => expect(connect).toBeEnabled());
  fireEvent.click(connect);
  expect(await screen.findByRole("link", { name: "Open Shopify sign-in" })).toHaveAttribute("href", "https://shop.myshopify.com/admin/oauth/authorize?state=test");
  fireEvent.click(screen.getByRole("button", { name: "Cancel Shopify sign-in" }));
  await waitFor(() => expect(screen.queryByText("Finish authorization in Shopify.")).not.toBeInTheDocument());
  popup.mockRestore();
});

it("retries a status outage without asking to reset app credentials", async () => {
  vi.mocked(api).mockRejectedValueOnce(new Error("Temporarily unavailable"));
  render(<ShopifyIntegration />);
  fireEvent.click(screen.getByRole("button", { name: "Shopify integration" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("Temporarily unavailable");
  fireEvent.click(screen.getByRole("button", { name: "Refresh Shopify connections" }));
  await waitFor(() => expect(screen.queryByRole("alert")).not.toBeInTheDocument());
  expect(screen.getByLabelText("Store domain")).toBeInTheDocument();
});
