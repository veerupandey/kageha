#!/usr/bin/env node
// Compare two local binaries through the ordinary chat endpoint using fresh,
// synthetic-only state. The benchmark's loopback adapter sends requests to
// Gemini without streaming, then frames the response as indexed SSE. No API
// key is passed to the child servers. Each invocation writes incremental JSON
// results to a new output directory.
//
// GOOGLE_API_KEY=... node scripts/demos/lean_loop_comparison.mjs \
//   --baseline /path/to/baseline --current /path/to/current --output /new/path
// Use --case and --condition to run a subset; pass the same --marker to pair
// separately started subsets. The resulting measurements are exploratory.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import http from "node:http";
import crypto from "node:crypto";
import { spawn } from "node:child_process";
import { DatabaseSync } from "node:sqlite";
const args = process.argv.slice(2), option = (name, fallback) => {
  const i = args.indexOf(name);
  return i < 0 ? fallback : args[i + 1];
};
const scripted = args.includes("--scripted");
const key = process.env.GOOGLE_API_KEY;
if (!key && !scripted) throw Error("GOOGLE_API_KEY is required unless --scripted is used");
const baseline = path.resolve(option("--baseline", "")), current = path.resolve(option("--current", ""));
const output = path.resolve(option("--output", ""));
if (!option("--baseline") || !option("--current") || !option("--output") || fs.existsSync(output)) throw Error("Specify --baseline, --current and a new --output directory");
for (const p of [baseline, current]) if (!fs.existsSync(p)) throw Error("Binary unavailable: " + p);
fs.mkdirSync(output, { recursive: true, mode: 448 });
const model = option("--model", scripted ? "scripted-executor" : "gemini-flash-lite-latest"), reviewer = option("--reviewer-model", scripted ? "scripted-reviewer" : "gemini-3.1-flash-lite");
if (model === reviewer) throw Error("The reviewer model must differ from the executor model");
const delay = Number(option("--delay-ms", "0"));
const marker = option("--marker", "HELDOUT_" + crypto.randomBytes(10).toString("hex"));
if (!/^HELDOUT_[a-zA-Z0-9_]{12,80}$/.test(marker)) throw Error("Invalid synthetic --marker");
const cases = [
  { id: "read", prompt: "Record the required answer outcome, then use read_file to read fixtures/value.txt and return its exact contents.", check: "read" },
  { id: "recover", prompt: "First try read_file on fixtures/missing.txt. If it does not exist, read fixtures/value.txt instead and return its exact contents.", check: "recover" },
  { id: "delegate", prompt: "Delegate reading parent/fixtures/value.txt to one worker, while independently reading fixtures/value.txt yourself. Wait for the worker and return both results.", check: "delegate" }
];
const caseFilter = option("--case", "all"), conditionFilter = option("--condition", "all");
const selectedCases = caseFilter === "all" ? cases.map((c) => c.id) : caseFilter.split(",");
if (!selectedCases.length || selectedCases.some((id) => !cases.some((c) => c.id === id))) throw Error("Unknown --case");
if (!["all", "baseline_reviewer", "baseline_no_reviewer", "current"].includes(conditionFilter)) throw Error("Unknown --condition");
let calls = 0, rateLimits = 0, reviewerCalls = 0, reflectionCalls = 0, completionReviews = 0;
const providerErrors = new Map();
let observedCalls = new Map();
// This deterministic control measures integration only; it is not a model
// quality test. It gets fixture content solely from the tool response.
function scriptedReply(input) {
  const messages = input.messages || [];
  const system = messages.filter((m) => m.role === "system").map((m) => String(m.content || "")).join("\n");
  if (system.startsWith("You review the journal of a request")) return { content: '{"lessons":[]}' };
  if (system.startsWith("You review an autonomous agent's work")) return { content: '{"outcomes":{"answer":{"pass":true,"gap":""}}}' };
  const user = String(messages.filter((m) => m.role === "user").at(-1)?.content || "");
  const toolCalls = messages.flatMap((m) => m.tool_calls || []);
  const callsByName = (name) => toolCalls.filter((c) => c.function?.name === name);
  const toolMessages = messages.filter((m) => m.role === "tool");
  const action = (name, parameters) => ({ content: null, tool_calls: [{ id: `scripted-${calls}`, type: "function", function: { name, arguments: JSON.stringify(parameters) } }] });
  const evidence = toolMessages.map((m) => String(m.content || "")).join("\n").match(/HELDOUT_[a-zA-Z0-9_]+/)?.[0];
  if (user.startsWith("Record the required answer") && !callsByName("record_required_outcomes").length) {
    return action("record_required_outcomes", { outcomes: [{ id: "answer", description: "Return the exact fixture contents", kind: "answer", verify: "Answer matches fixture" }] });
  }
  if (user.startsWith("Delegate reading")) {
    if (!callsByName("read_file").length) return action("read_file", { path: "fixtures/value.txt" });
    if (!callsByName("agent_spawn").length) return action("agent_spawn", { agent_id: "main", objective: "Read parent/fixtures/value.txt and return the exact contents." });
    if (!callsByName("agent_wait_any").length) {
      const spawn = callsByName("agent_spawn").at(-1);
      const raw = toolMessages.find((m) => m.tool_call_id === spawn.id)?.content;
      let job; try { job = JSON.parse(raw).id; } catch {}
      if (job) return action("agent_wait_any", { job_ids: [job], timeout_ms: 30000 });
    }
    return { content: evidence ? `Both results: ${evidence}` : "Unable to obtain both results." };
  }
  if (user.includes("fixtures/value.txt")) {
    const reads = callsByName("read_file");
    if (!reads.length) return action("read_file", { path: user.startsWith("First try") ? "fixtures/missing.txt" : user.includes("parent/fixtures") ? "parent/fixtures/value.txt" : "fixtures/value.txt" });
    if (user.startsWith("First try") && reads.length === 1) return action("read_file", { path: "fixtures/value.txt" });
    return { content: evidence || "Unable to read the fixture." };
  }
  return { content: "Unable to complete this request." };
}
const adapter = http.createServer(async (req, res) => {
  try {
    let body = "";
    for await (const part of req) body += part;
    const input = JSON.parse(body), stream = input.stream === true;
    for (const message of input.messages || []) {
      for (const call of message.tool_calls || []) {
        try { observedCalls.set(call.id, { name: call.function.name, arguments: JSON.parse(call.function.arguments) }); } catch {}
      }
    }
    input.stream = false;
    delete input.stream_options;
    calls++;
    const isReflection = input.messages?.some((m) => m.role === "system" && typeof m.content === "string" && m.content.startsWith("You review the journal of a request an autonomous agent has finished"));
    const isCompletionReview = input.messages?.some((m) => m.role === "system" && typeof m.content === "string" && m.content.startsWith("You review an autonomous agent's work before"));
    if (input.model === reviewer && !isReflection) reviewerCalls++;
    if (isReflection) reflectionCalls++;
    if (isCompletionReview) completionReviews++;
    let text, status;
    if (scripted) {
      const message = scriptedReply(input);
      text = JSON.stringify({ choices: [{ message, finish_reason: message.tool_calls ? "tool_calls" : "stop" }] });
      status = 200;
    } else {
      const upstream = await fetch("https://generativelanguage.googleapis.com/v1beta/openai/chat/completions", { method: "POST", headers: { "content-type": "application/json", authorization: `Bearer ${key}` }, body: JSON.stringify(input), signal: AbortSignal.timeout(60_000) });
      text = await upstream.text();
      status = upstream.status;
      if (status === 429) rateLimits++;
      if (status >= 400) providerErrors.set(status, (providerErrors.get(status) || 0) + 1);
    }
    if (status !== 200 || !stream) {
      res.writeHead(status, { "content-type": "application/json" });
      res.end(text);
      return;
    }
    const response = JSON.parse(text), choice = response.choices?.[0];
    if (!choice?.message) throw Error("Invalid upstream response");
    const delta = { role: "assistant", content: choice.message.content ?? null };
    if (choice.message.tool_calls) delta.tool_calls = choice.message.tool_calls.map((call, index) => ({ ...call, index }));
    res.writeHead(200, { "content-type": "text/event-stream" });
    res.write("data: " + JSON.stringify({ choices: [{ index: 0, delta, finish_reason: choice.finish_reason }], usage: response.usage }) + "\n\n");
    res.end("data: [DONE]\n\n");
  } catch {
    providerErrors.set(502, (providerErrors.get(502) || 0) + 1);
    if (!res.headersSent) res.writeHead(502);
    res.end('{"error":"benchmark adapter failure"}');
  }
});
await new Promise((resolve) => adapter.listen(0, "127.0.0.1", resolve));
const provider = `http://127.0.0.1:${adapter.address().port}/v1`;
const results = [];
const hashFile = (file) => crypto.createHash("sha256").update(fs.readFileSync(file)).digest("hex");
const binaryHashes = { baseline: hashFile(baseline), current: hashFile(current) };
const save = () => fs.writeFileSync(path.join(output, "summary.json"), JSON.stringify({ version: 1, model, reviewer_model: reviewer, binary_sha256: binaryHashes, fixture_sha256: crypto.createHash("sha256").update(marker).digest("hex"), budget: { max_duration_seconds: 90, max_model_calls: 12 }, adapter: scripted ? "deterministic scripted provider" : "Gemini non-streaming response converted to indexed SSE", limitations: "Synthetic tasks, one sample per condition; provider latency and rate limits vary. Scripted runs test integration, not model intelligence. Reviewer configuration is confirmed separately from reviewer invocation. Reflection calls are counted only until the server is shut down. No private settings used.", results }, null, 2) + "\n", { mode: 384 });
const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
try {
  trials: for (const item of cases.filter((c) => selectedCases.includes(c.id))) for (const condition of [{ id: "baseline_reviewer", binary: baseline, reviewer: true }, { id: "baseline_no_reviewer", binary: baseline }, { id: "current", binary: current, reviewer: true }].filter((c) => conditionFilter === "all" || c.id === conditionFilter)) {
    observedCalls = new Map();
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "kageha-lean-trial-"));
    const project = path.join(dir, "project"), state = path.join(dir, "state");
    fs.mkdirSync(project, { mode: 448 });
    fs.mkdirSync(state, { mode: 448 });
    fs.mkdirSync(path.join(state, "empty-legacy"), { mode: 448 });
    const token = crypto.randomBytes(32).toString("hex"), log = path.join(dir, "server.log"), fd = fs.openSync(log, "w", 384);
    const env = { PATH: process.env.PATH || "/usr/bin:/bin", HOME: dir, TMPDIR: os.tmpdir(), KAGEHA_HOME: path.join(state, "home"), KAGEHA_PROVIDER_URL: provider, KAGEHA_MODEL: model, KAGEHA_API_KEY: "benchmark-adapter", KAGEHA_SERVER_TOKEN: token, KAGEHA_LEGACY_SESSIONS_DIR: path.join(state, "empty-legacy"), KAGEHA_BENCHMARK_TIMING: "1" };
    const child = spawn(condition.binary, ["serve", project, state, "127.0.0.1:0"], { env, stdio: ["ignore", fd, fd] });
    fs.closeSync(fd);
    const before = calls, before429 = rateLimits, beforeReview = reviewerCalls, beforeReflection = reflectionCalls, beforeCompletion = completionReviews, row = { case: item.id, condition: condition.id, reviewer_configured: false };
    const errorsBefore = new Map(providerErrors);
    try {
      let base;
      for (let i = 0; i < 1200; i++) {
        const match = fs.readFileSync(log, "utf8").match(/listening on ([^\s]+)/);
        if (match) {
          base = "http://" + match[1];
          break;
        }
        if (child.exitCode !== null) throw Error("Server exited");
        await pause(50);
      }
      if (!base) throw Error("Server startup timed out");
      const api = async (route, body) => {
        const res = await fetch(base + route, { method: body ? "POST" : "GET", headers: { authorization: "Bearer " + token, "content-type": "application/json" }, body: body ? JSON.stringify(body) : void 0, signal: AbortSignal.timeout(12e4) });
        const data = await res.json();
        if (!res.ok) throw Error(`HTTP ${res.status}`);
        return data;
      };
      await api("/api/settings/persistence", { enabled: true, max_duration_seconds: 90, max_model_calls: 12 });
      if (condition.reviewer) {
        await api("/api/provider-setup/connect", { provider_name: "benchmark-reviewer", model_id: "benchmark-reviewer", model: reviewer, protocol: "openai_compat", base_url: provider, api_key_env: "KAGEHA_API_KEY" });
        row.reviewer_configured = true;
      }
      const session = (await api("/api/sessions", {})).session_id;
      const fixture = path.join(state, "sessions", session, "workspace", "fixtures");
      fs.mkdirSync(fixture, { recursive: true, mode: 0o700 });
      fs.writeFileSync(path.join(fixture, "value.txt"), marker + "\n");
      const started = performance.now();
      const answer = await api("/api/chat/send", { session_id: session, client_request_id: "trial-" + item.id, message: item.prompt, max_steps: 12, ...condition.reviewer ? { model_roles: { reviewer: "benchmark-reviewer" } } : {} });
      row.elapsed_ms = Math.round(performance.now() - started);
      row.status = answer.status;
      row.failure_code = answer.failure_code || null;
      // Classify familiar failures without persisting provider error text,
      // which might contain credentials or request payloads.
      const detail = String(answer.failure_detail || "");
      row.failure_category = /rate.limit|throttl/i.test(detail) ? "provider_throttled" : /invalid.*tool.*index/i.test(detail) ? "invalid_streamed_tool_index" : /HTTP 404/i.test(detail) ? "provider_http_404" : /browser state must be private/i.test(detail) ? "state_permissions" : detail ? "other_execution_error" : null;
      const jobs = (await api("/api/jobs?session_id=" + encodeURIComponent(session))).jobs || [];
      const db = new DatabaseSync(path.join(state, "native-chats.sqlite"), { readOnly: true });
      const rows = db.prepare("SELECT sequence,chat_id,event,payload FROM ui_events WHERE event='tool_card' ORDER BY sequence").all();
      db.close();
      const tools = rows.map((r) => {
        try {
          return { ...JSON.parse(r.payload), chat_id: r.chat_id, sequence: r.sequence };
        } catch {
          return {};
        }
      });
      const reads = tools.filter((t) => t.name === "read_file");
      const successfulRead = reads.find((t) => t.chat_id === session && t.status === "done" && String(t.result_preview || "").includes(marker));
      const missingRead = reads.find((t) => t.chat_id === session && t.status === "failed" && observedCalls.get(t.id)?.arguments?.path === "fixtures/missing.txt");
      const found = !!successfulRead;
      const failedRead = !!missingRead && !!successfulRead && missingRead.sequence < successfulRead.sequence;
      const worker = jobs.some((j) => j.state === "succeeded" && String(j.result?.message || "").includes(marker));
      const text = String(answer.message || ""), claimed = ["success", "completed"].includes(answer.status), contains = text.includes(marker);
      row.checks = { contains_expected: contains, successful_read_evidence: found, missing_read_evidence: failedRead, successful_worker_evidence: worker };
      row.verified = claimed && contains && found && (item.check !== "recover" || failedRead) && (item.check !== "delegate" || worker);
      row.unverified_success_status = claimed && !row.verified;
      row.false_completion = row.unverified_success_status && /\b(completed|successfully|done|both results)\b/i.test(text) && !/\b(unable|cannot|could not|failed|incomplete|not completed)\b/i.test(text);
      row.workers = jobs.length;
    } catch (error) {
      row.error = String(error.message || error);
    } finally {
      row.model_calls = calls - before;
      row.reviewer_calls = reviewerCalls - beforeReview;
      row.reflection_calls = reflectionCalls - beforeReflection;
      row.completion_review_calls = completionReviews - beforeCompletion;
      row.rate_limited_responses = rateLimits - before429;
      row.provider_errors = Object.fromEntries([...providerErrors].map(([status, count]) => [status, count - (errorsBefore.get(status) || 0)]).filter(([, count]) => count > 0));
      child.kill("SIGTERM");
      await Promise.race([new Promise((resolve) => child.once("exit", resolve)), pause(3e3)]);
      if (child.exitCode === null) child.kill("SIGKILL");
      fs.rmSync(dir, { recursive: true, force: true });
      results.push(row);
      save();
      console.log(JSON.stringify(row));
    }
    if (args.includes("--stop-on-provider-error") && (Object.keys(row.provider_errors).length || row.error)) break trials;
    if (delay > 0) await pause(delay);
  }
} finally {
  adapter.close();
  save();
}
