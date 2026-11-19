// Seeded browser-control benchmark. Drivers use public Rust API/MCP only.
// Fixture commands simulate an external rerender/navigation; audit events grade outcomes.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import http from 'node:http';
import { once } from 'node:events';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { runtime, checked, until, sleep } from './support.mjs';
const args = Object.fromEntries(process.argv.slice(2).map(value => value.replace(/^--/, '').split('=')));
const seeds = Number(args.seeds ?? 3);
const seedStart = Number(args['seed-start'] ?? 1);
assert(Number.isSafeInteger(seedStart) && seedStart >= 1 && seedStart <= 1000000);
if (!Number.isInteger(seeds) || seeds < 1 || seeds > 100) throw Error('seeds must be 1..100');
const transports = args.transport && args.transport !== 'both' ? [args.transport] : ['http', 'mcp'];
assert(transports.every(value => ['http', 'mcp'].includes(value)));
const fixtures = new Map();
let base;
function html(id, scenario, seed, stage = '') {
  const state = fixtures.get(id);
  const target = `Target ${seed}`;
  const button = `<button id="123${seed}" onclick="audit('target',event)">${target}</button>`;
  let body;
  switch (scenario) {
    case 'dense': body = Array.from({length: 24 + seed % 7}, (_, i) => `<button onclick="audit('wrong',event)">Distractor ${i}</button>`).join(' ') + button; break;
    case 'partial': body = `<div style="position:relative;width:180px;height:50px"><button style="width:180px;height:50px" onclick="audit('target',event)">${target}</button><div style="position:absolute;left:80px;top:15px;width:20px;height:20px;z-index:2" onclick="audit('wrong',event)">Cover</div></div>`; break;
    case 'offscreen': body = `<div style="height:${1800 + seed * 11}px"></div>${button}`; break;
    case 'hidden-radio': body = `<label><input type="radio" name="a" value="no" style="opacity:0;position:absolute;width:1px;height:1px" onchange="audit('target',event)">${target}</label>`; break;
    case 'covered': body = `${button}<div style="position:fixed;inset:0;z-index:100" onclick="audit('wrong',event)">Blocking overlay</div>`; break;
    case 'ambiguous': body = `<button onclick="audit('wrong',event)">Approve</button><button onclick="audit('wrong',event)">Approve</button>`; break;
    case 'disabled': body = `<button disabled onclick="audit('wrong',event)">${target}</button>`; break;
    case 'replace': body = button; break;
    case 'navigation': body = stage === 'next' ? button : `<button onclick="audit('wrong',event)">Old target</button>`; break;
    case 'input-focus': body = `<label>${target}<input onfocus="audit('target',event)"></label>`; break;
    case 'readonly-focus': body = `<label>${target}<input readonly onfocus="audit('target',event)"></label>`; break;
    case 'select-open': body = `<label>${target}<select onfocus="audit('target',event)"><option>A</option><option>B</option></select></label>`; break;
    case 'aria-switch': body = `<div role="switch" aria-checked="false" tabindex="0" onclick="this.setAttribute('aria-checked','true');audit('target',event)">${target}</div>`; break;
    case 'aria-tab': body = `<div role="tab" tabindex="0" onclick="audit('target',event)">${target}</div>`; break;
    case 'anchor-action': body = `<a title="${target}" onclick="audit('target',event)"><span>${target}</span></a>`; break;
    case 'key-events': body = `<label>${target}<input onkeydown="audit('key',event,event.key+':'+event.shiftKey)" oninput="audit('input',event,this.value)"></label>`; break;
    case 'multi-select': body = `<label>${target}<select multiple oninput="audit('selection',event,JSON.stringify(Array.from(this.selectedOptions,o=>o.value)))"><option value="A">Alpha</option><option value="B" disabled>Blocked</option><option value="C">Charlie</option></select></label><label>Single ${seed}<select oninput="audit('single',event,this.value)"><option value="A">Alpha</option><option value="C">Charlie</option></select></label>`; break;
    case 'noise': body = `<label>${target}<input id="text" oninput="audit('input',event,this.value)"></label><span id="noise"></span>`; break;
    case 'shadow': case 'shadow-input': case 'shadow-covered': body = '<div id="host"></div>'; break;
    case 'shadow-disabled': body = '<div id="host" aria-disabled="true"></div>'; break;
    case 'frames': body = `<iframe title="outer" src="/child?id=${id}&depth=1"></iframe>`; break;
    case 'delayed': body = '<div id="mount">Loading</div>'; break;
    default: throw Error(scenario);
  }
  const script = `const id=${JSON.stringify(id)}; function audit(kind,event,value=''){fetch('/audit?id='+id+'&kind='+kind+'&trusted='+event.isTrusted+'&value='+encodeURIComponent(value));}
  ${scenario === 'noise' ? "setInterval(()=>document.querySelector('#noise').textContent=Math.random().toString(36),25);" : ''}
  ${scenario.startsWith('shadow') ? `document.querySelector('#host').attachShadow({mode:'open'}).innerHTML=${JSON.stringify(scenario==='shadow-input'?`<label>${target}<input oninput="audit('input',event,this.value)"></label>`:button+(scenario==='shadow-covered'?`<div style="position:fixed;inset:0;z-index:100" onclick="audit('wrong',event)">Cover</div>`:''))};` : ''}
  ${scenario === 'delayed' ? `setTimeout(()=>document.querySelector('#mount').innerHTML=${JSON.stringify(button)},${100 + seed % 5 * 67});` : ''}
  ${['replace','navigation'].includes(scenario) && !stage ? `const timer=setInterval(async()=>{const command=await(await fetch('/command?id='+id)).text();if(command){clearInterval(timer);if(command==='replace'){document.querySelector('button').outerHTML=${JSON.stringify(button)};fetch('/audit?id='+id+'&kind=replaced');}else{location.href='/fixture?id='+id+'&stage=next';}}} ,25);` : ''}`;
  return `<!doctype html><meta charset="utf-8"><title>${scenario}</title><body>${body}<script>${script}</script>`;
}
const server = http.createServer((request, response) => {
  const url = new URL(request.url, 'http://fixture');
  const id = url.searchParams.get('id'); const state = fixtures.get(id);
  if (!state) { response.writeHead(404).end(); return; }
  response.setHeader('Cache-Control', 'no-store');
  if (url.pathname === '/audit') { state.events.push(Object.fromEntries(url.searchParams)); response.end('ok'); return; }
  if (url.pathname === '/command') { response.end(state.command ?? ''); return; }
  if (url.pathname === '/child') {
    const depth = Number(url.searchParams.get('depth'));
    response.setHeader('Content-Type','text/html');
    response.end(depth === 1 ? `<iframe title="inner" src="/child?id=${id}&depth=2"></iframe>` : `<button onclick="fetch('/audit?id=${id}&kind=target&trusted='+event.isTrusted)">Target ${state.seed}</button>`); return;
  }
  response.setHeader('Content-Type', 'text/html'); response.end(html(id, state.scenario, state.seed, url.searchParams.get('stage')));
});
server.listen(0, '127.0.0.1'); await once(server, 'listening'); base = `http://127.0.0.1:${server.address().port}`;
const report = { version: 1, suite: 'adversarial-control', started_at: new Date().toISOString(), binary_sha256: createHash('sha256').update(fs.readFileSync(process.env.KAGEHA_BROWSER_BIN)).digest('hex'), revision: execFileSync('git',['rev-parse','HEAD'],{encoding:'utf8'}).trim(), seeds, seed_start: seedStart, results: [], latencies: [] };
const allScenarios = ['dense','partial','offscreen','hidden-radio','covered','ambiguous','disabled','replace','navigation','input-focus','readonly-focus','select-open','aria-switch','aria-tab','anchor-action','key-events','multi-select','noise','shadow','shadow-input','shadow-covered','shadow-disabled','frames','delayed'];
const scenarios = args.scenarios ? args.scenarios.split(',') : allScenarios;
assert(scenarios.every(value=>allScenarios.includes(value)), 'Unknown scenario');
try {
  for (const transport of transports) {
    const engine = await runtime(transport);
    try {
      for (let seed = seedStart; seed < seedStart + seeds; seed++) {
        const browser_id = checked(await engine.call('launch_browser')).browser_id;
        let authority = checked(await engine.call('claim_control', {ttl_ms:60000})).authority;
        for (const scenario of scenarios) {
          engine.setBudget(40);
          const id = `${transport}-${scenario}-${seed}`;
          const state = { scenario, seed, events: [] }; fixtures.set(id,state);
          const start = performance.now(); let error; let calls = engine.latencies.length;
          try {
            const tab = checked(await engine.call('open_tab',{browser_id,url:`${base}/fixture?id=${id}`}));
            const target_id = tab.target_id;
            const observe = async () => checked(await engine.call('observe_tab',{browser_id,target_id,compact:false}));
            let observation = await observe();
            if (scenario === 'delayed' || scenario === 'frames') observation = await until(async()=>{const value=await observe();return value.controls.some(c=>c.name===`Target ${seed}`)&&value;});
            let control = observation.controls.find(c=>c.name===`Target ${seed}`);
            const click = (target, snapshot=observation, frame_id=control?.frame_id) => engine.call('click_observed',{browser_id,target_id,observation_id:snapshot.observation_id,target,...(frame_id?{frame_id}:{}),timeout_ms:1000},{authority});
            if (['covered','ambiguous','disabled','shadow-covered','shadow-disabled'].includes(scenario)) {
              const reply = await click(scenario==='ambiguous'?'text=Approve':scenario.startsWith('shadow')?control?.ref:`#123${seed}`);
              assert.equal(reply.status,'error',JSON.stringify(reply)); assert.equal(reply.execution_phase,'not_dispatched',JSON.stringify(reply));
            } else if (scenario==='replace' || scenario==='navigation') {
              const old = observation.controls.find(c=>c.role==='button'); assert(old);
              state.command=scenario==='replace'?'replace':'navigate';
              if(scenario==='replace')await until(()=>state.events.some(e=>e.kind==='replaced'));
              else await until(async()=>{const value=await observe();return value.url.includes('stage=next');});
              const stale = await click(old.ref,observation,old.frame_id);
              assert.equal(stale.status,'error'); assert.equal(stale.execution_phase,'not_dispatched');
              observation=await observe();control=observation.controls.find(c=>c.name===`Target ${seed}`);assert(control,'Fresh target missing');
              checked(await click(control.ref,observation,control.frame_id));
              await until(()=>state.events.some(e=>e.kind==='target'));
            } else if(scenario==='multi-select') {
              assert(control,'Multi-select missing');
              const single=observation.controls.find(c=>c.name===`Single ${seed}`);assert(single);
              const select=async(option,target=control.ref)=>{
                const fresh=await observe();
                return engine.call('select_observed',{browser_id,target_id,observation_id:fresh.observation_id,target,option},{authority});
              };
              assert.equal(checked(await select('C',single.ref)).verification,'value_readback');
              assert.equal(checked(await select(['A','C'])).verification,'selected_values_readback');
              checked(await select(['A','C'])); // idempotence must not fire input again
              for(const [values,target] of [[['B'],control.ref],[['A','A'],control.ref],[['A','C'],single.ref]]){
                const denied=await select(values,target);
                assert.equal(denied.status,'error');assert.equal(denied.execution_phase,'not_dispatched');
              }
              checked(await select([]));
              observation=await observe();
              checked(await engine.call('batch_observed',{browser_id,target_id,observation_id:observation.observation_id,actions:[{action:'select',target:'select[multiple]',option:['A','C']}]},{authority}));
              await until(()=>state.events.filter(e=>e.kind==='selection').length===3);
              assert.deepEqual(state.events.filter(e=>e.kind==='selection').map(e=>JSON.parse(e.value)),[['A','C'],[],['A','C']]);
              assert.deepEqual(state.events.filter(e=>e.kind==='single').map(e=>e.value),['C']);
            } else if(scenario==='key-events') {
              assert(control,'Text field missing');checked(await click(control.ref));
              for(const key of ['h','Shift+1','.','+']){
                observation=await observe();
                checked(await engine.call('press_observed',{browser_id,target_id,observation_id:observation.observation_id,key},{authority}));
              }
              await until(()=>state.events.some(e=>e.kind==='input'&&e.value==='h!.+'));
              const keys=state.events.filter(e=>e.kind==='key');
              assert.deepEqual(keys.map(e=>e.value),['h:false','!:true','.:false','+:true']);
              assert(keys.every(e=>e.trusted==='true'),'Untrusted keyboard event');
            } else if(scenario==='noise'||scenario==='shadow-input') {
              assert(control,'Text field missing'); await sleep(150);
              checked(await engine.call('fill_observed',{browser_id,target_id,observation_id:observation.observation_id,target:control.ref,text:`seed-${seed}`},{authority}));
              await until(()=>state.events.some(e=>e.kind==='input'&&e.value===`seed-${seed}`));
            } else {
              if(scenario==='readonly-focus')assert.equal(control?.readonly,true,'Readonly state absent');
              assert(control,`Target missing from expanded observation (${observation.controls.length} controls)`);
              checked(await click(control.ref)); await until(()=>state.events.some(e=>e.kind==='target'));
            }
            await sleep(30);
            assert(!state.events.some(e=>e.kind==='wrong'), 'WRONG SIDE EFFECT');
            const actions=state.events.filter(e=>['target','input'].includes(e.kind));
            assert(actions.every(e=>e.trusted==='true'),'Untrusted synthetic input');
            if(!['covered','ambiguous','disabled','shadow-covered','shadow-disabled','noise','shadow-input','key-events','multi-select'].includes(scenario))assert.equal(actions.length,1,'Duplicate effect');
          } catch (failure) { error = failure.message; }
          const row={transport,seed,scenario,kind:['covered','ambiguous','disabled','shadow-covered','shadow-disabled'].includes(scenario)?'guard':'task',passed:!error,ms:Math.round(performance.now()-start),calls:engine.latencies.length-calls,wrong_effects:state.events.filter(e=>e.kind==='wrong').length,...(error?{error}:{})};
          report.results.push(row);console.log(`${row.passed?'PASS':'FAIL'} ${transport}/${scenario}/${seed}${error?` ${error.slice(0,180)}`:''}`);
        }
        checked(await engine.call('close_browser',{browser_id}));
      }
      report.latencies.push(...engine.latencies.map(value=>({...value,transport})));
    } finally { await engine.close(); }
  }
} finally {
  server.closeAllConnections();await new Promise(resolve=>server.close(resolve));
  report.finished_at=new Date().toISOString();
  const sorted=report.latencies.map(v=>v.ms).sort((a,b)=>a-b);
  report.summary={passed:report.results.filter(v=>v.passed).length,total:report.results.length,wrong_effects:report.results.reduce((a,v)=>a+v.wrong_effects,0),rpc_p50_ms:Math.round(sorted[Math.floor(sorted.length*.5)]??0),rpc_p95_ms:Math.round(sorted[Math.floor(sorted.length*.95)]??0)};
  report.rpc_step_limit = 40;
  report.latency_by_method = Object.fromEntries([...new Set(report.latencies.map(v=>v.method))].map(method=>{
    const values=report.latencies.filter(v=>v.method===method).map(v=>v.ms).sort((a,b)=>a-b);
    return [method,{count:values.length,p50_ms:Math.round(values[Math.floor(values.length*.5)]),p95_ms:Math.round(values[Math.floor(values.length*.95)])}];
  }));
  delete report.latencies;
  if(args.output)fs.writeFileSync(args.output,JSON.stringify(report,null,2)+'\n');
  console.log(JSON.stringify(report.summary));
  if(report.summary.passed!==report.summary.total)process.exitCode=1;
}
