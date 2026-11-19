// Read-only readiness checks. Never resets sites, accepts terms, or logs credentials.
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
const args=Object.fromEntries(process.argv.slice(2).map(v=>v.replace(/^--/,'').split('=')));
const output=path.resolve(args.output??'crates/kageha-browser/bench/results/external-preflight.json');
const report={version:1,checked_at:new Date().toISOString(),platform:process.platform,architecture:process.arch,benchmarks:{}};
function command(bin,args){try{return execFileSync(bin,args,{encoding:'utf8',timeout:10000,stdio:['ignore','pipe','ignore']}).trim();}catch{return null;}}
const disk=command('df',['-k','.']);
const fields=disk?.split('\n').at(-1).trim().split(/\s+/);
report.local_free_gib=fields?Math.floor(Number(fields[3])/1024**2):null;
const docker=command('docker',['info','--format','{{json .ServerVersion}} {{json .Architecture}} {{json .MemTotal}}']);
report.docker={available:!!docker,server:docker};
const siteNames=['SHOPPING','SHOPPING_ADMIN','REDDIT','GITLAB','WIKIPEDIA','MAP','HOMEPAGE'];
const sites=[];
for(const name of siteNames){
  const url=process.env[name];
  if(!url){sites.push({name,configured:false});continue;}
  try{const parsed=new URL(url);if(!['http:','https:'].includes(parsed.protocol)||parsed.username||parsed.password)throw Error('invalid endpoint');
    const response=await fetch(parsed,{method:'GET',redirect:'manual',signal:AbortSignal.timeout(5000)});
    await response.body?.cancel();sites.push({name,configured:true,http_status:response.status,reachable:response.status<500});
  }catch{sites.push({name,configured:true,reachable:false});}
}
const isolated=process.env.WEBARENA_BENCHMARK_ISOLATED==='true';
report.benchmarks.webarena={ready:isolated&&sites.every(site=>site.reachable),isolated_confirmation:isolated,sites,
  official_setup:'https://github.com/web-arena-x/webarena/blob/main/environment_docker/README.md',
  local_images_present:command('docker',['images','--format','{{.Repository}}:{{.Tag}}'])?.split('\n').filter(name=>/shopping_final|shopping_admin_final|gitlab-populated|postmill|kiwix/.test(name))??[],
  recommended_environment_volume_gib:1000,note:'No endpoints or images are provisioned by this check. Public demo sites are not evaluation environments.'};
let token=process.env.HF_TOKEN??process.env.HUGGING_FACE_HUB_TOKEN;
if(!token){const file=path.join(os.homedir(),'.cache/huggingface/token');if(fs.existsSync(file))token=fs.readFileSync(file,'utf8').trim();}
let status=null;
if(token){try{
 const response=await fetch('https://huggingface.co/datasets/ServiceNow/WorkArena-Instances/resolve/main/instances_v2.json',{headers:{Authorization:`Bearer ${token}`},signal:AbortSignal.timeout(15000)});
 status=response.status;await response.body?.cancel();
}catch{status='network_error';}}
const customPool=process.env.SNOW_INSTANCE_POOL;
const customConfigured=!!(process.env.SNOW_INSTANCE_URL&&process.env.SNOW_INSTANCE_UNAME&&process.env.SNOW_INSTANCE_PWD)||!!(customPool&&fs.existsSync(customPool));
report.benchmarks.workarena={ready:status===200||customConfigured,credential_present:!!token,gated_configuration_http:status,custom_configuration_present:customConfigured,
  access_page:'https://huggingface.co/datasets/ServiceNow/WorkArena-Instances',note:'A ready configuration still needs the official instance health/release checks before task mutation. This check never accepts gated dataset terms.'};
report.benchmarks.miniwob={ready:!!process.env.MINIWOB_ROOT&&fs.existsSync(path.join(process.env.MINIWOB_ROOT,'miniwob/html/miniwob')),catalog_families:125};
fs.mkdirSync(path.dirname(output),{recursive:true});fs.writeFileSync(output,JSON.stringify(report,null,2)+'\n');
console.log(JSON.stringify(report,null,2));
if(!report.benchmarks.webarena.ready||!report.benchmarks.workarena.ready)process.exitCode=2;
