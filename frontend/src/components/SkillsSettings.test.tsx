import "@testing-library/jest-dom/vitest";
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { api } from "../api/client";
import { SkillsSettings } from "./SkillsSettings";

vi.mock("../api/client", () => ({
  api: vi.fn(async (path: string, opts?: RequestInit) => {
    if (String(path).startsWith("/api/knowledge") || path === "/api/extensions/skills") {
      if (opts?.method === "POST") {
        return { ok: true };
      }
      return {
        enabled: true,
        skills: [
          { name: "demo-skill", description: "A demo skill", origin: "standalone", plugin: "", plugin_enabled: true, editable: true, can_pin: true, loads: 2, pinned: false },
        ],
      };
    }
    if (path === "/api/skills/install") {
      return { installed: ["new-skill"], skipped: [] };
    }
    return { ok: true };
  }),
}));

describe("SkillsSettings", () => {
  afterEach(() => {
    cleanup();
  });

  it("lists installed skills and installs from a URL", async () => {
    render(<SkillsSettings />);
    expect(await screen.findByText("demo-skill")).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("Skill source"), {
      target: { value: "https://github.com/owner/skill-repo" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Install" }));
    await waitFor(() =>
      expect(vi.mocked(api)).toHaveBeenCalledWith(
        "/api/skills/install",
        expect.objectContaining({
          method: "POST",
          body: JSON.stringify({ source: "https://github.com/owner/skill-repo" }),
        }),
      ),
    );
    expect(await screen.findByText(/Installed: new-skill/)).toBeInTheDocument();
  });

  it("pins a workspace skill", async () => {
    render(<SkillsSettings />);
    const pin = await screen.findByRole("button", { name: "Pin for this workspace" });
    fireEvent.click(pin);
    await waitFor(() =>
      expect(vi.mocked(api)).toHaveBeenCalledWith(
        "/api/knowledge/skill",
        expect.objectContaining({
          method: "POST",
          body: JSON.stringify({ name: "demo-skill", action: "pin" }),
        }),
      ),
    );
  });
});


describe("SkillsSettings peer sharing", () => {
  afterEach(() => {
    cleanup();
  });

  it("shows a blocked reason and disables the toggle for an unsafe skill", async () => {
    vi.mocked(api).mockImplementation(async (path: string) => {
      if (String(path).startsWith("/api/knowledge") || path === "/api/extensions/skills") {
        return {
          enabled: true,
          skills: [
            {
              name: "shell-skill",
              description: "x",
              origin: "standalone", plugin: "", plugin_enabled: true, editable: true, can_pin: true,
              peer_shareable: false,
              peer_shareable_opted_in: false,
              peer_shareable_blocked_reason:
                "allowed-tools includes unsafe tools for peer sharing: bash",
            },
          ],
        };
      }
      return { ok: true };
    });
    render(<SkillsSettings />);
    expect(await screen.findByText("allowed-tools includes unsafe tools for peer sharing: bash")).toBeInTheDocument();
    const blocked = await screen.findByRole("button", { name: "Blocked from peer sharing" });
    expect(blocked).toBeDisabled();
  });

  it("marks a safe skill as peer-shareable", async () => {
    let shared = false;
    vi.mocked(api).mockImplementation(async (path: string, opts?: RequestInit) => {
      if (String(path).startsWith("/api/knowledge") && opts?.method === "POST") {
        const body = JSON.parse(String(opts.body || "{}"));
        shared = body.action === "peer_share";
        return { ok: true };
      }
      if (String(path).startsWith("/api/knowledge") || path === "/api/extensions/skills") {
        return {
          enabled: true,
          skills: [
            {
              name: "safe-skill",
              description: "x",
              origin: "standalone", plugin: "", plugin_enabled: true, editable: true, can_pin: true,
              peer_shareable: shared,
              peer_shareable_opted_in: shared,
              peer_shareable_blocked_reason: "",
            },
          ],
        };
      }
      return { ok: true };
    });
    render(<SkillsSettings />);
    const allow = await screen.findByRole("button", { name: "Allow peer sharing" });
    fireEvent.click(allow);
    await waitFor(() =>
      expect(vi.mocked(api)).toHaveBeenCalledWith(
        "/api/knowledge/skill",
        expect.objectContaining({
          method: "POST",
          body: JSON.stringify({ name: "safe-skill", action: "peer_share" }),
        }),
      ),
    );
    expect(await screen.findByRole("button", { name: "Shareable with peers" })).toBeInTheDocument();
  });
});

describe("SkillsSettings learned skills", () => {
  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
  });

  const learned = { name: "inbox-digest", description: "Summarize the inbox", origin: "learned", source: "learned",
    plugin: "", plugin_enabled: true, editable: true, can_pin: true, degraded: true, restored_from: 2 };
  function serve(archived = false) {
    const posts: unknown[] = [];
    vi.mocked(api).mockImplementation(async (path: string, opts?: RequestInit) => {
      if (opts?.method === "POST") {
        const body = JSON.parse(String(opts.body || "{}"));
        posts.push(body);
        if (body.action === "archive") archived = true;
        if (body.action === "restore") archived = false;
        return { ok: true };
      }
      if (path === "/api/extensions/skills") return { enabled: true, skills: [{ ...learned, archived }] };
      return { ok: true };
    });
    return posts;
  }

  it("badges a learned, degraded skill and disables and enables it", async () => {
    const posts = serve();
    render(<SkillsSettings />);
    expect(await screen.findByText("Learned")).toBeInTheDocument();
    expect(screen.getByText("Degraded")).toHaveAttribute("title", expect.stringContaining("Revision 2 is active again"));
    fireEvent.click(screen.getByRole("button", { name: "Disable inbox-digest" }));
    await waitFor(() => expect(posts).toEqual([{ name: "inbox-digest", action: "archive" }]));
    // A disabled skill stays listed so it can be enabled again.
    expect(await screen.findByText("Disabled")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Pin for this workspace" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Enable inbox-digest" }));
    await waitFor(() => expect(posts).toEqual([
      { name: "inbox-digest", action: "archive" }, { name: "inbox-digest", action: "restore" }]));
  });

  it("deletes a skill only after confirmation", async () => {
    const posts = serve();
    const confirm = vi.spyOn(window, "confirm").mockReturnValueOnce(false).mockReturnValueOnce(true);
    render(<SkillsSettings />);
    const remove = await screen.findByRole("button", { name: "Delete inbox-digest" });
    fireEvent.click(remove);
    expect(confirm).toHaveBeenCalledTimes(1);
    expect(posts).toEqual([]);
    fireEvent.click(remove);
    await waitFor(() => expect(posts).toEqual([{ name: "inbox-digest", action: "delete" }]));
    expect(confirm.mock.calls[0][0]).toContain("inbox-digest");
  });
});
