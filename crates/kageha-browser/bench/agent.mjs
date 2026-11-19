// Model-driven full MiniWoB catalog evaluation. Production browser execution is Rust.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { createHash } from 'node:crypto';
import { runtime, checked, sleep } from './support.mjs';
import { serveFixture, evaluator, revision } from './miniwob-fixture.mjs';
import { mappedRef, panelInput, panelAction, pointAction } from './agent-tools.mjs';
import { postJson } from './provider-http.mjs';
const args=Object.fromEntries(process.argv.slice(2).map(value=>value.replace(/^--/,'').split('=')));
const catalog=JSON.parse(fs.readFileSync(new URL('./miniwob-catalog.json',import.meta.url)));
const tasks=args.tasks?catalog.tasks.filter(task=>args.tasks.split(',').includes(task.id)):catalog.tasks;
assert(tasks.length,'No selected tasks');
const workers=Number(args.workers??2),maxTurns=Number(args.turns??12),seed=Number(args.seed??1);
assert(Number.isInteger(workers)&&workers>0&&workers<=4);
assert(Number.isInteger(maxTurns)&&maxTurns>0&&maxTurns<=40);
assert(Number.isSafeInteger(seed)&&seed>=1);
const output=path.resolve(args.output??'crates/kageha-browser/bench/results/miniwob-agent.json');
fs.mkdirSync(path.dirname(output),{recursive:true});
const traceFile=output.replace(/\.json$/,'')+'.jsonl';
fs.writeFileSync(traceFile,'',{mode:0o600});
const providerFile=process.env.BROWSER_EVAL_PROVIDER_FILE;
assert(providerFile,'Set BROWSER_EVAL_PROVIDER_FILE to an explicit private provider configuration');
assert((fs.statSync(providerFile).mode&0o077)===0,'Provider file must be private');
const provider=JSON.parse(fs.readFileSync(providerFile,'utf8'));
assert(['openai_compat','openai_responses'].includes(provider.provider),'Unsupported provider');
assert(new URL(provider.base_url).protocol==='https:','Provider must use HTTPS');
const model=process.env.BROWSER_EVAL_MODEL??provider.model;
const reasoning=args.reasoning??'low';
assert(['none','low','high'].includes(reasoning),'Unsupported reasoning effort');
const key=provider.api_key;
assert(typeof key==='string'&&key&&typeof model==='string'&&model,'Model and API credential required');
const isOpenAI=provider.provider==='openai_compat';
const isResponses=provider.provider==='openai_responses';
const isCloudflare=isOpenAI&&new URL(provider.base_url).hostname==='api.cloudflare.com';
const rpm=Number(args.rpm??(isCloudflare?18:60));assert(Number.isFinite(rpm)&&rpm>0&&rpm<=120);
let nextRequest=0,requestInterval=60000/rpm;
async function slot(){const due=Math.max(Date.now(),nextRequest);nextRequest=due+requestInterval;await sleep(Math.max(0,due-Date.now()));}
let providerCalls=0,totalTokens=0,providerRetries=0;
const maximumCalls=Number(args['max-calls']??1500);
const fixture=await serveFixture();
const report={version:1,suite:'miniwob-full-catalog-agent',source_revision:revision,catalog_revision:catalog.revision,
  binary_sha256:createHash('sha256').update(fs.readFileSync(process.env.KAGEHA_BROWSER_BIN)).digest('hex'),
  agent_sha256:createHash('sha256').update(fs.readFileSync(new URL(import.meta.url))).digest('hex'),
  model,reasoning_effort:reasoning,maximum_output_tokens:4096,provider:provider.label??provider.provider,requests_per_minute:rpm,transport:'http',seed,workers,task_count:tasks.length,maximum_turns:maxTurns,maximum_actions:40,
  episode_max_ms:1000000,wall_task_ms:180000,started_at:new Date().toISOString(),results:[],
  protocol:'Independent vision/DOM agent; not Kageha planner and not an official BrowserGym leaderboard run. All browser actions use Rust. Evaluator only resets and grades.'};
