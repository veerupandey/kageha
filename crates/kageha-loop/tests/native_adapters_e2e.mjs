import { credentialFixture,credentialServiceFixture,switchCredentialFixture } from "./credential_fixture.mjs";
// Native provider, document, OAuth, media, SSH and channel contracts without Python.
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import https from "node:https";
import { spawn, execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
const crate = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const binary =
  process.env.KAGEHA_LOOP_BIN ||
  path.resolve(crate, "../../target/release/kageha-loop");
const root = fs.mkdtempSync("/tmp/kageha-native-adapters-");
const token = "native-adapters-test-authentication-token";
const children = [];
const realSshImage = process.env.KAGEHA_REAL_SSH_IMAGE;
let sshContainer;
const records = [];
let base, child;
let sent = [];
let updates = [];
let cloneReference;
let releaseToken;
let devicePolls = 0;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function until(fn) {
  const end = Date.now() + 20000;
  while (Date.now() < end) {
    const v = await fn();
    if (v) return v;
    await sleep(50);
  }
  throw Error("Timed out");
}
const wav = Buffer.alloc(44 + 16000);
wav.write("RIFF");
wav.writeUInt32LE(wav.length - 8, 4);
wav.write("WAVEfmt ", 8);
wav.writeUInt32LE(16, 16);
wav.writeUInt16LE(1, 20);
wav.writeUInt16LE(1, 22);
wav.writeUInt32LE(8000, 24);
wav.writeUInt32LE(16000, 28);
wav.writeUInt16LE(2, 32);
wav.writeUInt16LE(16, 34);
wav.write("data", 36);
wav.writeUInt32LE(16000, 40);
const png = Buffer.from(
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jWZkAAAAASUVORK5CYII=",
  "base64",
);
fs.mkdirSync(path.join(root, "bin"));
fs.mkdirSync(path.join(root, "workspace"));
fs.writeFileSync(
  path.join(root, "isolated-models.yaml"),
  "models: []\nroles: {}\n",
  { mode: 0o600 },
);
fs.writeFileSync(path.join(root, "isolated.env"), "", { mode: 0o600 });
fs.writeFileSync(
  path.join(root, "workspace/notes.txt"),
  "Native adapter evidence",
);
// HTTP adapters run through pooled Rust transport; curl is absent from backend PATH.
// Fast fixture by default; opt-in Docker coverage uses the real OpenSSH client.
if (realSshImage) fs.symlinkSync("/usr/bin/ssh", path.join(root, "bin/ssh"));
else
  fs.writeFileSync(
    path.join(root, "bin/ssh"),
    `#!${process.execPath}\nimport fs from 'node:fs';import {spawnSync} from 'node:child_process';const input=fs.readFileSync(0);const result=spawnSync('/bin/sh',['-c','uname() { printf "Linux\\\\n"; }; '+process.argv.at(-1)],{input,env:{PATH:'/usr/bin:/bin'}});process.stdout.write(result.stdout||'');process.exit(result.status??1);`,
    { mode: 0o700 },
  );
execFileSync(
  "/usr/bin/openssl",
  [
    "req",
    "-x509",
    "-newkey",
    "rsa:2048",
    "-nodes",
    "-days",
    "1",
    "-subj",
    "/CN=127.0.0.1",
    "-addext",
    "subjectAltName=IP:127.0.0.1",
    "-keyout",
    path.join(root, "key.pem"),
    "-out",
    path.join(root, "cert.pem"),
  ],
  { stdio: "ignore" },
);
const fixture = https.createServer(
  {
    key: fs.readFileSync(path.join(root, "key.pem")),
    cert: fs.readFileSync(path.join(root, "cert.pem")),
  },
  async (req, res) => {
    try {
      let chunks = [];
      for await (const c of req) chunks.push(c);
      const bytes = Buffer.concat(chunks);
      const raw = bytes.toString();
      let body;
      try {
        body = JSON.parse(raw);
      } catch {
        body = { raw };
      }
      records.push({ url: req.url, body, headers: req.headers });
      res.setHeader("Content-Type", "application/json");
      if (req.url?.startsWith("/gateway/")) {
        assert.equal(req.headers.authorization, "Bearer gateway-token");
        res.end(
          JSON.stringify(
            req.url.endsWith("/qr")
              ? { qr: "native-whatsapp-fixture", phase: "qr" }
              : req.url.endsWith("/updates")
                ? { updates: [] }
                : { ok: true },
          ),
        );
        return;
      }
      if (req.url === "/github-device") {
        res.end(
          JSON.stringify({
            device_code: "private-device-code",
            user_code: "SHOW-CODE",
            verification_uri: provider + "/verify",
            expires_in: 600,
            interval: 1,
          }),
        );
        return;
      }
      if (req.url === "/github-token") {
        const form = new URLSearchParams(raw);
        assert.equal(form.get("device_code"), "private-device-code");
        devicePolls++;
        res.end(
          JSON.stringify(
            devicePolls === 1
              ? { error: "authorization_pending" }
              : { access_token: "fixture-access-token", expires_in: 3600 },
          ),
        );
        return;
      }
      if (req.url === "/oauth/token") {
        const form = new URLSearchParams(raw);
        assert.equal(form.get("client_secret"), "oauth-client-secret");
        if (form.get("grant_type") === "authorization_code") {
          assert(["fixture-code", "delayed-code"].includes(form.get("code")));
          if (form.get("code") === "delayed-code")
            await new Promise((r) => (releaseToken = r));
          assert.equal(form.get("code_verifier").length, 43);
        }
        res.end(
          JSON.stringify({
            access_token: "fixture-access-token",
            refresh_token: "fixture-refresh-token",
            expires_in:
              form.get("grant_type") === "authorization_code" ? 0 : 3600,
          }),
        );
        return;
      }
      if (req.url === "/profile") {
        assert.equal(req.headers.authorization, "Bearer fixture-access-token");
        res.end(
          JSON.stringify({
            emailAddress: "fixture@example.test",
            id: "fixture-account",
            login: "fixture-user",
          }),
        );
        return;
      }
      if (req.url === "/v1/models") {
        assert.equal(req.headers["x-api-key"], "anthropic-secret");
        res.end(JSON.stringify({ data: [{ id: "fixture-anthropic" }] }));
        return;
      }
      if (req.url === "/v1/messages") {
        assert.equal(req.headers["x-api-key"], "anthropic-secret");
        assert.equal(req.headers["anthropic-version"], "2023-06-01");
        const last = body.messages.at(-1);
        if (last.content.some((v) => v.type === "tool_result")) {
          assert(last.content.some((v) => v.tool_use_id === "anthropic-read"));
          res.end(
            JSON.stringify({
              role: "assistant",
              content: [
                { type: "text", text: "Anthropic native tools verified" },
              ],
            }),
          );
        } else
          res.end(
            JSON.stringify({
              role: "assistant",
              content: [
                {
                  type: "tool_use",
                  id: "anthropic-read",
                  name: "read_file",
                  input: { path: "notes.txt" },
                },
              ],
            }),
          );
        return;
      }
      if (req.url === "/v1/audio/speech" || req.url === "/v1/tts") {
        cloneReference = body.voice_profile;
        res.setHeader("Content-Type", "audio/wav");
        res.end(wav);
        return;
      }
      if (req.url === "/v1/audio/transcriptions") {
        assert(bytes.includes(wav));
        assert(!bytes.includes(Buffer.from('name="language"')));
        res.end(JSON.stringify({ text: "Native transcription verified" }));
        return;
      }
      if (req.url === "/v1/images/generations") {
        res.end(
          JSON.stringify({ data: [{ b64_json: png.toString("base64") }] }),
        );
        return;
      }
      if (req.url?.includes("/botfixture-bot/")) {
        if (req.url.endsWith("/getMe"))
          res.end(
            JSON.stringify({
              ok: true,
              result: { username: "native-fixture" },
            }),
          );
        else if (req.url.endsWith("/getUpdates"))
          res.end(
            JSON.stringify({
              ok: true,
              result: updates.filter((u) => u.update_id >= body.offset),
            }),
          );
        else if (req.url.endsWith("/sendMessage")) {
          sent.push(body);
          res.end(JSON.stringify({ ok: true, result: { message_id: 88 } }));
        } else throw Error(req.url);
        return;
      }
      if (req.url === "/v1/chat/completions") {
        assert(body.messages.every((m) => m.is_error === undefined));
        const last = body.messages.at(-1);
        const user = body.messages.findLast((m) => m.role === "user");
        const text =
          typeof user?.content === "string"
            ? user.content
            : user?.content?.find((b) => b.type === "text")?.text || "";
        let message = { role: "assistant", content: "Native fixture complete" };
        const call = (name, args, id = "native-action") => ({
          role: "assistant",
          content: null,
          tool_calls: [
            {
              id,
              type: "function",
              function: { name, arguments: JSON.stringify(args) },
            },
          ],
        });
        if (text === "discover workspace file") {
          if (last.role !== "tool") message = call("list_workspace_files", {});
          else {
            const listing = JSON.parse(last.content);
            assert(listing.entries.some(([name, kind]) => name === "notes.txt" && kind === "file"));
            assert(!listing.entries.some(([name]) => name === ".private-note" || name === "outside-link"));
            message.content = "Workspace listing verified";
          }
        } else if (text === "discover installed skill") {
          assert.match(JSON.stringify(body.messages), /skills\/routing-proof\/SKILL.md/);
          if (last.role !== "tool") {
            assert.doesNotMatch(JSON.stringify(body.messages), /Use the routing fixture exactly/);
            const tool = body.tools.find(tool => tool.function.name === "skill_read");
            assert.equal(tool.function.parameters.properties.name.type, "string");
            message = call("skill_read", { name: "routing-proof" });
          } else message.content = last.content;
        } else if (text === "read connected Gmail profile") {
          const spec = body.tools.find(tool => tool.function.name === "integration_read");
          assert.deepEqual(spec.function.parameters.properties.integration.enum, ["gmail"]);
          assert.deepEqual(spec.function.parameters.properties.operation.enum, ["status", "search", "message", "thread"]);
          if (last.role !== "tool") message = call("integration_read", { integration: "gmail", path: "profile" });
          else message.content = last.content;
        } else if (text === "Check Gmail for the Seequent interview and prepare system design questions") {
          assert(body.tools.some(tool => tool.function.name === "integration_read"));
          if (last.role !== "tool") message = call("integration_read", { integration: "gmail", operation: "search", query: "Seequent interview" });
          else {
            assert.equal(JSON.parse(last.content).connector_outcome.state, "disconnected");
            message.content = "Please connect Gmail or share the interview invitation. For prep, practice designing a retrieval system and explaining its evaluation plan.";
          }
        } else if (text.startsWith("extract ")) {
          if (last.role !== "tool")
            message = call("read_file", { path: text.slice(8).split("\n")[0] });
          else message.content = last.content;
        } else if (text.startsWith("remote upload ")) {
          if (last.role !== "tool")
            message = call("remote_upload", {
              path: text.slice("remote upload ".length),
              destination: "received/evidence.docx",
            });
          else message.content = last.content;
        } else if (
          text === "remote operation" ||
          text === "failed remote operation" ||
          text === "denied remote mutation"
        ) {
          if (last.role !== "tool")
            message = call("remote_exec", {
              command:
                text === "denied remote mutation"
                  ? "printf forbidden > denied-marker"
                  : text === "failed remote operation"
                    ? "exit 7"
                    : "printf native-remote",
              timeout_s: 5,
            });
          else message.content = last.content;
        } else if (text.startsWith("draw an image")) {
          if (last.role !== "tool")
            message = call("generate_image", { prompt: "fixture" });
          else message.content = last.content;
        } else if (Array.isArray(user?.content)) {
          assert.equal(user.content[1].type, "image_url");
          assert(
            user.content[1].image_url.url.startsWith("data:image/png;base64,"),
          );
          message.content = "Image bytes received";
        } else if (text === "allowed incoming message")
          message.content = "Native channel reply";
        if (text === "channel followup") {
          assert(
            body.messages.some((m) => m.content === "allowed incoming message"),
          );
          assert(
            body.messages.some((m) => m.content === "Native channel reply"),
          );
          message.content = "Channel history preserved";
        }
        res.end(JSON.stringify({ choices: [{ message }] }));
        return;
      }
      throw Error("Unknown fixture route " + req.url);
    } catch (e) {
      res.statusCode = 500;
      res.end(JSON.stringify({ error: String(e) }));
    }
  },
);
await new Promise((r) => fixture.listen(0, "127.0.0.1", r));
const provider = `https://127.0.0.1:${fixture.address().port}`;
async function api(url, body, method = body ? "POST" : "GET") {
  const response = await fetch(base + url, {
    method,
    headers: {
      Authorization: `Bearer ${token}`,
      ...(body ? { "Content-Type": "application/json" } : {}),
    },
    body: body ? JSON.stringify(body) : undefined,
  });
  return { status: response.status, data: await response.json() };
}
async function ok(url, body, method) {
  const r = await api(url, body, method);
  assert.equal(r.status, 200, JSON.stringify(r));
  return r.data;
}
const credentialService=await credentialServiceFixture(root);if(credentialService.child)children.push(credentialService.child);
async function start() {
  const offset = fs.existsSync(path.join(root, "backend.log"))
    ? fs.statSync(path.join(root, "backend.log")).size
    : 0;
  const log = fs.openSync(path.join(root, "backend.log"), "a");
  child = spawn(
    binary,
    [
      "serve",
      path.join(root, "workspace"),
      path.join(root, "state"),
      "127.0.0.1:0",
    ],
    {
      env: {
        PATH: path.join(root, "bin"),
        KAGEHA_MODEL: "fixture",
        KAGEHA_MODELS_FILE: path.join(root, "isolated-models.yaml"),
        KAGEHA_MODEL_ENV_FILE: path.join(root, "isolated.env"),
        KAGEHA_PROVIDER_URL: provider + "/v1/chat/completions",
        ...credentialFixture(root),
        ...credentialService.env,
        KAGEHA_SERVER_TOKEN: token,
        CURL_CA_BUNDLE: path.join(root, "cert.pem"),
        KAGEHA_OAUTH_MODEL_PROVIDERS: JSON.stringify(["native-login"]),
        KAGEHA_OAUTH_PROVIDER_NATIVE_LOGIN_CLIENT_ID:
          "registered-native-client",
        KAGEHA_OAUTH_PROVIDER_NATIVE_LOGIN_CLIENT_SECRET: "oauth-client-secret",
        KAGEHA_OAUTH_PROVIDER_NATIVE_LOGIN_REDIRECT_URI:
          "http://127.0.0.1:9000/api/integrations/provider-native-login/callback",
        KAGEHA_OAUTH_PROVIDER_NATIVE_LOGIN_AUTHORIZE_URL:
          provider + "/authorize",
        KAGEHA_OAUTH_PROVIDER_NATIVE_LOGIN_TOKEN_URL: provider + "/oauth/token",
        KAGEHA_OAUTH_PROVIDER_NATIVE_LOGIN_MODEL_BASE_URL: provider + "/v1",
        KAGEHA_OAUTH_PROVIDER_NATIVE_LOGIN_MODEL: "fixture-oauth",
        KAGEHA_GITHUB_CLIENT_ID: "registered-github-client",
        KAGEHA_OAUTH_GITHUB_DEVICE_URL: provider + "/github-device",
        KAGEHA_OAUTH_GITHUB_TOKEN_URL: provider + "/github-token",
        KAGEHA_OAUTH_GITHUB_PROFILE_URL: provider + "/profile",
        KAGEHA_OAUTH_GMAIL_AUTHORIZE_URL: provider + "/authorize",
        KAGEHA_OAUTH_GMAIL_TOKEN_URL: provider + "/oauth/token",
        KAGEHA_OAUTH_GMAIL_PROFILE_URL: provider + "/profile",
        KAGEHA_OAUTH_GMAIL_API_URL: provider,
      },
      stdio: ["ignore", log, log],
    },
  );
  fs.closeSync(log);
  children.push(child);
  base = await until(() =>
    fs
      .readFileSync(path.join(root, "backend.log"), "utf8")
      .slice(offset)
      .match(/127[.]0[.]0[.]1:[0-9]+/g)
      ?.at(-1),
  );
  base = "http://" + base;
  await until(async () => {
    try {
      return (
        await fetch(base + "/health", {
          headers: { Authorization: `Bearer ${token}` },
        })
      ).ok;
    } catch {
      return false;
    }
  });
}
async function chat() {
  return (await ok("/api/sessions", {})).session_id;
}
async function turn(chat, id, message, extra = {}) {
  return ok("/api/chat/send", {
    session_id: chat,
    client_request_id: id,
    message,
    ...extra,
  });
}
async function upload(chat, name, bytes) {
  const data = new FormData();
  data.append("file", new Blob([bytes]), name);
  const r = await fetch(base + `/api/sessions/${chat}/upload`, {
    method: "POST",
    headers: { Authorization: `Bearer ${token}` },
    body: data,
  });
  assert.equal(r.status, 200);
  return r.json();
}
try {
  await start();
  await switchCredentialFixture(ok);
  const session = await chat();
  const discovered = await ok("/api/provider-setup/discover", {
    base_url: provider + "/v1",
    protocol: "anthropic_compat",
    api_key: "anthropic-secret",
  });
  assert.equal(discovered.models[0].id, "fixture-anthropic");
  await ok("/api/provider-setup/connect", {
    provider_name: "anthropic",
    base_url: provider + "/v1",
    protocol: "anthropic_compat",
    api_key: "anthropic-secret",
    models: ["fixture-anthropic"],
  });
  assert(
    (
      await turn(session, "anthropic", "test native tools", {
        model: "anthropic/fixture-anthropic",
      })
    ).message.includes("verified"),
  );
  // Office extraction uses native ZIP/XML, not unzip, Python, Office or a document server.
  fs.mkdirSync(path.join(root, "office/word"), { recursive: true });
  fs.writeFileSync(
    path.join(root, "office/word/document.xml"),
    '<w:document xmlns:w="urn:w"><w:p><w:r><w:t>Office native evidence</w:t></w:r></w:p></w:document>',
  );
  execFileSync(
    "/usr/bin/zip",
    ["-q", "-r", path.join(root, "reference.docx"), "word"],
    { cwd: path.join(root, "office") },
  );
  const doc = await upload(
    session,
    "reference.docx",
    fs.readFileSync(path.join(root, "reference.docx")),
  );
  assert(
    (await turn(session, "docx", "extract " + doc.path)).message.includes(
      "Office native evidence",
    ),
  );
  const pdfObjects = [
    "<< /Type /Catalog /Pages 2 0 R >>",
    "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>",
    "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
  ];
  const stream = "BT /F1 12 Tf 72 720 Td (Native PDF evidence) Tj ET";
  pdfObjects.push(
    `<< /Length ${stream.length} >>\nstream\n${stream}\nendstream`,
  );
  let pdf = "%PDF-1.4\n";
  const positions = [0];
  for (const [at, obj] of pdfObjects.entries()) {
    positions.push(Buffer.byteLength(pdf));
    pdf += `${at + 1} 0 obj\n${obj}\nendobj\n`;
  }
  const xref = Buffer.byteLength(pdf);
  pdf +=
    `xref\n0 ${positions.length}\n0000000000 65535 f \n` +
    positions
      .slice(1)
      .map((v) => `${String(v).padStart(10, "0")} 00000 n \n`)
      .join("") +
    `trailer\n<< /Size ${positions.length} /Root 1 0 R >>\nstartxref\n${xref}\n%%EOF\n`;
  const pdfUpload = await upload(session, "evidence.pdf", Buffer.from(pdf));
  assert(
    (await turn(session, "pdf", "extract " + pdfUpload.path)).message.includes(
      "Native PDF evidence",
    ),
  );
  const prose = await upload(
    session,
    "prose.txt",
    Buffer.from("Tool failed: this is reference text"),
  );
  assert.equal(
    (await turn(session, "literal-error-text", "extract " + prose.path))
      .message,
    "Tool failed: this is reference text",
  );
  assert.equal(
    (await ok(`/api/sessions/${session}/state`)).turn.failed_calls,
    0,
  );
  const image = await upload(session, "pixel.png", png);
  assert.equal(
    (
      await turn(session, "vision", "inspect image", {
        attachments: [image.path],
      })
    ).message,
    "Image bytes received",
  );
  await ok("/api/media-providers", {
    id: "native-media",
    protocol: "openai_compat",
    base_url: provider + "/v1",
    models: { tts: "speech", stt: "transcription", image: "image" },
  });
  const tts = await fetch(base + `/api/sessions/${session}/tts`, {
    method: "POST",
    headers: {
      Authorization: `Bearer ${token}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({ text: "hello", provider: "native-media" }),
  });
  assert.equal(tts.status, 200);
  assert.deepEqual(Buffer.from(await tts.arrayBuffer()), wav);
  const voice = await ok("/api/voice-profiles", {
    name: "Native voice",
    transcript: "hello",
    audio_base64: wav.toString("base64"),
  });
  assert.equal(voice.profile.duration_seconds, 1);
  assert(
    !JSON.stringify(await ok("/api/voice-profiles")).includes(
      wav.toString("base64"),
    ),
  );
  await ok("/api/media-providers", {
    id: "cloning",
    protocol: "audio8_http",
    base_url: provider + "/v1",
    models: { tts: "clone" },
  });
  const preview = await fetch(base + "/api/voice-preview", {
    method: "POST",
    headers: {
      Authorization: `Bearer ${token}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({ provider: "cloning", voice: voice.profile.id }),
  });
  assert.equal(preview.status, 200);
  assert.equal(cloneReference.audio_base64, wav.toString("base64"));
  const mic = new FormData();
  mic.append("language", "en");
  mic.append("file", new Blob([wav]), "clip.wav");
  const stt = await fetch(base + `/api/sessions/${session}/stt`, {
    method: "POST",
    headers: { Authorization: `Bearer ${token}` },
    body: mic,
  });
  assert.equal(stt.status, 200, await stt.clone().text());
  assert.equal((await stt.json()).text, "Native transcription verified");
  const drawn = await turn(session, "image", "draw an image");
  const artifact = JSON.parse(drawn.message);
  assert(drawn.turn_artifacts.includes(artifact.path));
  assert(
    (await ok("/api/sessions/" + session)).messages.some(
      (m) => m.role === "assistant" && m.attachments?.includes(artifact.path),
    ),
  );
  const imageFile = await fetch(
    base + `/api/sessions/${session}/files/${artifact.path}`,
    { headers: { Authorization: `Bearer ${token}` } },
  );
  assert.deepEqual(Buffer.from(await imageFile.arrayBuffer()), png);
  const setup = {
    client_id: "fixture-client.apps.googleusercontent.com",
    client_secret: "oauth-client-secret",
    redirect_uri: base + "/api/integrations/gmail/callback",
  };
  await ok("/api/integrations/gmail/setup", setup, "PUT");
  const oauth = await ok("/api/integrations/gmail/connect", {});
  const auth = new URL(oauth.authorization_url);
  assert.equal(auth.searchParams.get("code_challenge_method"), "S256");
  assert(!oauth.authorization_url.includes("oauth-client-secret"));
  const state = auth.searchParams.get("state");
  assert.equal(
    (await api("/api/integrations/gmail/callback?state=bad&code=fixture-code"))
      .status,
    400,
  );
  await ok(
    "/api/integrations/gmail/callback?state=" + state + "&code=fixture-code",
  );
  assert.equal(
    (
      await api(
        "/api/integrations/gmail/callback?state=" +
          state +
          "&code=fixture-code",
      )
    ).status,
    400,
  );
  assert.equal(
    (await ok("/api/integrations/gmail/check", {})).email,
    "fixture@example.test",
  );
  assert(
    !JSON.stringify(await ok("/api/integrations/gmail")).includes(
      "fixture-access-token",
    ),
  );
  assert((await ok("/api/integrations/gmail")).agents.includes("main"));
  // Saving a specialist assignment retains main, and stale saves cannot erase it.
  await ok("/api/integrations/gmail/access", { agents: ["research"], expected_agents: ["main"] }, "PUT");
  assert.equal((await api("/api/integrations/gmail/access", { agents: [], expected_agents: ["main"] }, "PUT")).status, 409);
  assert.deepEqual((await ok("/api/integrations/gmail")).agents.sort(), ["main", "research"]);
  const routingChat = await chat();
  await turn(routingChat, "integration-routing", "read connected Gmail profile");
  const routingCards = async () => (await ok(`/api/sessions/${routingChat}/events`)).events
    .map(event => event.tool_card).filter(card => card?.result_preview);
  fs.writeFileSync(path.join(root, "workspace", ".private-note"), "hidden fixture");
  fs.symlinkSync(path.join(root, "cert.pem"), path.join(root, "workspace", "outside-link"));
  assert.equal((await turn(routingChat, "file-discovery", "discover workspace file")).message, "Workspace listing verified");
  assert.match((await routingCards()).find(card => card.name === "integration_read")?.result_preview || "", /fixture@example.test/);
  const skillFolder = path.join(root, "routing-proof");
  fs.mkdirSync(skillFolder);
  fs.writeFileSync(path.join(skillFolder, "SKILL.md"),
    "---\nname: routing-proof\ndescription: Discover the routing fixture\n---\nUse the routing fixture exactly.\n");
  await ok("/api/skills/install", { source: skillFolder });
  await turn(routingChat, "skill-routing", "discover installed skill");
  assert.match((await routingCards()).find(card => card.name === "skill_read")?.result_preview || "", /Use the routing fixture exactly/);
  assert(
    records.some(
      (r) =>
        r.url === "/oauth/token" &&
        r.body.raw.includes("grant_type=refresh_token"),
    ),
  );
  const delayed = await ok("/api/integrations/gmail/connect", {});
  const delayedState = new URL(delayed.authorization_url).searchParams.get(
    "state",
  );
  const callback = api(
    "/api/integrations/gmail/callback?state=" +
      delayedState +
      "&code=delayed-code",
  );
  await until(() => releaseToken);
  await ok("/api/integrations/gmail", undefined, "DELETE");
  releaseToken();
  assert.equal((await callback).status, 409);
  assert.equal((await ok("/api/integrations/gmail")).connected, false);
  const disconnected = await turn(routingChat, "gmail-disconnected-interview",
    "Check Gmail for the Seequent interview and prepare system design questions");
  assert.match(disconnected.message, /connect Gmail.*practice designing a retrieval system/);
  const reconnect = await ok("/api/integrations/gmail/connect", {});
  await ok(
    "/api/integrations/gmail/callback?state=" +
      new URL(reconnect.authorization_url).searchParams.get("state") +
      "&code=fixture-code",
  );
  assert((await ok("/api/integrations/gmail")).agents.includes("main"));
  assert.match((await turn(routingChat, "gmail-reconnected", "read connected Gmail profile")).message, /fixture@example.test/);
  const device = await ok("/api/integrations/github/device", {});
  assert.equal(device.user_code, "SHOW-CODE");
  assert(!JSON.stringify(device).includes("private-device-code"));
  assert.equal(
    (await ok("/api/integrations/github/device/poll", {})).pending,
    true,
  );
  await ok("/api/integrations/github/device/poll", {});
  assert.equal(devicePolls, 1);
  await sleep(1100);
  assert.equal(
    (await ok("/api/integrations/github/device/poll", {})).connected,
    true,
  );
  const modelLogin = await ok("/api/provider-setup/oauth", {
    provider: "native-login",
  });
  const modelState = new URL(modelLogin.authorization_url).searchParams.get(
    "state",
  );
  await ok(
    "/api/integrations/provider-native-login/callback?state=" +
      modelState +
      "&code=fixture-code",
  );
  await turn(session, "oauth-model", "use oauth model", {
    model: "native-login/fixture-oauth",
  });
  assert(
    records.some(
      (r) =>
        r.url === "/v1/chat/completions" &&
        r.body.model === "fixture-oauth" &&
        r.headers.authorization === "Bearer fixture-access-token",
    ),
  );
  assert.equal(
    (await ok("/api/provider-setup")).connections.filter(
      (c) => c.id === "native-login",
    ).length,
    1,
  );
  await ok("/api/integrations/provider-native-login", undefined, "DELETE");
  assert.equal(
    (await ok("/api/models")).all.find(
      (m) => m.id === "native-login/fixture-oauth",
    ).ready,
    false,
  );
  const remoteDir = path.join(root, "remote");
  fs.mkdirSync(remoteDir);
  let sshOptions = { host: "fixture.example" };
  if (realSshImage) {
    const identity = path.join(root, "ssh-fixture-key");
    execFileSync("/usr/bin/ssh-keygen", [
      "-q",
      "-t",
      "ed25519",
      "-N",
      "",
      "-f",
      identity,
    ]);
    sshContainer = `kageha-ssh-${path.basename(root).toLowerCase()}`;
    execFileSync(
      "docker",
      [
        "run",
        "-d",
        "--name",
        sshContainer,
        "-p",
        "127.0.0.1::22",
        "--mount",
        `type=bind,source=${remoteDir},target=${remoteDir}`,
        "--mount",
        `type=bind,source=${identity}.pub,target=/fixture-authorized-key,readonly`,
        realSshImage,
        "sh",
        "-ec",
        "ssh-keygen -A; exec /usr/sbin/sshd -D -e -o PasswordAuthentication=no -o KbdInteractiveAuthentication=no -o PermitRootLogin=prohibit-password -o AuthorizedKeysFile=/fixture-authorized-key -o StrictModes=no",
      ],
      { stdio: "pipe" },
    );
    const mapped = execFileSync("docker", ["port", sshContainer, "22/tcp"], {
      encoding: "utf8",
    }).trim();
    sshOptions = {
      host: "127.0.0.1",
      port: Number(mapped.split(":").at(-1)),
      user: "root",
      identity,
      host_public_key: await until(async () => {
        try {
          return execFileSync("docker", ["exec", sshContainer, "cat", "/etc/ssh/ssh_host_ed25519_key.pub"], { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] }).trim();
        } catch { return false; }
      }),
    };
  }
  await ok("/api/remote/hosts", {
    id: "fixture-host",
    name: "Native SSH",
    ...sshOptions,
    workdir: remoteDir,
  });
  await until(
    async () => (await ok("/api/remote/test", { host_id: "fixture-host" })).ok,
  );
  if (realSshImage) {
    // Testing a different trusted key must fail without changing the saved pin.
    const other = path.join(root, "wrong-host-key");
    execFileSync("/usr/bin/ssh-keygen", [
      "-q",
      "-t",
      "ed25519",
      "-N",
      "",
      "-f",
      other,
    ]);
    assert.equal(
      (await ok("/api/remote/test", { ...sshOptions, workdir: remoteDir, host_public_key: fs.readFileSync(other + ".pub", "utf8").trim() })).ok,
      false,
    );
    assert.equal((await ok("/api/remote/test", { host_id: "fixture-host" })).ok, true);
  }
  await ok("/api/remote/activate", {
    session_id: session,
    host_id: "fixture-host",
    active: true,
  });
  await ok("/api/permissions", { session_id: session, profile: "ask" });
  const action = turn(session, "remote", "remote operation");
  const pending = await until(async () => {
    const p = (await ok("/api/sessions/" + session)).pending_approval;
    return p?.action === "remote_exec" && p;
  });
  assert.equal(
    (await api("/api/remote/activate", { session_id: session, active: false }))
      .status,
    409,
  );
  await ok("/api/approvals", {
    approval_id: pending.approval_id,
    approved: true,
  });
  assert.equal(JSON.parse((await action).message).stdout, "native-remote");
  const executionReceipts = (
    await ok(`/api/remote/receipts?session_id=${session}`)
  ).receipts;
  assert.equal(executionReceipts.length, 1);
  assert.equal(executionReceipts[0].state, "completed");
  assert.equal(executionReceipts[0].result.is_error, false);
  assert(!JSON.stringify(executionReceipts).includes("native-remote"));
  assert.deepEqual(
    (await ok("/api/remote/receipts?session_id=unrelated-chat")).receipts,
    [],
  );
  const executionAck = `/api/remote/receipts/${executionReceipts[0].receipt}/acknowledge`;
  assert.equal(
    (
      await api(executionAck, {
        target: executionReceipts[0].target,
        allow_continuation_without_replay: false,
      })
    ).status,
    403,
  );
  assert.equal(
    (
      await api(executionAck, {
        target: executionReceipts[0].target,
        allow_continuation_without_replay: true,
      })
    ).status,
    409,
  );
  if (realSshImage) {
    const denied = turn(session, "denied-remote", "denied remote mutation");
    const permission = await until(async () => {
      const p = (await ok("/api/sessions/" + session)).pending_approval;
      return p?.action === "remote_exec" && p;
    });
    await ok("/api/approvals", {
      approval_id: permission.approval_id,
      approved: false,
    });
    await denied;
    assert.equal(fs.existsSync(path.join(remoteDir, "denied-marker")), false);
  }
  const failing = turn(session, "failed-command", "failed remote operation");
  const failingApproval = await until(async () => {
    const p = (await ok("/api/sessions/" + session)).pending_approval;
    return p?.action === "remote_exec" && p;
  });
  await ok("/api/approvals", {
    approval_id: failingApproval.approval_id,
    approved: true,
  });
  assert(
    (await failing).message.includes("Remote command exited with code"),
  );
  assert.equal(
    (await ok(`/api/sessions/${session}/state`)).turn.failed_calls,
    1,
  );
  const transfer = turn(session, "transfer", "remote upload " + doc.path);
  const transferApproval = await until(async () => {
    const p = (await ok("/api/sessions/" + session)).pending_approval;
    return p?.action === "remote_upload" && p;
  });
  await ok("/api/approvals", {
    approval_id: transferApproval.approval_id,
    approved: true,
  });
  assert.equal(JSON.parse((await transfer).message).ok, true);
  assert.deepEqual(
    fs.readFileSync(path.join(remoteDir, "received/evidence.docx")),
    fs.readFileSync(path.join(root, "reference.docx")),
  );
  await ok("/api/remote/activate", { session_id: session, active: false });
  await ok(
    "/api/channels/whatsapp_qr",
    {
      base_url: provider + "/gateway",
      token: "gateway-token",
      allowed_users: "123",
      enabled: true,
    },
    "PUT",
  );
  const qr = await fetch(base + "/api/channels/whatsapp_qr/qr.svg", {
    headers: { Authorization: `Bearer ${token}` },
  });
  assert.equal(qr.status, 200);
  assert((await qr.text()).includes("<svg"));
  await ok("/api/channels/whatsapp_qr", undefined, "DELETE");
  updates = [
    {
      update_id: 1,
      message: { from: { id: 999 }, chat: { id: 999 }, text: "unknown sender" },
    },
    {
      update_id: 2,
      message: {
        from: { id: 123 },
        chat: { id: 123 },
        text: "allowed incoming message",
      },
    },
  ];
  await ok(
    "/api/channels/telegram",
    {
      bot_token: "fixture-bot",
      base_url: provider,
      allowed_users: "123",
      enabled: true,
    },
    "PUT",
  );
  await until(() => sent.length === 1);
  assert.equal(sent[0].chat_id, "123");
  assert.equal(sent[0].text, "Native channel reply");
  await sleep(2300);
  assert.equal(sent.length, 1);
  updates.push({
    update_id: 3,
    message: { from: { id: 123 }, chat: { id: 123 }, text: "channel followup" },
  });
  await until(() => sent.length === 2);
  assert.equal(sent[1].text, "Channel history preserved");
  assert.equal((await ok("/api/channels/telegram")).connected, true);
  assert(!JSON.stringify(await ok("/api/channels")).includes("fixture-bot"));
  await ok(
    "/api/channels/telegram",
    { enabled: false, allowed_users: "123" },
    "PUT",
  );
  child.kill("SIGKILL");
  await new Promise((r) => child.once("exit", r));
  await start();
  assert.equal((await ok("/api/integrations/gmail")).connected, true);
  assert.equal((await ok("/api/voice-profiles")).profiles.length, 1);
  assert(
    (await ok("/api/sessions/" + session)).messages.some((m) =>
      m.text.includes("Office native evidence"),
    ),
  );
  console.log(
    JSON.stringify({
      status: "passed",
      checks: [
        "Anthropic discovery and tool calls",
        "DOCX and PDF extraction",
        "multimodal images",
        "WAV speech",
        "voice profile integrity and cloning",
        "multipart transcription",
        "image artifacts",
        "OAuth PKCE, refresh, replay rejection, disconnect races, device polling and model login",
        realSshImage
          ? "real loopback OpenSSH, approval, failed-command status and hash-verified upload"
          : "SSH protocol fixture, approval, failed-command status and hash-verified upload",
        ...(realSshImage
          ? [
              "changed SSH host key rejected",
              "denied remote mutation never dispatched",
            ]
          : []),
        "target switching fenced during run",
        "Telegram allowlist, multi-turn history, one-time replies and gateway QR rendering",
        "restart persistence",
        "no Python or curl on backend PATH",
      ],
    }),
  );
} catch (error) {
  if(process.env.KAGEHA_TEST_CREDENTIAL_MODE==='service'){try{const {unixCredentialTransport}=await import('../../../sdk/typescript/credentials.mjs');const transport=await unixCredentialTransport({socket:path.join(root,'fixture-credential-service.sock'),tokenFile:path.join(root,'fixture-replacement.token')});const info=await transport('/v1/info',{});const status=await api('/api/channels/telegram');console.error(JSON.stringify({credential_budget:info.request_budgets,limit:info.requests_per_minute,channel_connected:status.data.connected,channel_listener:status.data.listener_running}));}catch{console.error('Credential/channel diagnostic unavailable');}}
  console.error(fs.readFileSync(path.join(root, "backend.log"), "utf8"));
  throw error;
} finally {
  if (sshContainer) {
    try {
      execFileSync("docker", ["rm", "-f", sshContainer], { stdio: "ignore" });
    } catch {
      /* container creation may itself have failed */
    }
  }
  for (const c of children) {
    if (c.exitCode === null) {
      c.kill("SIGTERM");
      await Promise.race([new Promise((r) => c.once("exit", r)), sleep(1000)]);
    }
  }
  fixture.closeAllConnections();
  await new Promise((r) => fixture.close(r));
  if (!process.env.KEEP_NATIVE_ADAPTER_FIXTURE)
    fs.rmSync(root, { recursive: true, force: true });
}
