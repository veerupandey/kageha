// Opt-in real-model evaluation: Kageha loop + native helper + owned AppKit app.
// The provider relay only scopes tools and records evidence; it never selects actions.
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import http from 'node:http';
import crypto from 'node:crypto';
import assert from 'node:assert/strict';
import {spawn,spawnSync} from 'node:child_process';
const repo=process.cwd(), provider=process.env.KAGEHA_MODEL_BENCH_PROVIDER||'openai';
assert(['openai','cloudflare'].includes(provider),'Unsupported benchmark provider');
const model=process.env.KAGEHA_MODEL_BENCH_MODEL||(provider==='cloudflare'?'@cf/deepseek-ai/deepseek-v4.1-flash-1017':'gpt-5.4');
const suite=process.env.KAGEHA_MODEL_BENCH_SUITE||'controls';
assert(['controls','multiturn','textedit-demo','multiapp-demo'].includes(suite),'Unsupported benchmark suite');
const documentDemo=['textedit-demo','multiapp-demo'].includes(suite);
const seed=Number(process.env.KAGEHA_MODEL_BENCH_SEED||27011);assert(Number.isSafeInteger(seed)&&seed>0&&seed<1000000,'Invalid fixture seed');
let fileEnv={};if(process.env.KAGEHA_MODEL_BENCH_ENV_FILE){for(const line of fs.readFileSync(process.env.KAGEHA_MODEL_BENCH_ENV_FILE,'utf8').split('\n')){const m=line.match(/^\s*(?:export\s+)?([A-Z_][A-Z_0-9]*)\s*=\s*(.*)$/);if(m)fileEnv[m[1]]=m[2].trim().replace(/^(["'])(.*)\1$/,'$2')}}
const credential=name=>process.env[name]||fileEnv[name];
const key=credential(provider==='cloudflare'?'CLOUDFLARE_API_TOKEN':'OPENAI_API_KEY');
assert(key,'Configure provider credentials through environment or KAGEHA_MODEL_BENCH_ENV_FILE; credentials are never logged');
const account=credential('CLOUDFLARE_ACCOUNT_ID');
if(provider==='cloudflare')assert(/^[a-f0-9]{32}$/i.test(account||''),'Invalid Cloudflare account configuration');
const endpoint=provider==='cloudflare'?`https://api.cloudflare.com/client/v4/accounts/${account}/ai/v1/chat/completions`:'https://api.openai.com/v1/chat/completions';
const reasoning=provider==='cloudflare'?(process.env.KAGEHA_MODEL_BENCH_REASONING||'low'):undefined;
const host=process.env.KAGEHA_LOOP_BIN,helper=process.env.KAGEHA_COMPUTER_BIN;
assert(host&&helper,'Set absolute KAGEHA_LOOP_BIN and KAGEHA_COMPUTER_BIN');
const resumed=process.env.KAGEHA_DEMO_RESUME_REPORT?JSON.parse(fs.readFileSync(process.env.KAGEHA_DEMO_RESUME_REPORT,'utf8')):null;
const root=fs.mkdtempSync(path.join(os.tmpdir(),`kageha-${provider}-computer-`));fs.chmodSync(root,0o700);
const runtimeRoot=resumed?(resumed.runtime_root||resumed.root):root;
for(const n of ['workspace','computer','legacy'])fs.mkdirSync(path.join(root,n),{mode:0o700});
const token=crypto.randomBytes(24).toString('hex');
const hash=p=>crypto.createHash('sha256').update(fs.readFileSync(p)).digest('hex');
const report={model,provider,suite,reasoning,seed,harness_sha256:hash(new URL(import.meta.url)),suite_version:2,runtime_root:runtimeRoot,resumed_from:process.env.KAGEHA_DEMO_RESUME_REPORT,scope:'Real provider model through Kageha Rust agent loop; AX-only owned fixture, not screenshot or public benchmark score',root,host_sha256:hash(host),helper_sha256:hash(helper),provider_calls:[],trials:[],approvals:[]};
const persist=()=>fs.writeFileSync(path.join(root,'report.json'),JSON.stringify(report,null,2),{mode:0o600});
const sleep=ms=>new Promise(r=>setTimeout(r,ms));
let fixture,child,base,pid,wid,chat,demoConfig,calculator,stopApprovals=false;
const targets=[];
let readState;
const relay=http.createServer(async(req,res)=>{
  try{
    let body='';for await(const c of req)body+=c;
    const value=JSON.parse(body);assert.equal(value.model,model);
    value.tools=value.tools.filter(t=>t.function.name.startsWith('computer_')&&t.function.name!=='computer_apps');
    report.tool_results ??= []; report.tool_results.push(...value.messages.filter(m=>m.role==='tool').map(m=>({id:m.tool_call_id,content:m.content})).filter(m=>!report.tool_results.some(p=>p.id===m.id)));
    value.messages.unshift({role:'system',content:`This is an owned native computer-use benchmark. Only control these exact process/window targets: ${JSON.stringify(targets.length?targets:[{pid,window_id:wid}])}. Never inspect or control any other application/window. Use only the provided computer tools, no filesystem or shell. This suite uses AX control: use include_screenshot:false. Get fresh observations before input. Use element_token for native elements; set_value uses value. A dispatched click is not task completion: observe actual state. Do not retry uncertain input. The host approves only this exact disposable window.`});
    if(reasoning)value.reasoning_effort=reasoning;
    const at=Date.now();
    const upstream=await fetch(endpoint,{method:'POST',headers:{Authorization:`Bearer ${key}`,'Content-Type':'application/json'},body:JSON.stringify(value),signal:AbortSignal.timeout(90000)});
    const text=await upstream.text();let response;try{response=JSON.parse(text)}catch{response={error:{message:'Non-JSON upstream response'}}}
    report.provider_calls.push({status:upstream.status,user_turns:value.messages.filter(m=>m.role==='user').length,user_history_sha256:crypto.createHash('sha256').update(JSON.stringify(value.messages.filter(m=>m.role==='user'))).digest('hex'),ms:Date.now()-at,model:response.model,usage:response.usage,error:response.error,tool_calls:response.choices?.[0]?.message?.tool_calls,content:response.choices?.[0]?.message?.content});persist();
    res.writeHead(upstream.status,{'Content-Type':'application/json'});res.end(JSON.stringify(response));
  }catch(e){report.relay_error=String(e);persist();res.writeHead(502,{'Content-Type':'application/json'});res.end(JSON.stringify({error:{message:String(e)}}));}
});
async function api(url,body){const r=await fetch(base+url,{method:body?'POST':'GET',headers:{Authorization:`Bearer ${token}`,'Content-Type':'application/json'},body:body?JSON.stringify(body):undefined,signal:AbortSignal.timeout(240000)});const v=await r.json();assert.equal(r.status,200,JSON.stringify(v));return v;}
async function until(fn,ms=30000){const end=Date.now()+ms;while(Date.now()<end){const v=await fn();if(v)return v;await sleep(75)}throw Error('Timed out');}
let approvalLoop;
try{
  if(documentDemo){
    if(resumed){ demoConfig=resumed.demo_target; }else if(process.env.KAGEHA_COMPUTER_DEMO_CONFIG){
      demoConfig=JSON.parse(fs.readFileSync(process.env.KAGEHA_COMPUTER_DEMO_CONFIG,'utf8'));
    }else{
      const file=path.join(root,'workspace','Kageha-kickoff-brief.txt');fs.writeFileSync(file,'',{flag:'wx',mode:0o600});
      const texteditPids=()=>spawnSync('/usr/bin/pgrep',['-x','TextEdit'],{encoding:'utf8'}).stdout.trim().split('\n').filter(Boolean).map(Number);
      const before=texteditPids();
      const opened=spawnSync('/usr/bin/open',['-n','-a','/System/Applications/TextEdit.app',file,'--args','-ApplePersistenceIgnoreState','YES','-NSQuitAlwaysKeepsWindows','NO'],{encoding:'utf8'});
      assert.equal(opened.status,0,opened.stderr);
      const owner=await until(()=>{const fresh=texteditPids().filter(p=>!before.includes(p));assert(fresh.length<=1,'Ambiguous newly launched TextEdit owner');return fresh[0]});
      const nativeWindows=()=>new Promise((resolve,reject)=>{
        const probe=spawn(helper,['serve','--stdio'],{stdio:['pipe','pipe','ignore']});let buffer='';const timer=setTimeout(()=>{probe.kill();reject(Error('Native window inventory timed out'))},5000);
        probe.on('error',e=>{clearTimeout(timer);reject(e)});
        probe.stdout.on('data',chunk=>{buffer+=String(chunk);if(buffer.includes('\n')){clearTimeout(timer);probe.stdin.end();try{resolve(JSON.parse(buffer.split('\n')[0]))}catch(e){reject(e)}}});
        probe.stdin.write(JSON.stringify({id:1,method:'list_windows',args:{pid:owner}})+'\n');
      });
      const window=await until(async()=>{const v=await nativeWindows();return v.result?.windows?.find(w=>w.is_on_screen&&w.title===path.basename(file))});
      demoConfig={file,pid:owner,window_id:window.window_id,created_by:'LaunchServices open -n; pre-existing TextEdit processes excluded'};
    }
    assert(path.isAbsolute(demoConfig.file)&&fs.existsSync(demoConfig.file),'Missing owned demo document');
    pid=demoConfig.pid;wid=demoConfig.window_id;assert(Number.isSafeInteger(pid)&&Number.isSafeInteger(wid),'Missing exact demo target');
    if(!resumed)assert(!fs.readFileSync(demoConfig.file,'utf8'),'Demo must start with the empty setup document');
    readState=()=>({text:fs.readFileSync(demoConfig.file,'utf8'),sha256:hash(demoConfig.file)});
    report.scope='Real Cloudflare model through Kageha Rust loop controlling an owned TextEdit instance; verified saved document, not a fixture task';
    report.document=demoConfig.file;report.native_app='TextEdit';report.demo_target=demoConfig;
  }else{
  const contents=path.join(root,'BenchmarkFixture.app','Contents');fs.mkdirSync(path.join(contents,'MacOS'),{recursive:true});
  fs.writeFileSync(path.join(contents,'Info.plist'),`<?xml version="1.0"?><plist version="1.0"><dict><key>CFBundleIdentifier</key><string>test.kageha.native.${crypto.randomBytes(6).toString('hex')}</string><key>CFBundleName</key><string>Kageha Benchmark</string><key>CFBundleExecutable</key><string>BenchmarkFixture</string><key>CFBundlePackageType</key><string>APPL</string></dict></plist>`);
  const fixtureBin=path.join(contents,'MacOS','BenchmarkFixture');const build=spawnSync('/usr/bin/xcrun',['swiftc','-O',path.join(repo,'crates/kageha-computer/bench/BenchmarkFixture.swift'),'-o',fixtureBin],{encoding:'utf8'});assert.equal(build.status,0,build.stderr);
  const signed=spawnSync('/usr/bin/codesign',['--force','--sign','-',path.join(root,'BenchmarkFixture.app')],{encoding:'utf8'});assert.equal(signed.status,0,signed.stderr);
  assert.equal(spawnSync('/usr/bin/codesign',['--verify','--strict',path.join(root,'BenchmarkFixture.app')]).status,0,'Fixture signature verification failed');
  report.fixture_executable_sha256=hash(fixtureBin);report.fixture_info_sha256=hash(path.join(contents,'Info.plist'));report.fixture_signed=true;
  const oracle=path.join(root,'oracle.json');fixture=spawn(fixtureBin,[oracle,String(seed)],{detached:true,stdio:['pipe','pipe','ignore']});
  const initial=await until(()=>{try{const v=JSON.parse(fs.readFileSync(oracle));return v.ready&&v}catch{return false}});pid=initial.pid;wid=initial.primary_window;
  // AppKit must be running before setup activation, as in the scripted suite.
  const resetAck=new Promise(resolve=>fixture.stdout.once('data',resolve)); fixture.stdin.write('reset\n'); await resetAck;
  if(process.env.KAGEHA_MODEL_BENCH_PREFLIGHT==='1'){
    const probe=spawn(helper,['serve','--stdio'],{stdio:['pipe','pipe','inherit']});
    const result=new Promise(resolve=>probe.stdout.once('data',c=>resolve(JSON.parse(String(c)))));
    probe.stdin.write(JSON.stringify({id:1,method:'get_window_state',args:{pid,window_id:wid,include_screenshot:false}})+'\n');
    report.native_preflight=await result;probe.stdin.end();
    assert(!report.native_preflight.error,'Native preflight failed: '+JSON.stringify(report.native_preflight.error));
  }
  report.fixture_sha256=hash(path.join(repo,'crates/kageha-computer/bench/BenchmarkFixture.swift'));
  readState=()=>JSON.parse(fs.readFileSync(oracle));
  }
  targets.push({pid,window_id:wid});
  if(suite==='multiapp-demo'&&resumed){ calculator=resumed.calculator;targets.push(calculator);report.calculator=calculator; }else if(suite==='multiapp-demo'){
    const ids=()=>spawnSync('/usr/bin/pgrep',['-x','Calculator'],{encoding:'utf8'}).stdout.trim().split('\n').filter(Boolean).map(Number);
    const before=ids();assert.equal(spawnSync('/usr/bin/open',['-n','-a','/System/Applications/Calculator.app','--args','-ApplePersistenceIgnoreState','YES']).status,0);
    const owner=await until(()=>ids().find(p=>!before.includes(p)));
    const windows=()=>new Promise(resolve=>{const probe=spawn(helper,['serve','--stdio'],{stdio:['pipe','pipe','ignore']});let text='';probe.stdout.on('data',c=>{text+=c;if(text.includes('\n')){probe.stdin.end();resolve(JSON.parse(text.split('\n')[0]))}});probe.stdin.write(JSON.stringify({id:1,method:'list_windows',args:{pid:owner}})+'\n')});
    const w=await until(async()=>{const r=await windows();return r.result?.windows?.find(w=>w.is_on_screen&&w.bounds.width>150)});
    calculator={pid:owner,window_id:w.window_id};targets.push(calculator);report.calculator=calculator;
  }
  await new Promise(r=>relay.listen(0,'127.0.0.1',r));
  const log=path.join(root,'host.log'),fd=fs.openSync(log,'a',0o600);
  const env={...process.env,KAGEHA_SERVER_TOKEN:token,KAGEHA_PROVIDER_URL:`http://127.0.0.1:${relay.address().port}/v1/chat/completions`,KAGEHA_MODEL:model,KAGEHA_COMPUTER_BIN:helper,KAGEHA_COMPUTER_STATE_DIR:path.join(runtimeRoot,'computer'),KAGEHA_COMPUTER_ALLOW_INPUT:'1',KAGEHA_LEGACY_SESSIONS_DIR:path.join(root,'legacy')};
  delete env.KAGEHA_API_KEY;delete env.OPENAI_API_KEY;
  child=spawn(host,['serve',path.join(runtimeRoot,'workspace'),path.join(runtimeRoot,'state'),'127.0.0.1:0'],{env,stdio:['ignore',fd,fd]});fs.closeSync(fd);
  base=await until(()=>{if(child.exitCode!==null)throw Error('Host exited');return fs.readFileSync(log,'utf8').match(/listening on ([^\s]+)/)?.[1]}).then(s=>`http://${s}`);
  chat=resumed?resumed.trials[0].reply.session_id:(await api('/api/sessions',{})).session_id;
  approvalLoop=(async()=>{while(!stopApprovals){const s=await api(`/api/sessions/${chat}`),p=s.pending_approval;if(p){const approved=p.action==='computer_execute'&&(targets.length?targets:[{pid,window_id:wid}]).some(t=>p.detail.includes(`process ${t.pid}, window ${t.window_id} (`));await api('/api/approvals',{approval_id:p.approval_id,approved,scope:'once'});report.approvals.push({action:p.action,detail:p.detail,approved});persist();if(!approved)throw Error('Model requested out-of-scope action')}await sleep(75)}})();
  const title=`doc-GPT54-東京-${seed}`;
  const saved=()=>JSON.parse(fs.readFileSync(path.join(root,'saved.json')));
  const controlCases=[
    {id:'unicode-field',prompt:`In process ${pid}, window ${wid}, set Primary field to exactly ${title}. Verify it. Do not change anything else.`,grade:s=>s.primary===title&&s.saves===0},
    {id:'checkbox-enable',prompt:'Enable Enable setting and verify it is checked. Keep the text and other controls unchanged.',grade:s=>s.primary===title&&s.checkbox===1&&s.radio_a===1},
    {id:'radio-selection',prompt:'Select Option B and verify B is selected and A is not. Preserve the text and checked setting.',grade:s=>s.radio_b===1&&s.radio_a===0&&s.checkbox===1&&s.primary===title},
    {id:'persisted-save',prompt:'Click Save document exactly once now. Verify the form state without saving twice.',grade:s=>s.saves===1&&saved().title===title&&saved().enabled===true&&saved().option==='B'},
    {id:'delayed-click',prompt:'Click Delayed action exactly once. Do not retry this click or change other controls.',grade:s=>s.delayed_clicks===1&&s.saves===1},
    {id:'window-isolated-click',prompt:'Click Increment once exactly once in the PRIMARY window, leaving the identical secondary-window button untouched.',grade:s=>s.primary_clicks===1&&s.secondary_clicks===0},
    {id:'multiturn-unicode-edit',prompt:'Continue in the same primary window. Change Primary field to exactly draft-東京-👩🏽‍💻 and verify it. Do not save again.',grade:s=>s.primary==='draft-東京-👩🏽‍💻'&&s.saves===1&&saved().title===title},
    {id:'toggle-off',prompt:'Turn Enable setting OFF and verify it is unchecked. Keep the unsaved draft text unchanged.',grade:s=>s.checkbox===0&&s.primary==='draft-東京-👩🏽‍💻'},
    {id:'radio-return',prompt:'Select Option A again and verify A is selected and B is not. Leave the unchecked setting and draft intact.',grade:s=>s.radio_a===1&&s.radio_b===0&&s.checkbox===0&&s.primary==='draft-東京-👩🏽‍💻'},
    {id:'second-save-workflow',prompt:'Replace Primary field with exactly doc-final-東京, enable Enable setting, select Option B, then click Save document ONCE. Verify the resulting values. Do not touch the disabled or secure controls.',grade:s=>s.primary==='doc-final-東京'&&s.checkbox===1&&s.radio_b===1&&s.saves===2&&saved().title==='doc-final-東京'&&s.delayed_clicks===1&&s.primary_clicks===1}
  ];
  const initialTitle=`doc-Cloudflare-東京-${seed}`;
  const draftTitle=`draft-correction-👩🏽‍💻-${seed}`;
  const multiCases=[
    {id:'initial-document',prompt:`In process ${pid}, window ${wid}, set Primary field to exactly ${initialTitle}, enable Enable setting, select Option B, then save once. Remember this exact Primary field value for later turns.`,grade:s=>s.primary===initialTitle&&s.checkbox===1&&s.radio_b===1&&s.saves===1&&saved().title===initialTitle},
    {id:'unsaved-correction',prompt:`Correction: change the field to exactly ${draftTitle}, turn the setting off and select Option A. Do NOT save; keep the saved document intact.`,grade:s=>s.primary===draftTitle&&s.checkbox===0&&s.radio_a===1&&s.saves===1&&saved().title===initialTitle},
    {id:'recall-saved-title',readOnly:true,prompt:'What exact Primary field value did I ask you to save in my first message? Reply with that field text exactly, not the OS window title. Do not change any controls.',grade:(s,reply)=>reply.message.includes(initialTitle)&&s.primary===draftTitle&&s.saves===1},
    {id:'restore-from-history',prompt:'Restore the original Primary field text from my first message, without me repeating it. Keep the setting OFF and Option A. Do not save.',grade:s=>s.primary===initialTitle&&s.checkbox===0&&s.radio_a===1&&s.saves===1},
    {id:'narrow-followup',prompt:'Now change ONLY the radio choice to Option B. Keep the field text and the unchecked setting exactly as they are. Do not save.',grade:s=>s.primary===initialTitle&&s.checkbox===0&&s.radio_b===1&&s.saves===1},
    {id:'delayed-actions-once',prompt:'In the same PRIMARY window click Delayed action once and Increment once once. Do not repeat either action and do not touch the identically labeled secondary-window button.',grade:s=>s.delayed_clicks===1&&s.primary_clicks===1&&s.saves===1},
    {id:'commit-followup',prompt:'Turn the setting ON, preserve the current title and Option B, then save once. Do not repeat either earlier action button.',grade:s=>s.primary===initialTitle&&s.checkbox===1&&s.radio_b===1&&s.saves===2&&saved().title===initialTitle&&s.delayed_clicks===1&&s.primary_clicks===1},
    {id:'new-unsaved-draft',prompt:'Start another unsaved edit: set the field to exactly draft-cancel-me. Do not save or change the toggles.',grade:s=>s.primary==='draft-cancel-me'&&s.saves===2&&saved().title===initialTitle},
    {id:'cancel-last-edit',prompt:'Cancel that last unsaved edit: restore the saved Primary field text using our conversation. Keep the setting enabled and B selected. Do not save again.',grade:s=>s.primary===initialTitle&&s.checkbox===1&&s.radio_b===1&&s.saves===2},
    {id:'readonly-final-audit',readOnly:true,prompt:'Verify the final Primary field text, enabled setting and radio selection. Report that exact field value and selected option. Do not change or save anything.',grade:(s,reply)=>s.primary===initialTitle&&s.checkbox===1&&s.radio_b===1&&s.saves===2&&s.delayed_clicks===1&&s.primary_clicks===1&&reply.message.includes(initialTitle)&&/B/.test(reply.message)}
  ];
  const demoSections=['Purpose','Scope','Milestones','Acceptance checks','Risks','Next steps'];
  let savedDemoHash;
  const demoCases=[
    {id:'create-kickoff-brief',prompt:`Use only Kageha's computer tools to edit the ALREADY OPEN empty TextEdit document Kageha-kickoff-brief.txt in process ${pid}, window ${wid}. Create and SAVE a concise professional kickoff brief for a fictional internal project called Kageha Rust Desktop Pilot. Include a title and these sections: Purpose, Scope, Milestones, Acceptance checks, Risks, Next steps. Aim for 250-350 words. Cover a two-week pilot of native document editing, explicit action approval, recovery without duplicate input, and independent saved-file verification. Label this as a pilot proposal, not a production-readiness claim. Write through the observed AXTextArea using the computer tools, then save with Cmd+S in the exact focused document window. Re-observe between every action. Do not use a shell, filesystem writer, browser, or any other application/window. Verify the text in TextEdit after saving.`,grade:s=>s.text.length>900&&s.text.includes('Kageha Rust Desktop Pilot')&&demoSections.every(h=>s.text.includes(h))},
    {id:'revise-user-visibility',prompt:'Revise that same brief, preserving all existing sections. Add a User visibility section that clearly says the user sees changes in the actual macOS app, while the Rust Kageha right panel currently has no live desktop preview. Also note that Screen Recording permission is required for screenshot capture and that AX interaction does not imply visible mouse-cursor movement. Add a clear follow-up to implement live preview and takeover UI. Save with Cmd+S, then verify the revised text. Do not claim the app is production-ready.',grade:s=>demoSections.every(h=>s.text.includes(h))&&s.text.includes('User visibility')&&/Screen Recording/i.test(s.text)&&/preview/i.test(s.text)&&/cursor/i.test(s.text)},
    {id:'verify-saved-deliverable',readOnly:true,prompt:'Perform a final READ-ONLY check in TextEdit. Confirm the document contains the kickoff brief and User visibility section, check the document edited/saved indicator if exposed, and report the file name plus a concise summary. Do not edit or save again. Do not claim screenshot or live-panel proof, since neither was captured.',grade:s=>s.sha256===savedDemoHash&&s.text.includes('User visibility')}
  ];
  const sawCalculator=(number)=>(report.tool_results||[]).some(t=>{try{const v=JSON.parse(t.content);return v.result?.pid===calculator.pid&&v.result.elements?.some(e=>String(e.value??e.label??'').replaceAll(',','').includes(String(number)))}catch{return false}});
  const multiAppCases=[
    {id:'calculate-subtotal',prompt:`Use the real Calculator app in process ${calculator?.pid}, window ${calculator?.window_id} to calculate 48 multiplied by 27. Enter the calculation using computer tools, read the displayed result, and report it. Leave TextEdit unchanged for now. Before keyboard input explicitly focus the Calculator window with focus_window args {} and take a fresh observation; press_key takes a complete chord in args.key.`,grade:s=>s.text===''&&sawCalculator(1296)},
    {id:'write-budget-memo',prompt:`Now use the already open TextEdit document in process ${pid}, window ${wid}. Write a concise proposal memo for 48 pilot licenses priced at $27 each, using the Calculator result from the previous turn as the subtotal. Include Breakdown, Total, Approval notes and Next steps. Label it a proposal; do not purchase or submit anything. Save the document with Cmd+S, explicitly focusing this exact window before keyboard input. Verify the text after saving.`,grade:s=>s.text.replaceAll(',','').includes('1296')&&['breakdown','total','approval notes','next steps'].every(h=>s.text.toLowerCase().includes(h))},
    {id:'recalculate-and-revise',prompt:'Add a one-time $150 setup fee to the previously calculated subtotal. Use Calculator to calculate the grand total, read its result, then update and SAVE the TextEdit memo with the subtotal, fee and grand total while retaining its sections. Do not purchase, send or submit anything.',grade:s=>sawCalculator(1446)&&s.text.replaceAll(',','').includes('1446')&&s.text.includes('150')&&s.text.replaceAll(',','').includes('1296')},
    {id:'readonly-budget-audit',readOnly:true,prompt:'Perform a final read-only audit of the TextEdit budget memo. Report the subtotal, setup fee and grand total. Do not edit or save again.',grade:s=>s.text.replaceAll(',','').includes('1446')&&s.text.includes('150')}
  ];
  let cases=suite==='multiapp-demo'?multiAppCases:documentDemo?demoCases:suite==='multiturn'?multiCases:controlCases;
  if(resumed){cases=cases.filter(t=>['recalculate-and-revise','readonly-budget-audit'].includes(t.id));cases[0].prompt='The Kageha host restarted. Use fresh observations of both authorized apps; old input authority and tokens are invalid. Continue the unfinished revision from the existing document. Calculator may already show the verified grand total, so inspect it before repeating any inputs. Finish only the missing edits and save. '+cases[0].prompt;}
  for(const t of cases){const at=Date.now(),start=report.provider_calls.length;const reply=await api('/api/chat/send',{session_id:chat,client_request_id:(resumed?`resume-${Date.now()}-`:'')+t.id,message:t.prompt,max_steps:suite==='multiapp-demo'?64:28});await sleep(150);const state=readState();let observedText;for(const result of report.tool_results||[]){try{const value=JSON.parse(result.content);const area=value.result?.elements?.find(e=>e.role==='AXTextArea');if(area)observedText=area.value}catch{}}let passed=false;try{passed=t.grade(state,reply)&&(!documentDemo||t.id==='calculate-subtotal'||state.text===observedText)&&(!t.readOnly||!report.provider_calls.slice(start).some(c=>c.tool_calls?.some(call=>call.function.name==='computer_execute')))&&(documentDemo||(state.secondary==='initial-secondary'&&state.secondary_clicks===0&&state.disabled==='unchanged-disabled'&&state.disabled_clicks===0&&state.secure===''))}catch{};if(documentDemo&&t.id==='revise-user-visibility'&&passed)savedDemoHash=state.sha256;report.trials.push({id:t.id,prompt:t.prompt,passed,saved_matches_editor:documentDemo?state.text===observedText:undefined,panel_state:documentDemo?await api(`/api/sessions/${chat}/computer`):undefined,ms:Date.now()-at,model_calls:report.provider_calls.length-start,reply,oracle:state});persist();console.log(JSON.stringify({case:t.id,passed,model_calls:report.provider_calls.length-start}));}
  report.passed=report.trials.filter(t=>t.passed).length;report.failed=report.trials.length-report.passed;
  report.tool_errors=(report.tool_results||[]).filter(t=>{try{const v=JSON.parse(t.content);return v.error||['error','unknown'].includes(v.status)||['error','unknown'].includes(v.outcome?.status)}catch{return true}});
  report.provider_errors=report.provider_calls.filter(c=>c.status!==200);
  report.artifact_changed=report.host_sha256!==hash(host)||report.helper_sha256!==hash(helper);
  if(report.failed||report.artifact_changed||report.tool_errors.length||report.provider_errors.length)process.exitCode=1;
}catch(e){report.error=String(e);process.exitCode=1;}
finally{
  stopApprovals=true;if(approvalLoop)await approvalLoop.catch(e=>{report.approval_error=String(e)});
  for(const p of [child,fixture])if(p&&p.exitCode===null){p.kill('SIGTERM');await Promise.race([new Promise(r=>p.once('exit',r)),sleep(3000)])}
  relay.closeAllConnections();if(relay.listening)await new Promise(r=>relay.close(r));persist();console.log(JSON.stringify({report:path.join(root,'report.json'),passed:report.passed,failed:report.failed,error:report.error}));
}
