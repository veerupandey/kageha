import { test } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import http from "node:http";
import { spawnSync } from "node:child_process";
import { analyzeTimings, experienceTimings, fatalFailure, fixtureHash, observedFastest, percentile, runTrialSequence, schedule, summarize, summarizeExperience, summarizeWaitModes, waitComparisonSchedule } from "./agent_speed_benchmark_lib.mjs";
import { observedBenchmarkTurn } from "./agent_experience_benchmark.mjs";

test("observes early commentary and completion even before acceptance returns", async () => {
  let stream;
  const server = http.createServer((_req, res) => {
    stream = res;
    res.writeHead(200, {"Content-Type":"text/event-stream"});
    res.write('event: snapshot\ndata: {}\n\n');
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const base = `http://127.0.0.1:${server.address().port}`;
  const send = (events) => stream.write(`event: update\ndata: ${JSON.stringify({events})}\n\n`);
  try {
    const run = await observedBenchmarkTurn({message:"fixture"}, {base, session:"chat", token:"test", timeoutMs:1000,
      api: async (_route, request) => {
        assert.equal(request.event_stream, "session");
        send([{turn_id:"other",kind:"assistant_progress",payload:{origin:"model",text:"foreign"}},
          {turn_id:"turn",kind:"assistant_progress",payload:{origin:"model",text:"checking"}}]);
        await new Promise(resolve => setTimeout(resolve, 10));
        send([{turn_id:"turn",kind:"tool_card",tool_card:{status:"done"}},
          {turn_id:"turn",kind:"done",payload:{status:"success",message:"done"}}]);
        await new Promise(resolve => setTimeout(resolve, 10));
        return {status:"accepted",turn_id:"turn"};
      }});
    assert.equal(run.answer.message, "done");
    assert.equal(run.experience.local_acknowledgment_ms, null);
    assert(run.experience.first_model_commentary_ms <= run.experience.first_confirmed_tool_result_ms);
    assert(run.experience.first_confirmed_tool_result_ms <= run.experience.completion_ms);
    assert(run.experience.completion_ms <= run.experience.server_acceptance_ms);
  } finally {
    server.closeAllConnections();
    await new Promise(resolve => server.close(resolve));
  }
});

test("reports admission failure and closes the event feed", async () => {
  let closed = false;
  const server = http.createServer((_req, res) => {
    res.on("close", () => { closed = true; });
    res.writeHead(200, {"Content-Type":"text/event-stream"});
    res.write('event: snapshot\ndata: {}\n\n');
  });
  await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
  try {
    const base = `http://127.0.0.1:${server.address().port}`;
    await assert.rejects(observedBenchmarkTurn({}, {base, session:"chat", token:"test", timeoutMs:1000,
      api: async () => { throw Error("admission rejected"); }}), /admission rejected/);
    await new Promise(resolve => setTimeout(resolve, 10));
    assert.equal(closed, true);
  } finally {
    server.closeAllConnections();
    await new Promise(resolve => server.close(resolve));
  }
});

test("records only model commentary and successful non-cached parent tool results", () => {
  const event = (elapsed_ms, frame) => ({elapsed_ms, frame: {turn_id: "parent", ...frame}});
  const observed = experienceTimings([
    event(10, {kind:"assistant_progress", payload:{origin:"host", text:"Preparing"}}),
    event(15, {kind:"assistant_progress", turn_id:"child", payload:{origin:"model", text:"Child"}}),
    event(20, {kind:"tool_card", tool_card:{status:"pending"}}),
    event(25, {kind:"tool_card", tool_card:{status:"failed"}}),
    event(30, {kind:"tool_card", tool_card:{status:"done", cached:true}}),
    event(40, {kind:"assistant_progress", payload:{origin:"model", text:"Reviewing"}}),
    event(50, {kind:"tool_card", tool_card:{status:"done"}}),
  ], "parent", 35, 100);
  assert.equal(observed.local_acknowledgment_ms, null);
  assert.equal(observed.server_acceptance_ms, 35);
  assert.equal(observed.first_model_commentary_ms, 40);
  assert.equal(observed.first_confirmed_tool_result_ms, 50);
  assert.equal(observed.completion_ms, 100);
  const missing = experienceTimings([], "parent", null, -1);
  assert.equal(missing.server_acceptance_ms, null);
  assert.equal(missing.first_model_commentary_ms, null);
  assert.equal(missing.first_confirmed_tool_result_ms, null);
  assert.equal(missing.completion_ms, null);
});

test("milestone summaries count missing values without including failed trials", () => {
  const rows = [
    {width:1, verified:true, experience_timings:{server_acceptance_ms:20, first_model_commentary_ms:70, completion_ms:100}},
    {width:1, verified:true, experience_timings:{server_acceptance_ms:40, completion_ms:120}},
    {width:1, verified:false, experience_timings:{server_acceptance_ms:1}},
  ];
  const result = summarizeExperience(rows)[0];
  assert.deepEqual(result.metrics.server_acceptance_ms, {samples:2,missing:0,p50_ms:20,p95_ms:40});
  assert.deepEqual(result.metrics.first_model_commentary_ms, {samples:1,missing:1,p50_ms:70,p95_ms:70});
  assert.deepEqual(result.metrics.first_confirmed_tool_result_ms, {samples:0,missing:2,p50_ms:null,p95_ms:null});
});

test("pairs wait modes at width four, alternating order and retaining failed attempts", () => {
  assert.deepEqual(waitComparisonSchedule(3), [{width:4,wait_mode:"any"},{width:4,wait_mode:"all"},
    {width:4,wait_mode:"all"},{width:4,wait_mode:"any"},{width:4,wait_mode:"any"},{width:4,wait_mode:"all"}]);
  const result = summarizeWaitModes([{width:4,wait_mode:"any",verified:true,elapsed_ms:100,cost_usd:null},
    {width:4,wait_mode:"all",verified:false,elapsed_ms:20,cost_usd:null}]);
  assert.equal(result[0].p50_ms, 100);
  assert.equal(result[1].failed, 1);
  assert.equal(result[1].p50_ms, null);
});

test("separates observed worker windows from complete adapter timings without inventing missing data", () => {
  const jobs = [{session_id: "a", spec: {persona_id: "first"}, started_at: 10, ended_at: 14},
    {session_id: "b", spec: {persona_id: "second"}, started_at: 11, ended_at: 16}];
  const events = [{chat_id: "a", step: 1, elapsed_ms: 1100}, {chat_id: "a", step: 2, elapsed_ms: 1400}, {chat_id: "b", step: 1, elapsed_ms: 3000}];
  const starts = [{chat_id: "a", step: 1}, {chat_id: "a", step: 2}, {chat_id: "b", step: 1}, {chat_id: "b", step: 2}];
  const actual = analyzeTimings(jobs, events, 8000, starts);
  assert.equal(actual.worker_span_ms, 6000);
  assert.equal(actual.worker_start_spread_ms, 1000);
  assert.equal(actual.outside_worker_span_ms, 2000);
  assert.equal(actual.workers[0].model_adapter_ms, 2500);
  assert.equal(actual.workers[0].other_ms, 1500);
  assert.equal(actual.workers[1].model_adapter_ms, null);
  assert.equal(actual.workers[1].other_ms, null);
  assert.equal(analyzeTimings(jobs, [], 5000).outside_worker_span_ms, null);
  const retryWithMissingStep = [...events, {chat_id: "b", step: 1, elapsed_ms: 10}];
  assert.equal(analyzeTimings(jobs, retryWithMissingStep, 8000, starts).workers[1].model_adapter_ms, null);
});

test("interleaves widths, rotates order, and rejects unbounded runs", () => {
  assert.deepEqual(schedule(3), [1, 2, 4, 2, 4, 1, 4, 1, 2]);
  assert.throws(() => schedule(0));
  assert.throws(() => schedule(101));
});

test("reports nearest-rank timings for verified trials only and does not invent cost", () => {
  const result = summarize([
    {width: 2, verified: true, elapsed_ms: 40, cost_usd: null},
    {width: 2, verified: true, elapsed_ms: 20, cost_usd: 0.1},
    {width: 2, verified: false, elapsed_ms: 1, cost_usd: 0},
  ])[1];
  assert.deepEqual(result, {width: 2, attempts: 3, verified: 2, failed: 1, p50_ms: 20, p95_ms: 40, mean_cost_usd: null});
  assert.equal(percentile([], 0.95), null);
});

test("names observed fastest widths only with three verified trials per width and no failures", () => {
  const sample = (width, p50_ms) => ({width, attempts: 3, verified: 3, failed: 0, p50_ms});
  const rows = [sample(1, 100), sample(2, 70), sample(4, 70)];
  assert.deepEqual(observedFastest(rows).widths, [2, 4]);
  assert.equal(observedFastest([sample(1, 100), {...sample(2, 70), verified: 2}, sample(4, 70)]).available, false);
  assert.equal(observedFastest([sample(1, 100), {...sample(2, 70), attempts: 4, failed: 1}, sample(4, 70)]).available, false);
  assert.equal(observedFastest([sample(1, 100), sample(2, 70), sample(2, 70)]).available, false);
});

test("authentication failures replace generic verification errors and stop future trials", async () => {
  const attempted = [];
  const saved = [];
  const reason = fatalFailure({error: "Exactly four workers must execute", parent: {failure_detail: "Model returned HTTP 401"}});
  assert.match(reason, /HTTP 401/);
  const trials = await runTrialSequence([1, 2, 4], async (width) => {
    attempted.push(width);
    return {width, verified: false, error: reason, stop_reason: reason};
  }, (completed) => saved.push([...completed]));
  assert.deepEqual(attempted, [1]);
  assert.equal(saved[0][0].error, reason);
  assert.equal(trials.length, 1);
  assert.equal(summarize(trials)[0].failed, 1);
  assert.equal(observedFastest(summarize(trials)).available, false);
  assert.match(fatalFailure({jobs: [{result: {failure_detail: "Model returned HTTP 403"}}]}), /HTTP 403/);
  assert.match(fatalFailure(null, "Error: Model is not configured"), /configuration is incomplete/);
  assert.match(fatalFailure(null, "API 503: Saved credentials are locked"), /--unlock-vault/);
  assert.match(fatalFailure(null, "Keychain status -25293"), /--unlock-vault/);
  assert.equal(fatalFailure({parent: {failure_detail: "Model returned HTTP 429"}}), null);
});

test("nonfatal failed trial does not prevent later widths from running", async () => {
  const attempted = [];
  const trials = await runTrialSequence([1, 2, 4], async (width) => {
    attempted.push(width);
    return {width, verified: false, stop_reason: null};
  });
  assert.deepEqual(attempted, [1, 2, 4]);
  assert.equal(trials.length, 3);
});

test("fixture hash changes when fixture content changes", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "kageha-fixture-test-"));
  try {
    fs.mkdirSync(path.join(root, "fixtures"));
    for (let i = 1; i <= 4; i++) fs.writeFileSync(path.join(root, `fixtures/task-${i}.txt`), `token-${i}`);
    const hash = fixtureHash(root);
    fs.writeFileSync(path.join(root, "fixtures/task-2.txt"), "changed");
    assert.notEqual(fixtureHash(root), hash);
  } finally { fs.rmSync(root, {recursive: true, force: true}); }
});

