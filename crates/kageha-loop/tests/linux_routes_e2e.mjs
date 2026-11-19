// Qualifies an installed Linux bundle against a dedicated Linux SSH fixture.
// The fixture account must have the release's kageha-linux-session in /usr/local/bin.
import assert from 'node:assert/strict';
import { execFileSync, spawn } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import { once } from 'node:events';
import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import { setTimeout as sleep } from 'node:timers/promises';

const required = (name) => {
  const value = process.env[name];
  assert(value, `${name} is required for Linux route qualification`);
  return value;
};
const bundle = path.resolve(required('KAGEHA_TEST_BUNDLE'));
const host = required('KAGEHA_TEST_SSH_HOST');
const user = required('KAGEHA_TEST_SSH_USER');
const identity = path.resolve(required('KAGEHA_TEST_SSH_IDENTITY'));
const serverKey = required('KAGEHA_TEST_SSH_HOST_KEY').trim();
const port = Number(process.env.KAGEHA_TEST_SSH_PORT || '22');
const fixtureRoot = required('KAGEHA_TEST_SSH_WORKSPACE');
const liveUrl = process.env.KAGEHA_TEST_LIVE_MODEL_URL;
const live = Boolean(liveUrl);
if (process.env.KAGEHA_REQUIRE_LIVE_MODEL === '1') assert(live, 'Live model qualification must be configured');
assert(/^[\w.:][\w.:-]*$/.test(host) && /^[a-zA-Z_][\w.-]*$/.test(user));
assert(Number.isSafeInteger(port) && port > 0 && port <= 65535);
assert(/^\/[a-zA-Z0-9_/.-]+$/.test(fixtureRoot) && !fixtureRoot.split('/').includes('..'));
assert(/^ssh-ed25519 [A-Za-z0-9+/=]+(?: .*)?$/.test(serverKey));

const state = fs.mkdtempSync(path.join(os.tmpdir(), 'kg-linux-routes-'));
const marker = randomBytes(8).toString('hex');
const remoteRoot = `${fixtureRoot}/kageha-test-${marker}`;
const keyFile = path.join(state, 'known_hosts');
fs.writeFileSync(keyFile, `${port === 22 ? host : `[${host}]:${port}`} ${serverKey.split(' ').slice(0,2).join(' ')}\n`, {mode:0o600});
const ssh = (command) => execFileSync('ssh',['-F','/dev/null','-o','BatchMode=yes','-o','StrictHostKeyChecking=yes',
  '-o',`UserKnownHostsFile=${keyFile}`,'-o','GlobalKnownHostsFile=/dev/null','-o','IdentitiesOnly=yes','-p',String(port),'-i',identity,
  '--',`${user}@${host}`,command],{encoding:'utf8',timeout:30000}).trimEnd();
const q = (text) => `'${text.replaceAll("'", "'\\''")}'`;
let server, child, base, token, webJob;
const chats = [];
const pending = new Set();
let childLog = '';
const results = [];

async function until(label, fn, limit = 45_000) {
  const stop = Date.now() + limit;
  let error;
  do {
    try { const result = await fn(); if (result) return result; } catch (cause) { error = cause; }
    await sleep(250);
  } while (Date.now() < stop);
  throw Error(`Timed out waiting for ${label}`,{cause:error});
}

async function api(route, body, options = {}) {
  const response = await fetch(base + route,{method:options.method || (body === undefined ? 'GET' : 'POST'),
    headers:{Authorization:`Bearer ${token}`,'Content-Type':'application/json',...options.headers},
    body:body === undefined ? undefined : JSON.stringify(body),signal:AbortSignal.timeout(options.timeout || 60000)});
  const result = await response.json();
  assert.equal(response.status,options.status || 200,`${route} ${JSON.stringify(result).slice(0,800)}`);
  return result;
}

const modelName = live ? 'linux-qualification-live' : 'linux-qualification-fixture';
const modelId = live ? required('KAGEHA_TEST_LIVE_MODEL') : 'fixture';
const reportCommand = `mkdir -p outputs && printf '%s' ${q(marker)} > outputs/audit.txt`;
const backgroundCommand = `while ! test -f outputs/release-job; do sleep 1; done; printf '%s' ${q(marker)} > outputs/background.txt`;
const media = `<!doctype html><title>Kageha Linux qualification</title><h1>Remote inventory</h1><a href="proof.txt" download="proof.txt">Download inventory</a>`;
const tool = (name, args, id = name) => ({id:`fixture-${id}`,type:'function',function:{name,arguments:JSON.stringify(args)}});

