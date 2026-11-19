import { afterEach, describe, expect, it, vi } from "vitest";
import { UPLOAD_TIMEOUT_MS, uploadSessionFiles } from "./uploadAttachments";

describe("uploadSessionFiles", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("uploads all files in parallel and preserves order", async () => {
    let inFlight = 0;
    let maxInFlight = 0;

    vi.spyOn(global, "fetch").mockImplementation(async (_url, init) => {
      const body = init?.body as FormData;
      const file = body.get("file") as File;
      inFlight += 1;
      maxInFlight = Math.max(maxInFlight, inFlight);
      const delay = file.name === "a.png" ? 40 : 5;
      await new Promise((resolve) => setTimeout(resolve, delay));
      inFlight -= 1;
      return {
        ok: true,
        json: async () => ({ path: `inputs/${file.name}` }),
      } as Response;
    });

    const files = [
      new File(["a"], "a.png", { type: "image/png" }),
      new File(["b"], "b.png", { type: "image/png" }),
      new File(["c"], "c.md", { type: "text/markdown" }),
    ];
    const paths = await uploadSessionFiles("sess1", files);

    expect(paths).toEqual(["inputs/a.png", "inputs/b.png", "inputs/c.md"]);
    expect(maxInFlight).toBeGreaterThan(1);
  });

  it("returns an empty list for no files", async () => {
    const fetchSpy = vi.spyOn(global, "fetch");
    await expect(uploadSessionFiles("sess1", [])).resolves.toEqual([]);
    expect(fetchSpy).not.toHaveBeenCalled();
  });
});

it("retains successful uploads for retry and reports completed-file progress", async () => {
  const attempts: string[] = [];
  const fetchMock = vi.spyOn(global, "fetch").mockImplementation(async (_url, init) => {
    const name = ((init?.body as FormData).get("file") as File).name;
    attempts.push(name);
    if (name === "b.txt" && attempts.filter(n => n === name).length === 1) throw new Error("offline");
    return { ok: true, json: async () => ({path:`inputs/${name}`}) } as Response;
  });
  const files = [new File(["a"], "a.txt"), new File(["b"], "b.txt")];
  const progress = vi.fn();
  await expect(uploadSessionFiles("retry", files, progress)).rejects.toThrow("offline");
  expect(progress).toHaveBeenLastCalledWith(1, 2);
  await expect(uploadSessionFiles("retry", files, progress)).resolves.toEqual(["inputs/a.txt", "inputs/b.txt"]);
  expect(attempts).toEqual(["a.txt", "b.txt", "b.txt"]);
  expect(progress).toHaveBeenLastCalledWith(2, 2);
  fetchMock.mockRestore();
});

it("rejects a successful HTTP response that has no uploaded path", async () => {
  const fetchMock = vi.spyOn(global, "fetch").mockResolvedValue({ok:true,json:async()=>({})} as Response);
  await expect(uploadSessionFiles("missing", [new File(["x"], "x.txt")])).rejects.toThrow("Upload did not complete");
  fetchMock.mockRestore();
});

it("ends a hung upload without resubmitting, and keeps successful files for explicit retry", async () => {
  const deadline = new AbortController();
  const timeoutSpy = vi.spyOn(AbortSignal, "timeout").mockReturnValue(deadline.signal);
  let hanging = true;
  const attempts: string[] = [];
  const fetchMock = vi.spyOn(global, "fetch").mockImplementation(async (_url, init) => {
    const name = ((init?.body as FormData).get("file") as File).name;
    attempts.push(name);
    if (name === "b.txt" && hanging) {
      return new Promise((_resolve, reject) => init!.signal!.addEventListener("abort", () => reject(init!.signal!.reason), { once: true }));
    }
    return { ok: true, json: async () => ({ path: `inputs/${name}` }) } as Response;
  });
  try {
    const files = [new File(["a"], "a.txt"), new File(["b"], "b.txt")];
    const progress = vi.fn();
    const pending = uploadSessionFiles("timeout", files, progress);
    const rejection = expect(pending).rejects.toThrow("Uploading the attachment took too long");
    await vi.waitFor(() => expect(progress).toHaveBeenCalledWith(1, 2));
    deadline.abort(new DOMException("Upload deadline", "TimeoutError"));
    await rejection;
    expect(attempts).toEqual(["a.txt", "b.txt"]);
    expect(timeoutSpy).toHaveBeenCalledWith(UPLOAD_TIMEOUT_MS);
    hanging = false;
    timeoutSpy.mockRestore();
    await expect(uploadSessionFiles("timeout", files)).resolves.toEqual(["inputs/a.txt", "inputs/b.txt"]);
    expect(attempts).toEqual(["a.txt", "b.txt", "b.txt"]);
  } finally {
    timeoutSpy.mockRestore();
    fetchMock.mockRestore();
  }
});
