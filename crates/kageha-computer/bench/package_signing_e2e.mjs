// Packaging fault injection: signing/notarization tools are fakes, never Apple's
// service or the user's Keychain. Darwin app-directory exchange is real.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {spawnSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
assert.equal(process.platform,'darwin','Atomic bundle exchange tests require macOS');
const root=fs.mkdtempSync(path.join(os.tmpdir(),'kageha-signing-tests-'));
fs.chmodSync(root,0o700);
const bin=path.join(root,'mock-tools');fs.mkdirSync(bin);
const script=fileURLToPath(new URL('../scripts/package_macos.sh',import.meta.url));
const mock=`#!${process.execPath}
const fs=require('fs'),path=require('path'),crypto=require('crypto'),cp=require('child_process');
const tool=path.basename(process.argv[1]),args=process.argv.slice(2),env=process.env;
fs.appendFileSync(env.TEST_LOG,JSON.stringify({tool,args})+'\\n');
const fingerprint='A'.repeat(40),team=env.TEST_TEAM||'ABCDE12345';
if(tool==='security'){
 if(env.TEST_IDENTITIES==='none')console.log('0 valid identities found');
 else if(env.TEST_IDENTITIES==='development')console.log('  1) '+fingerprint+' "Apple Development: Example (ABCDE12345)"');
 else {console.log('  1) '+fingerprint+' "Developer ID Application: Example ('+team+')"');if(env.TEST_IDENTITIES==='multiple')console.log('  2) '+'B'.repeat(40)+' "Developer ID Application: Another (ABCDE12345)"');}
}else if(tool==='codesign'){
 const app=args.at(-1),meta=path.join(app,'.test-signature.json');
 if(args.includes('--sign')){if(env.TEST_FAIL_SIGN==='1')process.exit(1);const identity=args[args.indexOf('--sign')+1],kind=identity==='-'?'adhoc':'developer';
 const contents=fs.readFileSync(path.join(app,'Contents/MacOS/kageha-computer'));
 fs.writeFileSync(meta,JSON.stringify({kind,team,requirement:kind==='developer'?'developer-'+team:'hash-'+crypto.createHash('sha256').update(contents).digest('hex')}));
 }else{if(!fs.existsSync(meta))process.exit(1);const signed=JSON.parse(fs.readFileSync(meta));
 if(args.includes('--test-requirement') && args[args.indexOf('--test-requirement')+1]!=='='+signed.requirement)process.exit(1);
 if(args.includes('--display')){if(args.includes('--requirements'))console.error((signed.kind==='adhoc'?'# ':'')+'designated => '+signed.requirement);else console.error(signed.kind==='developer'?'Authority=Developer ID Application: Example ('+signed.team+')\\nTeamIdentifier='+signed.team:'Signature=adhoc\\nTeamIdentifier=not set');}}
}else if(tool==='xcrun'){
 if(args[0]==='notarytool'&&args[1]==='history'){if(env.TEST_FAIL_PROFILE==='1')process.exit(1);console.log('{}');}
 else if(args[0]==='notarytool'&&args[1]==='submit')console.log(JSON.stringify({id:'fixture-submission',status:env.TEST_NOTARY_STATUS||'Accepted'}));
 else if(args[0]==='stapler'&&args[1]==='validate'&&env.TEST_FAIL_STAPLE==='1')process.exit(1);
}else if(tool==='ditto'){fs.writeFileSync(args.at(-1),'fixture archive');
}else if(tool==='syspolicy_check'){if(env.TEST_FAIL_ASSESS==='1')process.exit(1);
}else if(tool==='swift'){if(env.TEST_FAIL_SWAP==='1')process.exit(1);const result=cp.spawnSync('/usr/bin/swift',args,{stdio:'inherit'});process.exit(result.status??1);
}else process.exit(90);
`;
for(const tool of ['security','codesign','xcrun','ditto','syspolicy_check','swift']){fs.writeFileSync(path.join(bin,tool),mock);fs.chmodSync(path.join(bin,tool),0o755)}
let count=0;const passed=[];
function fixture(){const dir=path.join(root,'case-'+(++count));fs.mkdirSync(dir);const binary=path.join(dir,'source binary'),output=path.join(dir,'Kageha Computer.app'),log=path.join(dir,'commands.jsonl');fs.writeFileSync(binary,'binary-v1');fs.writeFileSync(log,'');return {dir,binary,output,log}}
function run(f,options=[],extra={}){const env={...process.env,PATH:bin+':'+process.env.PATH,TEST_LOG:f.log,TEST_IDENTITIES:'one',KAGEHA_COMPUTER_SIGN_IDENTITY:'',KAGEHA_COMPUTER_TEAM_ID:'',KAGEHA_COMPUTER_VERSION:'0.1.0',KAGEHA_COMPUTER_NOTARY_PROFILE:'fixture-profile',...extra};return spawnSync('/bin/bash',[script,'--binary',f.binary,'--output',f.output,...options],{env,encoding:'utf8'})}
function ok(result){assert.equal(result.status,0,result.stderr+result.stdout)}
function refused(result,pattern){assert.notEqual(result.status,0,result.stdout);assert.match(result.stderr,pattern)}
const text=f=>fs.readFileSync(path.join(f.output,'Contents/MacOS/kageha-computer'),'utf8');
const commands=f=>fs.readFileSync(f.log,'utf8').trim().split('\n').filter(Boolean).map(line=>JSON.parse(line));
function test(name,fn){fn();passed.push(name);console.log('PASS '+name)}
try{
 test('missing certificate fails before staging or replacement',()=>{const f=fixture();refused(run(f,[],{TEST_IDENTITIES:'none'}),/No Developer ID/);assert(!fs.existsSync(f.output));assert.deepEqual(fs.readdirSync(f.dir).sort(),['commands.jsonl','source binary'])});
 test('Apple Development is not mistaken for Developer ID',()=>{const f=fixture();refused(run(f,[],{TEST_IDENTITIES:'development'}),/No Developer ID/)});
 test('ambiguous identities require an explicit choice',()=>{const f=fixture();refused(run(f,[],{TEST_IDENTITIES:'multiple'}),/Multiple Developer ID/);ok(run(f,[],{TEST_IDENTITIES:'multiple',KAGEHA_COMPUTER_SIGN_IDENTITY:'A'.repeat(40)}))});
 test('production signing uses hardened runtime, timestamp and team pin',()=>{const f=fixture();ok(run(f,[],{KAGEHA_COMPUTER_TEAM_ID:'ABCDE12345'}));const sign=commands(f).find(c=>c.tool==='codesign'&&c.args.includes('--sign'));assert(sign.args.includes('--timestamp'));assert(sign.args.includes('runtime'));assert.equal(text(f),'binary-v1');fs.writeFileSync(f.binary,'binary-v2');ok(run(f));assert.equal(text(f),'binary-v2');assert(fs.readdirSync(f.dir).some(name=>name.startsWith('.kageha-computer-previous-')))});
 test('wrong Apple team is refused before promotion',()=>{const f=fixture();refused(run(f,[],{KAGEHA_COMPUTER_TEAM_ID:'OTHER12345'}),/different configured Apple team/);assert(!fs.existsSync(f.output))});
 test('a signing failure preserves the installed executable',()=>{const f=fixture();ok(run(f));fs.writeFileSync(f.binary,'binary-v2');refused(run(f,[],{TEST_FAIL_SIGN:'1'}),/.*/s);assert.equal(text(f),'binary-v1')});
 test('ad-hoc rebuild needs deliberate identity migration and preserves rollback',()=>{const f=fixture();ok(run(f,['--ad-hoc']));fs.writeFileSync(f.binary,'binary-v2');refused(run(f,['--ad-hoc']),/changes the macOS app identity/);assert.equal(text(f),'binary-v1');ok(run(f,['--ad-hoc','--allow-identity-change']));assert.equal(text(f),'binary-v2');const backup=fs.readdirSync(f.dir).find(n=>n.startsWith('.kageha-computer-previous-'));assert.equal(fs.readFileSync(path.join(f.dir,backup,'Contents/MacOS/kageha-computer'),'utf8'),'binary-v1')});
 test('Developer ID cannot be downgraded to ad-hoc',()=>{const f=fixture();ok(run(f));refused(run(f,['--ad-hoc','--allow-identity-change']),/Refusing to downgrade/);assert.equal(text(f),'binary-v1')});
 test('one-time ad-hoc to Developer ID transition is explicit',()=>{const f=fixture();ok(run(f,['--ad-hoc']));refused(run(f),/changes the macOS app identity/);ok(run(f,['--allow-identity-change']));assert.equal(text(f),'binary-v1')});
 test('notarization rejection and staple failure preserve the installed app',()=>{const f=fixture();ok(run(f));fs.writeFileSync(f.binary,'binary-v2');refused(run(f,['--notarize'],{TEST_NOTARY_STATUS:'Invalid'}),/not accepted/);assert.equal(text(f),'binary-v1');refused(run(f,['--notarize'],{TEST_FAIL_STAPLE:'1'}),/.*/s);assert.equal(text(f),'binary-v1')});
 test('notary credentials and Gatekeeper assessment fail closed',()=>{const f=fixture();refused(run(f,['--notarize'],{TEST_FAIL_PROFILE:'1'}),/credentials/);assert(!fs.existsSync(f.output));refused(run(f,['--notarize'],{TEST_FAIL_ASSESS:'1'}),/.*/s);assert(!fs.existsSync(f.output))});
 test('accepted notarization is stapled and assessed before atomic promotion',()=>{const f=fixture();ok(run(f,['--notarize']));const tools=commands(f),submit=tools.findIndex(c=>c.tool==='xcrun'&&c.args[1]==='submit'),staple=tools.findIndex(c=>c.tool==='xcrun'&&c.args[1]==='staple'),validate=tools.findIndex(c=>c.tool==='xcrun'&&c.args[1]==='validate'),assess=tools.findIndex(c=>c.tool==='syspolicy_check'),promote=tools.findIndex(c=>c.tool==='swift');assert(submit>=0&&submit<staple&&staple<validate&&validate<assess&&assess<promote)});
 test('failed atomic promotion preserves the installed app without a missing-path interval',()=>{const f=fixture();ok(run(f));fs.writeFileSync(f.binary,'binary-v2');refused(run(f,[],{TEST_FAIL_SWAP:'1'}),/.*/s);assert.equal(text(f),'binary-v1')});
 test('an existing installer lock is not removed by a competing install',()=>{const f=fixture();ok(run(f));const lock=f.output+'.install-lock';fs.mkdirSync(lock);fs.writeFileSync(path.join(lock,'pid'),'fixture-owner');refused(run(f),/Another install/);assert.equal(fs.readFileSync(path.join(lock,'pid'),'utf8'),'fixture-owner');assert.equal(text(f),'binary-v1')});
 test('notarization cannot use ad-hoc signing or a missing credential profile',()=>{const f=fixture();refused(run(f,['--ad-hoc','--notarize']),/cannot notarize/);refused(run(f,['--notarize'],{KAGEHA_COMPUTER_NOTARY_PROFILE:''}),/NOTARY_PROFILE/);assert(!fs.existsSync(f.output))});
 test('symlinked outputs and unrelated bundle identifiers are refused',()=>{const f=fixture();const elsewhere=path.join(f.dir,'elsewhere');fs.mkdirSync(elsewhere);fs.symlinkSync(elsewhere,f.output);refused(run(f),/symlinked app/);fs.unlinkSync(f.output);ok(run(f));const plist=path.join(f.output,'Contents/Info.plist');fs.writeFileSync(plist,fs.readFileSync(plist,'utf8').replace('ai.kageha.computer','unrelated.application'));refused(run(f),/different bundle identifier/)});
 console.log(JSON.stringify({status:'passed',cases:passed.length,root,scope:'mock signing and notarization; real Darwin atomic directory exchange'}));
}finally{fs.writeFileSync(path.join(root,'report.json'),JSON.stringify({passed,scope:'mock signing and notarization; real Darwin atomic directory exchange'},null,2))}