const system=`You control a browser to complete the instruction displayed on a synthetic web task. Use the screenshot and visible observations. Coordinates are CSS pixels of the supplied image. Do not manipulate rewards, timers, scripts or evaluator state. Return JSON {"actions":[...],"note":"brief intent"}.
Allowed actions:
{"action":"click","ref":"e1"} for an observed button/link/radio;
{"action":"fill","ref":"e2","text":"value"} for editable text;
{"action":"check","ref":"e3","checked":true} for checkbox/radio;
{"action":"select","ref":"e4","option":"exact option value"} for native select;
{"action":"select","ref":"e4","options":["value1","value2"]} sets the complete selection of a native multi-select; use [] to clear;
{"action":"press","key":"Tab"} or Enter, Escape, ArrowDown, Shift+Tab, Control+A;
{"action":"pointer","x":20,"y":70} to click visible widgets or focus inputs;
{"action":"type","text":"value"} inserts text into the currently focused input (does not clear; emits input, not keydown);
{"action":"move","x":20,"y":70} for hover;
{"action":"drag","x":20,"y":70,"to_x":90,"to_y":80};
{"action":"wheel","x":100,"y":100,"delta_y":100,"delta_x":0};
{"action":"wait","ms":500}; {"action":"done"}.
Prefer exact observed refs. You may return up to 6 typed ref actions if the interface stays stable. Return only one coordinate action per turn. For custom keyboard-driven widgets, focus the visible widget and use individual character press actions (for example l, s, Enter); type is text insertion, not physical typing. For password fields on these synthetic tasks, focus via pointer then type using the displayed synthetic value. Do not invent refs. Never repeat an uncertain dispatch. Fresh state will be supplied on the next turn. Complete the actual visible instruction, not just navigation. Use press and type together to clear inputs. Actions must be based on visible information.`;
async function decision(observation,image,history){
  if(providerCalls>=maximumCalls||totalTokens>=5000000)throw Error('model_budget_exhausted');
  providerCalls++;
  let response;
  for(let attempt=0;attempt<3;attempt++){
  await slot();
  const endpoint=provider.base_url.replace(/\/$/,'')+(isResponses?'/responses':'/chat/completions');
  const prompt='Return a JSON action plan.\n'+JSON.stringify({visible:observation,recent:history.slice(-5)});
  const payload=isResponses?{model,instructions:system,input:[{role:'user',content:[{type:'input_text',text:prompt},{type:'input_image',image_url:`data:image/jpeg;base64,${image}`,detail:'high'}]}],max_output_tokens:4096,reasoning:{effort:reasoning},text:{format:{type:'json_object'}},store:false}:{model,messages:[{role:'system',content:system},{role:'user',content:[{type:'text',text:prompt},{type:'image_url',image_url:{url:`data:image/jpeg;base64,${image}`}}]}],temperature:0,max_tokens:4096,reasoning_effort:reasoning,response_format:{type:'json_object'}};
  try {
    response=await postJson(endpoint,{Authorization:`Bearer ${key}`},payload);
  } catch(error) {
    if(attempt===2)throw error;
    providerRetries++;providerCalls++;await sleep(1000*2**attempt);continue;
  }
  if(response.ok)break;
  if(![429,500,502,503,504].includes(response.status)||attempt===2)throw Error(`provider_http_${response.status}`);
  if(response.status===429){
    // Rate limits apply to the shared account, not an individual worker. Slow
    // future inference requests globally; never retry a browser action here.
    requestInterval=Math.min(30000,requestInterval*2);
    nextRequest=Math.max(nextRequest,Date.now()+30000);
  }
  providerRetries++;providerCalls++;const retry=Number(response.headers.get('retry-after'));await sleep(Number.isFinite(retry)&&retry>0?Math.min(60000,retry*1000):1000*2**attempt);
  }
  if(!response.ok)throw Error(`provider_http_${response.status}`);
  const value=await response.json();totalTokens+=value.usage?.total_tokens??0;
  const text=isResponses?(value.output??[]).filter(item=>item.type==='message').flatMap(item=>item.content??[]).filter(item=>item.type==='output_text').map(item=>item.text??'').join(''):(value.choices?.[0]?.message?.content??'');
  let result;try{result=JSON.parse(text.trim().replace(/^```(?:json)?\s*/, '').replace(/\s*```$/, ''));}catch{throw Error((value.choices?.[0]?.finish_reason==='length'||value.status==='incomplete')?'provider_output_truncated':'provider_invalid_json');}
  if(!Array.isArray(result.actions))throw Error('provider_invalid_actions');
  if(result.actions.length>6){result.truncated_actions=result.actions.length-6;result.actions=result.actions.slice(0,6);}
  return result;
}
function save(){
  report.updated_at=new Date().toISOString();report.model_calls=providerCalls;report.provider_retries=providerRetries;report.total_model_tokens=totalTokens;
  report.summary={attempted:report.results.length,total:tasks.length,strict_success:report.results.filter(r=>r.grade?.done&&r.grade.raw===1).length,positive_reward:report.results.filter(r=>r.grade?.done&&r.grade.raw>0).length,browser_errors:report.results.filter(r=>r.errors.some(e=>e.kind==='browser')).length,setup_errors:report.results.filter(r=>r.outcome==='setup_error').length};
  fs.writeFileSync(output,JSON.stringify(report,null,2)+'\n');
}
let cursor=0,providerFailures=0;
async function worker(workerId){
  let engine;
  try{
    while(cursor<tasks.length&&!report.infrastructure_blocked){
      const task=tasks[cursor++],start=Date.now();let browser_id,tab,evalClient,grade;
      const errors=[],history=[];let actions=0,turns=0,outcome='not_solved';
      try{
        engine=await runtime('http');
        engine.setBudget(400);
        browser_id=checked(await engine.call('launch_browser')).browser_id;
        tab=checked(await engine.call('open_tab',{browser_id,url:`${fixture.base}${task.id}.html`}));
        const selection=()=>engine.call('browser_selection').then(checked);
        let chosen=await selection();
        let frame=checked(await engine.call('preview_tab',{browser_id,target_id:tab.target_id,binding_id:chosen.binding_id}));
        let authority=checked(await engine.call('claim_control',{ttl_ms:60000})).authority;
        checked(await engine.call('panel_action',{selection:{browser_id,target_id:tab.target_id,binding_id:chosen.binding_id},viewport:{width:frame.width,height:frame.height},input:{action:'layout',size:{width:332,height:214}}},{authority}));
        evalClient=await evaluator(engine,tab.target_id);await evalClient.reset(seed,1000000);
        const observe=()=>engine.call('observe_tab',{browser_id,target_id:tab.target_id,compact:false}).then(checked);
        for(;turns<maxTurns&&actions<40&&Date.now()-start<180000;turns++){
          grade=await evalClient.grade();if(grade.done){outcome=grade.raw===1?'success':grade.raw>0?'partial':'not_solved';break;}
          const original=await observe();chosen=await selection();
          const image=await engine.call('preview_tab',{browser_id,target_id:tab.target_id,binding_id:chosen.binding_id});checked(image);
          let plan;try{plan=await decision({controls:original.controls,text:original.text,frames:original.frames,truncated:original.truncated,width:image.result.width,height:image.result.height},image.binary_base64,history);}catch(error){errors.push({kind:'provider',code:error.message});outcome='provider_error';break;}
          fs.appendFileSync(traceFile,JSON.stringify({worker:workerId,task:task.id,turn:turns,plan,controls:original.controls,text:original.text})+'\n');
          let last;
          for(const action of plan.actions){
            if(actions>=40)break;
            const coordinate=panelAction(action.action);
            try{
              if(action.action==='done'){last={declared_done:true};break;}
              if(action.action==='wait'){await sleep(Math.min(2000,Math.max(0,Number(action.ms)||0)));last={waited:true};break;}
              authority=checked(await engine.call('claim_control',{ttl_ms:60000})).authority;
              let reply;
              if(coordinate){
                const current=await selection();if(current.binding_id!==chosen.binding_id)throw Error('binding_changed_before_dispatch');
                const native=panelInput(action);
                reply=await engine.call('panel_action',{selection:{browser_id,target_id:tab.target_id,binding_id:current.binding_id},viewport:{width:image.result.width,height:image.result.height},input:native},{authority});
              }else{
                const fresh=await observe();const target=mappedRef(original,fresh,action.ref);
                const params={browser_id,target_id:tab.target_id,observation_id:fresh.observation_id,target:target.ref,...(target.frame_id?{frame_id:target.frame_id}:{})};
                let method;
                if(action.action==='click'){method='click_observed';params.timeout_ms=1000;}
                else if(action.action==='fill'){method='fill_observed';params.text=action.text??'';}
                else if(action.action==='check'){method='check_observed';params.checked=action.checked??true;}
                else if(action.action==='select'){method='select_observed';if(!('options' in action)&&!('option' in action))throw Error('select_values_required');params.option=action.options??action.option;}
                else throw Error('unsupported_agent_action');
                reply=await engine.call(method,params,{authority});
              }
              actions++;
              last={action,status:reply.status,phase:reply.execution_phase,error:reply.error?.code,verification:reply.result?.verification};
              if(reply.status!=='ok'){errors.push({kind:'browser',code:reply.error?.code,phase:reply.execution_phase});break;}
              grade=await evalClient.grade();if(grade.done)break;
              // Clicks can replace menus, calendars, or whole documents. Resume
              // planning from fresh visible state instead of replaying the rest
              // of a model plan against controls from before the click.
              if(pointAction(action.action)||action.action==='click')break;
            }catch(error){last={action,error:error.message};errors.push({kind:'adapter',code:error.message});break;}
          }
          history.push({plan,last});
          if(grade?.done){outcome=grade.raw===1?'success':grade.raw>0?'partial':'not_solved';break;}
          if(last?.declared_done)break;
        }
        grade=await evalClient.grade();
        if(grade.done&&grade.raw===1)outcome='success';
      }catch(error){errors.push({kind:'setup',code:error.message.slice(0,1000)});outcome='setup_error';}
      finally{
        evalClient?.close();
        // A benchmark episode owns its whole disposable daemon. Always destroy
        // it, including on uncertain receipts, so later tasks cannot inherit a hold.
        if(engine){await engine.close();engine=null;}
      }
      providerFailures=outcome==='provider_error'?providerFailures+1:0;
      if(providerFailures>=3)report.infrastructure_blocked='Repeated provider failures; resume only after connectivity is repaired.';
      report.results.push({task:task.id,seed,worker:workerId,outcome,actions,turns,ms:Date.now()-start,grade,errors});save();
      console.log(`${report.results.length}/${tasks.length} ${task.id} ${outcome} raw=${grade?.raw??'n/a'} actions=${actions} errors=${errors.map(e=>e.code).join(',')}`);
    }
  }finally{if(engine)await engine.close();}
}
try{await Promise.all(Array.from({length:workers},(_,i)=>worker(i)));}
finally{await fixture.close();report.finished_at=new Date().toISOString();save();console.log(JSON.stringify(report.summary));}
if(report.summary.attempted!==tasks.length||report.results.some(r=>r.outcome!=='success'))process.exitCode=1;
