import { beforeEach, describe, expect, it, vi } from "vitest";
import { rememberDelivery, pendingDeliveries, acknowledgeDelivery, markDelivery, rememberQueue, restoredQueue, clearDeliverySession } from "./delivery";
describe("delivery recovery", () => {
  beforeEach(() => {
    const data = new Map<string,string>();
    vi.stubGlobal("localStorage", {get length(){return data.size;}, key:(i:number)=>[...data.keys()][i] ?? null, getItem:(k:string)=>data.get(k) ?? null, setItem:(k:string,v:string)=>data.set(k,v),removeItem:(k:string)=>{data.delete(k);}});
  });
  it("preserves unconfirmed replies and removes acknowledged content", () => {
    rememberDelivery({id:"r",sessionId:"s",text:"reply",createdAt:Date.now(),state:"pending"});
    markDelivery("r", "received");
    expect(pendingDeliveries("s")[0]).toMatchObject({text:"reply",state:"received"});
    expect(pendingDeliveries("other")).toEqual([]);
    acknowledgeDelivery("r");
    expect(pendingDeliveries("s")).toEqual([]);
  });
  it("restores queue without silently losing attachments", () => {
    rememberQueue("s", [{text:"follow up",files:[new File(["x"],"x.txt")]}]);
    expect(restoredQueue("s")).toEqual([{text:"follow up",files:[],needsFiles:true,needsReview:true}]);
    expect(restoredQueue("other")).toEqual([]);
    rememberQueue("s", []);
    expect(restoredQueue("s")).toEqual([]);
  });
  it("tolerates corrupt saved data", () => {
    localStorage.setItem("kageha.delivery.v1", JSON.stringify({pending:[null],queues:{s:{bad:true}}}));
    expect(pendingDeliveries("s")).toEqual([]);
    expect(restoredQueue("s")).toEqual([]);
  });
  it("expires old unconfirmed content", () => {
    rememberDelivery({id:"r",sessionId:"s",text:"old",createdAt:Date.now()-8*86400_000,state:"pending"});
    expect(pendingDeliveries("s")).toEqual([]);
  });
});

it("uses independent records so another tab's request is never rewritten", () => {
  const data=new Map<string,string>();
  const written:string[]=[];
  vi.stubGlobal("localStorage",{get length(){return data.size;},key:(i:number)=>[...data.keys()][i] ?? null,getItem:(key:string)=>data.get(key) ?? null,setItem:(key:string,value:string)=>{written.push(key);data.set(key,value);},removeItem:(key:string)=>{data.delete(key);}});
  rememberDelivery({id:"a",sessionId:"s1",text:"first tab",createdAt:Date.now(),state:"pending"});
  const keyA=written.at(-1);
  rememberDelivery({id:"b",sessionId:"s2",text:"second tab",createdAt:Date.now(),state:"pending"});
  expect(written.at(-1)).not.toBe(keyA);
  acknowledgeDelivery("a");
  markDelivery("a","received");
  expect(pendingDeliveries("s1")).toEqual([]);
  expect(pendingDeliveries("s2")[0].text).toBe("second tab");
});

it("migrates legacy recovery without resurrecting acknowledged content", () => {
  const data=new Map<string,string>();
  vi.stubGlobal("localStorage",{get length(){return data.size;},key:(i:number)=>[...data.keys()][i] ?? null,getItem:(key:string)=>data.get(key) ?? null,setItem:(key:string,value:string)=>{data.set(key,value);},removeItem:(key:string)=>{data.delete(key);}});
  const legacy={pending:[{id:"m",sessionId:"s",text:"legacy",createdAt:Date.now(),state:"pending"}],queues:{s:[{text:"follow up",needsFiles:false}]}};
  localStorage.setItem("kageha.delivery.v1",JSON.stringify(legacy));
  expect(pendingDeliveries("s")[0].text).toBe("legacy");
  expect(restoredQueue("s")[0]).toMatchObject({text:"follow up",needsReview:true});
  acknowledgeDelivery("m");
  // Simulates a slow older tab restoring its pre-migration snapshot.
  localStorage.setItem("kageha.delivery.v1",JSON.stringify(legacy));
  expect(pendingDeliveries("s")).toEqual([]);
});

it("clears only deleted session recovery data", () => {
  const data=new Map<string,string>();
  vi.stubGlobal("localStorage",{get length(){return data.size;},key:(i:number)=>[...data.keys()][i] ?? null,getItem:(key:string)=>data.get(key) ?? null,setItem:(key:string,value:string)=>{data.set(key,value);},removeItem:(key:string)=>{data.delete(key);}});
  for(const sid of ["deleted","kept"]){rememberDelivery({id:sid,sessionId:sid,text:sid,createdAt:Date.now(),state:"pending"});rememberQueue(sid,[{text:sid,files:[]}]);}
  clearDeliverySession("deleted");
  expect(pendingDeliveries("deleted")).toEqual([]);
  expect(restoredQueue("deleted")).toEqual([]);
  expect(pendingDeliveries("kept")).toHaveLength(1);
  expect(restoredQueue("kept")).toHaveLength(1);
});

it("retains recoverable text in memory when quota blocks persistence", () => {
  vi.stubGlobal("localStorage",{get length(){return 0;},key:()=>null,getItem:()=>null,setItem(){throw new Error("quota");},removeItem(){}});
  rememberDelivery({id:"quota",sessionId:"quota",text:"keep",createdAt:Date.now(),state:"pending"});
  expect(pendingDeliveries("quota")[0].text).toBe("keep");
  acknowledgeDelivery("quota");
  expect(pendingDeliveries("quota")).toEqual([]);
});

it("keeps both independent records when tab writes interleave", () => {
  const data=new Map<string,string>();
  let nested=false;
  vi.stubGlobal("localStorage",{get length(){return data.size;},key:(i:number)=>[...data.keys()][i] ?? null,getItem:(key:string)=>data.get(key) ?? null,
    setItem:(key:string,value:string)=>{
      if(!nested && key.includes("pending.outer")){
        nested=true;
        rememberDelivery({id:"inner",sessionId:"tabs",text:"other tab",createdAt:Date.now(),state:"pending"});
      }
      data.set(key,value);
    },removeItem:(key:string)=>{data.delete(key);}});
  rememberDelivery({id:"outer",sessionId:"tabs",text:"first tab",createdAt:Date.now(),state:"pending"});
  expect(pendingDeliveries("tabs").map(item=>item.text).sort()).toEqual(["first tab","other tab"]);
});

it("an older tab cannot restore a deleted session's recovery text", () => {
  const data=new Map<string,string>();
  vi.stubGlobal("localStorage",{get length(){return data.size;},key:(i:number)=>[...data.keys()][i] ?? null,getItem:(key:string)=>data.get(key) ?? null,setItem:(key:string,value:string)=>{data.set(key,value);},removeItem:(key:string)=>{data.delete(key);}});
  clearDeliverySession("gone");
  rememberDelivery({id:"old-tab",sessionId:"gone",text:"deleted private text",createdAt:Date.now(),state:"pending"});
  rememberQueue("gone",[{text:"old queue",files:[]}]);
  expect(pendingDeliveries("gone")).toEqual([]);
  expect(restoredQueue("gone")).toEqual([]);
  expect([...data.values()].join()).not.toContain("deleted private text");
});
