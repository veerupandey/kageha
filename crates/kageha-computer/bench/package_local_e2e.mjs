// Real local signing of disposable copies. Never installs/re-signs the source app.
import fs from 'node:fs';import os from 'node:os';import path from 'node:path';import crypto from 'node:crypto';import assert from 'node:assert/strict';import {spawnSync} from 'node:child_process';import {fileURLToPath} from 'node:url';
assert.equal(process.platform,'darwin');const helper=process.env.KAGEHA_COMPUTER_BIN;assert(helper&&path.isAbsolute(helper),'Set KAGEHA_COMPUTER_BIN to the existing built helper');const hash=()=>crypto.createHash('sha256').update(fs.readFileSync(helper)).digest('hex');const original=hash();
const root=fs.mkdtempSync(path.join(os.tmpdir(),'kageha-local-signing-'));fs.chmodSync(root,0o700);const output=path.join(root,'Kageha Computer.app'),script=fileURLToPath(new URL('../scripts/package_macos.sh',import.meta.url));
const env={...process.env,KAGEHA_COMPUTER_SIGN_IDENTITY:'',KAGEHA_COMPUTER_TEAM_ID:'',KAGEHA_COMPUTER_VERSION:'0.1.0'};
const run=(extra=[],vars={})=>spawnSync('/bin/bash',[script,'--binary',helper,'--output',output,'--ad-hoc',...extra],{env:{...env,...vars},encoding:'utf8'});
const report={scope:'real macOS ad-hoc signatures and atomic bundle exchange in a disposable directory; no Apple submission',root};
try{
 let r=run();assert.equal(r.status,0,r.stderr);report.first_package=true;
 r=run();assert.equal(r.status,0,r.stderr);report.identical_update_preserves_requirement=true;
 r=run([],{KAGEHA_COMPUTER_VERSION:'0.1.1'});assert.notEqual(r.status,0);assert.match(r.stderr,/changes the macOS app identity/);report.identity_change_refused=true;
 let version=spawnSync('plutil',['-extract','CFBundleShortVersionString','raw','-o','-',output+'/Contents/Info.plist'],{encoding:'utf8'});assert.equal(version.stdout.trim(),'0.1.0');
 r=run(['--allow-identity-change'],{KAGEHA_COMPUTER_VERSION:'0.1.1'});assert.equal(r.status,0,r.stderr);report.explicit_scratch_identity_change=true;
 const verified=spawnSync('codesign',['--verify','--strict',output],{encoding:'utf8'});assert.equal(verified.status,0,verified.stderr);report.signature_verified=true;report.rollback_bundles=fs.readdirSync(root).filter(n=>n.startsWith('.kageha-computer-previous-')).length;assert(report.rollback_bundles>=2);
 report.source_helper_unchanged=original===hash();assert(report.source_helper_unchanged);report.status='passed';
}catch(error){report.status='failed';report.error=String(error);process.exitCode=1}
finally{fs.writeFileSync(path.join(root,'report.json'),JSON.stringify(report,null,2));console.log(JSON.stringify(report))}
