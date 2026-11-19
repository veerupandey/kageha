import "@testing-library/jest-dom/vitest";
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { api } from "../api/client";
import { PeerNetwork } from "./PeerNetwork";

const trustedPeer = {
  peer_id: "peer-1",
  name: "Right",
  status: "trusted",
  trust_state: "trusted",
  fingerprint: "ab12",
  endpoint: "http://127.0.0.1:8790",
  capabilities: [
    {
      capability_id: "knowledge.query",
      version: "1",
      description: "Ask how a problem was solved.",
      required_permissions: ["knowledge.read"],
    },
  ],
};



vi.mock("../api/client", () => ({
  api: vi.fn(async (path: string, opts?: RequestInit) => {
    if (path === "/api/peers") {
      return { peers: [trustedPeer], local_identity: { agent_id: "left" } };
    }
    if (path === "/api/peers/pairing") return { requests: [] };
    if (path === "/api/peers/capabilities") return { capabilities: [] };
    if (path === "/api/peers/tasks") return { tasks: [] };
    if (path === "/api/peers/grants") return { grants: [] };
    if (path === "/api/peers/ask-knowledge" && opts?.method === "POST") {
      const body = JSON.parse(String(opts.body || "{}")) as { peer_id?: string; query?: string };
      return {
        status: "ok",
        peer_id: body.peer_id,
        query: body.query,
        lessons: [
          { task: "fix flaky retry test", summary: "Added jitter to the retry backoff." },
        ],
      };
    }
    return { ok: true };
  }),
}));

describe("PeerNetwork ask knowledge", () => {
  afterEach(() => {
    cleanup();
  });

  it("shows a Trusted badge for a trusted peer", async () => {
    render(<PeerNetwork onClose={() => {}} />);
    expect(await screen.findByText("Right")).toBeInTheDocument();
    expect(screen.getByText("Trusted")).toBeInTheDocument();
  });

  it("shows Query Solutions for peers advertising knowledge.query and renders returned lessons", async () => {
    render(<PeerNetwork onClose={() => {}} />);
    expect(await screen.findByText("Right")).toBeInTheDocument();
    const askButtons = screen.getAllByRole("button", { name: "Query Solutions" });
    expect(askButtons).toHaveLength(1);

    fireEvent.click(askButtons[0]);
    const textarea = await screen.findByPlaceholderText(/How did you fix/);
    fireEvent.change(textarea, { target: { value: "how did you fix the flaky retry test" } });
    const dialog = screen.getByRole("dialog");
    fireEvent.click(within(dialog).getByRole("button", { name: "Search Solutions" }));

    await waitFor(() =>
      expect(vi.mocked(api)).toHaveBeenCalledWith(
        "/api/peers/ask-knowledge",
        expect.objectContaining({
          method: "POST",
          body: JSON.stringify({
            peer_id: "peer-1",
            query: "how did you fix the flaky retry test",
            grant_wait_seconds: 10,
            timeout_seconds: 30,
          }),
        }),
      ),
    );
    expect(await screen.findByText("fix flaky retry test")).toBeInTheDocument();
    expect(screen.getByText("Added jitter to the retry backoff.")).toBeInTheDocument();
  });

  it("shows a clear pending message on timeout instead of a generic error", async () => {
    vi.mocked(api).mockImplementationOnce(async () => ({ peers: [trustedPeer], local_identity: {} }));
    render(<PeerNetwork onClose={() => {}} />);
    await screen.findByText("Right", {}, { timeout: 4000 });
    fireEvent.click(screen.getByRole("button", { name: "Query Solutions" }));
    const textarea = await screen.findByPlaceholderText(/How did you fix/);
    fireEvent.change(textarea, { target: { value: "anything" } });
    const dialog = screen.getByRole("dialog");

    vi.mocked(api).mockImplementationOnce(async () => ({
      status: "timeout",
      error: "peer owner has not approved the knowledge request yet",
    }));
    fireEvent.click(within(dialog).getByRole("button", { name: "Search Solutions" }));

    expect(await screen.findByText(/has not approved this request yet/)).toBeInTheDocument();
  });
});



const peerWithShareableSkill = {
  ...trustedPeer,
  skills: ["invoice-formatter"],
};

