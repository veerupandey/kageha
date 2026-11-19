import { credentialFixture } from "./credential_fixture.mjs";
// Python-free catalog/settings + real multi-turn model/tool execution for both wire protocols.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import http from "node:http";
import { spawn } from "node:child_process";
const root = fs.mkdtempSync(path.join(os.tmpdir(), "kageha-model-catalog-"));
const binary =
  process.env.KAGEHA_LOOP_BIN || path.resolve("target/debug/kageha-loop");
const token = "model-catalog-fixture-token-123456789";
const resources = path.join(root, "resources");
fs.mkdirSync(resources);
fs.copyFileSync(
  new URL("../../../resources/models.yaml", import.meta.url),
  path.join(resources, "models.yaml"),
);
const records = [];
let lastCatalogState;
let child, base;
const fixture = http.createServer(async (req, res) => {
  if (req.method === "GET") {
    assert(["/v1/models", "/gateway/v1/models"].includes(req.url));
    if (req.url === "/gateway/v1/models")
      assert.equal(req.headers["anthropic-version"], "2023-06-01");
    res.setHeader("Content-Type", "application/json");
    res.end(JSON.stringify({ data: [{ id: "discovered-model" }] }));
    return;
  }
  let bytes = "";
  for await (const chunk of req) bytes += chunk;
  const body = JSON.parse(bytes);
  records.push({ url: req.url, body, headers: req.headers });
  assert(
    ["fixture-openai", "fixture-anthropic", "ui-override"].includes(body.model),
  );
  const anthropic = req.url === "/gateway/v1/messages";
  assert(anthropic || req.url === "/v1/chat/completions");
  if (anthropic) assert.equal(req.headers["anthropic-version"], "2023-06-01");
  const result = anthropic
    ? body.messages.some(
        (m) =>
          Array.isArray(m.content) &&
          m.content.some((v) => v.type === "tool_result"),
      )
    : body.messages.some((m) => m.role === "tool");
  res.setHeader("Content-Type", "application/json");
  res.end(
    JSON.stringify(
      anthropic
        ? {
            role: "assistant",
            content: result
              ? [{ type: "text", text: "Read catalog evidence successfully." }]
              : [
                  {
                    type: "tool_use",
                    id: "read-one",
                    name: "read_file",
                    input: { path: "notes.txt" },
                  },
                ],
          }
        : {
            choices: [
              {
                message: result
                  ? {
                      role: "assistant",
                      content: "Read catalog evidence successfully.",
                    }
                  : {
                      role: "assistant",
                      content: null,
                      tool_calls: [
                        {
                          id: "read-one",
                          type: "function",
                          function: {
                            name: "read_file",
                            arguments: JSON.stringify({ path: "notes.txt" }),
                          },
                        },
                      ],
                    },
              },
            ],
          },
    ),
  );
});
await new Promise((r) => fixture.listen(0, "127.0.0.1", r));
const endpoint = `http://127.0.0.1:${fixture.address().port}`;
const catalog = path.join(root, "models.yaml");
// JSON is also YAML, keeping fixture generation independent of any YAML package.
const data = {
  providers: {
    oa: {
      protocol: "openai_compat",
      base_url: endpoint + "/v1",
    },
    an: {
      protocol: "anthropic_compat",
      base_url: endpoint + "/gateway",
    },
    missing: {
      protocol: "openai_compat",
      base_url: "https://example.invalid/v1",
      api_key_env: "MISSING_KEY",
    },
    unsupported: { protocol: "gemini", base_url: "https://example.invalid" },
  },
  models: [
    {
      id: "oa/model",
      provider: "oa",
      model: "fixture-openai",
      display_name: "Local fixture",
      context_window: 123456,
      capabilities: ["tool_calling", "vision"],
    },
    { id: "an/model", provider: "an", model: "fixture-anthropic" },
    { id: "needs-key", provider: "missing", model: "missing" },
    { id: "unsupported", provider: "unsupported", model: "unsupported" },
  ],
  roles: { default: ["needs-key", "unsupported", "oa/model"] },
};
fs.writeFileSync(catalog, JSON.stringify(data));
fs.mkdirSync(path.join(root, "workspace"));
fs.writeFileSync(
  path.join(root, "workspace/notes.txt"),
  "Catalog fixture evidence",
);
data.providers.fromfile = {
  protocol: "openai_compat",
  base_url: "https://example.invalid/v1",
  api_key_env: "CATALOG_FIXTURE_KEY",
};
data.models.push({
  id: "dotenv-model",
  provider: "fromfile",
  model: "dotenv-model",
});
fs.writeFileSync(catalog, JSON.stringify(data));
fs.writeFileSync(
  path.join(root, "model.env"),
  "CATALOG_FIXTURE_KEY=private-fixture-credential\n",
);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function until(fn) {
  const end = Date.now() + 20000;
  while (Date.now() < end) {
    const v = await fn();
    if (v) return v;
    await sleep(40);
  }
  throw Error(
    "Timed out: " +
      fs.readFileSync(path.join(root, "server.log"), "utf8") +
      "\n" +
      JSON.stringify(lastCatalogState),
  );
}
async function start(defaultCatalog = false) {
  const log = path.join(root, "server.log");
  fs.writeFileSync(log, "");
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
        PATH: "",
        ...credentialFixture(root),
        KAGEHA_SERVER_TOKEN: token,
        ...(defaultCatalog ? {} : { KAGEHA_MODELS_FILE: catalog }),
        KAGEHA_MODEL_ENV_FILE: path.join(root, "model.env"),
        KAGEHA_HOME: path.join(root, "home"),
        KAGEHA_RESOURCE_ROOT: resources,
      },
      stdio: ["ignore", fs.openSync(log, "a"), fs.openSync(log, "a")],
    },
  );
  base = await until(() => {
    if (child.exitCode !== null) throw Error(fs.readFileSync(log, "utf8"));
    const m = fs
      .readFileSync(log, "utf8")
      .match(/listening on (127\.0\.0\.1:\d+)\n/);
    return m && "http://" + m[1];
  });
}
async function stop() {
  if (child && child.exitCode === null) {
    child.kill("SIGTERM");
    await new Promise((r) => child.once("exit", r));
  }
}
async function api(route, body) {
  const res = await fetch(base + route, {
    method: body ? "POST" : "GET",
    headers: {
      Authorization: "Bearer " + token,
      "Content-Type": "application/json",
    },
    body: body ? JSON.stringify(body) : undefined,
  });
  const value = await res.json();
  assert.equal(res.status, 200, JSON.stringify(value));
  return value;
}
try {
  await start();
  const list = await api("/api/models");
  assert.equal(list.default_model, "oa/model");
  assert.equal(list.all.find((v) => v.id === "dotenv-model").ready, true);
  assert(!JSON.stringify(list).includes("private-fixture-credential"));
  assert(
    !JSON.stringify(await api("/api/provider-setup")).includes(
      "private-fixture-credential",
    ),
  );
  for (const [protocol, base_url] of [
    ["openai_compat", endpoint + "/v1"],
    ["anthropic_compat", endpoint + "/gateway"],
  ])
    assert.equal(
      (await api("/api/provider-setup/discover", { protocol, base_url }))
        .models[0].id,
      "discovered-model",
    );
  const model = list.all.find((v) => v.id === "oa/model");
  assert.equal(model.label, "Local fixture");
  assert.equal(model.context_window, 123456);
  assert(model.capabilities.includes("vision"));
  assert.equal(list.all.find((v) => v.id === "needs-key").ready, false);
  assert.equal(list.all.find((v) => v.id === "unsupported").supported, false);
  await api("/api/models/fallback", {
    enabled: false,
    providers: {},
    models: {},
  });
  for (const model_id of ["oa/model", "an/model"]) {
    const chat = (await api("/api/sessions", {})).session_id;
    await api("/api/chat/send", {
      session_id: chat,
      client_request_id: model_id.replace("/", "-"),
      message: "Read notes.txt and report the evidence",
      model: model_id,
    });
    const state = await until(async () => {
      const state = await api("/api/sessions/" + chat);
      lastCatalogState = {
        model_id,
        status: state.status,
        messages: state.messages,
        turns: state.turns,
        requests: records.length,
      };
      if (
        ["error", "failed", "blocked", "cancelled", "interrupted"].includes(
          state.status,
        )
      ) {
        throw Error(
          "Catalog turn failed: " +
            JSON.stringify({
              model_id,
              status: state.status,
              events: (await api("/api/sessions/" + chat + "/events")).events,
            }),
        );
      }
      return (
        state.messages?.some(
          (m) => m.text === "Read catalog evidence successfully.",
        ) && state
      );
    });
    assert(state.messages.some((m) => m.text.includes("Read notes.txt")));
  }
  assert(
    records.some(
      (r) =>
        r.url === "/gateway/v1/messages" &&
        r.body.messages.some((m) =>
          m.content?.some?.(
            (v) =>
              v.type === "tool_result" &&
              v.content.includes("Catalog fixture evidence"),
          ),
        ),
    ),
  );
  await api("/api/provider-setup/connect", {
    provider_name: "oa",
    protocol: "openai_compat",
    base_url: endpoint + "/v1",
    model: "ui-override",
    model_id: "oa/model",
  });
  await stop();
  data.models[0].model = "catalog-new";
  fs.writeFileSync(catalog, JSON.stringify(data));
  await start();
  assert.equal(
    (await api("/api/models")).all.find((v) => v.id === "oa/model").model,
    "ui-override",
  );
  await stop();
  fs.mkdirSync(path.join(root, "home"), { recursive: true });
  fs.mkdirSync(path.join(root, "workspace/.kageha"), { recursive: true });
  fs.writeFileSync(
    path.join(root, "home/models.yaml"),
    JSON.stringify({
      providers: {
        layered: { protocol: "openai_compat", base_url: endpoint + "/v1" },
      },
      models: [
        { id: "layered-fixture", provider: "layered", model: "home-model" },
      ],
    }),
  );
  fs.writeFileSync(
    path.join(root, "workspace/.kageha/models.yaml"),
    JSON.stringify({
      models: [
        {
          id: "layered-fixture",
          provider: "layered",
          model: "workspace-model",
          context_window: 54321,
        },
      ],
    }),
  );
  await start(true);
  const shipped = (await api("/api/models")).all;
  assert(shipped.length > 50);
  for (const provider of [
    "openai",
    "anthropic",
    "commandcode",
    "cloudflare",
    "kiro",
  ])
    assert(shipped.some((v) => v.provider === provider));
  assert.equal(
    shipped.find((v) => v.id === "layered-fixture").model,
    "workspace-model",
  );
  console.log(
    JSON.stringify({
      ok: true,
      protocols: 2,
      python: false,
      catalog: true,
      override_preserved: true,
      requests: records.length,
    }),
  );
} finally {
  await stop();
  await new Promise((r) => fixture.close(r));
  fs.rmSync(root, { recursive: true, force: true });
}
