import "@testing-library/jest-dom/vitest";
import { afterEach, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { ChannelsSettings } from "./ChannelsSettings";
import { api } from "../api/client";

vi.mock("../api/client", () => ({ api: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

const channel = (live_status: string, connected = false) => ({
  id: "whatsapp_qr", name: "WhatsApp", tagline: "Messaging", fields: [],
  supports_qr: true, supports_check: true, setup_hint: "", docs_url: "",
  status: { configured: true, connected, listener_running: true, live_status },
});

it.each([
  ["connecting", "Connecting"], ["reconnecting", "Reconnecting"],
  ["qr", "Scan QR code"], ["error", "Connection error"],
])("shows %s without claiming the listener is stopped", async (state, label) => {
  vi.mocked(api).mockResolvedValue({ channels: [channel(state)] });
  render(<ChannelsSettings />);
  expect(await screen.findByText(label)).toBeInTheDocument();
  expect(screen.queryByText(/listener isn't running yet/)).not.toBeInTheDocument();
});

it("fresh QR polling overrides a stale connected summary", async () => {
  vi.mocked(api).mockImplementation(async (url) => url === "/api/channels"
    ? { channels: [channel("ready", true)] }
    : { status: "qr", qr: "private-pairing-code" });
  render(<ChannelsSettings />);
  expect(await screen.findByText("Connected")).toBeInTheDocument();
  expect(await screen.findByText("Scan QR code")).toBeInTheDocument();
  expect(screen.queryByText("Connected")).not.toBeInTheDocument();
  expect(screen.getByAltText("WhatsApp QR code")).toHaveAttribute("src", expect.stringContaining("/api/channels/whatsapp_qr/qr.svg"));
});
