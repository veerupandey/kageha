// External executable uses the same Rust example against embedded and service ports.
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";
import { spawn, execFileSync } from "node:child_process";
const root = fs.mkdtempSync("/tmp/kageha-credential-examples-");
fs.chmodSync(root, 0o700);
const example = path.resolve("target/debug/examples/embedded"),
  binary = path.resolve("target/debug/kageha-credentials");
let child;
const secret = "disposable-example-value";
const write = (n, v) => {
  const p = root + "/" + n;
  fs.writeFileSync(p, v, { mode: 0o600 });
  return p;
};
try {
  const embedded = execFileSync(example, [], {
    env: { PATH: "" },
    encoding: "utf8",
  });
  assert(embedded.includes("revision 1"));
  assert(!embedded.includes(secret));
  const token = crypto.randomBytes(32).toString("hex"),
    socket = root + "/credentials.sock";
  const config = write(
    "host.json",
    JSON.stringify({
      vault: root + "/vault.sqlite",
      key_id: "key",
      key_files: { key: write("master.key", crypto.randomBytes(32)) },
      socket,
      grants: [
        {
          principal: "my-framework",
          namespace: "example",
          ids: ["model"],
          purposes: ["model"],
          destinations: ["https://model.example"],
          permissions: ["write", "resolve"],
        },
      ],
      clients: [
        {
          principal: "my-framework",
          authentication_generation: 1,
          uid: process.getuid(),
          local_token_sha256: crypto
            .createHash("sha256")
            .update(token)
            .digest("hex"),
        },
      ],
    }),
  );
  const log = write("service.log", "");
  child = spawn(binary, ["serve", "--config", config], {
    env: { PATH: "" },
    stdio: ["ignore", fs.openSync(log, "a"), fs.openSync(log, "a")],
  });
  for (let i = 0; i < 200 && !fs.existsSync(socket); i++) {
    if (child.exitCode !== null) throw Error(fs.readFileSync(log, "utf8"));
    await new Promise((r) => setTimeout(r, 20));
  }
  assert(fs.existsSync(socket));
  const connection = write(
    "connection.json",
    JSON.stringify({
      transport: "unix",
      socket,
      token_file: write("client.token", token),
    }),
  );
  const remote = execFileSync(example, [], {
    env: { PATH: "", KAGEHA_CREDENTIAL_EXAMPLE_CONNECTION: connection },
    encoding: "utf8",
  });
  assert(remote.includes("revision 1"));
  assert(!remote.includes(secret));
  assert(!fs.readFileSync(log, "utf8").includes(secret));
  console.log(
    JSON.stringify({
      passed: true,
      external_rust_example: true,
      embedded: true,
      local_service: true,
      python_backend: false,
    }),
  );
} finally {
  if (child && child.exitCode === null && child.signalCode === null) {
    child.kill("SIGTERM");
    await new Promise((r) => {
      const t = setTimeout(() => child.kill("SIGKILL"), 3000);
      child.once("exit", () => {
        clearTimeout(t);
        r();
      });
    });
  }
  fs.rmSync(root, { recursive: true, force: true });
}
