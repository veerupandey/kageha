// Exploratory paired comparisons against heavy executors; never a Mini baseline.
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
const root=fs.mkdtempSync('/tmp/kageha-browser-model-matrix-');fs.chmodSync(root,0o700);
const secrets={...process.env};
if(process.env.KAGEHA_TEST_ENV_FILE)for(const line of fs.readFileSync(process.env.KAGEHA_TEST_ENV_FILE,'utf8').split('\n')){const m=line.match(/^\s*(?:export\s+)?(OPENAI_API_KEY|CLOUDFLARE_API_TOKEN|CLOUDFLARE_ACCOUNT_ID)\s*=\s*(.*)$/);if(m&&!secrets[m[1]])secrets[m[1]]=m[2].trim().replace(/^(["'])(.*)\1$/,'$2');}
assert(secrets.OPENAI_API_KEY&&secrets.CLOUDFLARE_API_TOKEN&&secrets.CLOUDFLARE_ACCOUNT_ID,'Configure provider credentials outside chat');
const source=fs.readFileSync(new URL('./workflow_e2e.mjs',import.meta.url),'utf8');
// Freeze the same fixture; typed and heavy providers retain separate transports.
const frozen=source;
fs.writeFileSync(root+'/workflow_e2e.mjs',frozen,{mode:0o600});fs.copyFileSync(new URL('./workflow_site.mjs',import.meta.url),root+'/workflow_site.mjs');
for(const [name,variable] of [['host','KAGEHA_LOOP_BIN'],['browser','KAGEHA_BROWSER_BIN']]){assert(process.env[variable]);fs.copyFileSync(process.env[variable],root+'/'+name);fs.chmodSync(root+'/'+name,0o700);}
const hash=file=>crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
const models=(process.env.KAGEHA_TEST_HEAVY_MODELS||'gpt-5.4,gpt-6.1-sol,gpt-6.1-terra,@cf/deepseek-ai/deepseek-v4.1-flash-1017').split(',');
const report={scope:'One counterbalanced Off/decision-first pair per heavy executor; exploratory, not statistically conclusive',root,decision_model:process.env.KAGEHA_TEST_DECISION_MODEL||'@cf/cloudflare/clef-flash',host_sha256:hash(root+'/host'),browser_sha256:hash(root+'/browser'),harness_sha256:hash(root+'/workflow_e2e.mjs'),runs:[]};
const persist=()=>fs.writeFileSync(root+'/report.json',JSON.stringify(report,null,2),{mode:0o600});persist();console.log(JSON.stringify({started:true,root,models}));
for(const [i,model] of models.entries())for(const mode of i%2?['active','off']:['off','active']){
 const cf=model.startsWith('@cf/'),row={model,mode},log=root+`/model-${i}-${mode}.log`;report.runs.push(row);persist();
 const fd=fs.openSync(log,'w',0o600);
 const child=spawn(process.execPath,[root+'/workflow_e2e.mjs','--real','--decision-first','--typed-decisions',`--decisions=${mode}`,...(mode==='active'?['--real-decisions']:[])],{env:{...process.env,KAGEHA_LOOP_BIN:root+'/host',KAGEHA_BROWSER_BIN:root+'/browser',KAGEHA_TEST_MODEL:model,KAGEHA_TEST_DECISION_MODEL:report.decision_model,KAGEHA_TEST_PROVIDER_URL:cf?`https://api.cloudflare.com/client/v4/accounts/${secrets.CLOUDFLARE_ACCOUNT_ID}/ai/v1/chat/completions`:'https://api.openai.com/v1/chat/completions',KAGEHA_TEST_API_KEY:cf?secrets.CLOUDFLARE_API_TOKEN:secrets.OPENAI_API_KEY},stdio:['ignore',fd,fd]});fs.closeSync(fd);
 row.exit_code=await new Promise(r=>child.on('exit',code=>r(code??1)));
 let summary;for(const line of fs.readFileSync(log,'utf8').split('\n'))try{const j=JSON.parse(line);if(j.passed||j.failed)summary=j;}catch{}
 row.passed=row.exit_code===0&&summary?.passed===true;row.fixture_root=summary?.root;row.log=log;
 if(row.fixture_root){const file=row.fixture_root+(row.passed?'/result.json':'/benchmark-failure.json');if(fs.existsSync(file)){const result=JSON.parse(fs.readFileSync(file));row.metrics=result.benchmark||result;}}
 persist();console.log(JSON.stringify({model,mode,passed:row.passed,elapsed_ms:row.metrics?.elapsed_ms,executor_calls:row.metrics?.executor_calls,decision_calls:row.metrics?.decision_calls,routed:row.metrics?.decision_applied}));
}
report.comparisons=models.map(model=>{const off=report.runs.find(r=>r.model===model&&r.mode==='off'),active=report.runs.find(r=>r.model===model&&r.mode==='active');return {model,off_passed:off?.passed,active_passed:active?.passed,off_ms:off?.metrics?.elapsed_ms,active_ms:active?.metrics?.elapsed_ms,active_minus_off_percent:off?.passed&&active?.passed?100*(active.metrics.elapsed_ms/off.metrics.elapsed_ms-1):null};});report.finished_at=new Date().toISOString();persist();console.log(JSON.stringify({finished:true,root,comparisons:report.comparisons}));if(report.runs.some(r=>!r.passed))process.exitCode=1;
