import "@testing-library/jest-dom/vitest";

import { afterEach, expect, it, vi } from "vitest";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { PermissionMenu } from "./PermissionMenu";
const api = vi.hoisted(() => vi.fn());
vi.mock("../api/client", () => ({ api }));
afterEach(() => {
  cleanup();
  api.mockReset();
});
const ask = {
  profile: "ask",
  reviewer_available: true,
  chat_scoped_profiles: true,
};
async function open() {
  fireEvent.pointerDown(screen.getByRole("button", { name: /Permissions:/ }), {
    button: 0,
    ctrlKey: false,
  });
  await screen.findByRole("menuitemradio", { name: /Ask for approval/ });
}
it("loads the current chat and shows all review choices", async () => {
  api.mockResolvedValue(ask);
  render(<PermissionMenu sessionId="chat-one" />);
  await screen.findByRole("button", { name: "Permissions: Ask for approval" });
  expect(api).toHaveBeenCalledWith("/api/permissions?session_id=chat-one");
  await open();
  expect(
    screen.getByRole("menuitemradio", { name: /Ask for approval/ }),
  ).toHaveAttribute("aria-checked", "true");
  expect(
    screen.getByRole("menuitemradio", { name: /Approve for me/ }),
  ).toBeInTheDocument();
  expect(screen.getByText(/Tool and workspace limits/)).toBeInTheDocument();
});
it("requires confirmation before Full access is sent", async () => {
  api.mockResolvedValue(ask);
  render(<PermissionMenu sessionId="chat-one" />);
  await screen.findByRole("button", { name: "Permissions: Ask for approval" });
  await open();
  fireEvent.click(screen.getByRole("menuitemradio", { name: /Full access/ }));
  await screen.findByRole("dialog");
  expect(api).toHaveBeenCalledTimes(1);
  api.mockResolvedValue({ ...ask, profile: "full" });
  fireEvent.click(screen.getByRole("button", { name: "Enable Full access" }));
  await waitFor(() =>
    expect(api).toHaveBeenCalledWith(
      "/api/permissions",
      expect.objectContaining({
        body: JSON.stringify({
          session_id: "chat-one",
          profile: "full",
          confirm_full: true,
        }),
      }),
    ),
  );
  await screen.findByRole("button", { name: "Permissions: Full access" });
});
it("disables review when no model is configured", async () => {
  api.mockResolvedValue({ ...ask, reviewer_available: false });
  render(<PermissionMenu sessionId="chat" />);
  await screen.findByRole("button", { name: "Permissions: Ask for approval" });
  await open();
  expect(
    screen.getByRole("menuitemradio", { name: /Approve for me/ }),
  ).toHaveAttribute("aria-disabled", "true");
});
it("does not display a saved mode when the backend rejects it", async () => {
  api
    .mockResolvedValueOnce(ask)
    .mockRejectedValueOnce(new Error("Disconnected"));
  render(<PermissionMenu sessionId="chat" />);
  await screen.findByRole("button", { name: "Permissions: Ask for approval" });
  await open();
  fireEvent.click(
    screen.getByRole("menuitemradio", { name: /Approve for me/ }),
  );
  await screen.findByRole("alert");
  expect(
    screen.getByRole("button", { name: "Permissions: Ask for approval" }),
  ).toBeInTheDocument();
});
it("ignores a previous chat's late save response", async () => {
  let resolve!: (value: unknown) => void;
  const saved = new Promise((r) => {
    resolve = r;
  });
  api
    .mockResolvedValueOnce(ask)
    .mockReturnValueOnce(saved)
    .mockResolvedValueOnce(ask);
  const view = render(<PermissionMenu sessionId="old-chat" />);
  await screen.findByRole("button", { name: "Permissions: Ask for approval" });
  await open();
  fireEvent.click(
    screen.getByRole("menuitemradio", { name: /Approve for me/ }),
  );
  view.rerender(<PermissionMenu sessionId="new-chat" />);
  await waitFor(() =>
    expect(api).toHaveBeenCalledWith("/api/permissions?session_id=new-chat"),
  );
  resolve({ ...ask, profile: "auto_review" });
  await screen.findByRole("button", { name: "Permissions: Ask for approval" });
  expect(
    screen.queryByRole("button", { name: "Permissions: Approve for me" }),
  ).not.toBeInTheDocument();
});
it("does not label a retained routine grant as independent review or Ask", async () => {
  api.mockResolvedValue({ ...ask, legacy_routine: true });
  render(<PermissionMenu sessionId="chat" />);
  await screen.findByRole("button", {
    name: "Permissions: Workspace default",
  });
  await open();
  expect(screen.getByText(/inherits the workspace’s routine approvals/)).toBeInTheDocument();
  expect(
    screen.getByRole("menuitemradio", { name: /Ask for approval/ }),
  ).toHaveAttribute("aria-checked", "false");
  expect(
    screen.getByRole("menuitemradio", { name: /Approve for me/ }),
  ).toHaveAttribute("aria-checked", "false");
});
it("disables profiles restricted by the administrator", async () => {
  api.mockResolvedValue({
    ...ask,
    profile: "auto_review",
    requirements: {
      allowed_profiles: ["ask", "auto_review"],
      forced_profile: "auto_review",
    },
  });
  render(<PermissionMenu sessionId="chat" />);
  await screen.findByRole("button", { name: "Permissions: Approve for me" });
  await open();
  expect(
    screen.getByRole("menuitemradio", { name: /Full access/ }),
  ).toHaveAttribute("aria-disabled", "true");
  expect(
    screen.getByRole("menuitemradio", { name: /Ask for approval/ }),
  ).toHaveAttribute("aria-disabled", "true");
  expect(screen.getByText(/administrator controls/)).toBeInTheDocument();
});
it("offers an exact reviewed retry without sending an action", async () => {
  const authority = {
    session: "chat",
    turn: "turn",
    binding: "browser.workspace",
    generation: 1,
    resource: "https://example.test",
    input_digest: "a".repeat(64),
  };
  const denialData = {
    ...ask,
    denials: [
      {
        record: {
          id: "denied",
          revision: 2,
          request: {
            authority,
            detail: "Click sign in",
            action: "browser_click",
          },
        },
        review: { rationale: "Unclear authorization" },
      },
    ],
  };
  api.mockImplementation((_, options) =>
    Promise.resolve(
      options ? { retry_requested: true, input_dispatched: false } : denialData,
    ),
  );
  render(<PermissionMenu sessionId="chat" />);
  await screen.findByRole("button", { name: "Permissions: Ask for approval" });
  await open();
  fireEvent.click(
    screen.getByRole("menuitem", { name: /Review denied actions/ }),
  );
  await screen.findByRole("dialog");
  expect(screen.getByText("Unclear authorization")).toBeInTheDocument();
  fireEvent.click(
    screen.getByRole("button", { name: "Request one reviewed retry" }),
  );
  await screen.findByText(/No action was executed/);
  expect(api).toHaveBeenLastCalledWith("/api/review-overrides", {
    method: "POST",
    body: JSON.stringify({
      approval_id: "denied",
      authority,
      expected_revision: 2,
    }),
  });
});
it("shows inherited worker permissions without allowing escalation", async () => {
  api.mockResolvedValue({
    ...ask,
    profile: "auto_review",
    owner_session: "parent-chat",
  });
  render(<PermissionMenu sessionId="worker-chat" />);
  await screen.findByRole("button", { name: "Permissions: Approve for me" });
  await open();
  expect(
    screen.getByText("Change permissions in the parent chat."),
  ).toBeInTheDocument();
  for (const item of screen.getAllByRole("menuitemradio"))
    expect(item).toHaveAttribute("aria-disabled", "true");
});

it("confirms Full access before creating a chat and saves to that chat", async () => {
  api.mockResolvedValue(ask);
  const ensureSession = vi.fn(async (beforeOpen: (id: string) => Promise<void>) => {
    await beforeOpen("draft-chat");
    return { sessionId: "draft-chat" };
  });
  render(<PermissionMenu sessionId={null} ensureSession={ensureSession} />);
  await screen.findByRole("button", { name: "Permissions: Ask for approval" });
  await open();
  fireEvent.click(screen.getByRole("menuitemradio", { name: /Full access/ }));
  await screen.findByRole("dialog");
  expect(ensureSession).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "Enable Full access" }));
  await waitFor(() => expect(api).toHaveBeenCalledWith("/api/permissions", {
    method: "POST",
    body: JSON.stringify({ session_id: "draft-chat", profile: "full", confirm_full: true }),
  }));
});
