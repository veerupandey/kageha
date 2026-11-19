import {test} from 'node:test';
import assert from 'node:assert/strict';
import {mappedRef,panelInput,pointAction,panelAction} from './agent-tools.mjs';
const control={ref:'e2',backend_node_id:71,document_id:'doc1',role:'menuitem',name:'Open'};
test('reference refresh survives renumbering and new role families without retargeting',()=>{
 const refreshed={...control,ref:'e9'};
 assert.equal(mappedRef({controls:[control]},{controls:[refreshed]},'e2').ref,'e9');
 for(const field of ['backend_node_id','document_id','frame_id','role','name']){
  assert.throws(()=>mappedRef({controls:[control]},{controls:[{...refreshed,[field]:'different'}]},'e2'),/replaced/);
 }
});
test('model arguments cannot override panel scope or inject extra native fields',()=>{
 assert.deepEqual(panelInput({action:'pointer',x:20,y:30,workspace_id:'other',button:'right',ref:'e2'}),{action:'click',x:20,y:30,button:'left'});
 assert.throws(()=>panelInput({action:'evaluate',code:'evil'}));
});
test('keyboard clearing and typing can execute in one plan; pointer pauses for fresh state',()=>{
 assert(panelAction('press'));assert(panelAction('type'));
 assert(!pointAction('press'));assert(!pointAction('type'));assert(pointAction('pointer'));
});
