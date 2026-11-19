import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { BufferJSON } from '@whiskeysockets/baileys';
import { routeInbound, toWhatsAppJid } from './routing.mjs';
import { RetryMessageStore } from './retry-store.mjs';
const own = ['15550000000:4@s.whatsapp.net', '999@lid'];
const event = (text, extra = {}) => ({ key: { remoteJid: '120123@g.us', participant: '15551111111@s.whatsapp.net', id: 'incoming', ...extra }, message: { conversation: text } });

test('group commands keep sender and group destination separate', () => {
  const routed = routeInbound(event('/agent hello'), own, new Set());
  assert.equal(routed.sender, '15551111111');
  assert.equal(routed.chat, '120123@g.us');
  assert.equal(routed.text, 'hello');
  assert.equal(toWhatsAppJid(routed.chat), '120123@g.us');
  assert.equal(toWhatsAppJid('15551111111'), '15551111111@s.whatsapp.net');
  assert.throws(() => toWhatsAppJid('status@broadcast'));
});
test('group chatter needs a trigger unless all mode is selected', () => {
  assert.equal(routeInbound(event('hello everyone'), own, new Set()), null);
  assert.ok(routeInbound(event('hello everyone'), own, new Set(), 'all'));
  assert.equal(routeInbound(event('/agent hello'), own, new Set(), 'off'), null);
});
test('mentions and replies to known agent messages trigger groups', () => {
  const mentioned = event('');
  mentioned.message = { extendedTextMessage: { text: 'hello', contextInfo: { mentionedJid: ['15550000000@s.whatsapp.net'] } } };
  assert.ok(routeInbound(mentioned, own, new Set()));
  mentioned.message.extendedTextMessage.contextInfo = { stanzaId: 'agent-reply' };
  assert.ok(routeInbound(mentioned, own, new Set(['agent-reply'])));
  assert.equal(routeInbound(mentioned, own, new Set()), null);
});
test('manual owner group commands work and agent echoes are ignored', () => {
  assert.equal(routeInbound(event('/agent hi', { fromMe: true }), own, new Set()).sender, '15550000000');
  assert.equal(routeInbound(event('/agent hi', { fromMe: true }), own, new Set(['incoming'])), null);
});
test('wrapped messages and participant phone identity are handled', () => {
  const msg = event('', { participant: '888@lid', participantPn: '15551111111@s.whatsapp.net' });
  msg.message = { ephemeralMessage: { message: { conversation: '/agent hello' } } };
  assert.equal(routeInbound(msg, own, new Set()).sender, '15551111111');
});
test('sent content and buffers survive restarts for retransmission', () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'wa-retry-'));
  try {
    const key = { id: 'sent1', remoteJid: '120123@g.us' };
    new RetryMessageStore(directory, BufferJSON).put(key, { conversation: 'hello', imageMessage: { jpegThumbnail: Buffer.from([1, 2, 3]) } });
    const restored = new RetryMessageStore(directory, BufferJSON);
    assert.equal(restored.get(key).conversation, 'hello');
    assert.deepEqual(restored.get(key).imageMessage.jpegThumbnail, Buffer.from([1, 2, 3]));
    assert.ok(restored.hasId('sent1'));
    assert.equal(restored.get({ ...key, remoteJid: 'other@g.us' }), undefined);
    assert.equal(fs.statSync(restored.file).mode & 0o777, 0o600);
  } finally { fs.rmSync(directory, { recursive: true, force: true }); }
});

test('personal linking accepts ordinary and prefixed text only in the owner self-chat', async () => {
  const {selfCommand}=await import('./personal.mjs');
  const stored={hasId:id=>id==='relay'};
  const message=event('Kageha: status',{fromMe:true,remoteJid:'15550000000@s.whatsapp.net'});
  assert.equal(selfCommand(message,own,stored),'status');
  for(const update of [{fromMe:false},{remoteJid:'1201@g.us'},{remoteJid:'15551111111@s.whatsapp.net'},{id:'relay'}])
    assert.equal(selfCommand({...message,key:{...message.key,...update}},own,stored),null);
  assert.equal(selfCommand(event(' hi ',message.key),own,stored),'hi');
  assert.equal(selfCommand(event('Kageha:',message.key),own,stored),null);
  assert.equal(selfCommand(event('Kageha Personal · hi',message.key),own,stored),null);
  const linkedId=event('',{...message.key,remoteJid:'999@lid'});
  linkedId.message={extendedTextMessage:{text:'how are you?'}};
  assert.equal(selfCommand(linkedId,own,stored),'how are you?');
});
test('device retries resolve both self addresses and never return content to another chat', async () => {
  const {savedSelfMessage}=await import('./personal.mjs');
  const body={conversation:'original reply'};
  const store={get:({remoteJid,id})=>remoteJid==='15550000000@s.whatsapp.net' && id==='original' ? body:undefined};
  for(const remoteJid of ['15550000000@s.whatsapp.net','15550000000:5@s.whatsapp.net','999:6@lid'])
    assert.equal(savedSelfMessage(store,{remoteJid,id:'original'},own),body);
  assert.equal(savedSelfMessage(store,{remoteJid:'100@lid',id:'original'},own),undefined);
  assert.equal(savedSelfMessage(store,{remoteJid:'999@lid',id:'unrelated'},own),undefined);
});