function fixtureReply(body) {
  const index = body.messages.findLastIndex((message) => message.role === 'user');
  const message = body.messages[index]?.content || '';
  const calls = body.messages.slice(index+1).filter(item=>item.role==='tool');
  const final = content => ({role:'assistant',content});
  const use = (...args) => ({role:'assistant',content:null,tool_calls:[tool(...args)]});
  if (!body.tools?.length) return final('The task is complete.');
  if (message.startsWith('Create a release audit')) return [
    ()=>use('record_required_outcomes',{outcomes:[{id:'report',kind:'artifact',description:'Audit on the SSH host'}]}),
    ()=>use('remote_exec',{command:reportCommand}),
    ()=>use('remote_verify_file',{path:'outputs/audit.txt'}),
    ()=>use('amend_required_outcome',{id:'report',reason:'Bind the verified remote file',proof:JSON.parse(calls[2].content).proof}),
    ()=>use('record_outcome_evidence',{outcome_id:'report',reference:JSON.parse(calls[2].content).evidence_reference}),
  ][calls.length]?.() || final('Audit verified on the server.');
  if (message.startsWith('Open the Linux inventory')) {
    const url = message.match(/http:\/\/127\.0\.0\.1:\d+\//)[0];
    if (!calls.length) return use('browser_open',{url});
    if (calls.length === 1) return use('browser_snapshot',{});
    if (calls.length === 2) {
      const snapshot = JSON.parse(calls[1].content);
      const link = snapshot.controls.find(control=>control.name==='Download inventory');
      assert(link?.ref,`Browser did not observe the inventory link: ${calls[1].content.slice(0,500)}`);
      return use('browser_click',{target:link.ref,observation_id:snapshot.observation_id});
    }
    return final('Inventory download started.');
  }
  if (message.startsWith('Start the release background job')) {
    return !calls.length ? use('remote_job_start',{command:backgroundCommand,timeout_s:240}) : final(calls[0].content);
  }
  if (message.startsWith('Read the managed job status')) {
    const id = message.match(/[a-f0-9]{32}/)[0];
    return !calls.length ? use('remote_job',{job_id:id,action:'status'}) : final(calls[0].content);
  }
  if (message.startsWith('Process the Linux inventory')) {
    return !calls.length ? use('shell',{command:'cat outputs/proof.txt > outputs/processed.txt'}) : final(calls[0].content);
  }
  if (message.startsWith('Use the Linux desktop terminal')) {
    const observe = () => use('computer_observe',{},`observe-${calls.length}`);
    const input = (method,args) => use('computer_execute',{method,args,observation_id:JSON.parse(calls.at(-1).content).observation_id},method);
    return [observe,()=>input('terminal',{}),observe,()=>input('type_text',{text:`printf '%s' ${q(marker)} > outputs/agent-terminal.txt`}),
      observe,()=>input('press_key',{key:'Return'})][calls.length]?.() || final('Entered the command through XTerm.');
  }
  if (message.startsWith('Retrieve the approved SSH audit')) {
    return !calls.length ? use('remote_download',{path:'outputs/audit.txt'}) : final(calls[0].content);
  }
  return final('Ready');
}

async function start(mode) {
  childLog = '';
  const env = {KAGEHA_DATA_DIR:path.join(state,mode),KAGEHA_WEBUI_PORT:'0',KAGEHA_LINUX_BROWSER:mode};
  for (const name of ['PATH','HOME','USER','XDG_RUNTIME_DIR','DBUS_SESSION_BUS_ADDRESS','KAGEHA_CREDENTIAL_KEY_FILE','DOCKER_HOST','KAGEHA_DESKTOP_IMAGE']) {
    if (process.env[name]) env[name] = process.env[name];
  }
  if (process.env.KAGEHA_TEST_CHROMIUM) env.KAGEHA_LINUX_CHROMIUM = process.env.KAGEHA_TEST_CHROMIUM;
  child = spawn(path.join(bundle,'start'),[],{env,cwd:state,stdio:['ignore','pipe','pipe']});
  for (const stream of [child.stdout,child.stderr]) stream.on('data',chunk=>{childLog+=chunk;});
  base = await Promise.race([
    until('packaged server',()=>{const match=childLog.match(/listening on (127\.0\.0\.1:\d+)/);return match && `http://${match[1]}`;}),
    once(child,'exit').then(([code])=>{throw Error(`Packaged Kageha exited at startup (${code})`);}),
  ]);
  token = execFileSync(path.join(bundle,'start'),['token'],{env,encoding:'utf8'}).trim();
}

async function stop() {
  if (child?.exitCode === null && child?.signalCode === null) {
    const stopped = once(child,'exit');
    child.kill();
    await stopped;
  }
  child = undefined;
}

async function create(hostId) {
  const {session_id:id} = await api('/api/sessions',{});
  chats.push({id,hostId});
  if (hostId) await api('/api/remote/activate',{session_id:id,host_id:hostId,active:true});
  return {id,route:`/api/sessions/${id}`};
}

async function turn(chat, message, target = 'remote') {
  const operation = api('/api/chat/send',{session_id:chat.id,client_request_id:randomBytes(10).toString('hex'),
    message,model:`${modelName}/${modelId}`,execution_target:target},{timeout:240000});
  let done = false;
  operation.finally(()=>{done=true;}).catch(()=>{});
  const allowed = new Set(['remote_exec','remote_download','remote_job_start','browser_open','browser_click','shell','computer_execute']);
  const elapsed = Date.now()+238000;
  while (!done && Date.now()<elapsed) {
    const approval = (await api(chat.route)).pending_approval;
    if (approval?.approval_id && !pending.has(approval.approval_id)) {
      pending.add(approval.approval_id);
      assert(allowed.has(approval.action),`Unexpected fixture approval ${approval.action}`);
      await api('/api/approvals',{approval_id:approval.approval_id,approved:true,scope:'once'});
    }
    await sleep(200);
  }
  const result = await operation;
  assert.equal((await api(chat.route+'/state')).turn.actual_target,target);
  return result;
}

async function gui(chat, provider) {
  const headers = {'X-Kageha-Computer':provider};
  await api(chat.route+'/computer/refresh',{}, {headers});
  const desktop = await api(chat.route+'/computer');
  assert.equal(desktop.provider,provider);
  assert.equal(desktop.connected,true);
  const frame = (await api(chat.route+'/computer/preview'));
  assert(frame.result.width >= 1024 && frame.result.image.data.length>1000);
  await api(chat.route+'/computer/lease',{action:'take'},{headers});
  await api(chat.route+'/computer/action',{action:'terminal',frame_id:frame.frame_id},{headers,status:409});
  const input = async (args) => {
    const fresh = await api(chat.route+'/computer/preview');
    await api(chat.route+'/computer/action',{...args,frame_id:fresh.frame_id},{headers});
  };
  await input({action:'terminal'});
  await sleep(550);
  await input({action:'type',text:`printf '%s' ${q(marker)} > outputs/terminal.txt`});
  await input({action:'key',key:'Return'});
  await api(chat.route+'/computer/lease',{action:'return'},{headers});
}

try {
  ssh(`umask 077; mkdir -p ${q(remoteRoot + '/fixture')}; printf %s ${q(media)} > ${q(remoteRoot + '/fixture/index.html')}; printf %s ${q(marker)} > ${q(remoteRoot + '/fixture/proof.txt')}`);
  webJob = randomBytes(16).toString('hex');
  ssh(`cd ${q(remoteRoot + '/fixture')} && printf %s ${q('exec python3 -u -m http.server 0 --bind 127.0.0.1')} | /usr/local/bin/kageha-linux-session job-start ${webJob} 900`);
  const remotePort = await until('SSH fixture HTTP server',()=>ssh(`/usr/local/bin/kageha-linux-session job-output ${webJob} 0`).match(/port (\d+)/)?.[1]);
  server = http.createServer(async (request,response)=>{
    try {
      if (request.url === '/v1/chat/completions') {
        let raw=''; for await (const chunk of request) raw+=chunk;
        response.setHeader('Content-Type','application/json');
        response.end(JSON.stringify({choices:[{finish_reason:'stop',message:fixtureReply(JSON.parse(raw))}]}));
      } else {
        response.end(request.url === '/proof.txt' ? marker : media);
      }
    } catch (error) { console.error(error.message); response.writeHead(500); response.end(String(error)); }
  });
  server.listen(0,'127.0.0.1'); await once(server,'listening');
  const localUrl = `http://127.0.0.1:${server.address().port}/`;
  const configure = () => api('/api/provider-setup/connect',{provider_name:modelName,protocol:'openai_compat',
    base_url:liveUrl || localUrl+'v1',model:modelId,model_id:`${modelName}/${modelId}`,
    ...(live ? {api_key:required('KAGEHA_TEST_LIVE_MODEL_KEY')} : {})});

  if (process.env.KAGEHA_TEST_SKIP_LOCAL !== '1') {
    await start('headless'); await configure();
    const headless = await create();
    await turn(headless,`Open the Linux inventory at ${localUrl} with Chromium and click Download inventory.`, 'local');
    assert.equal((await api(headless.route+'/browser')).url,localUrl);
    await api(headless.route,undefined,{method:'DELETE'});
    await stop();
    results.push('Linux host headless Chromium');
  }
  await start('desktop'); await configure();
  if (process.env.KAGEHA_TEST_SKIP_LOCAL !== '1') {
    const local = await create();
    await gui(local,'linux_host');
    await until('local desktop wrote the marker',()=>{
      const root=path.join(state,'desktop','state','sessions');
      return fs.readdirSync(root,{recursive:true}).filter(name=>name.endsWith('/outputs/terminal.txt'))
        .some(name=>fs.readFileSync(path.join(root,name),'utf8')===marker);
    });
    await api(local.route,undefined,{method:'DELETE'});
    results.push('Linux host X11 keyboard and terminal');
  }

  await api('/api/remote/hosts',{id:'qualification-ssh',name:'Qualification SSH',host,user,port,identity,
    host_public_key:serverKey,workdir:remoteRoot,enable_browser:true,enable_desktop:true,enable_jobs:true});
  const probe = await api('/api/remote/test',{host_id:'qualification-ssh'});
  assert.equal(probe.ok,true,JSON.stringify(probe));
  for (const feature of ['browser','desktop','jobs']) assert.equal(probe.report[feature],'available',JSON.stringify(probe));
  const remote = await create('qualification-ssh');
  await turn(remote,`Create a release audit on this SSH host. Run this exact shell command ${reportCommand}. Leave the file on SSH and attach the remote_verify_file proof to an artifact outcome.`);
  assert.equal((await api(remote.route+'/state')).turn.status,'success');
  assert.equal(ssh(`cat ${q(remoteRoot+'/outputs/audit.txt')}`),marker);
  const download = await turn(remote,'Retrieve the approved SSH audit from outputs/audit.txt with remote_download. Reply with the exact artifact_path.');
  const artifact = download.message.match(/artifacts\/[a-f0-9]+-audit\.txt/)?.[0];
  assert(artifact,`Missing SSH artifact in ${download.message}`);
  const file = await fetch(base + remote.route+'/files/'+artifact,{headers:{Authorization:`Bearer ${token}`}});
  assert.equal(file.status,200);
  assert.equal(await file.text(),marker);
  const remoteUrl = `http://127.0.0.1:${remotePort}/`;
  await turn(remote,`Open the Linux inventory at ${remoteUrl} with Chromium and click Download inventory.`);
  assert.equal((await api(remote.route+'/browser')).url,remoteUrl);
  await until('remote Chromium download',()=>ssh(`test -f ${q(remoteRoot+'/outputs/proof.txt')} && cat ${q(remoteRoot+'/outputs/proof.txt')}`) === marker);
  await gui(remote,'ssh_desktop');
  await until('remote desktop wrote the marker',()=>ssh(`test -f ${q(remoteRoot+'/outputs/terminal.txt')} && cat ${q(remoteRoot+'/outputs/terminal.txt')}`) === marker);
  const typingTask = `Use the Linux desktop terminal to run printf '%s' ${q(marker)} > outputs/agent-terminal.txt. Use computer_observe and computer_execute to launch XTerm, type the command, and press Return with a fresh observation before each input.`;
  await turn(remote,typingTask);
  await until('agent-controlled SSH desktop wrote the marker',()=>ssh(`test -f ${q(remoteRoot+'/outputs/agent-terminal.txt')} && cat ${q(remoteRoot+'/outputs/agent-terminal.txt')}`) === marker);
  const job = await turn(remote,`Start the release background job using remote_job_start. Run ${backgroundCommand}. Reply with the job ID.`);
  const jobId = job.message.match(/[a-f0-9]{32}/)?.[0];
  assert(jobId,`Missing job ID in ${job.message}`);
  const probeJob = () => ssh(`/usr/local/bin/kageha-linux-session job-status ${jobId}`);
  assert.equal(probeJob(),'running');
  await stop();
  assert.equal(probeJob(),'running');
  ssh(`touch ${q(remoteRoot+'/outputs/release-job')}`);
  await start('desktop');
  await until('job survived restarting Kageha',()=>ssh(`test -f ${q(remoteRoot+'/outputs/background.txt')} && cat ${q(remoteRoot+'/outputs/background.txt')}`) === marker);
  assert.match((await turn(remote,`Read the managed job status for ${jobId}.`)).message,/exited|completed|0/i);
  await api('/api/remote/activate',{session_id:remote.id,active:false});
  results.push('SSH artifact evidence, Chromium download, X11 input, and durable jobs');
  if (process.env.KAGEHA_TEST_DOCKER === '1') {
    const workstation = await create();
    await api(workstation.route+'/machine',{active:true});
    const desktop = await api(workstation.route+'/computer');
    const container = desktop.containerName;
    const docker = (args, options = {}) => execFileSync('docker',args,{encoding:'utf8',timeout:20000,...options}).trim();
    assert(/^kageha-linux-[a-f0-9]{32}$/.test(container));
    try {
      const serve = `#!/bin/sh\nread -r method path rest\nprintf 'HTTP/1.1 200 OK\\r\\nContent-Type: text/html\\r\\nConnection: close\\r\\n\\r\\n'\n`+
        `if [ "$path" = /proof.txt ]; then printf '%s' ${q(marker)}; else printf '%s' ${q(media)}; fi\n`;
      docker(['exec','-i',container,'sh','-c','cat >/tmp/kageha-qualification-http && chmod 700 /tmp/kageha-qualification-http'], {input:serve});
      docker(['exec','-d',container,'socat','TCP-LISTEN:18083,reuseaddr,fork,bind=127.0.0.1','EXEC:/tmp/kageha-qualification-http']);
      await turn(workstation,'Open the Linux inventory at http://127.0.0.1:18083/ with Chromium and click Download inventory.', 'machine');
      await until('Docker Chromium download',()=>docker(['exec',container,'cat','/work/outputs/proof.txt']) === marker);
      await turn(workstation,'Process the Linux inventory with a shell command. Copy outputs/proof.txt exactly to outputs/processed.txt.', 'machine');
      assert.equal(docker(['exec',container,'cat','/work/outputs/processed.txt']),marker);
      await gui(workstation,'docker_desktop');
      assert.equal(docker(['exec',container,'cat','/work/outputs/terminal.txt']),marker);
      await turn(workstation,typingTask,'machine');
      assert.equal(docker(['exec',container,'cat','/work/outputs/agent-terminal.txt']),marker);
      results.push('Docker Chromium download, shell processing, and X11 terminal');
    } finally {
      await api(workstation.route+'/machine',{active:false});
      await api(workstation.route,undefined,{method:'DELETE'});
    }
  }
  const receipt = {commit:process.env.GITHUB_SHA || null,image:process.env.KAGEHA_DESKTOP_IMAGE || null,hostPlatform:`${process.platform}-${process.arch}`,model:live?'live':'deterministic',results};
  if (process.env.KAGEHA_TEST_RECEIPT) fs.writeFileSync(process.env.KAGEHA_TEST_RECEIPT,JSON.stringify(receipt,null,2)+'\n');
  console.log(JSON.stringify(receipt));
} catch (error) {
  console.error(childLog.slice(-6000));
  throw error;
} finally {
  if (child) for (const chat of chats.filter(chat=>chat.hostId)) {
    try { await api('/api/remote/activate',{session_id:chat.id,active:false}); } catch {}
  }
  await stop();
  server?.close();
  if (webJob) { try { ssh(`/usr/local/bin/kageha-linux-session job-stop ${webJob}`); } catch {} }
  try { ssh(`rm -rf -- ${q(remoteRoot)}`); } catch {}
  fs.rmSync(state,{recursive:true,force:true});
}
