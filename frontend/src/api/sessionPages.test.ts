import { afterEach, expect, it, vi } from "vitest";
import { loadSessionPages } from "./sessionPages";
afterEach(() => vi.unstubAllGlobals());
const session = (i: number) => ({ session_id: `chat-${i}`, pinned: i === 250, project_id: i === 260 ? "old-project" : null });

it("loads beyond the first page and retains older pinned and project chats", async () => {
  const all = Array.from({ length: 301 }, (_, i) => session(i));
  const fetch = vi.fn(async (url: string) => {
    const query = new URL(url, "http://test").searchParams;
    const offset = Number(query.get("offset") || 0), limit = Number(query.get("limit"));
    return { ok: true, json: async () => ({ sessions: all.slice(offset, offset + limit), next_offset: offset + limit < all.length ? offset + limit : null }) };
  });
  vi.stubGlobal("fetch", fetch);
  const publish = vi.fn();
  await loadSessionPages(40, publish, new AbortController().signal);
  expect(publish.mock.calls[0][0]).toHaveLength(40);
  expect(publish.mock.calls[0][1]).toBe(false);
  expect(publish.mock.lastCall?.[0]).toHaveLength(301);
  expect(publish.mock.lastCall?.[0]).toContainEqual(session(250));
  expect(publish.mock.lastCall?.[0]).toContainEqual(session(260));
  expect(publish.mock.lastCall?.[1]).toBe(true);
  expect(fetch).toHaveBeenCalledTimes(3);
});

it("continues over a page filtered to zero visible sessions", async () => {
  vi.stubGlobal("fetch", vi.fn()
    .mockResolvedValueOnce({ ok: true, json: async () => ({ sessions: [], next_offset: 51 }) })
    .mockResolvedValueOnce({ ok: true, json: async () => ({ sessions: [session(52)], next_offset: null }) }));
  const publish = vi.fn();
  await loadSessionPages(50, publish, new AbortController().signal);
  expect(publish.mock.lastCall).toEqual([[session(52)], true]);
});

it("ignores a response from an aborted older refresh", async () => {
  const controller = new AbortController();
  vi.stubGlobal("fetch", vi.fn(async () => {
    controller.abort();
    return { ok: true, json: async () => ({ sessions: [session(1)], next_offset: 1 }) };
  }));
  const publish = vi.fn();
  await loadSessionPages(50, publish, controller.signal);
  expect(publish).not.toHaveBeenCalled();
});

it("rejects a repeated cursor instead of polling the same page forever", async () => {
  vi.stubGlobal("fetch", vi.fn(async () => ({ ok: true, json: async () => ({ sessions: [], next_offset: 0 }) })));
  await expect(loadSessionPages(50, vi.fn(), new AbortController().signal)).rejects.toThrow("pagination cursor");
});
