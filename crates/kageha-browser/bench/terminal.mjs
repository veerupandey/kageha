// Original MiniWoB terminal regression: inspect visible output, never hidden answers.
// The terminal is a local JavaScript simulation; no OS commands are executed.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import {createHash} from 'node:crypto';
import {runtime,checked,sleep} from './support.mjs';
import {serveFixture,evaluator,revision} from './miniwob-fixture.mjs';
const args=Object.fromEntries(process.argv.slice(2).map(v=>v.replace(/^--/,'').split('=')));
const seeds=Number(args.seeds??3);
assert(Number.isSafeInteger(seeds)&&seeds>=1&&seeds<=100);
const fixture=await serveFixture();
const report={suite:'miniwob-terminal-keyboard',source_revision:revision,binary_sha256:createHash('sha256').update(fs.readFileSync(process.env.KAGEHA_BROWSER_BIN)).digest('hex'),results:[]};
try{
  for(const transport of ['http','mcp'])for(let seed=1;seed<=seeds;seed++){
    const engine=await runtime(transport);let grader;
    const row={transport,seed,passed:false};
    try{
      engine.setBudget(60);
      const browser_id=checked(await engine.call('launch_browser')).browser_id;
      const tab=checked(await engine.call('open_tab',{browser_id,url:fixture.base+'terminal.html'}));
      let selection=checked(await engine.call('browser_selection'));
      const frame=checked(await engine.call('preview_tab',{browser_id,target_id:tab.target_id,binding_id:selection.binding_id}));
      const authority=checked(await engine.call('claim_control',{ttl_ms:60000})).authority;
      const panel=input=>engine.call('panel_action',{selection:{browser_id,target_id:tab.target_id,binding_id:selection.binding_id},viewport:{width:332,height:214},input},{authority}).then(checked);
      checked(await engine.call('panel_action',{selection:{browser_id,target_id:tab.target_id,binding_id:selection.binding_id},viewport:{width:frame.width,height:frame.height},input:{action:'layout',size:{width:332,height:214}}},{authority}));
      selection=checked(await engine.call('browser_selection'));
      grader=await evaluator(engine,tab.target_id);await grader.reset(seed,1000000);
      // The original task focuses its keyboard sink after 200ms.
      await sleep(250);
      const observe=()=>engine.call('observe_tab',{browser_id,target_id:tab.target_id,compact:false}).then(checked);
      const instruction=(await observe()).text.join(' ');
      const suffix=instruction.match(/extension\s+(\.[\w.]+)/)?.[1];
      assert(suffix||instruction.includes('no file extension'),'Unknown visible instruction');
      const press=key=>panel({action:'press',key});
      await press('l');await press('s');await press('Enter');
      const output=await observe();
      const command=output.text.findLastIndex(text=>text==='ls');
      assert(command>=0,'Visible command echo missing');
      const line=output.text.slice(command+1).find(text=>/^[\w.-]+(?: [\w.-]+){2,5}$/.test(text));
      assert(line,'Visible ls output missing');
      const file=line.split(' ').find(name=>suffix?name.endsWith(suffix):!name.includes('.'));
      assert(file,'No matching filename in visible output');
      row.visible_listing=line;row.selected_file=file;
      for(const char of `rm ${file}`)await press(char===' '?'Space':char);
      await press('Enter');
      row.grade=await grader.grade();assert.equal(row.grade.done,true);assert.equal(row.grade.raw,1);
      row.passed=true;row.calls=engine.latencies.length;
    }catch(error){row.error=error.message;}
    finally{grader?.close();await engine.close();}
    report.results.push(row);console.log(JSON.stringify(row));
  }
}finally{
  await fixture.close();report.summary={passed:report.results.filter(row=>row.passed).length,total:report.results.length};
  if(args.output)fs.writeFileSync(args.output,JSON.stringify(report,null,2)+'\n');
}
if(report.summary.passed!==seeds*2)process.exitCode=1;
