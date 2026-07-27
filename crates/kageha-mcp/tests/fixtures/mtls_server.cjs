// Local HTTPS OAuth/MCP fixture. Metadata is public; token and MCP calls require
// the same CA-verified client certificate. Not used by production code.
const https = require("node:https");
const fs = require("node:fs");
const dir = process.argv[2];
const log = process.argv[3];
const server = https.createServer({
  key: fs.readFileSync(dir + "/server.key"), cert: fs.readFileSync(dir + "/server.crt"),
  ca: fs.readFileSync(dir + "/ca.crt"), requestCert: true, rejectUnauthorized: false,
}, async (req, res) => {
  const origin = `https://127.0.0.1:${server.address().port}`;
  const peer = req.socket.getPeerCertificate();
  const identity = req.client.authorized ? peer.subject?.CN ?? "" : "";
  fs.appendFileSync(log, JSON.stringify({path:req.url,method:req.method,identity}) + "\n");
  res.setHeader("Content-Type", "application/json");
  if (req.method === "GET" && req.url === "/mcp") {
    res.writeHead(401, {"WWW-Authenticate": `Bearer resource_metadata="${origin}/.well-known/oauth-protected-resource/mcp"`});
    return res.end("{}");
  }
  if (req.method === "GET" && req.url === "/.well-known/oauth-protected-resource/mcp")
    return res.end(JSON.stringify({resource:origin+"/mcp",authorization_servers:[origin]}));
  if (req.method === "GET" && req.url === "/.well-known/oauth-authorization-server")
    return res.end(JSON.stringify({issuer:origin+"/",authorization_endpoint:origin+"/authorize",token_endpoint:origin+"/ordinary-token",
      mtls_endpoint_aliases:{token_endpoint:origin+"/mtls-token"},token_endpoint_auth_methods_supported:["tls_client_auth"],
      code_challenge_methods_supported:["S256"]}));
  if (identity !== "fixture-client") {res.statusCode=403;return res.end("{}");}
  if (req.method === "POST" && req.url === "/mtls-token") {
    let input="";for await(const chunk of req)input+=chunk;
    const fields=new URLSearchParams(input);
    if (fields.get("client_id")!=="fixture-client" || !["authorization_code","refresh_token"].includes(fields.get("grant_type"))) {res.statusCode=400;return res.end("{}");}
    return res.end(JSON.stringify({access_token:"fixture-token",refresh_token:"fixture-refresh",expires_in:fields.get("grant_type")==="authorization_code"?1:3600}));
  }
  if (req.method !== "POST" || req.url !== "/mcp" || req.headers.authorization !== "Bearer fixture-token") {
    res.statusCode=403;return res.end("{}");
  }
  let input="";for await(const chunk of req)input+=chunk;
  const rpc=JSON.parse(input);
  if (rpc.id == null) return res.end("{}");
  let result;
  if (rpc.method==="initialize") result={protocolVersion:"2024-11-05",capabilities:{tools:{}},serverInfo:{name:"fixture",version:"1"}};
  else if (rpc.method==="tools/list") result={tools:[{name:"lookup",description:"Read fixture",inputSchema:{type:"object",properties:{}}}]};
  else if (rpc.method==="tools/call") result={content:[{type:"text",text:"certificate-bound result"}]};
  else {res.statusCode=400;return res.end("{}");}
  res.end(JSON.stringify({jsonrpc:"2.0",id:rpc.id,result}));
});
server.listen(0,"127.0.0.1",()=>process.stdout.write(String(server.address().port)+"\n"));
