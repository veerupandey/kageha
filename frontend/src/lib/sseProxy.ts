/** Keep SSE responses unbuffered when a reverse proxy forwards /api. */
export function applySseProxyHeaders(
  proxyRes: { headers: Record<string, unknown> },
  res: {
    getHeader(name: string): unknown;
    setHeader(name: string, value: string): void;
  },
): boolean {
  const ctype = String(proxyRes.headers["content-type"] || "");
  if (!ctype.includes("text/event-stream")) return false;
  if (!res.getHeader("Content-Type")) {
    res.setHeader("Content-Type", ctype || "text/event-stream; charset=utf-8");
  }
  res.setHeader("Cache-Control", "no-cache, no-transform");
  res.setHeader("X-Accel-Buffering", "no");
  res.setHeader("Content-Encoding", "identity");
  delete proxyRes.headers["content-length"];
  delete proxyRes.headers["content-encoding"];
  return true;
}
