import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { expect, it, vi } from "vitest";
import { api } from "../api/client";
import { PastChatAttachments } from "./PastChatAttachments";

vi.mock("../api/client", () => ({ api: vi.fn() }));

it("searches metadata and attaches only after a specific source is selected", async () => {
  const request = vi.mocked(api);
  request.mockImplementation(async (_path, options) => (
    options?.method === "POST" ? { path: "inputs/current.txt" } : {
      files: [{ filename: "Résumé.txt", sources: [
        { chat_id: "old", title: "Earlier chat", filename: "Résumé.txt", path: "artifacts/resume.txt", updated_at: 1, sha256: "selected-hash" },
      ] }],
      truncated: false,
    }
  ) as never);
  render(<PastChatAttachments sessionId="current" />);
  const dialog = screen.getByLabelText("Attach from past chats") as HTMLDialogElement;
  dialog.showModal = () => { dialog.setAttribute("open", ""); };

  fireEvent.click(screen.getByRole("button", { name: "From past chats" }));
  fireEvent.change(screen.getByRole("textbox", { name: "Filename" }), { target: { value: "Résumé" } });
  const attach = await screen.findByRole("button", { name: "Attach Résumé.txt from Earlier chat" });
  expect(request).toHaveBeenCalledWith(
    "/api/sessions/current/files/from-past-chats?q=R%C3%A9sum%C3%A9",
    expect.objectContaining({ signal: expect.anything() }),
  );
  expect(request.mock.calls.every(([, options]) => options?.method !== "POST")).toBe(true);

  fireEvent.click(attach);
  await waitFor(() => expect(screen.getByRole("status").textContent).toContain("Attached Résumé.txt to this chat."));
  expect(request).toHaveBeenCalledWith("/api/sessions/current/files/from-past-chats", {
    method: "POST",
    body: JSON.stringify({ chat_id: "old", path: "artifacts/resume.txt", sha256: "selected-hash" }),
  });
});

it("keeps the picker usable when the server returns an older response", async () => {
  vi.mocked(api).mockResolvedValue({});
  render(<PastChatAttachments sessionId="current" />);
  const dialog = screen.getByLabelText("Attach from past chats") as HTMLDialogElement;
  dialog.showModal = () => { dialog.setAttribute("open", ""); };

  fireEvent.click(screen.getByRole("button", { name: "From past chats" }));
  expect(await screen.findByText("Past-chat attachments are unavailable from this server.")).toBeTruthy();
  expect(screen.getByRole("textbox", { name: "Filename" })).toBeTruthy();
});
