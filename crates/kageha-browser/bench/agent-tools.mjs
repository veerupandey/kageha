// Host adapter invariants. Identity and scoping come from Rust, never from the model.
export function mappedRef(original, fresh, ref) {
  const old=original.controls.find(control=>control.ref===ref);
  if(!old?.backend_node_id||!old.document_id)throw Error('unobserved_ref_identity');
  const now=fresh.controls.find(control=>control.backend_node_id===old.backend_node_id
    &&control.document_id===old.document_id&&control.frame_id===old.frame_id
    &&control.role===old.role&&control.name===old.name);
  if(!now)throw Error('target_replaced_before_dispatch');
  return now;
}
export const pointAction = action => ['pointer','move','drag','wheel'].includes(action);
export const panelAction = action => pointAction(action)||['type','press'].includes(action);
export function panelInput(value){
  switch(value.action){
    case 'pointer':return {action:'click',x:value.x,y:value.y,button:'left'};
    case 'type':return {action:'type',text:value.text};
    case 'press':return {action:'press',key:value.key};
    case 'move':return {action:'move',x:value.x,y:value.y};
    case 'drag':return {action:'drag',x:value.x,y:value.y,to_x:value.to_x,to_y:value.to_y};
    case 'wheel':return {action:'wheel',x:value.x,y:value.y,delta_x:value.delta_x??0,delta_y:value.delta_y??0};
    default:throw Error('unsupported_panel_action');
  }
}
