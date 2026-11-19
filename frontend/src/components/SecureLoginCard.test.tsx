import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { api } from "../api/client";
import { useAppStore } from "../store";
import { SecureLoginCard } from "./SecureLoginCard";

vi.mock("../api/client", () => ({ api: vi.fn() }));
afterEach(() => { cleanup(); vi.restoreAllMocks(); });

describe("browser sign-in card", () => {
  it("opens the human browser and requires confirmation before resuming", async () => {
    const resolve = vi.fn();
    useAppStore.setState({ sessionId: "login-chat", resolveApproval: resolve, workspaceSurface: "artifacts", browserPanelMode: "default", prefs: { ...useAppStore.getState().prefs, rightPanelCollapsed: true } });
    vi.mocked(api).mockResolvedValue({ lockedBy: "user" });
    render(<SecureLoginCard detail={JSON.stringify({ website: "https://example.test", purpose: "Read my orders" })} />);
    expect(screen.getByRole("button", { name: "Approve access and resume" })).toBeDisabled();
    expect(screen.queryByLabelText(/password/i)).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Open sign-in browser" }));
    await waitFor(() => expect(useAppStore.getState().workspaceSurface).toBe("live"));
    expect(useAppStore.getState().liveSurface).toBe("browser");
    expect(useAppStore.getState().prefs.rightPanelCollapsed).toBe(false);
    expect(api).toHaveBeenCalledWith("/api/sessions/login-chat/browser/lease", expect.objectContaining({ body: JSON.stringify({ action: "take" }) }));
    expect(resolve).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("checkbox"));
    fireEvent.click(screen.getByRole("button", { name: "Approve access and resume" }));
    expect(resolve).toHaveBeenCalledWith(true, "", "once");
  });

  it("reports browser failures and supports cancellation", async () => {
    const resolve = vi.fn();
    useAppStore.setState({ sessionId: "login-chat", resolveApproval: resolve });
    vi.mocked(api).mockRejectedValue(new Error("Remote browser unavailable"));
    render(<SecureLoginCard detail="Sign in" />);
    fireEvent.click(screen.getByRole("button", { name: "Open sign-in browser" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Remote browser unavailable");
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(resolve).toHaveBeenCalledWith(false);
  });

  it("returns control only on the original Rust tab before resuming", async () => {
    const resolve = vi.fn();
    useAppStore.setState({ sessionId: "login-chat", resolveApproval: resolve });
    const state = { backend: "rust", bindingId: "service:profile:9", controlGeneration: 7,
      lockedBy: "user", tabs: [{ active: true, tabHandle: "other-tab" }] };
    vi.mocked(api).mockResolvedValue(state);
    render(<SecureLoginCard detail={JSON.stringify({ website: "https://example.test",
      rust_tab: "login-tab", binding_id: "service:profile:8", account_step: true })} />);
    expect(screen.getByText("Continue in browser")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Open browser tab" }));
    expect(useAppStore.getState().liveSurface).toBe("browser");
    expect(await screen.findByRole("alert")).toHaveTextContent("Return to the original sign-in tab");
    expect(api).not.toHaveBeenCalledWith(expect.stringContaining("/lease"), expect.anything());
    fireEvent.click(screen.getByRole("checkbox"));
    const resume = screen.getByRole("button", { name: "Approve access and resume" });
    fireEvent.click(resume);
    expect(await screen.findByRole("alert")).toHaveTextContent("Return to the original sign-in tab");
    expect(resolve).not.toHaveBeenCalled();

    const original = { ...state, tabs: [{ active: true, tabHandle: "login-tab" }] };
    vi.mocked(api).mockResolvedValue({ ...original, url: "https://other.test/login" });
    fireEvent.click(resume);
    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent("Return to the account website"));
    expect(resolve).not.toHaveBeenCalled();

    vi.mocked(api).mockResolvedValue({ ...original, url: "https://example.test/account" });
    fireEvent.click(resume);
    await waitFor(() => expect(resolve).toHaveBeenCalledWith(true, "", "once"));
    expect(api).toHaveBeenCalledWith("/api/sessions/login-chat/browser/lease", {
      method: "POST",
      body: JSON.stringify({ action: "return", bindingId: "service:profile:9", controlGeneration: 7 }),
    });
  });

  it("allows approval before reconnection without claiming the user signed in", () => {
    const resolve = vi.fn();
    useAppStore.setState({ sessionId: "login-chat", resolveApproval: resolve });
    render(<SecureLoginCard detail={JSON.stringify({ website: "https://example.test", purpose: "Continue LinkedIn setup", reconnect_required: true })} />);
    expect(screen.getByText("Approve browser reconnection")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Open sign-in browser" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Approve access and resume" })).toBeDisabled();
    fireEvent.click(screen.getByRole("checkbox", { name: /approve reconnecting/ }));
    fireEvent.click(screen.getByRole("button", { name: "Approve access and resume" }));
    expect(resolve).toHaveBeenCalledWith(true, "", "once");
  });

});