test("real trial verifier rejects missing evidence and wrong concurrency", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "kageha-trial-test-"));
  const reportPath = path.join(root, "report.json");
  const runner = new URL("./agent_repo_review.mjs", import.meta.url);
  const tasks = Array.from({length: 4}, (_, i) => ({id: `worker-${i}`, expected: `token-${i}`}));
  const report = {benchmark_width: 2, tasks, task_elapsed_ms: 100, peak_concurrent_workers: 2,
    parent: {session_id: "parent", status: "success", message: tasks.map((task) => task.expected).join(" ")},
    jobs: tasks.map((task) => ({session_id: task.id, spec: {persona_id: task.id}, state: "succeeded", result: {message: task.expected}})),
    tool_events: tasks.map((task) => ({chat_id: task.id, payload: {name: "read_file", status: "done", result_preview: task.expected}}))};
  const verify = () => { fs.writeFileSync(reportPath, JSON.stringify(report)); return spawnSync(process.execPath, [runner.pathname, "--verify-report", reportPath], {encoding: "utf8"}); };
  try {
    assert.equal(verify().status, 0);
    report.wait_mode = "all";
    assert.notEqual(verify().status, 0);
    report.tool_events.push({chat_id: "parent", payload: {name: "agent_wait_all", status: "done"}});
    assert.equal(verify().status, 0);
    report.tool_events.push({chat_id: "parent", payload: {name: "agent_wait_any", status: "done"}});
    assert.notEqual(verify().status, 0);
    report.tool_events.pop();
    report.tool_events.pop();
    report.wait_mode = "any";
    assert.notEqual(verify().status, 0);
    report.tool_events.push({chat_id: "parent", payload: {name: "agent_wait_any", status: "done"}});
    assert.equal(verify().status, 0);
    report.tool_events.pop();
    delete report.wait_mode;
    report.tool_events.push({chat_id: "parent", payload: {name: "read_file", status: "done", result_preview: "token-0"}});
    assert.notEqual(verify().status, 0);
    report.tool_events.pop();
    report.peak_concurrent_workers = 1;
    assert.notEqual(verify().status, 0);
    report.peak_concurrent_workers = 2;
    report.tool_events.pop();
    assert.notEqual(verify().status, 0);
    report.tool_events.push({chat_id: "worker-3", payload: {name: "read_file", status: "done", result_preview: "token-3"}});
    report.parent.message = "missing tokens";
    assert.notEqual(verify().status, 0);
  } finally { fs.rmSync(root, {recursive: true, force: true}); }
});
