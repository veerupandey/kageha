// Test-only key provisioning. Never invoke the user's Keychain or reuse their vault key.
import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";
export function credentialFixture(root) {
  const key = path.join(root, "fixture-credential-master.key");
  if (!fs.existsSync(key))
    fs.writeFileSync(key, crypto.randomBytes(32), { mode: 0o600 });
  return { KAGEHA_CREDENTIAL_KEY_FILE: key };
}

// Optional full-adapter service mode. Only disposable fixture databases are preseeded.
export async function credentialServiceFixture(root) {
  if (process.env.KAGEHA_TEST_CREDENTIAL_MODE !== "service")
    return { env: {}, child: null };
  const { spawn, execFileSync } = await import("node:child_process");
  const owner = "app:" + crypto.randomUUID();
  const state = path.join(root, "state");
  fs.mkdirSync(state, { recursive: true, mode: 0o700 });
  execFileSync("/usr/bin/sqlite3", [
    path.join(state, "native-chats.sqlite"),
    `CREATE TABLE ui_credential_owner(singleton INTEGER PRIMARY KEY CHECK(singleton=1),owner TEXT NOT NULL); INSERT INTO ui_credential_owner VALUES(1,'${owner}');`,
  ]);
  const socket = path.join(root, "fixture-credential-service.sock"),
    destinations = ["embedded", "service"];
  const parts = ["models", "oauth", "mcp", "media", "channels"];
  const grants = parts.map((p) => ({
    principal: p,
    namespace: owner + "/" + p,
    ids: ["*"],
    purposes: ["compat"],
    destinations: ["local"],
    permissions: p === "oauth" ? ["metadata", "resolve", "write", "revoke", "export"] : ["metadata", "resolve", "write", "revoke"],
  }));
  grants.push({
    principal: "migration",
    namespace: owner + "/oauth",
    ids: ["*"],
    purposes: ["migration"],
    destinations: ["local"],
    permissions: ["export"],
  });
  for (const namespace of ["system", ...parts.map((p) => owner + "/" + p)])
    grants.push({
      principal: "replacement",
      namespace,
      ids: ["*"],
      purposes: ["replacement"],
      destinations,
      permissions: ["admin", "export", "write"],
    });
  const clients = [],
    workloads = {};
  for (const principal of [...parts, "migration", "replacement"]) {
    const token = crypto.randomBytes(32).toString("hex"),
      tokenFile = path.join(root, "fixture-" + principal + ".token");
    fs.writeFileSync(tokenFile, token, { mode: 0o600 });
    clients.push({
      principal,
      authentication_generation: 1,
      management: principal === "replacement",
      uid: process.getuid(),
      local_token_sha256: crypto
        .createHash("sha256")
        .update(token)
        .digest("hex"),
    });
    workloads[principal] = { transport: "unix", socket, token_file: tokenFile };
  }
  const config = path.join(root, "fixture-credential-service.json"),
    key = path.join(root, "fixture-service-master.key");
  fs.writeFileSync(key, crypto.randomBytes(32), { mode: 0o600 });
  fs.writeFileSync(
    config,
    JSON.stringify({
      vault: path.join(root, "fixture-service-vault.sqlite"),
      key_id: "fixture",
      key_files: { fixture: key },
      socket,
      grants,
      clients,
    }),
    { mode: 0o600 },
  );
  const log = path.join(root, "fixture-credential-service.log"),
    fd = fs.openSync(log, "a", 0o600);
  const child = spawn(
    process.env.KAGEHA_CREDENTIALS_BIN ||
      path.resolve("target/debug/kageha-credentials"),
    ["serve", "--config", config],
    { env: { PATH: "" }, stdio: ["ignore", fd, fd] },
  );
  fs.closeSync(fd);
  for (let i = 0; i < 200 && !fs.existsSync(socket); i++) {
    if (child.exitCode !== null) throw Error(fs.readFileSync(log, "utf8"));
    await new Promise((r) => setTimeout(r, 20));
  }
  if (!fs.existsSync(socket))
    throw Error("Credential fixture service did not start");
  const backends = path.join(root, "fixture-credential-backends.json");
  fs.writeFileSync(
    backends,
    JSON.stringify([{ id: "service", device_id: owner, workloads }]),
    { mode: 0o600 },
  );
  return { env: { KAGEHA_CREDENTIAL_BACKENDS: backends }, child };
}
export async function switchCredentialFixture(ok) {
  if (process.env.KAGEHA_TEST_CREDENTIAL_MODE !== "service") return;
  await ok("/api/credentials/revoke", { name: "fixture-probe" });
  const decision = { id: "explicit-fixture-service", destination: "service" };
  await ok("/api/credentials/replacements", {
    action: "request",
    destination: "service",
    operation_id: "fixture-service-switch",
    decision,
  });
  const result = await ok("/api/credentials/replacements", {
    action: "resume",
    destination: "service",
    operation_id: "fixture-service-switch",
  });
  if (result.stage !== "verified")
    throw Error("Credential fixture switch blocked: " + JSON.stringify(result));
}
