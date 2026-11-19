import { afterEach, expect, it, vi } from "vitest";
import { downloadSession, uploadSession } from "./sessionTransfer";

afterEach(() => { vi.restoreAllMocks(); vi.unstubAllGlobals(); vi.useRealTimers(); });

it("uploads ZIP bytes directly and surfaces the server's archive validation error", async () => {
  const fetcher = vi.fn().mockResolvedValue(new Response(JSON.stringify({ error: "Unsupported archive version" }), { status: 400 }));
  vi.stubGlobal("fetch", fetcher);
  const file = new File(["ZIP bytes"], "chat.zip", { type: "application/zip" });
  await expect(uploadSession(file)).rejects.toThrow("Unsupported archive version");
  const [path, options] = fetcher.mock.calls[0];
  expect(path).toBe("/api/sessions/import");
  expect(options.body).toBe(file);
  expect(options.headers.get("Content-Type")).toBe("application/zip");
});

it("does not download an error response as an archive", async () => {
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response(JSON.stringify({ error: "Stop the running turn" }), { status: 400 })));
  const click = vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(() => {});
  await expect(downloadSession("session-1")).rejects.toThrow("Stop the running turn");
  expect(click).not.toHaveBeenCalled();
});

it("rejects a successful response that is not a ZIP download", async () => {
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response("<html>App shell</html>", { headers: { "Content-Type": "text/html" } })));
  const click = vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(() => {});
  await expect(downloadSession("session-1")).rejects.toThrow("did not return a session archive");
  expect(click).not.toHaveBeenCalled();
});

it("downloads the portable ZIP and releases its temporary URL", async () => {
  vi.useFakeTimers();
  const createObjectURL = vi.fn().mockReturnValue("blob:archive");
  const revokeObjectURL = vi.fn();
  vi.stubGlobal("URL", { createObjectURL, revokeObjectURL });
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response("ZIP bytes", { headers: { "Content-Type": "application/zip" } })));
  let filename = "";
  vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(function (this: HTMLAnchorElement) {
    filename = this.download;
  });
  await downloadSession("session-1");
  expect(filename).toBe("kageha-session-session-1.zip");
  expect(document.querySelector("a[download]")).toBeNull();
  vi.runAllTimers();
  expect(revokeObjectURL).toHaveBeenCalledWith("blob:archive");
});
