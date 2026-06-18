// Disposable single-tenant Linux host: mounted keys, mTLS, restart, rotation, revocation.
import assert from "node:assert/strict";
import fs from "node:fs";
import https from "node:https";
import crypto from "node:crypto";
import { execFileSync } from "node:child_process";
import { CredentialClient } from "../../../sdk/typescript/credentials.mjs";
const image =
  process.env.KAGEHA_CREDENTIALS_IMAGE || "kageha-credentials-validation:local";
const root = fs.mkdtempSync("/tmp/kageha-credential-linux-");
fs.chmodSync(root, 0o700);
const name = "kageha-vault-test-" + crypto.randomUUID().slice(0, 8),
  volume = name + "-data";
const docker = (...args) =>
  execFileSync("docker", args, {
    encoding: "utf8",
    stdio: ["pipe", "pipe", "pipe"],
  });
const file = (n) => root + "/" + n;
const write = (n, s) => fs.writeFileSync(file(n), s, { mode: 0o600 });
const openssl = (args) =>
  execFileSync("/usr/bin/openssl", args, { stdio: "ignore" });
const value = "LINUX-CREDENTIAL-CANARY-" + crypto.randomUUID();
write(
  "ca.cnf",
  "[req]\ndistinguished_name=dn\nx509_extensions=ext\nprompt=no\n[dn]\nCN=Fixture CA\n[ext]\nbasicConstraints=critical,CA:TRUE\nkeyUsage=critical,keyCertSign,cRLSign\n",
);
write(
  "leaf.cnf",
  "[req]\ndistinguished_name=dn\nprompt=no\n[dn]\nCN=localhost\n[ext]\nsubjectAltName=DNS:localhost\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth,clientAuth\n",
);
openssl([
  "req",
  "-x509",
  "-newkey",
  "rsa:2048",
  "-nodes",
  "-days",
  "1",
  "-keyout",
  file("ca.key"),
  "-out",
  file("ca.pem"),
  "-config",
  file("ca.cnf"),
]);
openssl([
  "req",
  "-newkey",
  "rsa:2048",
  "-nodes",
  "-keyout",
  file("key.pem"),
  "-out",
  file("leaf.csr"),
  "-config",
  file("leaf.cnf"),
]);
openssl([
  "x509",
  "-req",
  "-in",
  file("leaf.csr"),
  "-CA",
  file("ca.pem"),
  "-CAkey",
  file("ca.key"),
  "-CAcreateserial",
  "-days",
  "1",
  "-out",
  file("cert.pem"),
  "-extfile",
  file("leaf.cnf"),
  "-extensions",
  "ext",
]);
const cert = new crypto.X509Certificate(fs.readFileSync(file("cert.pem")));
write("master.key", crypto.randomBytes(32));
write("next.key", crypto.randomBytes(32));
write(
  "host.json",
  JSON.stringify({
    vault: "/state/vault.sqlite",
    key_id: "master",
    key_files: { master: "/state/master.key", next: "/state/next.key" },
    bind: "0.0.0.0:8443",
    server_cert: "/state/cert.pem",
    server_key: "/state/key.pem",
    client_ca: "/state/ca.pem",
    grants: [
      {
        principal: "operator",
        namespace: "models",
        ids: ["*"],
        purposes: ["admin"],
        destinations: ["local"],
        permissions: [
          "metadata",
          "resolve",
          "write",
          "revoke",
          "export",
          "admin",
        ],
      },
      {
        principal: "operator",
        namespace: "system",
        ids: ["*"],
        purposes: ["admin"],
        destinations: ["local"],
        permissions: ["admin"],
      },
    ],
    clients: [
      {
        principal: "operator",
        authentication_generation: 1,
        management: true,
        certificate_sha256: crypto
          .createHash("sha256")
          .update(cert.raw)
          .digest("hex"),
      },
    ],
  }),
);
let started = false;
try {
  docker("volume", "create", volume);
  const archive = execFileSync("/usr/bin/tar", ["-cf", "-", "-C", root, "."]);
  execFileSync(
    "docker",
    [
      "run",
      "--rm",
      "-i",
      "--user",
      "0:0",
      "--entrypoint",
      "/bin/sh",
      "-v",
      volume + ":/state",
      image,
      "-c",
      "tar --no-same-owner -xf - -C /state && chown -R 10001:10001 /state && chmod 700 /state && chmod 600 /state/*",
    ],
    { input: archive, stdio: ["pipe", "pipe", "pipe"] },
  );
  docker(
    "run",
    "--detach",
    "--name",
    name,
    "--read-only",
    "--cap-drop",
    "ALL",
    "--security-opt",
    "no-new-privileges:true",
    "-p",
    "127.0.0.1::8443",
    "-v",
    volume + ":/state",
    image,
    "serve",
    "--config",
    "/state/host.json",
  );
  started = true;
  let port = docker("port", name, "8443/tcp").trim().split(":").at(-1);
  const transport = (route, body) =>
    new Promise((resolve, reject) => {
      const req = https.request(
        {
          host: "localhost",
          port,
          servername: "localhost",
          path: route,
          method: "POST",
          ca: fs.readFileSync(file("ca.pem")),
          cert: fs.readFileSync(file("cert.pem")),
          key: fs.readFileSync(file("key.pem")),
          agent: false,
          headers: { "content-type": "application/json" },
          timeout: 5000,
        },
        (res) => {
          let bytes = "";
          res.on("data", (b) => {
            bytes += b;
            assert(bytes.length < 256 * 1024);
          });
          res.on("end", () =>
            res.statusCode >= 200 && res.statusCode < 300
              ? resolve(res.statusCode === 204 ? null : JSON.parse(bytes))
              : reject(Error("Credential request rejected " + res.statusCode)),
          );
        },
      );
      req.on("error", reject);
      req.on("timeout", () => req.destroy());
      req.end(JSON.stringify(body));
    });
  const client = new CredentialClient(transport),
    reference = { namespace: "models", id: "primary" },
    access = { purpose: "admin", destination: "local" };
  for (let i = 0; i < 100; i++) {
    try {
      await transport("/v1/info", {});
      break;
    } catch (e) {
      if (i === 99) throw e;
      await new Promise((r) => setTimeout(r, 50));
    }
  }
  await client.put(reference, access, {
    operationId: "create",
    expectedRevision: 0,
    kind: "api_key",
    value: Buffer.from(value),
  });
  const lease = await client.resolve(reference, access);
  assert.equal(Buffer.from(lease.expose()).toString(), value);
  await client.validate(lease, access);
  const output = docker(
    "exec",
    name,
    "kageha-credentials",
    "rotate-key",
    "--config",
    "/state/host.json",
    "--next-key-id",
    "next",
  );
  assert.equal(JSON.parse(output).generation, 2);
  await assert.rejects(() => client.validate(lease, access));
  lease.dispose();
  const rotated = await client.resolve(reference, access);
  assert.equal(Buffer.from(rotated.expose()).toString(), value);
  rotated.dispose();
  docker("restart", "--time", "40", name);
  port = docker("port", name, "8443/tcp").trim().split(":").at(-1);
  for (let i = 0; i < 100; i++) {
    try {
      await client.metadata(reference, access);
      break;
    } catch (e) {
      if (i === 99) throw e;
      await new Promise((r) => setTimeout(r, 50));
    }
  }
  await client.revoke(reference, access, {
    operationId: "revoke",
    expectedRevision: 1,
  });
  await assert.rejects(() => client.resolve(reference, access));
  assert.equal(
    (await client.operation(reference, access, "create")).metadata.revision,
    1,
  );
  const raw = execFileSync("docker", [
    "exec",
    name,
    "cat",
    "/state/vault.sqlite",
  ]);
  assert(!raw.includes(Buffer.from(value)));
  assert(!docker("logs", name).includes(value));
  assert.equal(
    docker(
      "exec",
      name,
      "/bin/sh",
      "-c",
      "command -v python || command -v python3 || true",
    ).trim(),
    "",
  );
  await assert.rejects(
    () =>
      new Promise((resolve, reject) => {
        const req = https.get(
          {
            host: "localhost",
            port,
            servername: "localhost",
            path: "/v1/info",
            ca: fs.readFileSync(file("ca.pem")),
            agent: false,
          },
          resolve,
        );
        req.on("error", reject);
      }),
  );
  console.log(
    JSON.stringify({
      passed: true,
      linux_container: true,
      mutual_tls: true,
      key_rotation: true,
      restart: true,
      revocation: true,
      python_backend: false,
      plaintext_absent: true,
    }),
  );
} finally {
  if (started)
    try {
      docker("rm", "--force", name);
    } catch {}
  try {
    docker("volume", "rm", volume);
  } catch {}
  fs.rmSync(root, { recursive: true, force: true });
}