describe("PeerNetwork export skill", () => {
  afterEach(() => {
    cleanup();
  });

  it("shows a preview and only installs after an explicit confirmation", async () => {
    vi.mocked(api).mockImplementation(async (path: string, opts?: RequestInit) => {
      if (path === "/api/peers") {
        return { peers: [peerWithShareableSkill], local_identity: { agent_id: "left" } };
      }
      if (path === "/api/peers/pairing") return { requests: [] };
      if (path === "/api/peers/capabilities") return { capabilities: [] };
      if (path === "/api/peers/tasks") return { tasks: [] };
      if (path === "/api/peers/grants") return { grants: [] };
      if (path === "/api/peers/export-skill" && opts?.method === "POST") {
        return {
          status: "ok",
          peer_id: "peer-1",
          skill: "invoice-formatter",
          bundle_id: "bundle-123",
          preview: {
            name: "invoice-formatter",
            description: "Formats invoices",
            body: "Format the line items as a table.",
            allowed_tools: "read_file write_file",
            resource_count: "0",
          },
        };
      }
      if (path === "/api/peers/install-skill-bundle" && opts?.method === "POST") {
        const parsedBody = JSON.parse(String(opts.body || "{}"));
        expect(parsedBody.bundle_id).toBe("bundle-123");
        return { ok: true, installed: ["invoice-formatter"], skipped: [] };
      }
      return { ok: true };
    });

    render(<PeerNetwork onClose={() => {}} />);
    const skillButton = await screen.findByRole("button", { name: "invoice-formatter" });
    fireEvent.click(skillButton);

    const dialog = await screen.findByText("Request a skill from Right");
    const modal = dialog.closest("div")!.parentElement as HTMLElement;

    expect(within(modal).queryByRole("button", { name: "Install" })).not.toBeInTheDocument();
    fireEvent.click(within(modal).getByRole("button", { name: "Request" }));

    expect(await screen.findByText("Formats invoices")).toBeInTheDocument();
    expect(screen.getByText("Format the line items as a table.")).toBeInTheDocument();

    fireEvent.click(within(modal).getByRole("button", { name: "Install" }));

    await waitFor(() =>
      expect(vi.mocked(api)).toHaveBeenCalledWith(
        "/api/peers/install-skill-bundle",
        expect.objectContaining({ method: "POST" }),
      ),
    );
    expect(await screen.findByText(/Installed: invoice-formatter/)).toBeInTheDocument();
  });

  it("shows a pending message and no Install button on timeout", async () => {
    vi.mocked(api).mockImplementation(async (path: string, opts?: RequestInit) => {
      if (path === "/api/peers") {
        return { peers: [peerWithShareableSkill], local_identity: { agent_id: "left" } };
      }
      if (path === "/api/peers/pairing") return { requests: [] };
      if (path === "/api/peers/capabilities") return { capabilities: [] };
      if (path === "/api/peers/tasks") return { tasks: [] };
      if (path === "/api/peers/grants") return { grants: [] };
      if (path === "/api/peers/export-skill" && opts?.method === "POST") {
        return { status: "timeout", error: "not approved yet" };
      }
      return { ok: true };
    });

    render(<PeerNetwork onClose={() => {}} />);
    const skillButton = await screen.findByRole("button", { name: "invoice-formatter" });
    fireEvent.click(skillButton);
    const dialog = await screen.findByText("Request a skill from Right");
    const modal = dialog.closest("div")!.parentElement as HTMLElement;
    fireEvent.click(within(modal).getByRole("button", { name: "Request" }));

    expect(await screen.findByText(/has not approved this request yet/)).toBeInTheDocument();
    expect(within(modal).queryByRole("button", { name: "Install" })).not.toBeInTheDocument();
  });

  it("retries against the same pending grant instead of requesting a new one, and succeeds once approved", async () => {
    let exportCalls = 0;
    vi.mocked(api).mockImplementation(async (path: string, opts?: RequestInit) => {
      if (path === "/api/peers") {
        return { peers: [peerWithShareableSkill], local_identity: { agent_id: "left" } };
      }
      if (path === "/api/peers/pairing") return { requests: [] };
      if (path === "/api/peers/capabilities") return { capabilities: [] };
      if (path === "/api/peers/tasks") return { tasks: [] };
      if (path === "/api/peers/grants") return { grants: [] };
      if (path === "/api/peers/export-skill" && opts?.method === "POST") {
        exportCalls += 1;
        const body = JSON.parse(String(opts.body || "{}")) as { pending_grant_id?: string };
        if (exportCalls === 1) {
          expect(body.pending_grant_id).toBeUndefined();
          return { status: "timeout", error: "not approved yet", grant_id: "grant-abc" };
        }
        expect(body.pending_grant_id).toBe("grant-abc");
        return {
          status: "ok",
          peer_id: "peer-1",
          skill: "invoice-formatter",
          bundle_id: "bundle-456",
          grant_id: "grant-abc",
          preview: {
            name: "invoice-formatter",
            description: "Formats invoices",
            body: "Format the line items as a table.",
            allowed_tools: "read_file write_file",
            resource_count: "0",
          },
        };
      }
      return { ok: true };
    });

    render(<PeerNetwork onClose={() => {}} />);
    const skillButton = await screen.findByRole("button", { name: "invoice-formatter" });
    fireEvent.click(skillButton);
    const dialog = await screen.findByText("Request a skill from Right");
    const modal = dialog.closest("div")!.parentElement as HTMLElement;

    fireEvent.click(within(modal).getByRole("button", { name: "Request" }));
    const tryAgainButton = await screen.findByRole("button", { name: "Try again" });

    // While timed out, the input and the bottom Request button must be disabled
    // so the only way forward is retrying the same grant, not starting a new one.
    expect(within(modal).getByPlaceholderText("skill-name")).toBeDisabled();
    expect(within(modal).queryByRole("button", { name: "Request" })).not.toBeInTheDocument();

    fireEvent.click(tryAgainButton);

    await waitFor(() => expect(exportCalls).toBe(2));
    expect(await screen.findByText("Formats invoices")).toBeInTheDocument();
    expect(within(modal).getByRole("button", { name: "Install" })).toBeInTheDocument();
  });
});



