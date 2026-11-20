// Reproducible control-quality gate. Fails on any task/guard failure or wrong effect.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';
assert(process.env.KAGEHA_BROWSER_BIN, 'Set KAGEHA_BROWSER_BIN');
assert(process.env.MINIWOB_ROOT, 'Set MINIWOB_ROOT to the pinned MiniWoB++ checkout');
const here=path.dirname(fileURLToPath(import.meta.url));
const out=path.resolve(process.env.BROWSER_BENCH_REPORT_DIR??path.join(here,'../../../output/benchmarks/browser/gate'));
fs.mkdirSync(out,{recursive:true});
let failed=false;
for(const [suite,seeds] of [['adversarial',process.env.BROWSER_BENCH_SEEDS??'5'],['miniwob',process.env.MINIWOB_SEEDS??'10'],['terminal',process.env.BROWSER_TERMINAL_SEEDS??'3']]){
  const report=path.join(out,`${suite}-after.json`);
  const child=spawnSync(process.execPath,[path.join(here,`${suite}.mjs`),`--seeds=${seeds}`,'--transport=both',`--output=${report}`],{stdio:'inherit',timeout:15*60*1000});
  if(child.error||child.status!==0){failed=true;console.error(`${suite} failed`,child.error?.message??child.status);}
  else{
    const result=JSON.parse(fs.readFileSync(report,'utf8'));
    assert(result.summary.total>0);
    if(result.summary.passed!==result.summary.total||(result.summary.wrong_effects??0)!==0)failed=true;
  }
}
process.exitCode=failed?1:0;
