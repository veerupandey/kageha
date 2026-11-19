import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { MemoryClient } from "./client.mjs";
test(
  "standalone TypeScript writers use persistent backend epochs",
  { timeout: 15000 },
  async () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), "kageha-ts-sdk-"));
    const token = "typescript-sdk-test-token-at-least-thirty-two-bytes";
    fs.writeFileSync(path.join(root, "token"), token, { mode: 0o600 });
    const binary =
      process.env.KAGEHA_MEMORY_BIN ||
      path.resolve(
        path.dirname(fileURLToPath(import.meta.url)),
        "../../target/debug/kageha-memory",
      );
    const child = spawn(
      binary,
      [
        "--state",
        path.join(root, "state.sqlite"),
        "--bind",
        "127.0.0.1:0",
        "--token-file",
        path.join(root, "token"),
      ],
      { env: { PATH: "" }, stdio: ["ignore", "ignore", "pipe"] },
    );
    try {
      const address = await new Promise((resolve, reject) => {
        let log = "";
        child.on("error", reject);
        child.on("exit", (code) =>
          reject(Error(`Memory exited: ${code} ${log}`)),
        );
        child.stderr.on("data", (chunk) => {
          log += chunk.toString();
          const match = log.match(/memory listening on ([^\r\n]+)\r?\n/);
          if (match) resolve(match[1]);
        });
      });
      const client = new MemoryClient("http://" + address, token);
      const first = await client.claimWriter(
        "typescript-owner",
        0,
        "claim-first",
      );
      assert.deepEqual(
        await client.claimWriter("typescript-owner", 0, "claim-first"),
        first,
      );
      const writer = client.withWriter(first),
        access = { namespace: "typescript", actor: "test" };
      const request = {
        request_id: "ts-put",
        access,
        id: "note",
        kind: "note",
        body: "saved by TypeScript",
      };
      await writer.call({ operation: "put", request });
      await assert.rejects(
        client.call({ operation: "put", request }),
        (e) => e.code === "stale_generation",
      );
      await client.retireWriter(first);
      await assert.rejects(
        writer.call({ operation: "put", request }),
        (e) => e.code === "stale_generation",
      );
      const second = await client.claimWriter(
        "typescript-owner",
        first.epoch,
        "claim-second",
      );
      assert(second.epoch > first.epoch);
      const active = client.withWriter(second);
      const saved = await active.call({ operation: "get", access, id: "note" });
      assert.equal(saved.body, "saved by TypeScript");
      await active.call({
        operation: "delete",
        request: {
          request_id: "ts-delete",
          access,
          id: "note",
          expected_revision: saved.revision,
        },
      });
      assert.equal(
        await active.call({ operation: "get", access, id: "note" }),
        null,
      );
    } finally {
      const exit =
        child.exitCode === null
          ? new Promise((resolve) => child.once("exit", resolve))
          : Promise.resolve();
      child.kill("SIGTERM");
      await exit;
      fs.rmSync(root, { recursive: true, force: true });
    }
  },
);