describe("PeerNetwork collaboration tasks", () => {
  afterEach(() => {
    cleanup();
  });

  it("renders skill.export and knowledge.query task objectives readably instead of raw JSON", async () => {
    vi.mocked(api).mockImplementation(async (path: string) => {
      if (path === "/api/peers") return { peers: [trustedPeer], local_identity: { agent_id: "left" } };
      if (path === "/api/peers/pairing") return { requests: [] };
      if (path === "/api/peers/capabilities") return { capabilities: [] };
      if (path === "/api/peers/grants") return { grants: [] };
      if (path === "/api/peers/tasks") {
        return {
          tasks: [
            { task_id: "t1", objective: '{"skill":"invoice-formatter"}', state: "completed" },
            { task_id: "t2", objective: '{"query":"how did you fix the bug"}', state: "running" },
          ],
        };
      }
      return { ok: true };
    });
    render(<PeerNetwork onClose={() => {}} />);
    expect(await screen.findByText("skill.export: invoice-formatter")).toBeInTheDocument();
    expect(screen.getByText("knowledge.query: how did you fix the bug")).toBeInTheDocument();
  });
});



describe("PeerNetwork polish", () => {
  afterEach(() => {
    cleanup();
  });

  it("shows a loading state before peers arrive, distinct from the empty state", async () => {
    let resolvePeers: () => void = () => { throw new Error("Peer request has not started"); };
    vi.mocked(api).mockImplementation(async (path: string) => {
      if (path === "/api/peers") {
        await new Promise<void>((resolve) => {
          resolvePeers = resolve;
        });
        return { peers: [], local_identity: { agent_id: "left" } };
      }
      if (path === "/api/peers/pairing") return { requests: [] };
      if (path === "/api/peers/capabilities") return { capabilities: [] };
      if (path === "/api/peers/tasks") return { tasks: [] };
      if (path === "/api/peers/grants") return { grants: [] };
      return { ok: true };
    });
    render(<PeerNetwork onClose={() => {}} />);
    expect(screen.getByText("Loading peers…")).toBeInTheDocument();
    expect(screen.queryByText(/No peers discovered yet/)).not.toBeInTheDocument();
    resolvePeers?.();
    expect(await screen.findByText(/No peers discovered yet/)).toBeInTheDocument();
  });

  it("closes the Ask dialog on Escape", async () => {
    vi.mocked(api).mockImplementation(async (path: string) => {
      if (path === "/api/peers") return { peers: [trustedPeer], local_identity: { agent_id: "left" } };
      if (path === "/api/peers/pairing") return { requests: [] };
      if (path === "/api/peers/capabilities") return { capabilities: [] };
      if (path === "/api/peers/tasks") return { tasks: [] };
      if (path === "/api/peers/grants") return { grants: [] };
      return { ok: true };
    });
    render(<PeerNetwork onClose={() => {}} />);
    fireEvent.click(await screen.findByRole("button", { name: "Query Solutions" }));
    expect(await screen.findByRole("dialog")).toBeInTheDocument();
    fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
  });

  it("dismisses the error banner when Dismiss is clicked", async () => {
    vi.mocked(api).mockImplementation(async (path: string) => {
      if (path === "/api/peers") throw new Error("network unreachable");
      return { ok: true };
    });
    render(<PeerNetwork onClose={() => {}} />);
    expect(await screen.findByText("Error: network unreachable")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Dismiss error" }));
    expect(screen.queryByText("Error: network unreachable")).not.toBeInTheDocument();
  });
});



describe("PeerNetwork first-time user experience", () => {
  afterEach(() => {
    cleanup();
  });

  it("explains what connecting a peer is for", async () => {
    vi.mocked(api).mockImplementation(async (path: string) => {
      if (path === "/api/peers") return { peers: [], local_identity: { agent_id: "left" } };
      if (path === "/api/peers/pairing") return { requests: [] };
      if (path === "/api/peers/capabilities") return { capabilities: [] };
      if (path === "/api/peers/tasks") return { tasks: [] };
      if (path === "/api/peers/grants") return { grants: [] };
      return { ok: true };
    });
    render(<PeerNetwork onClose={() => {}} />);
    expect(
      await screen.findByText(/ask how it solved a problem or borrow one of its skills/),
    ).toBeInTheDocument();
  });

  it("tells the user a scan finished even when nothing new was found", async () => {
    vi.mocked(api).mockImplementation(async (path: string, opts?: RequestInit) => {
      if (path === "/api/peers") return { peers: [], local_identity: { agent_id: "left" } };
      if (path === "/api/peers/pairing") return { requests: [] };
      if (path === "/api/peers/capabilities") return { capabilities: [] };
      if (path === "/api/peers/tasks") return { tasks: [] };
      if (path === "/api/peers/grants") return { grants: [] };
      if (path === "/api/peers/discover" && opts?.method === "POST") return { peers: [] };
      return { ok: true };
    });
    render(<PeerNetwork onClose={() => {}} />);
    fireEvent.click(await screen.findByRole("button", { name: "Discover" }));
    expect(await screen.findByText(/Scan finished — no peers found/)).toBeInTheDocument();
  });

  it("requires confirmation before revoking trust, but not before a temporary block", async () => {
    const confirmSpy = vi.spyOn(window, "confirm").mockReturnValue(false);
    vi.mocked(api).mockImplementation(async (path: string) => {
      if (path === "/api/peers") return { peers: [trustedPeer], local_identity: { agent_id: "left" } };
      if (path === "/api/peers/pairing") return { requests: [] };
      if (path === "/api/peers/capabilities") return { capabilities: [] };
      if (path === "/api/peers/tasks") return { tasks: [] };
      if (path === "/api/peers/grants") return { grants: [] };
      return { ok: true };
    });
    render(<PeerNetwork onClose={() => {}} />);
    fireEvent.click(await screen.findByRole("button", { name: "Revoke trust" }));
    expect(confirmSpy).toHaveBeenCalled();
    // User declined the confirmation, so no revoke call should have gone out.
    expect(vi.mocked(api)).not.toHaveBeenCalledWith(
      expect.stringContaining("/revoke"),
      expect.anything(),
    );
    confirmSpy.mockRestore();
  });

  it("shows Unblock instead of Pair for a blocked peer, and calls the unblock endpoint", async () => {
    const blockedPeer = { ...trustedPeer, trust_state: "blocked", status: "blocked", permissions: [] };
    vi.mocked(api).mockImplementation(async (path: string, opts?: RequestInit) => {
      if (path === "/api/peers") return { peers: [blockedPeer], local_identity: { agent_id: "left" } };
      if (path === "/api/peers/pairing") return { requests: [] };
      if (path === "/api/peers/capabilities") return { capabilities: [] };
      if (path === "/api/peers/tasks") return { tasks: [] };
      if (path === "/api/peers/grants") return { grants: [] };
      if (path.endsWith("/unblock") && opts?.method === "POST") return { ok: true };
      return { ok: true };
    });
    render(<PeerNetwork onClose={() => {}} />);
    expect(await screen.findByRole("button", { name: "Unblock" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Pair" })).not.toBeInTheDocument();
    // Already blocked, so the redundant "Block (temporary)" action is hidden.
    expect(screen.queryByRole("button", { name: "Block (temporary)" })).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Unblock" }));
    await waitFor(() =>
      expect(vi.mocked(api)).toHaveBeenCalledWith(
        "/api/peers/peer-1/unblock",
        expect.objectContaining({ method: "POST" }),
      ),
    );
  });

  it("shows Pair (not Unblock) for a revoked peer, since revoke is permanent by design", async () => {
    const revokedPeer = { ...trustedPeer, trust_state: "revoked", status: "blocked", permissions: [] };
    vi.mocked(api).mockImplementation(async (path: string) => {
      if (path === "/api/peers") return { peers: [revokedPeer], local_identity: { agent_id: "left" } };
      if (path === "/api/peers/pairing") return { requests: [] };
      if (path === "/api/peers/capabilities") return { capabilities: [] };
      if (path === "/api/peers/tasks") return { tasks: [] };
      if (path === "/api/peers/grants") return { grants: [] };
      return { ok: true };
    });
    render(<PeerNetwork onClose={() => {}} />);
    expect(await screen.findByRole("button", { name: "Pair" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Unblock" })).not.toBeInTheDocument();
  });
});
