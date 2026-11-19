import {test} from 'node:test';
import assert from 'node:assert/strict';
import {readMessages} from './adapter.mjs';

const after=Date.parse('2026-01-01T00:00:00Z'), args={sender:'security@vendor.test',recipient:'user@example.test',after_ms:after};
function mocked(message, list={value:[{id:'id1',receivedDateTime:'2026-01-01T00:00:01Z'}]}) {
  const calls=[];
  return {calls,fetcher:async (url, options) => {
    calls.push({url,options});
    return {ok:true,text:async()=>JSON.stringify(calls.length===1?list:message)};
  }};
}
function message() { return {from:{emailAddress:{address:args.sender}},toRecipients:[{emailAddress:{address:args.recipient}}],
  receivedDateTime:'2026-01-01T00:00:01Z',internetMessageHeaders:[{name:'Authentication-Results',value:'trusted.mail.test; spf=pass; dmarc=pass action=none header.from=vendor.test'}],
  body:{contentType:'text',content:'Your code is AB12CD'}}; }

test('reads Graph over HTTPS and emits a normalized authenticated message', async()=>{
 const mock=mocked(message()); const result=await readMessages(args,mock.fetcher,'token-canary','trusted.mail.test');
 assert.equal(result.complete,true); assert.equal(result.messages[0].text,'Your code is AB12CD');
 assert(mock.calls.every(c=>c.url.startsWith('https://graph.microsoft.com/v1.0/me/messages')));
 assert(mock.calls.every(c=>c.options.redirect==='error' && c.options.headers.Authorization==='Bearer token-canary'));
});
test('rejects incomplete results, spoofed headers and missing authentication', async()=>{
 const incomplete=mocked(message(),{value:[], '@odata.nextLink':'next'});
 assert.equal((await readMessages(args,incomplete.fetcher,'token','trusted.mail.test')).complete,false);
 for (const headers of [[], [{name:'Authentication-Results',value:'trusted.mail.test; dmarc=fail header.from=vendor.test'}],
   [{name:'Authentication-Results',value:'trusted.mail.test; dmarc=pass header.from=other.test'}], [{name:'Authentication-Results',value:'forged.mail.test; dmarc=pass header.from=vendor.test'}],
   [{name:'Authentication-Results',value:'trusted.mail.test; dmarc=pass header.from=vendor.test'},{name:'Authentication-Results',value:'trusted.mail.test; dmarc=pass header.from=vendor.test'}]]) {
   const msg=message();msg.internetMessageHeaders=headers;const mock=mocked(msg);
   assert.deepEqual((await readMessages(args,mock.fetcher,'token','trusted.mail.test')).messages,[]);
 }
});
test('does not send a token to an arbitrary URL or disclose provider errors', async()=>{
 await assert.rejects(readMessages(args,async()=>({ok:false,text:async()=>{throw Error('should not read');}}),'token-canary','trusted.mail.test'),/mailbox request failed/);
 await assert.rejects(readMessages({...args,sender:"a'@evil.test"},async()=>{throw Error('called');},'token','trusted.mail.test'),/invalid mailbox request/);
});
