// Real typed providers, production Rust planner, labeled synthetic cases. No browser mutation.
import fs from 'node:fs';
import crypto from 'node:crypto';
import {spawn} from 'node:child_process';
import assert from 'node:assert/strict';
const binary=process.env.KAGEHA_DECISION_EVAL_BIN;
assert(binary,'Set KAGEHA_DECISION_EVAL_BIN to the Rust browser_decision_eval executable');
const credentials={...process.env};
if(process.env.KAGEHA_TEST_ENV_FILE)for(const line of fs.readFileSync(process.env.KAGEHA_TEST_ENV_FILE,'utf8').split('\n')){const m=line.match(/^\s*(?:export\s+)?(OPENAI_API_KEY|CLOUDFLARE_API_TOKEN|CLOUDFLARE_ACCOUNT_ID|CMD_API_KEY)\s*=\s*(.*)$/);if(m&&!credentials[m[1]])credentials[m[1]]=m[2].trim().replace(/^(["'])(.*)\1$/,'$2');}
const root=fs.mkdtempSync('/tmp/kageha-decision-calibration-');fs.chmodSync(root,0o700);
const source=process.env.KAGEHA_DECISION_CASES||'crates/kageha-browser-decisions/tests/fixtures/routing_cases.jsonl';
const fixtures=fs.readFileSync(source);fs.writeFileSync(root+'/cases.jsonl',fixtures,{mode:0o600});fs.copyFileSync(binary,root+'/evaluate');fs.chmodSync(root+'/evaluate',0o700);
const providers=[{name:'luna-decisions',model:'gpt-6-luna',key:credentials.OPENAI_API_KEY,protocol:'openai_decisions',url:'https://api.openai.com/v1/decisions'},{name:'clef',model:'@cf/cloudflare/clef',key:credentials.CLOUDFLARE_API_TOKEN,protocol:'clef',url:`https://api.cloudflare.com/client/v4/accounts/${credentials.CLOUDFLARE_ACCOUNT_ID}/ai/run/@cf/cloudflare/clef`},{name:'clef-flash',model:'@cf/cloudflare/clef-flash',key:credentials.CLOUDFLARE_API_TOKEN,protocol:'clef',url:`https://api.cloudflare.com/client/v4/accounts/${credentials.CLOUDFLARE_ACCOUNT_ID}/ai/run/@cf/cloudflare/clef-flash`},{name:'jev',model:'typesafe/jev-1.14.0',key:credentials.CMD_API_KEY,protocol:'system_one',url:'https://api.commandcode.ai/provider/v1/systemone'}].filter(p=>!process.env.KAGEHA_DECISION_MODELS||process.env.KAGEHA_DECISION_MODELS.split(',').includes(p.name));
const digest=b=>crypto.createHash('sha256').update(b).digest('hex');
const report={version:2,score_metric:process.env.KAGEHA_EVAL_CONFIDENCE_MODE==='1'?'provider_confidence':'selected_probability',scope:'Synthetic planner evaluation, not browser execution or production calibration',root,fixtures_sha256:digest(fixtures),binary_sha256:digest(fs.readFileSync(root+'/evaluate')),scenario_families:new Set(fixtures.toString().trim().split("\n").map(line=>JSON.parse(line).family)).size,notes:['Three name/value variants per family are correlated, not independent trials.','Family-disjoint calibration/holdout labels frozen before provider calls.','Threshold sweep replays each actual typed response through the production Rust planner without extra provider calls.','This evaluates selected actions; fallback is safe abstention, not task success.'],providers:[]};
console.log(JSON.stringify({started:true,root,providers:providers.map(p=>p.name)}));
const wilson=(errors,n)=>{if(!n)return null;const z=1.6448536269514722,p=errors/n;return (p+z*z/(2*n)+z*Math.sqrt(p*(1-p)/n+z*z/(4*n*n)))/(1+z*z/n);};
await Promise.all(providers.map(async p=>{
 assert(p.key,'Missing credential for '+p.name);
 const fd=fs.openSync(root+'/'+p.name+'.jsonl','w',0o600),err=fs.openSync(root+'/'+p.name+'.stderr','w',0o600);
 const child=spawn(root+'/evaluate',[],{env:{...process.env,KAGEHA_EVAL_URL:p.url,KAGEHA_EVAL_KEY:p.key,KAGEHA_EVAL_MODEL:p.model,KAGEHA_EVAL_PROTOCOL:p.protocol},stdio:['pipe',fd,err]});fs.closeSync(fd);fs.closeSync(err);child.stdin.end(fixtures);
 const exit=await new Promise(resolve=>child.on('exit',resolve));
 const rows=fs.readFileSync(root+'/'+p.name+'.jsonl','utf8').trim().split('\n').filter(Boolean).map(JSON.parse),summaries=[];
 for(const split of ['calibration','holdout'])for(const threshold of rows[0]?.thresholds.map(t=>t.threshold)||[]){
  const selected=rows.filter(r=>r.split===split),counts=selected.map(r=>({...r,choice:r.thresholds.find(t=>t.threshold===threshold)}));
  const accepted=counts.reduce((n,r)=>n+r.choice.accepted,0),wrong=counts.reduce((n,r)=>n+r.choice.wrong,0),expected=selected.reduce((n,r)=>n+r.expected.length,0),acted=counts.filter(r=>r.choice.accepted>0);
  const families=[...new Set(acted.map(r=>r.family))],wrongFamilies=families.filter(f=>acted.some(r=>r.family===f&&r.choice.wrong>0));
  summaries.push({split,threshold,cases:selected.length,provider_ok:selected.filter(r=>r.provider_ok).length,accepted,wrong,precision:accepted?1-wrong/accepted:null,correct_coverage:expected?counts.reduce((n,r)=>n+r.choice.correct,0)/expected:null,acted_cases:acted.length,wrong_cases:acted.filter(r=>r.choice.wrong).length,acted_families:families.length,wrong_families:wrongFamilies.length,exploratory_wrong_family_upper_95:wilson(wrongFamilies.length,families.length)});
 }
 const sorted=rows.map(r=>r.elapsed_ms).sort((a,b)=>a-b);
 const entry={name:p.name,model:p.model,exit,rows:rows.length,median_ms:sorted[Math.floor(sorted.length/2)],summaries};report.providers.push(entry);
 fs.writeFileSync(root+'/report.json',JSON.stringify(report,null,2),{mode:0o600});
 console.log(JSON.stringify({finished:p.name,exit,cases:rows.length,median_ms:entry.median_ms,holdout:summaries.filter(s=>s.split==='holdout'&&[.9,.95,.98,.99,.995,1].includes(s.threshold))}));
}));
report.finished_at=new Date().toISOString();fs.writeFileSync(root+'/report.json',JSON.stringify(report,null,2),{mode:0o600});
assert(report.providers.every(p=>p.exit===0&&p.rows===fixtures.toString().trim().split('\n').length),'Incomplete provider evaluation');
console.log(JSON.stringify({completed:true,root}));
