// HTTPS OAuth/MCP fixture; uses only synthetic credentials and data.
const https = require('node:https');
const fs = require('node:fs');
const crypto = require('node:crypto');
const dir = process.argv[2], log = process.argv[3], alg = process.argv[4], audience = process.argv[5];
const seen = new Set();
const server = https.createServer({key: fs.readFileSync(dir + '/server.key'), cert: fs.readFileSync(dir + '/server.crt')}, async (req,res) => {
  const origin = `https://127.0.0.1:${server.address().port}`;
  fs.appendFileSync(log, JSON.stringify({path:req.url, method:req.method}) + '\n');
  res.setHeader('Content-Type','application/json');
  if (req.method==='GET' && req.url==='/mcp') {res.writeHead(401,{'WWW-Authenticate':`Bearer resource_metadata="${origin}/.well-known/oauth-protected-resource/mcp"`});return res.end('{}');}
  if (req.method==='GET' && req.url==='/.well-known/oauth-protected-resource/mcp') return res.end(JSON.stringify({resource:origin+'/mcp',authorization_servers:[origin]}));
  if (req.method==='GET' && req.url==='/.well-known/oauth-authorization-server') return res.end(JSON.stringify({issuer:origin+'/',authorization_endpoint:origin+'/authorize',token_endpoint:origin+'/token',token_endpoint_auth_methods_supported:[alg==='HS256'?'client_secret_jwt':'private_key_jwt'],token_endpoint_auth_signing_alg_values_supported:[alg],code_challenge_methods_supported:['S256']}));
  let body='';for await (const chunk of req) body+=chunk;
  if (req.method==='POST' && req.url==='/token') {
    const p = new URLSearchParams(body), assertion = p.get('client_assertion')||'', parts = assertion.split('.');
    try {
      const header=JSON.parse(Buffer.from(parts[0],'base64url')), claims=JSON.parse(Buffer.from(parts[1],'base64url'));
      const data=Buffer.from(parts[0]+'.'+parts[1]), signature=Buffer.from(parts[2],'base64url');
      const key=alg==='HS256'?null:crypto.createPublicKey(fs.readFileSync(dir+'/public-'+alg+'.pem'));
      const valid=alg==='HS256' ? crypto.timingSafeEqual(signature,crypto.createHmac('sha256','0123456789abcdef0123456789abcdef').update(data).digest()) : alg==='ES256' ? crypto.verify('sha256',data,{key,dsaEncoding:'ieee-p1363'},signature) : alg==='RS256' ? crypto.verify('RSA-SHA256',data,key,signature) : crypto.verify(null,data,key,signature);
      if (!valid || header.alg!==alg || claims.iss!=='fixture-client' || claims.sub!==claims.iss || claims.aud!==(audience==='issuer'?origin+'/':origin+'/token') || claims.exp<=Date.now()/1000 || claims.exp-claims.iat>60 || seen.has(claims.jti) || p.get('resource')!==origin+'/mcp' || p.has('client_secret')) throw Error('bad assertion');
      seen.add(claims.jti);
      return res.end(JSON.stringify({access_token:'fixture-token',refresh_token:'fixture-refresh',expires_in:p.get('grant_type')==='authorization_code'?1:3600}));
    } catch (_) {res.statusCode=400;return res.end('{}');}
  }
  if (req.method!=='POST' || req.url!=='/mcp' || req.headers.authorization!=='Bearer fixture-token') {res.statusCode=403;return res.end('{}');}
  const rpc=JSON.parse(body);if(rpc.id==null)return res.end('{}');
  let result;if(rpc.method==='initialize') result={protocolVersion:'2024-11-05',capabilities:{tools:{}},serverInfo:{name:'fixture',version:'1'}};
  else if(rpc.method==='tools/list') result={tools:[{name:'lookup',description:'Read fixture',inputSchema:{type:'object',properties:{}}}]};
  else if(rpc.method==='tools/call') result={content:[{type:'text',text:'assertion-protected result'}]};else {res.statusCode=400;return res.end('{}');}
  res.end(JSON.stringify({jsonrpc:'2.0',id:rpc.id,result}));
});
server.listen(0,'127.0.0.1',()=>process.stdout.write(String(server.address().port)+'\n'));
