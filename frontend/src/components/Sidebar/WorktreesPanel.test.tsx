import "@testing-library/jest-dom/vitest";
import { afterEach, expect, it, vi } from "vitest";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { api, ApiError } from "../../api/client";
import { WorktreesPanel } from "./WorktreesPanel";
vi.mock("../../api/client", async () => ({
  ...(await vi.importActual<typeof import("../../api/client")>(
    "../../api/client",
  )),
  api: vi.fn(),
}));
afterEach(() => {
  cleanup();
  vi.resetAllMocks();
});
it("checks a recovered uncertain operation without posting another creation", async () => {
  const operation = {
    id: "original",
    state: "dispatched",
    label: "agent",
    base: "HEAD",
  };
  let pending = true;
  vi.mocked(api).mockImplementation(async (route, options) => {
    if (route === "/api/worktrees") {
      expect(options?.method).not.toBe("POST");
      return {
        project_root: "/workspace",
        worktrees: [],
        operations: pending ? [operation] : [],
      } as never;
    }
    expect(route).toBe("/api/worktrees/operations/original");
    expect(options?.method).toBe("POST");
    pending = false;
    return { operation: { ...operation, state: "completed" } } as never;
  });
  render(<WorktreesPanel onClose={() => {}} />);
  fireEvent.click(
    await screen.findByRole("button", { name: "Check worktree" }),
  );
  await waitFor(() =>
    expect(
      screen.queryByText("Worktree creation needs review."),
    ).not.toBeInTheDocument(),
  );
  expect(
    vi
      .mocked(api)
      .mock.calls.filter(
        ([route, options]) =>
          route === "/api/worktrees" && options?.method === "POST",
      ),
  ).toHaveLength(0);
});
it("reuses a creation identity after an interrupted response", async () => {
  const ids: string[] = [];
  vi.mocked(api).mockImplementation(async (route, options) => {
    if (!options?.method)
      return { project_root: "/workspace", worktrees: [] } as never;
    expect(route).toBe("/api/worktrees");
    ids.push(JSON.parse(options.body as string).request_id);
    if (ids.length === 1) throw new ApiError("Response interrupted", 502);
    return {
      path: "/workspace/tree",
      branch: "agent",
      root: "/workspace",
    } as never;
  });
  render(<WorktreesPanel onClose={() => {}} />);
  fireEvent.click(screen.getByRole("button", { name: "New" }));
  fireEvent.click(screen.getByRole("button", { name: "Create Worktree" }));
  await screen.findByText("Response interrupted");
  await waitFor(() =>
    expect(
      screen.getByRole("button", { name: "Create Worktree" }),
    ).toBeEnabled(),
  );
  fireEvent.click(screen.getByRole("button", { name: "Create Worktree" }));
  await waitFor(() => expect(ids).toHaveLength(2));
  expect(ids[0]).toBeTruthy();
  expect(ids[1]).toBe(ids[0]);
});
it("only offers cleanup for managed checkouts and preserves request identity on interruption", async () => {
  const ids: string[] = [];
  vi.mocked(api).mockImplementation(async (route, options) => {
    if (!options?.method)
      return {
        project_root: "/workspace",
        worktrees: [
          { path: "/workspace", branch: "main" },
          {
            path: "/workspace/tree",
            branch: "kageha/worker",
            creation_id: "owned",
          },
        ],
      } as never;
    expect(route).toBe("/api/worktrees/cleanup");
    const body = JSON.parse(options.body as string);
    expect(body.creation_id).toBe("owned");
    ids.push(body.request_id);
    if (ids.length === 1)
      throw new ApiError("Cleanup response interrupted", 502);
    return {
      cleanup: { id: ids[0], creation_id: "owned", state: "completed" },
    } as never;
  });
  render(<WorktreesPanel onClose={() => {}} />);
  fireEvent.click(
    await screen.findByRole("button", { name: "Remove unchanged" }),
  );
  await screen.findByText("Cleanup response interrupted");
  const button = screen.getByRole("button", { name: "Remove unchanged" });
  await waitFor(() => expect(button).toBeEnabled());
  fireEvent.click(button);
  await waitFor(() => expect(ids).toHaveLength(2));
  expect(ids[0]).toBeTruthy();
  expect(ids[1]).toBe(ids[0]);
});
it("checks unknown cleanup without replaying removal and explicitly resumes the next stage", async () => {
  let state = "removing";
  vi.mocked(api).mockImplementation(async (route, options) => {
    if (!options?.method)
      return {
        project_root: "/workspace",
        worktrees: [],
        cleanup_operations:
          state === "completed"
            ? []
            : [{ id: "original-cleanup", creation_id: "owned", state }],
      } as never;
    if (route === "/api/worktrees/cleanup/original-cleanup") {
      state = "worktree_removed";
      return { cleanup: { state } } as never;
    }
    expect(route).toBe("/api/worktrees/cleanup");
    expect(JSON.parse(options.body as string)).toEqual({
      request_id: "original-cleanup",
      creation_id: "owned",
    });
    state = "completed";
    return { cleanup: { state } } as never;
  });
  render(<WorktreesPanel onClose={() => {}} />);
  fireEvent.click(await screen.findByRole("button", { name: "Check cleanup" }));
  expect(
    vi
      .mocked(api)
      .mock.calls.filter(([route]) => route === "/api/worktrees/cleanup"),
  ).toHaveLength(0);
  fireEvent.click(
    await screen.findByRole("button", { name: "Resume cleanup" }),
  );
  await waitFor(() =>
    expect(
      screen.queryByRole("button", { name: "Resume cleanup" }),
    ).not.toBeInTheDocument(),
  );
});
