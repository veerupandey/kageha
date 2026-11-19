// A separately supervised tunnel keeps phone links alive through gateway reloads.
import { pageServer } from "./results.mjs";
import { once } from "node:events";
import fs from "node:fs";
import path from "node:path";
import { spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { DatabaseSync } from "node:sqlite";
const cfg = JSON.parse(
  fs.readFileSync(path.join(process.argv[2], "personal.json")),
);
if (!cfg.quickShareTunnel || cfg.shareOrigin)
  throw Error("Configure a dedicated result listener");
const db = new DatabaseSync(cfg.database);
db.exec("PRAGMA busy_timeout=5000");
db.prepare("DELETE FROM meta WHERE id='share-origin'").run();
const pages = pageServer(db);
pages.listen(0, "127.0.0.1");
await once(pages, "listening");
const child = spawn(
  cfg.cloudflared || "cloudflared",
  [
    "tunnel",
    "--no-autoupdate",
    "--url",
    "http://127.0.0.1:" + pages.address().port,
  ],
  { stdio: ["ignore", "ignore", "pipe"] },
);
for (const sig of ["SIGTERM", "SIGINT"]) process.on(sig, () => child.kill(sig));
for await (const line of createInterface({ input: child.stderr })) {
  const origin = line.match(/https:\/\/[a-z0-9-]+\.trycloudflare\.com\b/)?.[0];
  if (origin)
    db.prepare("INSERT OR REPLACE INTO meta VALUES('share-origin',?)").run(
      origin,
    );
}
db.prepare("DELETE FROM meta WHERE id='share-origin'").run();

pages.close();
