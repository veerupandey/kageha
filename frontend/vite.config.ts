import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import type { IncomingMessage, ServerResponse } from "node:http";
import { applySseProxyHeaders } from "./src/lib/sseProxy.ts";

const backendUrl = process.env.KAGEHA_WEBUI_BACKEND_URL || "http://127.0.0.1:8788";

// https://vite.dev/config/
export default defineConfig({
  plugins: [react(), tailwindcss()],
  server: {
    port: 5173,
    // Set VITE_ALLOWED_HOSTS for a remote deployment (comma-separated). Keep
    // the default permissive only for local development; production should use
    // the reverse proxy and an explicit host allowlist.
    allowedHosts: process.env.VITE_ALLOWED_HOSTS
      ? process.env.VITE_ALLOWED_HOSTS.split(",").map((host) => host.trim()).filter(Boolean)
      : true,
    proxy: {
      "/login": {
        target: backendUrl,
        changeOrigin: true,
        xfwd: true,
      },
      "/logout": {
        target: backendUrl,
        changeOrigin: true,
        xfwd: true,
      },
      "/api": {
        target: backendUrl,
        changeOrigin: true,
        // Preserve the original client address so LAN requests cannot inherit
        // the backend's loopback-only authentication exemption.
        xfwd: true,
        ws: true,
        // Long-running agent turns + SSE — do not time out mid-stream.
        timeout: 0,
        proxyTimeout: 0,
        configure: (proxy) => {
          proxy.on(
            "proxyRes",
            (
              proxyRes: IncomingMessage,
              _req: IncomingMessage,
              res: ServerResponse & { flush?: () => void },
            ) => {
              applySseProxyHeaders(
                proxyRes as IncomingMessage & { headers: Record<string, unknown> },
                res,
              );
              const ctype = String(proxyRes.headers["content-type"] || "");
              if (!ctype.includes("text/event-stream")) return;
              const flush = () => {
                if (typeof res.flush === "function") res.flush();
              };
              proxyRes.on("data", flush);
            },
          );
        },
      },
    },
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
    sourcemap: true,
    // Group genuinely-shared vendor libs into stable, cache-friendly chunks.
    // Mermaid/cytoscape are intentionally left alone so Vite keeps their
    // lazy per-diagram dynamic-import splitting (loaded only when a diagram
    // actually renders) rather than eagerly merging them into one big chunk.
    rollupOptions: {
      output: {
        advancedChunks: {
          groups: [
            { name: "katex", test: /node_modules\/katex/ },
            { name: "highlight", test: /node_modules\/highlight\.js/ },
            { name: "markdown", test: /node_modules\/(marked|dompurify)/ },
            { name: "react-vendor", test: /node_modules\/(react|react-dom|scheduler)\// },
          ],
        },
      },
    },
    // katex (~260 kB, lazy-loaded for math) and the eagerly-imported mermaid
    // engine are known-large, already code-split vendor bundles. Raise the
    // advisory ceiling so it flags genuinely new regressions, not these.
    chunkSizeWarningLimit: 3500,
  },
});
