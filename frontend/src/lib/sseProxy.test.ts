import { describe, expect, it } from "vitest";
import { applySseProxyHeaders } from "./sseProxy";

describe("Vite SSE proxy headers", () => {
  it("keeps event-stream responses unbuffered and drops Content-Length", () => {
    const headers: Record<string, unknown> = {
      "content-type": "text/event-stream; charset=utf-8",
      "content-length": "99999",
    };
    const set: Record<string, string> = {};
    const applied = applySseProxyHeaders(
      { headers },
      {
        getHeader: (name) => set[name],
        setHeader: (name, value) => {
          set[name] = value;
        },
      },
    );
    expect(applied).toBe(true);
    expect(set["Content-Type"]).toContain("text/event-stream");
    expect(set["Cache-Control"]).toBe("no-cache, no-transform");
    expect(set["X-Accel-Buffering"]).toBe("no");
    expect(set["Content-Encoding"]).toBe("identity");
    expect(headers["content-length"]).toBeUndefined();
    expect(headers["content-encoding"]).toBeUndefined();
  });

  it("leaves non-SSE responses alone", () => {
    const headers = { "content-type": "application/json", "content-length": "12" };
    expect(
      applySseProxyHeaders(
        { headers },
        { getHeader: () => undefined, setHeader: () => {} },
      ),
    ).toBe(false);
    expect(headers["content-length"]).toBe("12");
  });
});
