// Operator-configured, read-only MCP adapter for a Microsoft Graph mailbox.
// The access token is supplied in the MCP server's protected environment.
import readline from 'node:readline';
import { pathToFileURL } from 'node:url';

const graph = 'https://graph.microsoft.com/v1.0';
const email = s => typeof s === 'string' && /^[a-z0-9._+\-]+@[a-z0-9.\-]+$/i.test(s) && s.length <= 320;

export async function readMessages({sender, recipient, after_ms}, fetcher = fetch, token = process.env.KAGEHA_GRAPH_ACCESS_TOKEN, trustedAuthserv = process.env.KAGEHA_GRAPH_TRUSTED_AUTHSERV_ID) {
  if (!email(sender) || !email(recipient) || !Number.isSafeInteger(after_ms) || after_ms <= 0 || !token
      || typeof trustedAuthserv !== 'string' || !/^[a-z0-9](?:[a-z0-9.-]{0,251}[a-z0-9])?$/i.test(trustedAuthserv)) throw Error('invalid mailbox request');
  const domain = sender.split('@')[1].toLowerCase();
  async function get(url) {
    // Never follow redirects with the bearer token and never expose upstream error bodies.
    const res = await fetcher(url, {headers:{Authorization:`Bearer ${token}`,Prefer:'outlook.body-content-type="text"'},redirect:'error',signal:AbortSignal.timeout(10000)});
    if (!res.ok) throw Error('mailbox request failed');
    const body = await res.text();
    if (body.length > 2_000_000) throw Error('mailbox response too large');
    return JSON.parse(body);
  }
  const params = new URLSearchParams({'$filter':`receivedDateTime ge ${new Date(after_ms).toISOString()}`,'$select':'id,receivedDateTime','$top':'11'});
  const list = await get(`${graph}/me/messages?${params}`);
  if (!Array.isArray(list.value) || list.value.length > 10 || list['@odata.nextLink']) return {complete:false,messages:[]};
  const messages = [];
  for (const row of list.value) {
    if (typeof row.id !== 'string' || !row.id || row.id.length > 1024) throw Error('invalid message');
    const fields = new URLSearchParams({'$select':'from,toRecipients,receivedDateTime,internetMessageHeaders,body'});
    const msg = await get(`${graph}/me/messages/${encodeURIComponent(row.id)}?${fields}`);
    const from = msg.from?.emailAddress?.address;
    const recipients = msg.toRecipients?.map(r => r.emailAddress?.address);
    const headers = msg.internetMessageHeaders?.filter(h => h.name?.toLowerCase() === 'authentication-results');
    if (!email(from) || !Array.isArray(recipients) || !recipients.some(v => typeof v === 'string' && v.toLowerCase() === recipient.toLowerCase())) continue;
    if (from.toLowerCase() !== sender.toLowerCase() || !Array.isArray(headers) || headers.length !== 1 || typeof headers[0].value !== 'string') continue;
    const auth = headers[0].value.toLowerCase();
    // Require the operator's precise trusted verifier identity, not an arbitrary
    // Authentication-Results assertion. The deployment must strip untrusted copies.
    if (!auth.startsWith(trustedAuthserv.toLowerCase() + ';')) continue;
    // Reject duplicates and values other than an aligned, explicit DMARC pass.
    const dmarc = [...auth.matchAll(/(?:^|[;\s])dmarc\s*=\s*([^;\s]+)/g)];
    const aligned = [...auth.matchAll(/(?:^|[;\s])header\.from\s*=\s*([^;\s]+)/g)];
    if (dmarc.length !== 1 || dmarc[0][1] !== 'pass' || aligned.length !== 1 || aligned[0][1] !== domain) continue;
    const received = Date.parse(msg.receivedDateTime);
    if (!Number.isSafeInteger(received) || received < after_ms || msg.body?.contentType?.toLowerCase() !== 'text' || typeof msg.body?.content !== 'string' || msg.body.content.length > 131072) continue;
    messages.push({sender:from,recipient,received_ms:received,authentication:{dmarc:'pass',aligned_domain:domain},text:msg.body.content});
  }
  return {complete:true,messages};
}

export async function serve(input = process.stdin, output = process.stdout, reader = readMessages) {
  for await (const line of readline.createInterface({input,crlfDelay:Infinity})) {
    let request;
    try { request = JSON.parse(line); } catch { continue; }
    if (request.id === undefined) continue;
    let result;
    try {
      if (request.method === 'initialize') result = {protocolVersion:'2024-11-05',capabilities:{tools:{}},serverInfo:{name:'outlook-verification',version:'1'}};
      else if (request.method === 'tools/list') result = {tools:[{name:'verification_mail',description:'Private read-only verification mailbox',inputSchema:{type:'object',properties:{sender:{type:'string'},recipient:{type:'string'},after_ms:{type:'integer'}},required:['sender','recipient','after_ms'],additionalProperties:false}}]};
      else if (request.method === 'tools/call' && request.params?.name === 'verification_mail') result = {structuredContent:await reader(request.params.arguments)};
      else throw Error('unsupported request');
      output.write(JSON.stringify({jsonrpc:'2.0',id:request.id,result})+'\n');
    } catch {
      output.write(JSON.stringify({jsonrpc:'2.0',id:request.id,result:{isError:true,content:[{type:'text',text:'Protected mailbox request failed'}]}})+'\n');
    }
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) await serve();