test('the linked-device encoder sends an image inline and preserves a document filename', async () => {
  const { generateWAMessage } = await import('baileys-personal');
  const { whatsappMedia } = await import('../channels/whatsapp.mjs');
  const data = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVQIHWP4z8DwHwAFgAI/ScLbtAAAAABJRU5ErkJggg==','base64');
  const options = {userJid:'15550000000@s.whatsapp.net',upload:async file => {
    assert.ok(fs.statSync(file).size > 0);
    return {mediaUrl:'https://example.invalid/media',directPath:'/media'};
  }};
  const image = await generateWAMessage(options.userJid, whatsappMedia(data,'pixel.png','image/png'),options);
  assert.equal(image.message.imageMessage.mimetype,'image/png');
  const file = await generateWAMessage(options.userJid, whatsappMedia(Buffer.from('%PDF-1.7'),'brief.pdf','application/pdf'),options);
  assert.equal(file.message.documentMessage.fileName,'brief.pdf');
  assert.equal(file.message.documentMessage.mimetype,'application/pdf');
});

test("linked chat typing follows pending replies across overlapping tasks and reconnects", async () => {
  const { replyPresence } = await import("./personal.mjs");
  const { journal, receive } = await import("../personal/assistant.mjs");
  const db = journal(":memory:");
  const jid = own[0];
  const sent = [];
  const socket = {async sendPresenceUpdate(state, to) {
    assert.equal(to, jid);
    sent.push(state);
    if (sent.length === 1) throw Error("presence unavailable");
  }};
  try {
    const sync = replyPresence(db, socket, jid);
    const say = (id,text) => receive(db,{id,channel:"whatsapp",text});
    sync(0);
    say("1","Good morning ☀️");
    sync(100);
    assert.equal(say("1","Good morning ☀️"), "Morning ☀️");
    assert.equal(say("2","Thanks! 🙏🏽"), "Anytime 😊");
    sync(120);
    assert.deepEqual(sent,["composing"]);
    db.prepare("UPDATE outbox SET state='sent' WHERE id='ack:1'").run();
    sync(150);
    say("3","Find a table for two");
    db.prepare("UPDATE outbox SET state='sent' WHERE id='ack:2'").run();
    sync(4200);
    assert.deepEqual(sent,["composing","composing"]);
    // The task can keep running after the first acknowledgement is delivered.
    db.exec("UPDATE outbox SET state='sent' WHERE state='scheduled'");
    sync(4500);
    sync(9500);
    assert.deepEqual(sent,["composing","composing","paused"]);
    say("4","👋🏻");
    const reconnect = replyPresence(db,socket,jid);
    reconnect(9600);
    db.exec("UPDATE outbox SET retry=12345 WHERE state='pending'");
    reconnect(9700);
    assert.deepEqual(sent.slice(-2),["composing","paused"]);
  } finally {
    db.close();
  }
});

test("an encrypted WhatsApp PDF and the following text enter the same conversation",async()=>{
  const {generateWAMessage,downloadMediaMessage}=await import("baileys-personal");
  const {createServer}=await import("node:http");
  const {once}=await import("node:events");
  const {readDocument,selfInput}=await import("./personal.mjs");
  const {journal,receive}=await import("../personal/assistant.mjs");
  const {advance}=await import("../personal/runtime.mjs");
  const db=journal(":memory:");
  const plain=Buffer.from("%PDF-1.4\nRésumé fixture\n%%EOF\n");
  let encrypted;
  const server=createServer((req,res)=>{res.end(encrypted);});
  server.listen(0,"127.0.0.1");await once(server,"listening");
  try {
    const key={fromMe:true,remoteJid:own[0],id:"pdf"};
    const generated=await generateWAMessage(own[0],{document:plain,fileName:"resume.pdf",mimetype:"application/pdf"},{
      userJid:own[0],upload:async file=>{encrypted=fs.readFileSync(file);return {mediaUrl:"http://127.0.0.1:"+server.address().port+"/doc"};}
    });
    const wrapped={key,message:{documentWithCaptionMessage:{message:generated.message}}};
    const seen={hasId:()=>false};
    assert.equal(selfInput({...wrapped,key:{...key,remoteJid:"unrelated@g.us"}},own,seen),null);
    assert.equal(selfInput(wrapped,own,{hasId:()=>true}),null);
    const {document:header,text}=selfInput(wrapped,own,seen);
    const document=await readDocument(header,()=>downloadMediaMessage(wrapped,"stream",{}));
    assert.deepEqual(document.bytes,plain);
    assert.equal(receive(db,{id:"pdf",channel:"whatsapp",text,document}),"I’ve got resume.pdf.");
    const say=(id,text)=>receive(db,{id,channel:"whatsapp",text});
    say("review","Use my résumé for senior and staff roles");
    let uploads=0;const turns=[];
    const api=async(route,body)=>{
      if(route==="/api/sessions") return {session_id:"jobs"};
      if(route.endsWith("/upload")) {uploads++;assert.deepEqual(Buffer.from(await body.get("file").arrayBuffer()),plain);return {path:"inputs/resume.pdf"};}
      if(route.includes("/requests/")) throw Object.assign(Error("new"),{status:404});
      if(route==="/api/chat/stream"){turns.push(body);return {turn_id:"turn"+turns.length};}
      if(route.includes("/events?"))return {events:[{kind:"done",payload:{status:"success",message:"Ready."}}]};
      return {};
    };
    const latest=()=>db.prepare("SELECT * FROM tasks ORDER BY id DESC LIMIT 1").get();
    await advance(db,latest(),api);
    assert.deepEqual(turns[0].attachments,["inputs/resume.pdf"]);
    say("next","Which role is the best fit?");
    assert.equal(latest().session,"jobs");
    await advance(db,latest(),api);
    assert.equal(turns[1].session_id,"jobs");
    assert.equal(uploads,1);
    say("new","/new Talk about tomorrow’s weather");
    assert.equal(latest().session,null);
    await assert.rejects(readDocument({fileLength:16777217},()=>{throw Error("unexpected download")}),{code:"FILE_TOO_LARGE"});
  } finally {server.close();db.close();}
});
