//! OAuth authorization for an explicitly configured HTTPS MCP resource.
//! Tokens and PKCE state stay in the managed credential vault.
use crate::{native_io, ui_credentials, ui_settings, ui_store, webui::Database};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{io::Read, net::{IpAddr, TcpListener}, sync::atomic::AtomicBool, time::Duration};
use tiny_http::{Response, Server};
use url::Url;
use std::{collections::HashMap, sync::{Arc,Mutex,OnceLock}};

type Result<T> = ui_store::ApiResult<T>;
const KIND: &str = "config:mcp:servers";
const REGISTRATION: &str = "config:mcp:oauth-registration";

fn grant(name: &str) -> String { format!("mcp-{name}") }
fn endpoint(raw: &str) -> Result<Url> {
    let url = Url::parse(raw).map_err(|_| (400, "Invalid OAuth URL".into()))?;
    if url.scheme() != "https" || url.host_str().is_none() || !url.username().is_empty()
        || url.password().is_some() || url.query().is_some() || url.fragment().is_some() || raw.len() > 2048 {
        return Err((400, "OAuth endpoints require a plain HTTPS URL".into()));
    }
    Ok(url)
}
/// OAuth may exchange long-lived credentials: a remote plugin's metadata alone
/// cannot authorize arbitrary network destinations. Operators can explicitly
/// extend this exact-origin allowlist for their deployment.
#[cfg(test)]
static TEST_TRUST: OnceLock<Mutex<Option<String>>> = OnceLock::new();
#[cfg(test)]
static TEST_TRUST_SERIAL: Mutex<()> = Mutex::new(());
#[cfg(test)]
pub(crate) struct TestTrustGuard { _serial: std::sync::MutexGuard<'static,()> }
#[cfg(test)]
impl Drop for TestTrustGuard {fn drop(&mut self) {*TEST_TRUST.get_or_init(Default::default).lock().unwrap()=None;}}
#[cfg(test)]
pub(crate) fn test_trust_loopback(raw:&str)->Result<TestTrustGuard> {
    let lock=TEST_TRUST_SERIAL.lock().map_err(ui_store::db_error)?;
    let url=endpoint(raw)?;
    if url.path()!="/" || url.port().is_none() || !url.host_str().and_then(|v|v.parse::<IpAddr>().ok()).is_some_and(|v|v.is_loopback()) {
        return Err((400,"Test trust must be an exact loopback HTTPS origin".into()));
    }
    *TEST_TRUST.get_or_init(Default::default).lock().map_err(ui_store::db_error)?=Some(url.origin().ascii_serialization());
    Ok(TestTrustGuard{_serial:lock})
}
fn trusted(url: &Url) -> bool {
    #[cfg(test)]
    if TEST_TRUST.get_or_init(Default::default).lock().ok().and_then(|v|v.clone()).as_deref()==Some(url.origin().ascii_serialization().as_str()) {return true;}
    let Some(host)=url.host_str() else {return false};
    if host.parse::<IpAddr>().is_ok() || !host.contains('.') || host.eq_ignore_ascii_case("localhost")
        || host.ends_with(".local") || host.ends_with(".internal") {return false;}
    let origin=url.origin().ascii_serialization();
    if matches!(origin.as_str(),"https://api.githubcopilot.com"|"https://github.com") {return true;}
    std::env::var("KAGEHA_MCP_OAUTH_TRUSTED_ORIGINS").unwrap_or_default().split(',')
        .filter_map(|part| endpoint(part.trim()).ok()).any(|allowed|
            allowed.path()=="/" && allowed.origin().ascii_serialization()==origin)
}
fn redirect(raw: &str) -> Result<Url> {
    let url = Url::parse(raw).map_err(|_| (400, "Invalid OAuth callback".into()))?;
    let loopback = url.host_str().and_then(|host| host.parse::<IpAddr>().ok()).is_some_and(|ip| ip.is_loopback());
    if url.scheme() != "http" || !loopback || url.port().is_none() || url.path() == "/"
        || !url.username().is_empty() || url.password().is_some() || url.query().is_some() || url.fragment().is_some() {
        return Err((400, "OAuth callback must be an explicit loopback HTTP address and path".into()));
    }
    Ok(url)
}
fn resource_key(url: &Url) -> String { url.as_str().trim_end_matches('/').to_owned() }
fn metadata_url(base: &Url, well_known: &str) -> Result<String> {
    let mut result = base.clone();
    let suffix = base.path().trim_end_matches('/');
    result.set_path(&format!("/.well-known/{well_known}{suffix}"));
    Ok(result.to_string())
}
// Extract only an unambiguous Bearer resource_metadata parameter. Other
// schemes and all challenge text are kept out of user/model-visible errors.
fn challenge_metadata(challenges:&[String])->Result<Option<String>> {
    let mut found:Option<String>=None;
    for challenge in challenges {
        let mut pieces=Vec::new();let mut start=0;let mut quoted=false;let mut escaped=false;
        for (i,ch) in challenge.char_indices() {
            if escaped {escaped=false;continue;}
            if ch=='\\' && quoted {escaped=true;continue;}
            if ch=='"' {quoted=!quoted;}
            else if ch==',' && !quoted {pieces.push(&challenge[start..i]);start=i+1;}
        }
        if quoted || escaped {return Err((400,"Invalid OAuth authentication challenge".into()));}
        pieces.push(&challenge[start..]);let mut bearer=false;
        for part in pieces {
            let mut part=part.trim();
            if let Some((scheme,rest))=part.split_once(' ') {
                if !scheme.contains('=') && !rest.trim_start().starts_with('=') {bearer=scheme.eq_ignore_ascii_case("Bearer");part=rest.trim();}
            }
            if !bearer {continue;}
            let Some((key,value))=part.split_once('=') else {continue};
            if !key.trim().eq_ignore_ascii_case("resource_metadata") {continue;}
            let value=value.trim();
            let value=if value.starts_with('"') && value.ends_with('"') && value.len()>=2 {&value[1..value.len()-1]} else {value};
            if value.is_empty() || value.contains('\\') || value.contains('"') || value.chars().any(char::is_control) {return Err((400,"Invalid OAuth resource metadata URL".into()));}
            if found.as_deref().is_some_and(|previous|previous!=value) {return Err((400,"Ambiguous OAuth resource metadata".into()));}
            found=Some(value.to_owned());
        }
    }
    Ok(found)
}
fn probe(resource:&Url)->Result<Option<String>> {
    let reply=native_io::http(resource.as_str(),"GET",&[("Accept".into(),"application/json, text/event-stream".into())],vec![],
        &AtomicBool::new(false),Duration::from_secs(10),128*1024).map_err(|_|(502,"MCP OAuth discovery unavailable".into()))?;
    if reply.status!=401 {return Ok(None);}
    challenge_metadata(&reply.www_authenticate)
}
fn get_json(url: &str) -> Result<Value> {
    let reply = native_io::http(url, "GET", &[("Accept".into(), "application/json".into())], vec![],
        &AtomicBool::new(false), Duration::from_secs(10), 128 * 1024)
        .map_err(|_| (502, "OAuth metadata unavailable".into()))?;
    if reply.status != 200 { return Err((502, "OAuth metadata unavailable".into())); }
    serde_json::from_slice(&reply.bytes).map_err(|_| (502, "OAuth metadata invalid".into()))
}
fn validate_metadata(resource: &Url, protected: &Value, issuer: &Value, challenge:bool) -> Result<(String,String,String)> {
    let advertised = endpoint(protected["resource"].as_str().unwrap_or(""))?;
    if (challenge && advertised.as_str()!=resource.as_str()) || (!challenge && resource_key(&advertised)!=resource_key(resource)) { return Err((403, "OAuth resource metadata does not match this server".into())); }
    let servers = protected["authorization_servers"].as_array().filter(|items| items.len() == 1)
        .ok_or((400, "OAuth resource must identify one authorization server".into()))?;
    let authority = endpoint(servers[0].as_str().unwrap_or(""))?;
    let actual_issuer = endpoint(issuer["issuer"].as_str().unwrap_or(""))?;
    if resource_key(&authority) != resource_key(&actual_issuer) { return Err((403, "OAuth issuer metadata mismatch".into())); }
    let auth = endpoint(issuer["authorization_endpoint"].as_str().unwrap_or(""))?;
    let token = endpoint(issuer["token_endpoint"].as_str().unwrap_or(""))?;
    if auth.origin() != authority.origin() || token.origin() != authority.origin() {
        return Err((403, "OAuth endpoints do not belong to the discovered issuer".into()));
    }
    if !issuer["code_challenge_methods_supported"].as_array().is_some_and(|v| v.iter().any(|v| v == "S256")) {
        return Err((400, "OAuth server does not advertise PKCE S256".into()));
    }
    Ok((authority.to_string(), auth.to_string(), token.to_string()))
}
fn validate_client_document(document:&Value,id:&Url,callback:&Url)->Result<()> {
    if document["client_id"]!=id.as_str() || document["token_endpoint_auth_method"]!="none"
        || document["redirect_uris"].as_array().is_none_or(|items|!items.iter().any(|item|item==callback.as_str()))
        || document["grant_types"].as_array().is_none_or(|items|!items.iter().any(|item|item=="authorization_code"))
        || document["response_types"].as_array().is_none_or(|items|!items.iter().any(|item|item=="code")) {
        return Err((400,"OAuth client metadata document does not match this client and callback".into()));
    }
    Ok(())
}
fn row(db: &Database, name: &str) -> Result<Value> {
    if !crate::server::identifier(name) { return Err((400, "Invalid MCP server".into())); }
    let connection = db.lock().map_err(ui_store::db_error)?;
    ui_store::get(&connection, KIND, name).map_err(ui_store::db_error)?
        .ok_or((404, "MCP server not found".into()))
}
fn settings(config: &Value) -> Result<(Url, Url, String)> {
    if !matches!(config["transport"].as_str(), Some("http"|"streamable_http")) { return Err((400, "MCP OAuth requires an HTTP server".into())); }
    let resource = endpoint(config["url"].as_str().unwrap_or(""))?;
    let callback = redirect(config["oauth"]["callback_url"].as_str().unwrap_or(""))?;
    let client = config["oauth"]["client_id"].as_str().filter(|s| !s.is_empty() && s.len() <= 1024 && !s.chars().any(char::is_control))
        .ok_or((400, "MCP OAuth client is not configured".into()))?.to_owned();
    Ok((resource, callback, client))
}
/// Sanitize a plugin OAuth declaration before it can enter a preview or UI row.
/// The client secret is deliberately not returned or stored in these records.
pub(crate) fn public_declaration(value:&Value)->Result<Value> {
    let fields=value.as_object().ok_or((400,"Invalid MCP OAuth declaration".into()))?;
    if fields.keys().any(|key|!matches!(key.as_str(),"client_id"|"client_secret"|"callback_port"|"callback_url"|"scope"|"dynamic_registration")) {
        return Err((400,"Unsupported MCP OAuth options".into()));
    }
    let dynamic=value["dynamic_registration"]==true;
    if value.get("dynamic_registration").is_some_and(|v|!v.is_boolean()) {return Err((400,"Invalid OAuth registration mode".into()));}
    let client=value["client_id"].as_str().filter(|v|!v.is_empty() && v.len()<=1024 && !v.chars().any(char::is_control));
    if dynamic && client.is_some() || !dynamic && client.is_none() {return Err((400,"Invalid MCP OAuth client".into()));}
    let callback=redirect(value["callback_url"].as_str().unwrap_or(if dynamic {"http://127.0.0.1:12799/callback"} else {""}))?;
    if let Some(port) = value.get("callback_port") {
        if port.as_u64().is_none_or(|port| port > 65535 || Some(port as u16) != callback.port()) {
            return Err((400,"MCP OAuth callback port mismatch".into()));
        }
    }
    let secret=match value.get("client_secret") {
        None|Some(Value::Null)=>false,
        Some(Value::String(v)) if v.len()<=4096 && !v.chars().any(char::is_control)=>!v.is_empty(),
        _=>return Err((400,"Invalid MCP OAuth client secret".into()))
    };
    if dynamic && secret {return Err((400,"Dynamic OAuth client cannot include a secret".into()));}
    let mut public=json!({"callback_url":callback.as_str(),"client_secret_required":secret});
    if dynamic {public["dynamic_registration"]=json!(true);} else {public["client_id"]=json!(client.unwrap());}
    if let Some(scope)=value.get("scope") {
        let scope=scope.as_str().filter(|s|s.len()<=1024 && !s.chars().any(char::is_control)).ok_or((400,"Invalid MCP OAuth scope".into()))?;
        public["scope"]=json!(scope);
    }
    Ok(public)
}
fn client_document_trusted(config:&Value)->bool {
    config["oauth"]["client_metadata_document"]!=true || config["oauth"]["client_id"].as_str().and_then(|s|endpoint(s).ok()).is_some_and(|url|trusted(&url))
}
pub(crate) fn trust_valid(config:&Value)->bool {
    settings(config).ok().is_some_and(|(resource,_,_)|trusted(&resource)) && client_document_trusted(config)
        && config["oauth_binding"]["issuer"].as_str().and_then(|s|endpoint(s).ok()).is_some_and(|issuer|trusted(&issuer))
}
fn valid_assertion_credential(method:&str,algorithm:&str,credential:&str)->Result<()> {
    if credential.len()>65536 || credential.chars().any(char::is_control) {return Err((400,"Invalid OAuth assertion credential".into()));}
    match (method,algorithm) {
        ("client_secret_jwt","HS256") if credential.len()>=32=>Ok(()),
        ("private_key_jwt","EdDSA"|"RS256"|"ES256")=>{
            let der=zeroize::Zeroizing::new(data_encoding::BASE64URL_NOPAD.decode(credential.as_bytes()).map_err(|_|(400,"Invalid OAuth private key".into()))?);
            match algorithm {
                "EdDSA"=>{ring::signature::Ed25519KeyPair::from_pkcs8_maybe_unchecked(&der).map_err(|_|(400,"Invalid Ed25519 PKCS#8 key".into()))?;},
                "ES256"=>{ring::signature::EcdsaKeyPair::from_pkcs8(&ring::signature::ECDSA_P256_SHA256_FIXED_SIGNING,&der,&ring::rand::SystemRandom::new()).map_err(|_|(400,"Invalid P-256 PKCS#8 key".into()))?;},
                "RS256"=>{let key=ring::signature::RsaKeyPair::from_pkcs8(&der).map_err(|_|(400,"Invalid RSA PKCS#8 key".into()))?;
                    if key.public().modulus_len()<256 {return Err((400,"RSA key must be at least 2048 bits".into()));}},
                _=>unreachable!(),
            }
            Ok(())
        },
        _=>Err((400,"Unsupported OAuth assertion credential or algorithm".into())),
    }
}
/// Explicit local operator provisioning; key material never enters UI records.
pub(crate) fn provision_assertion(db:&Database,payload:&Value)->Result<Value> {
    let name=ui_store::text(payload,"name",64)?;
    if !crate::server::identifier(name) {return Err((400,"Invalid MCP server".into()));}
    let revision=payload["expected_revision"].as_u64().ok_or((400,"Expected revision is required".into()))?;
    let method=payload["method"].as_str().unwrap_or("");let algorithm=payload["algorithm"].as_str().unwrap_or("");
    let audience=match payload.get("assertion_audience") {None|Some(Value::Null)=>"token_endpoint",Some(Value::String(v)) if matches!(v.as_str(),"token_endpoint"|"issuer")=>v.as_str(),_=>return Err((400,"Invalid OAuth assertion audience choice".into()))};
    let credential=payload["credential"].as_str().ok_or((400,"OAuth assertion credential is required".into()))?;
    valid_assertion_credential(method,algorithm,credential)?;
    let issuer=endpoint(payload["issuer"].as_str().unwrap_or(""))?;
    if !trusted(&issuer) {return Err((403,"OAuth issuer origin is not trusted by this host".into()));}
    let kid=match payload.get("key_id") {None|Some(Value::Null)=>None,Some(Value::String(s)) if !s.is_empty() && s.len()<=256 && !s.chars().any(char::is_control)=>Some(s.as_str()),_=>return Err((400,"Invalid OAuth key identifier".into()))};
    let mut connection=db.lock().map_err(ui_store::db_error)?;
    let tx=connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(ui_store::db_error)?;
    let mut config=ui_store::get(&tx,KIND,name).map_err(ui_store::db_error)?.ok_or((404,"MCP server not found".into()))?;
    if config["revision"].as_u64()!=Some(revision) {return Err((409,"MCP configuration changed; review it again".into()));}
    if !matches!(config["transport"].as_str(),Some("http"|"streamable_http")) || !trusted(&endpoint(config["url"].as_str().unwrap_or(""))?) {return Err((403,"OAuth resource is not trusted".into()));}
    if ui_credentials::pending_get(&tx,&grant(name))?.is_some_and(|v|v["expires"].as_f64().is_some_and(|t|t>=ui_store::now())) {return Err((409,"Finish or cancel pending authorization first".into()));}
    if config["oauth"].is_null() {
        let client=payload["client_id"].as_str().filter(|s|!s.is_empty() && s.len()<=1024 && !s.chars().any(char::is_control)).ok_or((400,"OAuth client ID is required".into()))?;
        let callback=redirect(payload["callback_url"].as_str().unwrap_or("http://127.0.0.1:12799/callback"))?;
        config["oauth"]=json!({"client_id":client,"callback_url":callback.as_str()});
    } else {
        if payload["client_id"].as_str().is_some_and(|v|config["oauth"]["client_id"]!=v)
            || payload["callback_url"].as_str().is_some_and(|v|config["oauth"]["callback_url"]!=v) {
            return Err((409,"OAuth client identity does not match the configured server".into()));
        }
        settings(&config)?;
    }
    ui_credentials::delete(&tx,&format!("oauth:{}:grant",grant(name)))?;
    ui_settings::save_secret(&tx,&format!("mcp:{name}:oauth-client"),credential)?;
    config["oauth"]["client_secret_required"]=json!(true);config["oauth"]["client_secret_expires_at"]=Value::Null;
    config["oauth"]["client_auth_method"]=json!(method);config["oauth"]["client_auth_algorithm"]=json!(algorithm);
    config["oauth"]["credential_issuer"]=json!(issuer.as_str());config["oauth"]["key_id"]=json!(kid);
    config["oauth"]["assertion_audience"]=json!(audience);
    config["oauth_binding"]=Value::Null;config["enabled"]=json!(false);config["connected"]=json!(false);config["tools"]=json!([]);
    ui_store::put(&tx,KIND,name,config).map_err(ui_store::db_error)?;
    tx.commit().map_err(ui_store::db_error)?;
    Ok(json!({"ok":true,"server":name,"method":method,"algorithm":algorithm,"reconnect_required":true}))
}
fn client_assertion(values:&Value,token_endpoint:&str)->Result<zeroize::Zeroizing<String>> {
    let audience=match values["assertion_audience"].as_str().unwrap_or("token_endpoint") {
        "token_endpoint"=>endpoint(token_endpoint)?,
        "issuer"=>{let issuer=endpoint(values["issuer"].as_str().unwrap_or(""))?;
            if !trusted(&issuer) {return Err((403,"OAuth assertion issuer is not trusted".into()));}issuer},
        _=>return Err((400,"Invalid OAuth assertion audience choice".into())),
    };
    let client=values["client_id"].as_str().filter(|s|!s.is_empty() && s.len()<=1024).ok_or((400,"OAuth client ID unavailable".into()))?;
    let method=values["client_auth_method"].as_str().unwrap_or("");let algorithm=values["client_auth_algorithm"].as_str().unwrap_or("");
    let secret=values["client_secret"].as_str().unwrap_or("");valid_assertion_credential(method,algorithm,secret)?;
    let now=ui_store::now() as u64;
    let mut header=json!({"alg":algorithm,"typ":"JWT"});
    if let Some(kid)=values["key_id"].as_str().filter(|v|!v.is_empty() && v.len()<=256 && !v.chars().any(char::is_control)){header["kid"]=json!(kid);}
    let claims=json!({"iss":client,"sub":client,"aud":audience.as_str(),"iat":now,"exp":now+60,"jti":nonce()?});
    let encoded=zeroize::Zeroizing::new(format!("{}.{}",data_encoding::BASE64URL_NOPAD.encode(header.to_string().as_bytes()),data_encoding::BASE64URL_NOPAD.encode(claims.to_string().as_bytes())));
    let signature=match (method,algorithm) {
        ("client_secret_jwt","HS256")=>{
            use hmac::{Hmac,Mac};let mut mac=Hmac::<Sha256>::new_from_slice(secret.as_bytes()).map_err(|_|(400,"Invalid OAuth signing secret".into()))?;
            mac.update(encoded.as_bytes());mac.finalize().into_bytes().to_vec()
        },
        ("private_key_jwt","EdDSA")=>{
            let der=zeroize::Zeroizing::new(data_encoding::BASE64URL_NOPAD.decode(secret.as_bytes()).map_err(|_|(400,"Invalid OAuth private key".into()))?);
            let key=ring::signature::Ed25519KeyPair::from_pkcs8_maybe_unchecked(&der).map_err(|_|(400,"Invalid OAuth private key".into()))?;
            key.sign(encoded.as_bytes()).as_ref().to_vec()
        },
        ("private_key_jwt","ES256")=>{
            let der=zeroize::Zeroizing::new(data_encoding::BASE64URL_NOPAD.decode(secret.as_bytes()).map_err(|_|(400,"Invalid OAuth private key".into()))?);
            let rng=ring::rand::SystemRandom::new();
            let key=ring::signature::EcdsaKeyPair::from_pkcs8(&ring::signature::ECDSA_P256_SHA256_FIXED_SIGNING,&der,&rng).map_err(|_|(400,"Invalid OAuth private key".into()))?;
            key.sign(&rng,encoded.as_bytes()).map_err(|_|(503,"OAuth signing failed".into()))?.as_ref().to_vec()
        },
        ("private_key_jwt","RS256")=>{
            let der=zeroize::Zeroizing::new(data_encoding::BASE64URL_NOPAD.decode(secret.as_bytes()).map_err(|_|(400,"Invalid OAuth private key".into()))?);
            let key=ring::signature::RsaKeyPair::from_pkcs8(&der).map_err(|_|(400,"Invalid OAuth private key".into()))?;
            let mut signature=vec![0;key.public().modulus_len()];
            key.sign(&ring::signature::RSA_PKCS1_SHA256,&ring::rand::SystemRandom::new(),encoded.as_bytes(),&mut signature).map_err(|_|(503,"OAuth signing failed".into()))?;
            signature
        },
        _=>return Err((400,"Unsupported OAuth signing algorithm".into())),
    };
    Ok(zeroize::Zeroizing::new(format!("{}.{}",encoded.as_str(),data_encoding::BASE64URL_NOPAD.encode(&signature))))
}
struct TokenRequest {
    body: zeroize::Zeroizing<String>,
    headers: Vec<(String,String)>,
}
impl Drop for TokenRequest {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        for (_,value) in &mut self.headers { value.zeroize(); }
    }
}
fn token_request(values:&Value,token_endpoint:&str)->Result<TokenRequest> {
    let fields=values.as_object().ok_or((400,"Invalid OAuth form".into()))?;
    let method=values["client_auth_method"].as_str().unwrap_or("client_secret_post");
    if !matches!(method,"none"|"client_secret_post"|"client_secret_basic"|"client_secret_jwt"|"private_key_jwt"|"tls_client_auth"|"self_signed_tls_client_auth") {return Err((400,"Unsupported OAuth client authentication method".into()));}
    if method=="none" && values["client_secret"].as_str().is_some_and(|s|!s.is_empty()) {return Err((400,"Public OAuth client cannot send a secret".into()));}
    let mut serializer=url::form_urlencoded::Serializer::new(String::new());
    for (key,value) in fields {
        if matches!(key.as_str(),"client_auth_method"|"client_auth_algorithm"|"key_id"|"issuer"|"assertion_audience") || (method=="client_secret_basic" && matches!(key.as_str(),"client_id"|"client_secret"))
            || (matches!(method,"client_secret_jwt"|"private_key_jwt"|"tls_client_auth"|"self_signed_tls_client_auth") && key=="client_secret") {continue;}
        if let Some(value)=value.as_str().filter(|s|!s.is_empty()) {serializer.append_pair(key,value);}
    }
    if matches!(method,"client_secret_jwt"|"private_key_jwt") {
        let assertion=client_assertion(values,token_endpoint)?;
        serializer.append_pair("client_assertion_type","urn:ietf:params:oauth:client-assertion-type:jwt-bearer");
        serializer.append_pair("client_assertion",&assertion);
    }
    let mut headers=vec![("Content-Type".into(),"application/x-www-form-urlencoded".into()),("Accept".into(),"application/json".into())];
    if method=="client_secret_basic" {
        let client=values["client_id"].as_str().filter(|s|!s.is_empty()).ok_or((400,"OAuth client is unavailable".into()))?;
        let secret=values["client_secret"].as_str().filter(|s|!s.is_empty()).ok_or((409,"OAuth client credential is unavailable".into()))?;
        let encode=|v:&str| {let mut serializer=url::form_urlencoded::Serializer::new(String::new());serializer.append_pair("",v);zeroize::Zeroizing::new(serializer.finish())};
        let client=encode(client);let secret=encode(secret);
        let combined=zeroize::Zeroizing::new(format!("{}:{}",&client[1..],&secret[1..]));
        let basic=zeroize::Zeroizing::new(data_encoding::BASE64.encode(combined.as_bytes()));
        headers.push(("Authorization".into(),format!("Basic {}",basic.as_str())));
    }
    Ok(TokenRequest{body:zeroize::Zeroizing::new(serializer.finish()),headers})
}
fn form(url: &str, values: &Value) -> Result<Value> {
    endpoint(url)?;
    let request=token_request(values,url)?;
    let reply = native_io::http(url,"POST",&request.headers,request.body.as_bytes().to_vec(),
        &AtomicBool::new(false), Duration::from_secs(25), 1024*1024)
        .map_err(|_| (502,"OAuth token exchange failed".into()))?;
    let bytes = zeroize::Zeroizing::new(reply.bytes);
    if !(200..300).contains(&reply.status) { return Err((502,"OAuth token exchange failed".into())); }
    serde_json::from_slice(&bytes).map_err(|_| (502,"OAuth token response invalid".into()))
}
fn mutual_tls_token_endpoint(issuer:&Url,metadata:&Value,default:&str)->Result<String> {
    if !trusted(issuer) {return Err((403,"OAuth issuer is not trusted".into()));}
    let raw=metadata["mtls_endpoint_aliases"]["token_endpoint"].as_str().unwrap_or(default);
    let token=endpoint(raw)?;
    if !trusted(&token) {return Err((403,"OAuth mutual TLS token endpoint is not trusted".into()));}
    Ok(token.to_string())
}
fn mutual_tls_method(config:&Value)->bool {
    matches!(config["oauth"]["client_auth_method"].as_str(),Some("tls_client_auth"|"self_signed_tls_client_auth"))
}
fn form_mutual_tls(db:&Database,name:&str,config:&Value,url:&str,values:&Value)->Result<Value> {
    let target=endpoint(url)?;let issuer=endpoint(config["oauth_binding"]["issuer"].as_str().or_else(||values["issuer"].as_str()).unwrap_or(""))?;
    if !trusted(&issuer) || !trusted(&target) {return Err((403,"OAuth mutual TLS token endpoint is not trusted".into()));}
    let identity=crate::ui_mcp_mtls::identity(db,name,config)?;
    let request=token_request(values,url)?;
    let reply=crate::native_io::http_mutual_tls(url,"POST",&request.headers,request.body.as_bytes().to_vec(),
        &AtomicBool::new(false),Duration::from_secs(25),1024*1024,identity.as_bytes(),url)
        .map_err(|_|(502,"OAuth token exchange failed".into()))?;
    let bytes=zeroize::Zeroizing::new(reply.bytes);
    if !(200..300).contains(&reply.status) {return Err((502,"OAuth token exchange failed".into()));}
    serde_json::from_slice(&bytes).map_err(|_|(502,"OAuth token response invalid".into()))
}
// Registration creates a remote client, so an ambiguous result must never be
// retried automatically. Public, client_secret_post and client_secret_basic credentials are supported.
fn register(url:&str, values:&Value)->Result<Value> {
    endpoint(url)?;
    let body=zeroize::Zeroizing::new(serde_json::to_vec(values).map_err(|_|(400,"Invalid OAuth registration".into()))?);
    let reply=native_io::http(url,"POST",&[("Content-Type".into(),"application/json".into()),("Accept".into(),"application/json".into())],body.to_vec(),
        &AtomicBool::new(false),Duration::from_secs(25),128*1024).map_err(|_|(502,"OAuth client registration could not be confirmed".into()))?;
    let bytes=zeroize::Zeroizing::new(reply.bytes);
    if reply.status!=201 {return Err((502,"OAuth client registration could not be confirmed".into()));}
    serde_json::from_slice(&bytes).map_err(|_|(502,"OAuth client registration response invalid".into()))
}
fn register_client(db:&Database,name:&str,config:&Value,resource:&Url,callback:&Url,issuer:&str,registration_endpoint:&Url,method:&str,algorithm:&str,send:&dyn Fn(&str,&Value)->Result<Value>)->Result<()> {
    if !matches!(method,"none"|"client_secret_post"|"client_secret_basic"|"client_secret_jwt"|"private_key_jwt") || (method=="private_key_jwt" && !matches!(algorithm,"EdDSA"|"ES256")) || (method=="client_secret_jwt" && algorithm!="HS256") {return Err((400,"Unsupported OAuth client authentication method".into()));}
    {
        let mut connection=db.lock().map_err(ui_store::db_error)?;
        let tx=connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(ui_store::db_error)?;
        let current=ui_store::get(&tx,KIND,name).map_err(ui_store::db_error)?.ok_or((404,"MCP server not found".into()))?;
        if current["revision"]!=config["revision"] {return Err((409,"MCP configuration changed before registration".into()));}
        if method!="none" && ui_credentials::managed(&tx,&format!("mcp:{name}:oauth-client"))? {
            return Err((409,"MCP OAuth client credential already exists".into()));
        }
        if ui_store::get(&tx,REGISTRATION,name).map_err(ui_store::db_error)?.is_some() {
            return Err((409,"A previous OAuth registration is unresolved; review the provider registration before retrying".into()));
        }
        ui_store::put(&tx,REGISTRATION,name,json!({"status":"uncertain","resource":resource.as_str(),"issuer":issuer,"config_revision":config["revision"]})).map_err(ui_store::db_error)?;
        tx.commit().map_err(ui_store::db_error)?;
    }
    let mut registration=json!({"client_name":"Kageha","redirect_uris":[callback.as_str()],"grant_types":["authorization_code","refresh_token"],"response_types":["code"],"token_endpoint_auth_method":method});
    let mut private_key=None;let mut private_kid=None;
    if method=="client_secret_jwt" {registration["token_endpoint_auth_signing_alg"]=json!("HS256");}
    if method=="private_key_jwt" {
        use ring::signature::KeyPair;
        let rng=ring::rand::SystemRandom::new();let kid=nonce()?;
        let (der,jwk)=if algorithm=="EdDSA" {
            let der=ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).map_err(|_|(503,"OAuth key generation failed".into()))?;
            let key=ring::signature::Ed25519KeyPair::from_pkcs8(der.as_ref()).map_err(|_|(503,"OAuth key generation failed".into()))?;
            let jwk=json!({"kty":"OKP","crv":"Ed25519","use":"sig","alg":"EdDSA","kid":kid,"x":data_encoding::BASE64URL_NOPAD.encode(key.public_key().as_ref())});
            (zeroize::Zeroizing::new(data_encoding::BASE64URL_NOPAD.encode(der.as_ref())),jwk)
        } else {
            let der=ring::signature::EcdsaKeyPair::generate_pkcs8(&ring::signature::ECDSA_P256_SHA256_FIXED_SIGNING,&rng).map_err(|_|(503,"OAuth key generation failed".into()))?;
            let key=ring::signature::EcdsaKeyPair::from_pkcs8(&ring::signature::ECDSA_P256_SHA256_FIXED_SIGNING,der.as_ref(),&rng).map_err(|_|(503,"OAuth key generation failed".into()))?;
            let public=key.public_key().as_ref();
            if public.len()!=65 || public[0]!=4 {return Err((503,"OAuth public key generation failed".into()));}
            let jwk=json!({"kty":"EC","crv":"P-256","use":"sig","alg":"ES256","kid":kid,"x":data_encoding::BASE64URL_NOPAD.encode(&public[1..33]),"y":data_encoding::BASE64URL_NOPAD.encode(&public[33..])});
            (zeroize::Zeroizing::new(data_encoding::BASE64URL_NOPAD.encode(der.as_ref())),jwk)
        };
        registration["token_endpoint_auth_signing_alg"]=json!(algorithm);registration["jwks"]=json!({"keys":[jwk]});
        private_key=Some(der);private_kid=Some(kid);
    }
    let response=ui_credentials::SecretJson::new(send(registration_endpoint.as_str(),&registration)?);
    let client=response["client_id"].as_str().filter(|s|!s.is_empty() && s.len()<=1024 && !s.chars().any(char::is_control))
        .ok_or((502,"OAuth client registration response invalid".into()))?;
    if response["token_endpoint_auth_method"]!=method
        || (!response["token_endpoint_auth_signing_alg"].is_null() && response["token_endpoint_auth_signing_alg"]!=registration["token_endpoint_auth_signing_alg"])
        || response["redirect_uris"].as_array().is_none_or(|uris|uris.len()!=1 || uris[0]!=callback.as_str()) {
        return Err((502,"OAuth client registration returned unsupported credentials or redirects".into()));
    }
    let (secret,expires)=if method=="private_key_jwt" {
        if !response["client_secret"].is_null() || (!response["jwks"].is_null() && response["jwks"]!=registration["jwks"]) {return Err((502,"OAuth client registration returned unexpected credentials".into()));}
        (private_key.as_deref().map(|v|v.as_str()),0)
    } else if method!="none" {
        let secret=response["client_secret"].as_str().filter(|s|!s.is_empty() && s.len()<=4096 && !s.chars().any(char::is_control))
            .ok_or((502,"OAuth client registration returned invalid credentials".into()))?;
        let expires=response["client_secret_expires_at"].as_u64().ok_or((502,"OAuth client registration returned invalid credential expiry".into()))?;
        if expires!=0 && expires<=ui_store::now() as u64 {return Err((502,"OAuth client registration returned expired credentials".into()));}
        if method=="client_secret_jwt" {valid_assertion_credential(method,"HS256",secret)?;}
        (Some(secret),expires)
    } else {
        if !response["client_secret"].is_null() {return Err((502,"OAuth client registration returned unexpected credentials".into()));}
        (None,0)
    };
    let mut connection=db.lock().map_err(ui_store::db_error)?;
    let tx=connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(ui_store::db_error)?;
    let mut current=ui_store::get(&tx,KIND,name).map_err(ui_store::db_error)?.ok_or((409,"MCP configuration changed during registration".into()))?;
    let receipt=ui_store::get(&tx,REGISTRATION,name).map_err(ui_store::db_error)?.ok_or((409,"OAuth registration receipt is unavailable".into()))?;
    if current["revision"]!=config["revision"] || receipt["status"]!="uncertain" || receipt["config_revision"]!=config["revision"] {
        return Err((409,"MCP configuration changed during registration".into()));
    }
    if let Some(secret)=secret {
        if ui_credentials::managed(&tx,&format!("mcp:{name}:oauth-client"))? {return Err((409,"MCP OAuth client credential changed during registration".into()));}
        ui_settings::save_secret(&tx,&format!("mcp:{name}:oauth-client"),secret)?;
        current["oauth"]["client_secret_required"]=json!(true);
        current["oauth"]["client_auth_method"]=json!(method);
        current["oauth"]["client_secret_expires_at"]=json!(expires);
        if matches!(method,"client_secret_jwt"|"private_key_jwt") {
            current["oauth"]["client_auth_algorithm"]=json!(algorithm);
            current["oauth"]["credential_issuer"]=json!(issuer);current["oauth"]["key_id"]=json!(private_kid);
        }
    }
    current["oauth"]["client_id"]=json!(client);
    current["oauth"]["registration_issuer"]=json!(issuer);
    current["oauth"]["registration_resource"]=json!(resource.as_str());
    ui_store::put(&tx,KIND,name,current).map_err(ui_store::db_error)?;
    ui_store::put(&tx,REGISTRATION,name,json!({"status":"registered","resource":resource.as_str(),"issuer":issuer,"client_id":client})).map_err(ui_store::db_error)?;
    tx.commit().map_err(ui_store::db_error)?;
    Ok(())
}
fn nonce() -> Result<String> {
    let mut bytes=[0u8;32];
    std::fs::File::open("/dev/urandom").and_then(|mut f|f.read_exact(&mut bytes)).map_err(ui_store::db_error)?;
    Ok(data_encoding::BASE64URL_NOPAD.encode(&bytes))
}

/// Starts a bounded loopback OAuth handoff; no tokens or verifier leave the host.
pub(crate) fn start(db: &Database, name: &str) -> Result<Value> {
    let config=row(db,name)?;
    let exchange:Exchange=if mutual_tls_method(&config) {
        let db=db.clone();let name=name.to_owned();
        Arc::new(move |url,values|form_mutual_tls(&db,&name,&config,url,values))
    } else {Arc::new(form)};
    start_with(db,name,Arc::new(get_json),exchange,Arc::new(register),Arc::new(probe),std::env::var("KAGEHA_MCP_OAUTH_CLIENT_METADATA_URL").ok(),"http://127.0.0.1:12799/callback")
}
type Fetch = Arc<dyn Fn(&str)->Result<Value> + Send + Sync>;
type Exchange = Arc<dyn Fn(&str,&Value)->Result<Value> + Send + Sync>;
type Probe = Arc<dyn Fn(&Url)->Result<Option<String>> + Send + Sync>;
fn start_with(db: &Database, name: &str, fetch: Fetch, exchange: Exchange, registration: Exchange, probe:Probe, client_metadata_url:Option<String>, default_callback:&str) -> Result<Value> {
    let mut config = row(db,name)?;
    if !matches!(config["transport"].as_str(), Some("http"|"streamable_http")) {return Err((400,"MCP OAuth requires an HTTP server".into()));}
    let implicit_dynamic=config["oauth"].is_null();
    if implicit_dynamic {config["oauth"]=public_declaration(&json!({"dynamic_registration":true,"callback_url":default_callback}))?;}
    let resource=endpoint(config["url"].as_str().unwrap_or(""))?;
    let callback=redirect(config["oauth"]["callback_url"].as_str().unwrap_or(""))?;
    if !trusted(&resource) {return Err((403,"MCP OAuth resource origin is not trusted by this host".into()));}
    let challenged=probe(&resource)?;
    let used_challenge=challenged.is_some();
    let metadata=match challenged {
        Some(advertised)=>{let metadata=endpoint(&advertised)?;if metadata.origin()!=resource.origin(){return Err((403,"OAuth resource metadata origin does not match the server".into()));}metadata.to_string()},
        None=>metadata_url(&resource,"oauth-protected-resource")?,
    };
    let protected = fetch(&metadata)?;
    let auth_server = protected["authorization_servers"].as_array().filter(|v|v.len()==1)
        .and_then(|v|v[0].as_str()).ok_or((400,"OAuth resource must identify one authorization server".into()))?;
    let issuer_url = endpoint(auth_server)?;
    if !trusted(&issuer_url) {return Err((403,"MCP OAuth issuer origin is not trusted by this host".into()));}
    let issuer = fetch(&metadata_url(&issuer_url,"oauth-authorization-server")?)?;
    let (authority, auth, mut token) = validate_metadata(&resource,&protected,&issuer,used_challenge)?;
    if matches!(config["oauth"]["client_auth_method"].as_str(),Some("client_secret_jwt"|"private_key_jwt")) {
        let method=config["oauth"]["client_auth_method"].as_str().unwrap();
        let algorithm=config["oauth"]["client_auth_algorithm"].as_str().unwrap_or("");
        if config["oauth"]["credential_issuer"]!=authority
            || issuer["token_endpoint_auth_methods_supported"].as_array().is_none_or(|values|!values.iter().any(|v|v==method))
            || issuer["token_endpoint_auth_signing_alg_values_supported"].as_array().is_none_or(|values|!values.iter().any(|v|v==algorithm)) {
            return Err((403,"OAuth assertion authentication does not match issuer metadata".into()));
        }
    }
    if mutual_tls_method(&config) {
        let method=config["oauth"]["client_auth_method"].as_str().unwrap();
        if issuer["token_endpoint_auth_methods_supported"].as_array().is_none_or(|values|!values.iter().any(|v|v==method)) {
            return Err((403,"OAuth server does not advertise the configured mutual TLS method".into()));
        }
        token=mutual_tls_token_endpoint(&issuer_url,&issuer,&token)?;
        let _=crate::ui_mcp_mtls::identity(db,name,&config)?;
    }
    let listener = TcpListener::bind((callback.host_str().unwrap(),callback.port().unwrap()))
        .map_err(|_| (409,"OAuth callback port is unavailable".into()))?;
    let server = Server::from_listener(listener,None).map_err(|_|(503,"OAuth callback listener unavailable".into()))?;
    if config["oauth"]["dynamic_registration"]==true && config["oauth"]["client_id"].is_null()
        && issuer["client_id_metadata_document_supported"]==true {
        if let Some(raw)=client_metadata_url.as_deref().filter(|v|!v.is_empty()) {
            let document_url=endpoint(raw)?;
            if !trusted(&document_url) {return Err((403,"OAuth client metadata origin is not trusted by this host".into()));}
            validate_client_document(&fetch(document_url.as_str())?,&document_url,&callback)?;
            let mut connection=db.lock().map_err(ui_store::db_error)?;
            let tx=connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(ui_store::db_error)?;
            let mut current=ui_store::get(&tx,KIND,name).map_err(ui_store::db_error)?.ok_or((404,"MCP server not found".into()))?;
            if current["revision"]!=config["revision"] || (!implicit_dynamic && current["oauth"]!=config["oauth"]) || (implicit_dynamic && !current["oauth"].is_null()) {
                return Err((409,"MCP configuration changed before authorization".into()));
            }
            current["oauth"]=config["oauth"].clone();current["oauth"]["dynamic_registration"]=json!(false);
            current["oauth"]["client_metadata_document"]=json!(true);current["oauth"]["client_id"]=json!(document_url.as_str());
            current["oauth"]["registration_issuer"]=json!(authority);current["oauth"]["registration_resource"]=json!(resource.as_str());
            current["enabled"]=json!(false);current["connected"]=json!(false);current["tools"]=json!([]);
            ui_store::put(&tx,KIND,name,current).map_err(ui_store::db_error)?;tx.commit().map_err(ui_store::db_error)?;
            drop(connection);config=row(db,name)?;
        }
    }
    if config["oauth"]["client_metadata_document"]==true {
        let document_url=endpoint(config["oauth"]["client_id"].as_str().unwrap_or(""))?;
        if !trusted(&document_url) || issuer["client_id_metadata_document_supported"]!=true
            || config["oauth"]["registration_issuer"]!=authority || config["oauth"]["registration_resource"]!=resource.as_str() {
            return Err((403,"OAuth client metadata document binding changed".into()));
        }
        validate_client_document(&fetch(document_url.as_str())?,&document_url,&callback)?;
    }
    if config["oauth"]["dynamic_registration"]==true {
        if config["oauth"]["client_id"].is_null() {
            let registration_endpoint=endpoint(issuer["registration_endpoint"].as_str().unwrap_or(""))?;
            if registration_endpoint.origin()!=issuer_url.origin() {return Err((403,"OAuth registration endpoint does not belong to the issuer".into()));}
            if implicit_dynamic {
                let mut connection=db.lock().map_err(ui_store::db_error)?;
                let tx=connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(ui_store::db_error)?;
                let mut current=ui_store::get(&tx,KIND,name).map_err(ui_store::db_error)?.ok_or((404,"MCP server not found".into()))?;
                if current["revision"]!=config["revision"] || !current["oauth"].is_null() {return Err((409,"MCP configuration changed before authorization".into()));}
                current["oauth"]=config["oauth"].clone();current["enabled"]=json!(false);current["connected"]=json!(false);current["tools"]=json!([]);
                ui_store::put(&tx,KIND,name,current).map_err(ui_store::db_error)?;tx.commit().map_err(ui_store::db_error)?;
                drop(connection);config=row(db,name)?;
            }
            let method=match issuer.get("token_endpoint_auth_methods_supported").and_then(Value::as_array) {
                None=>"client_secret_basic",
                Some(methods) if methods.iter().any(|m|m=="none")=>"none",
                Some(methods) if methods.iter().any(|m|m=="client_secret_post")=>"client_secret_post",
                Some(methods) if methods.iter().any(|m|m=="client_secret_basic")=>"client_secret_basic",
                Some(methods) if methods.iter().any(|m|m=="client_secret_jwt") && issuer["token_endpoint_auth_signing_alg_values_supported"].as_array().is_some_and(|v|v.iter().any(|v|v=="HS256"))=>"client_secret_jwt",
                Some(methods) if methods.iter().any(|m|m=="private_key_jwt") && issuer["token_endpoint_auth_signing_alg_values_supported"].as_array().is_some_and(|v|v.iter().any(|v|v=="EdDSA" || v=="ES256"))=>"private_key_jwt",
                _=>return Err((400,"OAuth server requires an unsupported client authentication method".into())),
            };
            let algorithm=if method=="client_secret_jwt" {"HS256"} else if method=="private_key_jwt" {
                if issuer["token_endpoint_auth_signing_alg_values_supported"].as_array().is_some_and(|v|v.iter().any(|v|v=="EdDSA")) {"EdDSA"} else {"ES256"}
            } else {""};
            register_client(db,name,&config,&resource,&callback,&authority,&registration_endpoint,method,algorithm,registration.as_ref())?;
            config=row(db,name)?;
        }
        let receipt={let connection=db.lock().map_err(ui_store::db_error)?;ui_store::get(&connection,REGISTRATION,name).map_err(ui_store::db_error)?};
        if config["oauth"]["registration_issuer"]!=authority || config["oauth"]["registration_resource"]!=resource.as_str()
            || receipt.as_ref().is_none_or(|r|r["status"]!="registered" || r["resource"]!=resource.as_str() || r["issuer"]!=authority || r["client_id"]!=config["oauth"]["client_id"]) {
            return Err((403,"Dynamic OAuth registration does not match this resource and issuer".into()));
        }
    }
    let (_,_,client)=settings(&config)?;
    let issuer=authority;
    let state=nonce()?; let verifier=nonce()?;
    let challenge=data_encoding::BASE64URL_NOPAD.encode(&Sha256::digest(verifier.as_bytes()));
    let id=grant(name);
    let generation={let connection=db.lock().map_err(ui_store::db_error)?;ui_credentials::oauth_generation(&connection,&id)?};
    let pending = json!({"state":state,"verifier":verifier,"expires":ui_store::now()+600.0,"claimed":false,
        "resource":resource.to_string(),"callback":callback.to_string(),"client_id":client,"issuer":issuer,"token_endpoint":token,
        "config_revision":config["revision"],"generation":generation});
    {let connection=db.lock().map_err(ui_store::db_error)?;ui_credentials::pending_put(&connection,&id,pending.clone())?;}
    let mut authorization=Url::parse(&auth).map_err(|_|(400,"Invalid OAuth authorization endpoint".into()))?;
    authorization.query_pairs_mut().append_pair("response_type","code").append_pair("client_id",&client)
        .append_pair("redirect_uri",callback.as_str()).append_pair("resource",resource.as_str()).append_pair("state",&state)
        .append_pair("code_challenge",&challenge).append_pair("code_challenge_method","S256");
    if let Some(scope)=config["oauth"]["scope"].as_str().filter(|s|!s.is_empty() && s.len()<=1024 && !s.chars().any(char::is_control)) {
        authorization.query_pairs_mut().append_pair("scope",scope);
    }
    let db=db.clone(); let name=name.to_owned(); let callback_path=callback.path().to_owned(); let expected_state=state.clone();
    std::thread::Builder::new().name("mcp-oauth-callback".into()).spawn(move || {
        let deadline=std::time::Instant::now()+Duration::from_secs(600);
        while std::time::Instant::now()<deadline {
            match server.recv_timeout(Duration::from_secs(1)) {
                Ok(Some(request)) => {
                    if request.method()!=&tiny_http::Method::Get || request.url().split('?').next()!=Some(callback_path.as_str()) {
                        let _=request.respond(Response::from_string("Not found").with_status_code(404)); continue;
                    }
                    let result=finish_with(&db,&name,request.url(),exchange.as_ref());
                    let (status,message)=if result.is_ok(){(200,"Connection authorized. You can close this window.")}else{(400,"Authorization could not be completed. Return to Kageha and try again.")};
                    let _=request.respond(Response::from_string(message).with_status_code(status));
                    if result.is_ok() { break; }
                }
                Ok(None)=>{
                    let active = db.lock().ok().and_then(|connection| ui_credentials::pending_get(&connection,&grant(&name)).ok().flatten())
                        .is_some_and(|value| value["state"] == expected_state && value["expires"].as_f64().is_some_and(|v|v>=ui_store::now()));
                    if !active { break; }
                }, Err(_)=>break,
            }
        }
    }).map_err(ui_store::db_error)?;
    Ok(json!({"authorization_url":authorization.as_str(),"status":"authorization_required"}))
}

fn finish_with(db: &Database, name: &str, url: &str, exchange:&dyn Fn(&str,&Value)->Result<Value>) -> Result<()> {
    let state=crate::webui::form_decode(&crate::webui::query(url,"state")).unwrap_or_default();
    let code=crate::webui::form_decode(&crate::webui::query(url,"code")).unwrap_or_default();
    let response_issuer=crate::webui::form_decode(&crate::webui::query(url,"iss")).unwrap_or_default();
    let id=grant(name);
    let pending={let mut connection=db.lock().map_err(ui_store::db_error)?;let tx=connection.transaction().map_err(ui_store::db_error)?;
        let mut pending=ui_credentials::pending_get(&tx,&id)?.ok_or((400,"OAuth attempt unavailable".into()))?;
        if state.is_empty() || state.len()>1024 || code.is_empty() || code.len()>4096 || code.chars().any(char::is_control) || pending["state"]!=state || pending["claimed"]==true
            || pending["expires"].as_f64().is_none_or(|v|v<ui_store::now()) {return Err((400,"OAuth attempt invalid".into()));}
        if !response_issuer.is_empty() && response_issuer != pending["issuer"].as_str().unwrap_or("") {
            return Err((400,"OAuth response issuer mismatch".into()));
        }
        pending["claimed"]=json!(true);ui_credentials::pending_put(&tx,&id,pending.clone())?;tx.commit().map_err(ui_store::db_error)?;pending};
    let config=row(db,name)?; let (resource,callback,client)=settings(&config)?;
    if matches!(config["oauth"]["client_auth_method"].as_str(),Some("client_secret_jwt"|"private_key_jwt")) && config["oauth"]["credential_issuer"]!=pending["issuer"] {
        return Err((403,"OAuth assertion issuer binding changed".into()));
    }
    if pending["config_revision"]!=config["revision"] || pending["resource"]!=resource.as_str()
        || pending["callback"]!=callback.as_str() || pending["client_id"]!=client {return Err((409,"MCP configuration changed during authorization".into()));}
    let secret=zeroize::Zeroizing::new(if mutual_tls_method(&config) {String::new()} else {let connection=db.lock().map_err(ui_store::db_error)?;ui_settings::secret(&connection,&format!("mcp:{name}:oauth-client"))?});
    if config["oauth"]["client_secret_required"] == true && (secret.is_empty() || config["oauth"]["client_secret_expires_at"].as_u64().is_some_and(|v|v!=0 && v<=ui_store::now() as u64)) {return Err((409,"MCP OAuth client credential is unavailable or expired".into()));}
    if !trusted(&resource) || !trusted(&endpoint(pending["issuer"].as_str().unwrap_or(""))?) {
        return Err((403,"MCP OAuth trust changed during authorization".into()));
    }
    let fields=ui_credentials::SecretJson::new(json!({
        "issuer":pending["issuer"],"grant_type":"authorization_code","code":code,"client_id":client,"client_secret":secret.as_str(),"client_auth_method":config["oauth"]["client_auth_method"],"client_auth_algorithm":config["oauth"]["client_auth_algorithm"],"key_id":config["oauth"]["key_id"],"assertion_audience":config["oauth"]["assertion_audience"],
        "redirect_uri":callback.as_str(),"code_verifier":pending["verifier"],"resource":resource.as_str()}));
    let token=ui_credentials::SecretJson::new(exchange(pending["token_endpoint"].as_str().unwrap_or(""),&fields)?);
    let mut connection=db.lock().map_err(ui_store::db_error)?;let tx=connection.transaction().map_err(ui_store::db_error)?;
    let still_pending=ui_credentials::pending_get(&tx,&id)?.ok_or((409,"OAuth attempt was cancelled".into()))?;
    if still_pending["state"]!=state || still_pending["claimed"]!=true {return Err((409,"OAuth attempt was cancelled or replaced".into()));}
    let current=ui_store::get(&tx,KIND,name).map_err(ui_store::db_error)?.ok_or((409,"MCP configuration changed".into()))?;
    if current["revision"]!=pending["config_revision"] {return Err((409,"MCP configuration changed during authorization".into()));}
    ui_credentials::oauth_store(&tx,&id,&token,true,pending["generation"].as_u64().unwrap_or(0))?;
    let mut updated=current;
    updated["oauth_binding"]=json!({"resource":resource.as_str(),"client_id":client,"issuer":pending["issuer"],"token_endpoint":pending["token_endpoint"],"oauth":updated["oauth"],"authorization_id":ui_credentials::oauth_authority(&tx,&id)?});
    updated["connected"]=json!(false);
    ui_store::put(&tx,KIND,name,updated).map_err(ui_store::db_error)?;
    ui_credentials::pending_delete(&tx,&id)?;tx.commit().map_err(ui_store::db_error)?;
    Ok(())
}

/// Inject only the token granted for this exact configured resource and client.
pub(crate) fn authorize(db:&Database, name:&str, config:&mut Value) -> Result<()> {
    if mutual_tls_method(config) {
        let snapshot=config.clone();
        authorize_with(db,name,config,&|url,values|form_mutual_tls(db,name,&snapshot,url,values))
    } else {authorize_with(db,name,config,&form)}
}
fn authorize_with(db:&Database, name:&str, config:&mut Value, exchange:&dyn Fn(&str,&Value)->Result<Value>) -> Result<()> {
    if config["oauth"].is_null() { return Ok(()); }
    static LOCKS: OnceLock<Mutex<HashMap<String,Arc<Mutex<()>>>>> = OnceLock::new();
    let lock=LOCKS.get_or_init(Default::default).lock().map_err(ui_store::db_error)?.entry(name.into()).or_default().clone();
    let _guard=lock.lock().map_err(ui_store::db_error)?;
    let (resource,_,client)=settings(config)?; let id=grant(name); let binding=&config["oauth_binding"];
    if !trusted(&resource) || !client_document_trusted(config) || !trusted(&endpoint(binding["issuer"].as_str().unwrap_or(""))?) {
        return Err((403,"MCP OAuth origin is no longer trusted".into()));
    }
    let connection=db.lock().map_err(ui_store::db_error)?;
    let current=ui_store::get(&connection,KIND,name).map_err(ui_store::db_error)?.ok_or((404,"MCP server not found".into()))?;
    if ui_credentials::pending_get(&connection,&id)?.is_some_and(|v|v["expires"].as_f64().is_some_and(|expiry|expiry>=ui_store::now())) {
        return Err((409,"Finish or cancel the pending MCP authorization before using this connection".into()));
    }
    if current["url"] != config["url"] || current["oauth"] != config["oauth"] || current["oauth_binding"] != *binding {
        return Err((409,"MCP authorization configuration changed".into()));
    }
    if matches!(config["oauth"]["client_auth_method"].as_str(),Some("client_secret_jwt"|"private_key_jwt")) && config["oauth"]["credential_issuer"]!=binding["issuer"] {
        return Err((403,"OAuth assertion issuer binding changed".into()));
    }
    if binding["resource"]!=resource.as_str() || binding["client_id"]!=client || binding["oauth"]!=config["oauth"]
        || binding["authorization_id"].as_str().is_none_or(|value| ui_credentials::oauth_authority(&connection,&id).ok().flatten().as_deref()!=Some(value)) {
        return Err((403,"Connect this MCP server before using it".into()));
    }
    let metadata=ui_credentials::oauth_metadata(&connection,&id)?.ok_or((409,"MCP authorization is unavailable".into()))?;
    if metadata.state != kageha_contracts::credentials::CredentialState::Active { return Err((409,"MCP authorization needs reconnection".into())); }
    let needs_refresh=metadata.expires_at_ms.is_some_and(|expiry| expiry <= kageha_credentials::crypto::now_ms()+30000);
    let token = if needs_refresh {
        if !metadata.refreshable { return Err((409,"MCP authorization expired; reconnect it".into())); }
        let secret=zeroize::Zeroizing::new(if mutual_tls_method(config) {String::new()} else {ui_settings::secret(&connection,&format!("mcp:{name}:oauth-client"))?});
        if config["oauth"]["client_secret_required"] == true && (secret.is_empty() || config["oauth"]["client_secret_expires_at"].as_u64().is_some_and(|v|v!=0 && v<=ui_store::now() as u64)) {return Err((409,"MCP OAuth client credential is unavailable or expired".into()));}
        let token_endpoint=binding["token_endpoint"].as_str().ok_or((409,"MCP authorization metadata unavailable".into()))?.to_owned();
        let issuer=endpoint(binding["issuer"].as_str().unwrap_or(""))?;
        let target=endpoint(&token_endpoint)?;
        if (mutual_tls_method(config) && !trusted(&target)) || (!mutual_tls_method(config) && target.origin()!=issuer.origin()) {return Err((403,"OAuth issuer binding changed".into()));}
        let refresh=ui_credentials::oauth_refresh_begin(&connection,&id)?;
        drop(connection);
        let fields=ui_credentials::SecretJson::new(json!({"issuer":binding["issuer"],"grant_type":"refresh_token","refresh_token":refresh.refresh_token(),
            "client_id":client,"client_secret":secret.as_str(),"client_auth_method":config["oauth"]["client_auth_method"],"client_auth_algorithm":config["oauth"]["client_auth_algorithm"],"key_id":config["oauth"]["key_id"],"assertion_audience":config["oauth"]["assertion_audience"],"resource":resource.as_str()}));
        let received=exchange(&token_endpoint,&fields);
        let connection=db.lock().map_err(ui_store::db_error)?;
        let token=match received {Ok(value)=>ui_credentials::SecretJson::new(value),Err(error)=>{let _=ui_credentials::oauth_refresh_failed(&connection,&refresh);return Err(error);}};
        if let Err(error)=ui_credentials::oauth_refresh_finish(&connection,&refresh,&token) {let _=ui_credentials::oauth_refresh_failed(&connection,&refresh);return Err(error);}
        let current=ui_store::get(&connection,KIND,name).map_err(ui_store::db_error)?.ok_or((404,"MCP server not found".into()))?;
        if ui_credentials::pending_get(&connection,&id)?.is_some_and(|v|v["expires"].as_f64().is_some_and(|expiry|expiry>=ui_store::now())) {
            return Err((409,"Finish or cancel the pending MCP authorization before using this connection".into()));
        }
        if current["url"] != config["url"] || current["oauth"] != config["oauth"] || current["oauth_binding"] != *binding {
            return Err((409,"MCP authorization configuration changed during refresh".into()));
        }
        ui_credentials::oauth_access_with_revision(&connection,&id)?.1
    } else {
        ui_credentials::oauth_access_with_revision(&connection,&id)?.1
    };
    if !config["headers"].is_object() {config["headers"]=json!({});}
    let headers=config["headers"].as_object_mut().unwrap();
    if headers.keys().any(|key|key.eq_ignore_ascii_case("authorization")) {return Err((400,"Remove the conflicting MCP authorization header".into()));}
    headers.insert("Authorization".into(),json!(format!("Bearer {token}")));
    Ok(())
}

pub(crate) fn status(db:&Database,name:&str)->Result<Value> {
    let config=row(db,name)?;
    if config["oauth"].is_null() {
        if matches!(config["transport"].as_str(),Some("http"|"streamable_http")) {
            return Ok(json!({"status":"not_connected","pending":false,"enabled":config["enabled"]==true,"connected":config["connected"]==true,"registration":"required"}));
        }
        return Err((400,"MCP OAuth is not configured".into()));
    }
    if config["oauth"]["dynamic_registration"]==true && config["oauth"]["client_id"].is_null() {
        let connection=db.lock().map_err(ui_store::db_error)?;
        let registration=ui_store::get(&connection,REGISTRATION,name).map_err(ui_store::db_error)?;
        return Ok(json!({"status":"not_connected","pending":false,"enabled":false,"connected":false,"registration":if registration.is_some(){"uncertain"}else{"required"}}));
    }
    let (resource,_,client)=settings(&config)?;
    let id=grant(name);
    let connection=db.lock().map_err(ui_store::db_error)?;
    let pending=ui_credentials::pending_get(&connection,&id)?.is_some_and(|v|v["expires"].as_f64().is_some_and(|expiry|expiry>=ui_store::now()));
    let valid=trusted(&resource) && client_document_trusted(&config) && config["oauth_binding"]["issuer"].as_str().and_then(|v|endpoint(v).ok()).is_some_and(|issuer|trusted(&issuer))
        && config["oauth_binding"]["resource"]==resource.as_str() && config["oauth_binding"]["client_id"]==client && config["oauth_binding"]["oauth"]==config["oauth"]
        && config["oauth_binding"]["authorization_id"].as_str().is_some_and(|v|ui_credentials::oauth_authority(&connection,&id).ok().flatten().as_deref()==Some(v));
    let ready=valid && (!mutual_tls_method(&config) || crate::ui_mcp_mtls::identity_available(&connection,name,&config)) && ui_credentials::oauth_ready(&connection,&id)?;
    Ok(json!({"status":if ready {"ready"} else {"not_connected"},"pending":pending,"enabled":config["enabled"]==true,"connected":config["connected"]==true}))
}

pub(crate) fn cancel(db:&Database,name:&str)->Result<Value> {
    let _=row(db,name)?; let id=grant(name);
    let connection=db.lock().map_err(ui_store::db_error)?;
    if ui_credentials::pending_get(&connection,&id)?.is_some() {ui_credentials::pending_delete(&connection,&id)?;}
    Ok(json!({"ok":true}))
}

pub(crate) fn disconnect(db:&Database,name:&str)->Result<Value> {
    let _=row(db,name)?;
    let mut connection=db.lock().map_err(ui_store::db_error)?;
    let tx=connection.transaction().map_err(ui_store::db_error)?;
    let mut config=ui_store::get(&tx,KIND,name).map_err(ui_store::db_error)?.ok_or((404,"MCP server not found".into()))?;
    if config["oauth"].is_null() {return Err((400,"MCP OAuth is not configured".into()));}
    let id=grant(name);
    ui_credentials::delete(&tx,&format!("oauth:{id}:grant"))?;
    if ui_credentials::pending_get(&tx,&id)?.is_some() {ui_credentials::pending_delete(&tx,&id)?;}
    config["oauth_binding"]=Value::Null;config["enabled"]=json!(false);config["connected"]=json!(false);config["tools"]=json!([]);
    ui_store::put(&tx,KIND,name,config).map_err(ui_store::db_error)?;tx.commit().map_err(ui_store::db_error)?;
    Ok(json!({"ok":true}))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn dynamic_fixture(name:&str)->(Database,Value) {
        let connection=rusqlite::Connection::open_in_memory().unwrap();
        connection.execute_batch("CREATE TABLE native_chat_turns(sequence INTEGER PRIMARY KEY,chat_id TEXT,request_id TEXT,message TEXT,answer TEXT,status TEXT);").unwrap();
        crate::webui::initialize(&connection).unwrap();
        let oauth=public_declaration(&json!({"dynamic_registration":true})).unwrap();
        ui_store::put(&connection,KIND,name,json!({"name":name,"transport":"http","url":"https://api.githubcopilot.com/mcp","oauth":oauth,"enabled":false,"origin":"plugin","plugin":"fixture"})).unwrap();
        let db=crate::agent_host::DatabaseState::open(connection).unwrap();
        let config=row(&db,name).unwrap();(db,config)
    }
    #[test]
    fn dynamic_registration_is_bound_and_never_stores_provider_secrets() {
        let name="plugin-dynamic";let (db,config)=dynamic_fixture(name);
        assert_eq!(status(&db,name).unwrap()["registration"],"required");
        let resource=endpoint("https://api.githubcopilot.com/mcp").unwrap();let callback=redirect("http://127.0.0.1:12799/callback").unwrap();
        let registration_endpoint=endpoint("https://github.com/register").unwrap();
        register_client(&db,name,&config,&resource,&callback,"https://github.com/login/oauth",&registration_endpoint,"none","",&|url,fields| {
            assert_eq!(url,"https://github.com/register");assert_eq!(fields["token_endpoint_auth_method"],"none");
            assert_eq!(fields["redirect_uris"],json!(["http://127.0.0.1:12799/callback"]));
            Ok(json!({"client_id":"registered-client","token_endpoint_auth_method":"none","redirect_uris":["http://127.0.0.1:12799/callback"],"registration_access_token":"secret-canary"}))
        }).unwrap();
        let updated=row(&db,name).unwrap();assert_eq!(updated["oauth"]["client_id"],"registered-client");
        assert_eq!(updated["oauth"]["registration_resource"],resource.as_str());assert!(!updated.to_string().contains("secret-canary"));
        let connection=db.lock().unwrap();let receipt=ui_store::get(&connection,REGISTRATION,name).unwrap().unwrap();assert_eq!(receipt["status"],"registered");
        assert!(!receipt.to_string().contains("secret-canary"));
    }
    #[test]
    fn dynamic_registration_continues_through_loopback_authorization() {
        use std::io::{Read,Write};
        let listener=TcpListener::bind(("127.0.0.1",0)).unwrap();let port=listener.local_addr().unwrap().port();drop(listener);
        let name="plugin-dynamic-flow";let (db,_)=dynamic_fixture(name);
        {let connection=db.lock().unwrap();let mut config=ui_store::get(&connection,KIND,name).unwrap().unwrap();
            config["oauth"]=Value::Null;ui_store::put(&connection,KIND,name,config).unwrap();}
        assert_eq!(status(&db,name).unwrap()["registration"],"required");
        let fetch:Fetch=Arc::new(|url|if url.contains("oauth-protected-resource") {assert_eq!(url,"https://api.githubcopilot.com/oauth-protected-resource/fixture");Ok(json!({"resource":"https://api.githubcopilot.com/mcp","authorization_servers":["https://github.com/login/oauth"]}))}
            else {Ok(json!({"issuer":"https://github.com/login/oauth","authorization_endpoint":"https://github.com/login/oauth/authorize","token_endpoint":"https://github.com/login/oauth/access_token","registration_endpoint":"https://github.com/register","token_endpoint_auth_methods_supported":["none"],"code_challenge_methods_supported":["S256"]}))});
        let callback=format!("http://127.0.0.1:{port}/callback");
        let registration:Exchange=Arc::new(move |url,fields| {assert_eq!(url,"https://github.com/register");assert_eq!(fields["redirect_uris"],json!([callback]));
            Ok(json!({"client_id":"dynamic-client","token_endpoint_auth_method":"none","redirect_uris":[callback]}))});
        let exchange:Exchange=Arc::new(|_,fields| {assert_eq!(fields["client_id"],"dynamic-client");assert_eq!(fields["client_secret"],"");Ok(json!({"access_token":"dynamic-private-token","expires_in":3600}))});
        let opened=start_with(&db,name,fetch,exchange,registration,Arc::new(|_|challenge_metadata(&["Bearer realm=\"fixture\", resource_metadata=\"https://api.githubcopilot.com/oauth-protected-resource/fixture\"".into()])),None,&format!("http://127.0.0.1:{port}/callback")).unwrap();
        let params:std::collections::HashMap<_,_>=Url::parse(opened["authorization_url"].as_str().unwrap()).unwrap().query_pairs().into_owned().collect();
        assert_eq!(params["client_id"],"dynamic-client");assert_eq!(params["code_challenge_method"],"S256");
        let mut socket=std::net::TcpStream::connect(("127.0.0.1",port)).unwrap();socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        write!(socket,"GET /callback?state={}&code=test-code HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n",params["state"]).unwrap();
        let mut response=String::new();socket.read_to_string(&mut response).unwrap();assert!(response.starts_with("HTTP/1.1 200"));
        assert_eq!(status(&db,name).unwrap()["status"],"ready");let mut configured=row(&db,name).unwrap();authorize(&db,name,&mut configured).unwrap();
        assert_eq!(configured["headers"]["Authorization"],"Bearer dynamic-private-token");assert!(!row(&db,name).unwrap().to_string().contains("dynamic-private-token"));
    }
    #[test]
    fn dynamic_registration_rejects_endpoint_outside_issuer_before_posting() {
        let name="plugin-dynamic-untrusted";let (db,_)=dynamic_fixture(name);
        let temp=TcpListener::bind(("127.0.0.1",0)).unwrap();let port=temp.local_addr().unwrap().port();drop(temp);
        {let connection=db.lock().unwrap();let mut config=ui_store::get(&connection,KIND,name).unwrap().unwrap();config["oauth"]["callback_url"]=json!(format!("http://127.0.0.1:{port}/callback"));ui_store::put(&connection,KIND,name,config).unwrap();}
        let fetch:Fetch=Arc::new(|url|if url.contains("oauth-protected-resource") {Ok(json!({"resource":"https://api.githubcopilot.com/mcp","authorization_servers":["https://github.com/login/oauth"]}))}
            else {Ok(json!({"issuer":"https://github.com/login/oauth","authorization_endpoint":"https://github.com/login/oauth/authorize","token_endpoint":"https://github.com/login/oauth/access_token","registration_endpoint":"https://attacker.test/register","code_challenge_methods_supported":["S256"]}))});
        assert_eq!(start_with(&db,name,fetch,Arc::new(|_,_|panic!("must not exchange")),Arc::new(|_,_|panic!("must not register")),Arc::new(|_|Ok(None)),None,"http://127.0.0.1:12799/callback").unwrap_err().0,403);
        assert_eq!(status(&db,name).unwrap()["registration"],"required");
    }
    #[test]
    fn hosted_client_metadata_document_authorizes_without_registration() {
        use std::io::{Read,Write};
        let listener=TcpListener::bind(("127.0.0.1",0)).unwrap();let port=listener.local_addr().unwrap().port();drop(listener);
        let name="plugin-cimd";let (db,_)=dynamic_fixture(name);
        let callback=format!("http://127.0.0.1:{port}/callback");
        {let connection=db.lock().unwrap();let mut config=ui_store::get(&connection,KIND,name).unwrap().unwrap();config["oauth"]["callback_url"]=json!(callback);ui_store::put(&connection,KIND,name,config).unwrap();}
        let doc_id="https://github.com/client-metadata.json";
        let doc_callback=callback.clone();
        let fetch:Fetch=Arc::new(move |url|if url.contains("oauth-protected-resource") {Ok(json!({"resource":"https://api.githubcopilot.com/mcp","authorization_servers":["https://github.com/login/oauth"]}))}
            else if url==doc_id {Ok(json!({"client_id":doc_id,"redirect_uris":[doc_callback],"grant_types":["authorization_code"],"response_types":["code"],"token_endpoint_auth_method":"none"}))}
            else {Ok(json!({"issuer":"https://github.com/login/oauth","authorization_endpoint":"https://github.com/login/oauth/authorize","token_endpoint":"https://github.com/login/oauth/access_token","client_id_metadata_document_supported":true,"code_challenge_methods_supported":["S256"]}))});
        let exchange:Exchange=Arc::new(|_,fields|{assert_eq!(fields["client_id"],"https://github.com/client-metadata.json");Ok(json!({"access_token":"cimd-token","expires_in":3600}))});
        let opened=start_with(&db,name,fetch,exchange,Arc::new(|_,_|panic!("DCR must not be used")),Arc::new(|_|Ok(None)),Some(doc_id.into()),"http://127.0.0.1:12799/callback").unwrap();
        let params:std::collections::HashMap<_,_>=Url::parse(opened["authorization_url"].as_str().unwrap()).unwrap().query_pairs().into_owned().collect();
        assert_eq!(params["client_id"],doc_id);
        let mut socket=std::net::TcpStream::connect(("127.0.0.1",port)).unwrap();socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        write!(socket,"GET /callback?state={}&code=cimd-code HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n",params["state"]).unwrap();
        let mut response=String::new();socket.read_to_string(&mut response).unwrap();assert!(response.starts_with("HTTP/1.1 200"));
        assert_eq!(status(&db,name).unwrap()["status"],"ready");assert_eq!(row(&db,name).unwrap()["oauth"]["client_metadata_document"],true);
        let bad=json!({"client_id":doc_id,"redirect_uris":["http://127.0.0.1:1/attacker"],"grant_types":["authorization_code"],"response_types":["code"],"token_endpoint_auth_method":"none"});
        assert!(validate_client_document(&bad,&endpoint(doc_id).unwrap(),&redirect(&callback).unwrap()).is_err());
    }
    #[test]
    fn assertion_real_oauth_and_mcp_end_to_end() {
        use std::io::BufRead;
        use std::os::unix::fs::PermissionsExt;
        use std::process::{Command,Stdio};
        struct Server(std::process::Child);
        impl Drop for Server {fn drop(&mut self){let _=self.0.kill();let _=self.0.wait();}}
        let root=tempfile::tempdir().unwrap();let dir=root.path();
        let run=|args:&[&str]| {let status=Command::new("openssl").current_dir(dir).args(args).stdout(Stdio::null()).stderr(Stdio::null()).status().unwrap();assert!(status.success(),"openssl {args:?}");};
        run(&["req","-x509","-newkey","rsa:2048","-nodes","-keyout","ca.key","-out","ca.crt","-subj","/CN=JWT Fixture CA","-days","1"]);
        std::fs::write(dir.join("server.ext"),"subjectAltName=IP:127.0.0.1\nextendedKeyUsage=serverAuth\n").unwrap();
        run(&["req","-newkey","rsa:2048","-nodes","-keyout","server.key","-out","server.csr","-subj","/CN=JWT Fixture"]);
        run(&["x509","-req","-in","server.csr","-CA","ca.crt","-CAkey","ca.key","-CAcreateserial","-out","server.crt","-days","1","-extfile","server.ext"]);
        run(&["genpkey","-algorithm","ED25519","-out","private-EdDSA.pem"]);
        run(&["pkcs8","-topk8","-nocrypt","-in","private-EdDSA.pem","-outform","DER","-out","private-EdDSA.pk8"]);
        run(&["pkey","-in","private-EdDSA.pem","-pubout","-out","public-EdDSA.pem"]);
        run(&["genpkey","-algorithm","ED25519","-out","private-next-EdDSA.pem"]);
        run(&["pkcs8","-topk8","-nocrypt","-in","private-next-EdDSA.pem","-outform","DER","-out","private-next-EdDSA.pk8"]);
        run(&["genpkey","-algorithm","EC","-pkeyopt","ec_paramgen_curve:P-256","-out","private-ES256.pem"]);
        run(&["pkcs8","-topk8","-nocrypt","-in","private-ES256.pem","-outform","DER","-out","private-ES256.pk8"]);
        run(&["pkey","-in","private-ES256.pem","-pubout","-out","public-ES256.pem"]);
        run(&["genpkey","-algorithm","EC","-pkeyopt","ec_paramgen_curve:P-256","-out","private-next-ES256.pem"]);
        run(&["pkcs8","-topk8","-nocrypt","-in","private-next-ES256.pem","-outform","DER","-out","private-next-ES256.pk8"]);
        run(&["genpkey","-algorithm","RSA","-pkeyopt","rsa_keygen_bits:2048","-out","private-RS256.pem"]);
        run(&["pkcs8","-topk8","-nocrypt","-in","private-RS256.pem","-outform","DER","-out","private-RS256.pk8"]);
        run(&["pkey","-in","private-RS256.pem","-pubout","-out","public-RS256.pem"]);
        run(&["genpkey","-algorithm","RSA","-pkeyopt","rsa_keygen_bits:2048","-out","private-next-RS256.pem"]);
        run(&["pkcs8","-topk8","-nocrypt","-in","private-next-RS256.pem","-outform","DER","-out","private-next-RS256.pk8"]);
        let key=dir.join("vault.key");std::fs::write(&key,[44u8;32]).unwrap();std::fs::set_permissions(&key,std::fs::Permissions::from_mode(0o600)).unwrap();
        for alg in ["HS256","EdDSA","ES256","RS256"] {
            let log=dir.join(format!("{alg}.log"));let script=std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/oauth-jwt-server.cjs");
            let audience=if alg=="ES256" {"issuer"}else{"token_endpoint"};
            let child=Command::new("node").arg(script).arg(dir).arg(&log).arg(alg).arg(audience).stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn().unwrap();
            let mut server=Server(child);let mut line=String::new();std::io::BufReader::new(server.0.stdout.take().unwrap()).read_line(&mut line).unwrap();
            let origin=format!("https://127.0.0.1:{}",line.trim());
            let output=Command::new(std::env::current_exe().unwrap()).args(["--ignored","--exact","ui_mcp_oauth::tests::assertion_real_child","--nocapture"])
                .env("KAGEHA_CA_BUNDLE",dir.join("ca.crt")).env("KAGEHA_CREDENTIAL_KEY_FILE",&key).env("KAGEHA_HOME",dir.join(format!("home-{alg}")))
                .env("KAGEHA_TEST_JWT_ORIGIN",&origin).env("KAGEHA_TEST_JWT_DIR",dir).env("KAGEHA_TEST_JWT_ALG",alg).env("KAGEHA_TEST_JWT_AUDIENCE",audience).output().unwrap();
            assert!(output.status.success(),"JWT child failed: {}\n{}",String::from_utf8_lossy(&output.stdout),String::from_utf8_lossy(&output.stderr));
            let entries:Vec<Value>=std::fs::read_to_string(&log).unwrap().lines().map(|line|serde_json::from_str(line).unwrap()).collect();
            assert!(entries.iter().filter(|v|v["path"]=="/token" && v["method"]=="POST").count()>=2,"authorization and refresh must both use assertions");
            assert!(entries.iter().filter(|v|v["path"]=="/mcp" && v["method"]=="POST").count()>=2);
        }
    }
    #[test]
    #[ignore = "spawned in isolated process with test TLS trust and credential vault"]
    fn assertion_real_child() {
        use std::io::{Read,Write};
        let origin=std::env::var("KAGEHA_TEST_JWT_ORIGIN").unwrap();let dir=std::path::PathBuf::from(std::env::var_os("KAGEHA_TEST_JWT_DIR").unwrap());
        let alg=std::env::var("KAGEHA_TEST_JWT_ALG").unwrap();let audience=std::env::var("KAGEHA_TEST_JWT_AUDIENCE").unwrap();let trust=test_trust_loopback(&format!("{origin}/")).unwrap();
        let state_dir=dir.join(format!("state-{alg}"));std::fs::create_dir_all(&state_dir).unwrap();
        let connection=rusqlite::Connection::open(state_dir.join("state.sqlite")).unwrap();
        connection.execute_batch("CREATE TABLE native_chat_turns(sequence INTEGER PRIMARY KEY,chat_id TEXT,request_id TEXT,message TEXT,answer TEXT,status TEXT);").unwrap();crate::webui::initialize(&connection).unwrap();
        let initial=ui_store::put(&connection,KIND,"fixture",json!({"name":"fixture","transport":"http","url":format!("{origin}/mcp"),"enabled":false,"connected":false,"tools":[]})).unwrap();
        let db=crate::agent_host::DatabaseState::open(connection).unwrap();
        let listener=TcpListener::bind(("127.0.0.1",0)).unwrap();let port=listener.local_addr().unwrap().port();drop(listener);
        let credential=if alg=="HS256" {"0123456789abcdef0123456789abcdef".into()} else {data_encoding::BASE64URL_NOPAD.encode(&std::fs::read(dir.join(format!("private-{alg}.pk8"))).unwrap())};
        let configured=provision_assertion(&db,&json!({"name":"fixture","expected_revision":initial["revision"],"method":if alg=="HS256" {"client_secret_jwt"}else{"private_key_jwt"},"algorithm":alg,"assertion_audience":audience,"issuer":format!("{origin}/"),"client_id":"fixture-client","callback_url":format!("http://127.0.0.1:{port}/callback"),"credential":credential})).unwrap();
        assert_eq!(configured["reconnect_required"],true);
        let opened=start(&db,"fixture").unwrap();let auth=Url::parse(opened["authorization_url"].as_str().unwrap()).unwrap();
        let state=auth.query_pairs().find(|(k,_)|k=="state").unwrap().1.to_string();
        let mut socket=std::net::TcpStream::connect(("127.0.0.1",port)).unwrap();socket.set_read_timeout(Some(Duration::from_secs(15))).unwrap();
        write!(socket,"GET /callback?state={state}&code=fixture HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n").unwrap();
        let mut response=String::new();socket.read_to_string(&mut response).unwrap();assert!(response.starts_with("HTTP/1.1 200"),"{response}");
        let revision=row(&db,"fixture").unwrap()["revision"].as_u64().unwrap();crate::ui_mcp::activate(&db,"fixture",revision).unwrap();
        let (config,tool)=crate::ui_mcp::resolve(&db,&kageha_mcp::tool_id("fixture","lookup")).unwrap();
        let result=crate::ui_mcp::invoke(&db,"fixture-session","fixture-turn","fixture-call",&config,&tool,json!({}),&AtomicBool::new(false)).unwrap();
        assert!(result.to_string().contains("assertion-protected result"));
        let revision=row(&db,"fixture").unwrap()["revision"].as_u64().unwrap();
        let rotated=if alg=="HS256" {"fedcba9876543210fedcba9876543210".into()} else {data_encoding::BASE64URL_NOPAD.encode(&std::fs::read(dir.join(format!("private-next-{alg}.pk8"))).unwrap())};
        assert_ne!(credential,rotated);
        provision_assertion(&db,&json!({"name":"fixture","expected_revision":revision,"method":if alg=="HS256" {"client_secret_jwt"}else{"private_key_jwt"},"algorithm":alg,"issuer":format!("{origin}/"),"credential":rotated})).unwrap();
        assert!(crate::ui_mcp::resolve(&db,&kageha_mcp::tool_id("fixture","lookup")).is_err());assert_eq!(status(&db,"fixture").unwrap()["status"],"not_connected");drop(trust);
    }
    fn assertion_flow(method:&str,algorithm:&str,credential:String,public:Option<Vec<u8>>) {
        use std::io::{Read,Write};
        use hmac::{Hmac,Mac};
        let listener=TcpListener::bind(("127.0.0.1",0)).unwrap();let port=listener.local_addr().unwrap().port();drop(listener);
        let name=if method=="private_key_jwt" {"plugin-assertion-private"}else{"plugin-assertion-secret"};
        let (db,_)=dynamic_fixture(name);
        {let connection=db.lock().unwrap();let mut config=ui_store::get(&connection,KIND,name).unwrap().unwrap();
            config["oauth"]=Value::Null;ui_store::put(&connection,KIND,name,config).unwrap();}
        let revision=row(&db,name).unwrap()["revision"].as_u64().unwrap();
        let request=json!({"name":name,"expected_revision":revision,"method":method,"algorithm":algorithm,"issuer":"https://github.com/login/oauth",
            "client_id":"assertion-client","callback_url":format!("http://127.0.0.1:{port}/callback"),"credential":credential,"key_id":"fixture-key"});
        let result=provision_assertion(&db,&request).unwrap();assert_eq!(result["reconnect_required"],true);assert!(!result.to_string().contains(&credential));
        let config=row(&db,name).unwrap();assert!(!config.to_string().contains(&credential));
        assert_eq!(provision_assertion(&db,&request).unwrap_err().0,409);
        let method_owned=method.to_owned();let algorithm_owned=algorithm.to_owned();
        let fetch:Fetch=Arc::new(move |url|if url.contains("oauth-protected-resource") {Ok(json!({"resource":"https://api.githubcopilot.com/mcp","authorization_servers":["https://github.com/login/oauth"]}))}
            else {Ok(json!({"issuer":"https://github.com/login/oauth","authorization_endpoint":"https://github.com/login/oauth/authorize","token_endpoint":"https://github.com/login/oauth/access_token","token_endpoint_auth_methods_supported":[method_owned],"token_endpoint_auth_signing_alg_values_supported":[algorithm_owned],"code_challenge_methods_supported":["S256"]}))});
        let secret=credential.clone();let alg=algorithm.to_owned();
        let exchange:Exchange=Arc::new(move |url,fields| {
            let request=token_request(fields,url).unwrap();assert!(!request.body.contains(&secret));
            let params:std::collections::HashMap<_,_>=url::form_urlencoded::parse(request.body.as_bytes()).into_owned().collect();
            assert!(!params.contains_key("client_secret"));assert_eq!(params["client_assertion_type"],"urn:ietf:params:oauth:client-assertion-type:jwt-bearer");
            let parts:Vec<_>=params["client_assertion"].split('.').collect();assert_eq!(parts.len(),3);
            let header:Value=serde_json::from_slice(&data_encoding::BASE64URL_NOPAD.decode(parts[0].as_bytes()).unwrap()).unwrap();assert_eq!(header["alg"],alg);assert_eq!(header["kid"],"fixture-key");
            let claims:Value=serde_json::from_slice(&data_encoding::BASE64URL_NOPAD.decode(parts[1].as_bytes()).unwrap()).unwrap();
            assert_eq!(claims["iss"],"assertion-client");assert_eq!(claims["sub"],"assertion-client");assert_eq!(claims["aud"],url);
            assert_eq!(claims["exp"].as_u64().unwrap()-claims["iat"].as_u64().unwrap(),60);assert!(!claims["jti"].as_str().unwrap().is_empty());
            let signed=format!("{}.{}",parts[0],parts[1]);let sig=data_encoding::BASE64URL_NOPAD.decode(parts[2].as_bytes()).unwrap();
            if let Some(public)=&public {ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519,public).verify(signed.as_bytes(),&sig).unwrap();}
            else {let mut mac=Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();mac.update(signed.as_bytes());mac.verify_slice(&sig).unwrap();}
            Ok(json!({"access_token":"assertion-access","refresh_token":"assertion-refresh","expires_in":if fields["grant_type"]=="refresh_token" {3600} else {0}}))
        });
        let refresh_exchange=exchange.clone();
        let opened=start_with(&db,name,fetch,exchange,Arc::new(|_,_|panic!("pre-registered client must not register")),Arc::new(|_|Ok(None)),None,"http://127.0.0.1:12799/callback").unwrap();
        let params:std::collections::HashMap<_,_>=Url::parse(opened["authorization_url"].as_str().unwrap()).unwrap().query_pairs().into_owned().collect();
        let mut socket=std::net::TcpStream::connect(("127.0.0.1",port)).unwrap();socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        write!(socket,"GET /callback?state={}&code=fixture-code HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n",params["state"]).unwrap();
        let mut response=String::new();socket.read_to_string(&mut response).unwrap();assert!(response.starts_with("HTTP/1.1 200"));
        let mut config=row(&db,name).unwrap();authorize_with(&db,name,&mut config,refresh_exchange.as_ref()).unwrap();
        assert_eq!(config["headers"]["Authorization"],"Bearer assertion-access");
        let revision=row(&db,name).unwrap()["revision"].as_u64().unwrap();let mut rotated=request.clone();rotated["expected_revision"]=json!(revision);
        rotated["credential"]=if method=="client_secret_jwt" {json!("fedcba9876543210fedcba9876543210")}else{
            let next=ring::signature::Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new()).unwrap();json!(data_encoding::BASE64URL_NOPAD.encode(next.as_ref()))};
        provision_assertion(&db,&rotated).unwrap();assert!(authorize(&db,name,&mut config).is_err());assert_eq!(row(&db,name).unwrap()["enabled"],false);
    }
    #[test]
    fn client_secret_jwt_flow_signs_rotates_and_refreshes() {assertion_flow("client_secret_jwt","HS256","0123456789abcdef0123456789abcdef".into(),None);}
    #[test]
    fn private_key_jwt_rs256_uses_pkcs8_and_produces_verifiable_signature() {
        // Public test-only RSA key generated solely for this fixture.
        let secret=data_encoding::BASE64URL_NOPAD.encode(include_bytes!("../tests/fixtures/oauth-test-rsa.pk8"));
        let values=json!({"client_id":"rsa-client","client_secret":secret,"client_auth_method":"private_key_jwt","client_auth_algorithm":"RS256"});
        let assertion=client_assertion(&values,"https://github.com/token").unwrap();let parts:Vec<_>=assertion.split('.').collect();
        let sig=data_encoding::BASE64URL_NOPAD.decode(parts[2].as_bytes()).unwrap();
        ring::signature::UnparsedPublicKey::new(&ring::signature::RSA_PKCS1_2048_8192_SHA256,include_bytes!("../tests/fixtures/oauth-test-rsa-public.der"))
            .verify(format!("{}.{}",parts[0],parts[1]).as_bytes(),&sig).unwrap();
    }
    #[test]
    fn private_key_jwt_flow_signs_rotates_and_refreshes() {
        use ring::signature::KeyPair;
        let der=ring::signature::Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new()).unwrap();
        let key=ring::signature::Ed25519KeyPair::from_pkcs8(der.as_ref()).unwrap();
        assertion_flow("private_key_jwt","EdDSA",data_encoding::BASE64URL_NOPAD.encode(der.as_ref()),Some(key.public_key().as_ref().to_vec()));
    }
    #[test]
    fn assertion_audience_is_confined_to_issuer_or_token_endpoint() {
        let mut fields=json!({"client_id":"client","client_secret":"0123456789abcdef0123456789abcdef","client_auth_method":"client_secret_jwt","client_auth_algorithm":"HS256","issuer":"https://github.com/login/oauth"});
        let decode=|value:&str| {let parts:Vec<_>=value.split('.').collect();serde_json::from_slice::<Value>(&data_encoding::BASE64URL_NOPAD.decode(parts[1].as_bytes()).unwrap()).unwrap()};
        assert_eq!(decode(&client_assertion(&fields,"https://github.com/login/oauth/access_token").unwrap())["aud"],"https://github.com/login/oauth/access_token");
        fields["assertion_audience"]=json!("issuer");
        assert_eq!(decode(&client_assertion(&fields,"https://github.com/login/oauth/access_token").unwrap())["aud"],"https://github.com/login/oauth");
        fields["issuer"]=json!("https://attacker.example/issuer");assert_eq!(client_assertion(&fields,"https://github.com/login/oauth/access_token").unwrap_err().0,403);
        fields["assertion_audience"]=json!("https://attacker.example/");assert_eq!(client_assertion(&fields,"https://github.com/login/oauth/access_token").unwrap_err().0,400);
    }
    #[test]
    fn es256_signature_is_jose_fixed_width_and_rejects_algorithm_mismatch() {
        use ring::signature::KeyPair;
        let rng=ring::rand::SystemRandom::new();
        let pkcs8=ring::signature::EcdsaKeyPair::generate_pkcs8(&ring::signature::ECDSA_P256_SHA256_FIXED_SIGNING,&rng).unwrap();
        let key=ring::signature::EcdsaKeyPair::from_pkcs8(&ring::signature::ECDSA_P256_SHA256_FIXED_SIGNING,pkcs8.as_ref(),&rng).unwrap();
        let credential=data_encoding::BASE64URL_NOPAD.encode(pkcs8.as_ref());
        assert!(valid_assertion_credential("private_key_jwt","ES256",&credential).is_ok());
        assert!(valid_assertion_credential("private_key_jwt","RS256",&credential).is_err());
        assert!(valid_assertion_credential("private_key_jwt","EdDSA",&credential).is_err());
        let assertion=client_assertion(&json!({"client_id":"client","client_secret":credential,"client_auth_method":"private_key_jwt","client_auth_algorithm":"ES256"}),"https://github.com/token").unwrap();
        let parts:Vec<_>=assertion.split('.').collect();let signature=data_encoding::BASE64URL_NOPAD.decode(parts[2].as_bytes()).unwrap();assert_eq!(signature.len(),64);
        ring::signature::UnparsedPublicKey::new(&ring::signature::ECDSA_P256_SHA256_FIXED,key.public_key().as_ref()).verify(format!("{}.{}",parts[0],parts[1]).as_bytes(),&signature).unwrap();
        let name="plugin-alg-mismatch";let (db,_)=dynamic_fixture(name);
        let config=row(&db,name).unwrap();
        let mut modified=config;modified["oauth"]["client_id"]=json!("client");modified["oauth"]["dynamic_registration"]=json!(false);
        {let connection=db.lock().unwrap();ui_store::put(&connection,KIND,name,modified).unwrap();}
        let revision=row(&db,name).unwrap()["revision"].as_u64().unwrap();
        provision_assertion(&db,&json!({"name":name,"expected_revision":revision,"method":"private_key_jwt","algorithm":"ES256","issuer":"https://github.com/login/oauth","credential":credential})).unwrap();
        let fetch:Fetch=Arc::new(|url|if url.contains("oauth-protected-resource") {Ok(json!({"resource":"https://api.githubcopilot.com/mcp","authorization_servers":["https://github.com/login/oauth"]}))}
            else {Ok(json!({"issuer":"https://github.com/login/oauth","authorization_endpoint":"https://github.com/authorize","token_endpoint":"https://github.com/token","token_endpoint_auth_methods_supported":["private_key_jwt"],"token_endpoint_auth_signing_alg_values_supported":["RS256"],"code_challenge_methods_supported":["S256"]}))});
        assert_eq!(start_with(&db,name,fetch,Arc::new(|_,_|panic!("must not exchange")),Arc::new(|_,_|panic!("must not register")),Arc::new(|_|Ok(None)),None,"http://127.0.0.1:12799/callback").unwrap_err().0,403);
    }
    #[test]
    fn basic_client_credentials_are_form_encoded_and_not_in_token_body() {
        let request=token_request(&json!({"client_id":"id: name","client_secret":"s+e:cret","client_auth_method":"client_secret_basic","grant_type":"authorization_code","code":"value"}),"https://github.com/token").unwrap();
        assert!(!request.body.contains("client_id"));assert!(!request.body.contains("client_secret"));
        assert!(!request.body.contains("s%2Be"));assert!(request.body.contains("grant_type=authorization_code"));
        let auth=&request.headers.iter().find(|(name,_)|name=="Authorization").unwrap().1;
        let decoded=data_encoding::BASE64.decode(auth.strip_prefix("Basic ").unwrap().as_bytes()).unwrap();
        assert_eq!(decoded,b"id%3A+name:s%2Be%3Acret");
        assert!(token_request(&json!({"client_id":"id","client_auth_method":"client_secret_basic"}),"https://github.com/token").is_err());
        assert!(token_request(&json!({"client_id":"id","client_secret":"secret","client_auth_method":"none"}),"https://github.com/token").is_err());
    }
    #[test]
    fn confidential_basic_registration_and_token_exchange_use_sealed_secret() {
        use std::io::{Read,Write};
        let listener=TcpListener::bind(("127.0.0.1",0)).unwrap();let port=listener.local_addr().unwrap().port();drop(listener);
        let name="plugin-basic-flow";let (db,_)=dynamic_fixture(name);
        {let connection=db.lock().unwrap();let mut config=ui_store::get(&connection,KIND,name).unwrap().unwrap();
            config["oauth"]["callback_url"]=json!(format!("http://127.0.0.1:{port}/callback"));ui_store::put(&connection,KIND,name,config).unwrap();}
        let fetch:Fetch=Arc::new(|url|if url.contains("oauth-protected-resource") {Ok(json!({"resource":"https://api.githubcopilot.com/mcp","authorization_servers":["https://github.com/login/oauth"]}))}
            else {Ok(json!({"issuer":"https://github.com/login/oauth","authorization_endpoint":"https://github.com/login/oauth/authorize","token_endpoint":"https://github.com/login/oauth/access_token","registration_endpoint":"https://github.com/register","code_challenge_methods_supported":["S256"]}))});
        let registration:Exchange=Arc::new(|_,fields|{assert_eq!(fields["token_endpoint_auth_method"],"client_secret_basic");
            Ok(json!({"client_id":"basic-client","client_secret":"basic-secret-canary","client_secret_expires_at":0,"token_endpoint_auth_method":"client_secret_basic","redirect_uris":fields["redirect_uris"]}))});
        let exchange:Exchange=Arc::new(|_,fields|{assert_eq!(fields["client_auth_method"],"client_secret_basic");
            let request=token_request(fields,"https://github.com/login/oauth/access_token").unwrap();assert!(!request.body.contains("basic-secret-canary"));
            let auth=&request.headers.iter().find(|(name,_)|name=="Authorization").unwrap().1;
            assert_eq!(data_encoding::BASE64.decode(auth.strip_prefix("Basic ").unwrap().as_bytes()).unwrap(),b"basic-client:basic-secret-canary");
            Ok(json!({"access_token":"basic-access-token","refresh_token":"basic-refresh-token","expires_in":3600}))});
        let opened=start_with(&db,name,fetch,exchange,registration,Arc::new(|_|Ok(None)),None,"http://127.0.0.1:12799/callback").unwrap();
        let params:std::collections::HashMap<_,_>=Url::parse(opened["authorization_url"].as_str().unwrap()).unwrap().query_pairs().into_owned().collect();
        let mut socket=std::net::TcpStream::connect(("127.0.0.1",port)).unwrap();socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        write!(socket,"GET /callback?state={}&code=basic-code HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n",params["state"]).unwrap();
        let mut response=String::new();socket.read_to_string(&mut response).unwrap();assert!(response.starts_with("HTTP/1.1 200"));
        assert_eq!(status(&db,name).unwrap()["status"],"ready");
        let config=row(&db,name).unwrap();assert_eq!(config["oauth"]["client_auth_method"],"client_secret_basic");
        assert!(!config.to_string().contains("basic-secret-canary"));
        let connection=db.lock().unwrap();assert_eq!(ui_settings::secret(&connection,&format!("mcp:{name}:oauth-client")).unwrap(),"basic-secret-canary");
        let generation=ui_credentials::oauth_generation(&connection,&grant(name)).unwrap();
        ui_credentials::oauth_store(&connection,&grant(name),&json!({"access_token":"expired","refresh_token":"refresh-canary","expires_in":0}),true,generation).unwrap();
        let authority=ui_credentials::oauth_authority(&connection,&grant(name)).unwrap().unwrap();
        let mut config=config;config["oauth_binding"]["authorization_id"]=json!(authority);ui_store::put(&connection,KIND,name,config).unwrap();drop(connection);
        let mut config=row(&db,name).unwrap();
        authorize_with(&db,name,&mut config,&|_,fields| {
            assert_eq!(fields["grant_type"],"refresh_token");assert_eq!(fields["client_auth_method"],"client_secret_basic");
            let request=token_request(fields,"https://github.com/login/oauth/access_token").unwrap();assert!(!request.body.contains("basic-secret-canary"));
            assert!(request.headers.iter().any(|(name,value)|name=="Authorization" && value.starts_with("Basic ")));
            Ok(json!({"access_token":"refreshed","refresh_token":"rotated","expires_in":3600}))
        }).unwrap();
        assert_eq!(config["headers"]["Authorization"],"Bearer refreshed");
    }
    #[test]
    fn discovered_assertion_methods_are_selected_for_dynamic_registration() {
        for (method,algorithm) in [("client_secret_jwt","HS256"),("private_key_jwt","EdDSA"),("private_key_jwt","ES256")] {
            let listener=TcpListener::bind(("127.0.0.1",0)).unwrap();let port=listener.local_addr().unwrap().port();drop(listener);
            let name=if method=="client_secret_jwt" {"plugin-select-jwt-secret"}else if algorithm=="EdDSA" {"plugin-select-jwt-key"}else{"plugin-select-jwt-ec"};
            let (db,_)=dynamic_fixture(name);
            {let connection=db.lock().unwrap();let mut config=ui_store::get(&connection,KIND,name).unwrap().unwrap();config["oauth"]["callback_url"]=json!(format!("http://127.0.0.1:{port}/callback"));ui_store::put(&connection,KIND,name,config).unwrap();}
            let fetch:Fetch=Arc::new(move |url|if url.contains("oauth-protected-resource") {Ok(json!({"resource":"https://api.githubcopilot.com/mcp","authorization_servers":["https://github.com/login/oauth"]}))}
                else {Ok(json!({"issuer":"https://github.com/login/oauth","authorization_endpoint":"https://github.com/login/oauth/authorize","token_endpoint":"https://github.com/login/oauth/access_token","registration_endpoint":"https://github.com/register","token_endpoint_auth_methods_supported":[method],"token_endpoint_auth_signing_alg_values_supported":[algorithm],"code_challenge_methods_supported":["S256"]}))});
            let published=Arc::new(Mutex::new(None::<Value>));let observed=published.clone();
            let registration:Exchange=Arc::new(move |_,fields| {
                assert_eq!(fields["token_endpoint_auth_method"],method);assert_eq!(fields["token_endpoint_auth_signing_alg"],algorithm);
                if method=="private_key_jwt" {*observed.lock().unwrap()=Some(fields["jwks"]["keys"][0].clone());}
                let mut result=json!({"client_id":"fixture-jwt","token_endpoint_auth_method":method,"redirect_uris":fields["redirect_uris"]});
                if method=="client_secret_jwt" {result["client_secret"]=json!("0123456789abcdef0123456789abcdef");result["client_secret_expires_at"]=json!(0);}
                Ok(result)
            });
            start_with(&db,name,fetch,Arc::new(|_,_|panic!("no token exchange")),registration,Arc::new(|_|Ok(None)),None,"http://127.0.0.1:12799/callback").unwrap();
            let config=row(&db,name).unwrap();assert_eq!(config["oauth"]["client_auth_method"],method);assert_eq!(config["oauth"]["client_auth_algorithm"],algorithm);
            if algorithm=="ES256" {
                use ring::signature::KeyPair;
                let secret={let connection=db.lock().unwrap();ui_settings::secret(&connection,&format!("mcp:{name}:oauth-client")).unwrap()};
                let der=data_encoding::BASE64URL_NOPAD.decode(secret.as_bytes()).unwrap();
                let key=ring::signature::EcdsaKeyPair::from_pkcs8(&ring::signature::ECDSA_P256_SHA256_FIXED_SIGNING,&der,&ring::rand::SystemRandom::new()).unwrap();
                let public=key.public_key().as_ref();let jwk=published.lock().unwrap().take().unwrap();
                assert_eq!(jwk["kty"],"EC");assert_eq!(jwk["crv"],"P-256");assert!(jwk["d"].is_null());
                assert_eq!(jwk["x"],data_encoding::BASE64URL_NOPAD.encode(&public[1..33]));assert_eq!(jwk["y"],data_encoding::BASE64URL_NOPAD.encode(&public[33..]));
            }
            assert_eq!(status(&db,name).unwrap()["pending"],true);cancel(&db,name).unwrap();
        }
    }
    #[test]
    fn assertion_dynamic_registration_persists_matching_key_material() {
        use ring::signature::KeyPair;
        for method in ["client_secret_jwt","private_key_jwt"] {
            let name=if method=="client_secret_jwt" {"plugin-dcr-hmac"}else{"plugin-dcr-key"};
            let (db,config)=dynamic_fixture(name);let resource=endpoint("https://api.githubcopilot.com/mcp").unwrap();
            let callback=redirect("http://127.0.0.1:12799/callback").unwrap();let registration_endpoint=endpoint("https://github.com/register").unwrap();
            let published=std::sync::Mutex::new(None::<Value>);
            register_client(&db,name,&config,&resource,&callback,"https://github.com/login/oauth",&registration_endpoint,method,if method=="client_secret_jwt" {"HS256"} else {"EdDSA"},&|_,fields| {
                assert_eq!(fields["token_endpoint_auth_method"],method);
                if method=="private_key_jwt" {
                    assert_eq!(fields["token_endpoint_auth_signing_alg"],"EdDSA");
                    let jwk=fields["jwks"]["keys"][0].clone();assert_eq!(jwk["crv"],"Ed25519");assert!(jwk["d"].is_null());
                    *published.lock().unwrap()=Some(jwk);
                    Ok(json!({"client_id":"dynamic-jwt","token_endpoint_auth_method":method,"redirect_uris":[callback.as_str()],"jwks":fields["jwks"]}))
                } else {
                    assert_eq!(fields["token_endpoint_auth_signing_alg"],"HS256");
                    Ok(json!({"client_id":"dynamic-jwt","client_secret":"0123456789abcdef0123456789abcdef","client_secret_expires_at":0,"token_endpoint_auth_method":method,"redirect_uris":[callback.as_str()]}))
                }
            }).unwrap();
            let config=row(&db,name).unwrap();assert_eq!(config["oauth"]["client_auth_method"],method);
            let connection=db.lock().unwrap();let secret=ui_settings::secret(&connection,&format!("mcp:{name}:oauth-client")).unwrap();
            assert!(!config.to_string().contains(&secret));
            if method=="private_key_jwt" {
                let der=data_encoding::BASE64URL_NOPAD.decode(secret.as_bytes()).unwrap();let key=ring::signature::Ed25519KeyPair::from_pkcs8_maybe_unchecked(&der).unwrap();
                let public=published.lock().unwrap().take().unwrap();assert_eq!(public["x"],data_encoding::BASE64URL_NOPAD.encode(key.public_key().as_ref()));
                assert_eq!(config["oauth"]["key_id"],public["kid"]);
            }
        }
    }
    #[test]
    fn confidential_registration_seals_client_secret_and_honors_expiry() {
        let name="plugin-confidential";let (db,config)=dynamic_fixture(name);
        let resource=endpoint("https://api.githubcopilot.com/mcp").unwrap();let callback=redirect("http://127.0.0.1:12799/callback").unwrap();
        let registration_endpoint=endpoint("https://github.com/register").unwrap();
        register_client(&db,name,&config,&resource,&callback,"https://github.com/login/oauth",&registration_endpoint,"client_secret_post","",&|_,fields|{
            assert_eq!(fields["token_endpoint_auth_method"],"client_secret_post");
            Ok(json!({"client_id":"confidential-client","client_secret":"private-client-canary","client_secret_expires_at":0,"token_endpoint_auth_method":"client_secret_post","redirect_uris":["http://127.0.0.1:12799/callback"]}))
        }).unwrap();
        let configured=row(&db,name).unwrap();assert_eq!(configured["oauth"]["client_secret_required"],true);assert!(!configured.to_string().contains("private-client-canary"));
        let connection=db.lock().unwrap();assert_eq!(ui_settings::secret(&connection,&format!("mcp:{name}:oauth-client")).unwrap(),"private-client-canary");
        let receipt=ui_store::get(&connection,REGISTRATION,name).unwrap().unwrap();assert!(!receipt.to_string().contains("private-client-canary"));
        ui_credentials::oauth_store(&connection,&grant(name),&json!({"access_token":"old","refresh_token":"refresh","expires_in":0}),true,0).unwrap();
        let authority=ui_credentials::oauth_authority(&connection,&grant(name)).unwrap().unwrap();
        let mut configured=configured;configured["oauth"]["client_secret_expires_at"]=json!(1);
        configured["oauth_binding"]=json!({"resource":resource.as_str(),"client_id":"confidential-client","issuer":"https://github.com/login/oauth","token_endpoint":"https://github.com/login/oauth/access_token","oauth":configured["oauth"],"authorization_id":authority});
        ui_store::put(&connection,KIND,name,configured).unwrap();drop(connection);
        assert_eq!(authorize_with(&db,name,&mut row(&db,name).unwrap(),&|_,_|panic!("expired client secret must not be sent")).unwrap_err().0,409);
    }
    #[test]
    fn bearer_challenge_parsing_rejects_ambiguity_and_cross_origin() {
        assert_eq!(challenge_metadata(&["Basic realm=\"x\", Bearer resource_metadata=\"https://api.githubcopilot.com/meta\"".into()]).unwrap().as_deref(),Some("https://api.githubcopilot.com/meta"));
        assert!(challenge_metadata(&["Bearer resource_metadata=\"https://one.test/x\", resource_metadata=\"https://two.test/x\"".into()]).is_err());
        assert!(challenge_metadata(&["Bearer resource_metadata=\"unclosed".into()]).is_err());
        let (db,_)=dynamic_fixture("plugin-challenge-bad");
        assert_eq!(start_with(&db,"plugin-challenge-bad",Arc::new(|_|panic!("cross-origin metadata must not be fetched")),Arc::new(|_,_|panic!("no exchange")),Arc::new(|_,_|panic!("no registration")),Arc::new(|_|Ok(Some("https://attacker.test/meta".into()))),None,"http://127.0.0.1:12799/callback").unwrap_err().0,403);
    }
    #[test]
    fn dynamic_registration_uncertain_result_cannot_be_retried() {
        let name="plugin-dynamic-failure";let (db,config)=dynamic_fixture(name);
        let resource=endpoint("https://api.githubcopilot.com/mcp").unwrap();let callback=redirect("http://127.0.0.1:12799/callback").unwrap();
        let registration_endpoint=endpoint("https://github.com/register").unwrap();
        assert!(register_client(&db,name,&config,&resource,&callback,"https://github.com/login/oauth",&registration_endpoint,"none","",&|_,_|Err((502,"network timeout".into()))).is_err());
        assert_eq!(status(&db,name).unwrap()["registration"],"uncertain");
        assert_eq!(register_client(&db,name,&config,&resource,&callback,"https://github.com/login/oauth",&registration_endpoint,"none","",&|_,_|panic!("must not retry uncertain registration")).unwrap_err().0,409);
    }
    #[test]
    fn mutual_tls_alias_may_use_an_independently_trusted_origin() {
        let issuer=endpoint("https://github.com/login/oauth").unwrap();
        let meta=json!({"mtls_endpoint_aliases":{"token_endpoint":"https://api.githubcopilot.com/mtls-token"}});
        assert_eq!(mutual_tls_token_endpoint(&issuer,&meta,"https://github.com/token").unwrap(),"https://api.githubcopilot.com/mtls-token");
        let bad=json!({"mtls_endpoint_aliases":{"token_endpoint":"https://attacker.example/token"}});
        assert_eq!(mutual_tls_token_endpoint(&issuer,&bad,"https://github.com/token").unwrap_err().0,403);
    }
    #[test]
    fn metadata_binds_resource_issuer_and_pkce() {
        let resource=endpoint("https://mcp.example.test/mcp/").unwrap();
        let protected=json!({"resource":"https://mcp.example.test/mcp","authorization_servers":["https://auth.example.test/oauth"]});
        let issuer=json!({"issuer":"https://auth.example.test/oauth","authorization_endpoint":"https://auth.example.test/authorize","token_endpoint":"https://auth.example.test/token","code_challenge_methods_supported":["S256"]});
        assert!(validate_metadata(&resource,&protected,&issuer,false).is_ok());
        assert!(validate_metadata(&resource,&protected,&issuer,true).is_err());
        let mut bad=issuer.clone();bad["token_endpoint"]=json!("https://evil.test/token");assert!(validate_metadata(&resource,&protected,&bad,false).is_err());
        let mut bad=issuer.clone();bad["code_challenge_methods_supported"]=json!(["plain"]);assert!(validate_metadata(&resource,&protected,&bad,false).is_err());
        let mut bad=protected.clone();bad["resource"]=json!("https://elsewhere.test/mcp");assert!(validate_metadata(&resource,&bad,&issuer,false).is_err());
        assert!(redirect("http://127.0.0.1:12799/callback").is_ok());
        assert!(redirect("http://192.0.2.1:12799/callback").is_err());
        assert!(redirect("http://127.0.0.1:12799/callback?x=1").is_err());
        assert!(trusted(&endpoint("https://api.githubcopilot.com/mcp").unwrap()));
        assert!(!trusted(&endpoint("https://127.0.0.1/mcp").unwrap()));
        assert!(!trusted(&endpoint("https://evil.example/mcp").unwrap()));
    }
    #[test]
    fn bearer_is_scoped_to_granted_resource_and_revoked_on_disconnect() {
        let connection=rusqlite::Connection::open_in_memory().unwrap();
        connection.execute_batch("CREATE TABLE native_chat_turns(sequence INTEGER PRIMARY KEY,chat_id TEXT,request_id TEXT,message TEXT,answer TEXT,status TEXT);").unwrap();
        crate::webui::initialize(&connection).unwrap();
        let name="plugin-fixture-mcp";
        let id=grant(name);
        ui_credentials::oauth_store(&connection,&id,&json!({"access_token":"fixture-private-token","expires_in":3600}),true,0).unwrap();
        let authority=ui_credentials::oauth_authority(&connection,&id).unwrap().unwrap();
        let config=json!({"name":name,"transport":"http","url":"https://api.githubcopilot.com/mcp","enabled":true,"connected":true,
            "tools":[],"oauth":{"client_id":"fixture-client","callback_url":"http://127.0.0.1:12799/callback"},
            "oauth_binding":{"resource":"https://api.githubcopilot.com/mcp","client_id":"fixture-client",
                "issuer":"https://github.com/login/oauth","token_endpoint":"https://github.com/login/oauth/access_token","oauth":{"client_id":"fixture-client","callback_url":"http://127.0.0.1:12799/callback"},"authorization_id":authority}});
        ui_store::put(&connection,KIND,name,config).unwrap();
        let db=crate::agent_host::DatabaseState::open(connection).unwrap();
        let mut configured=row(&db,name).unwrap();
        authorize(&db,name,&mut configured).unwrap();
        assert_eq!(configured["headers"]["Authorization"],"Bearer fixture-private-token");
        assert!(!row(&db,name).unwrap().to_string().contains("fixture-private-token"));
        let mut changed=row(&db,name).unwrap(); changed["url"]=json!("https://attacker.example.test/mcp");
        assert!(authorize(&db,name,&mut changed).is_err());
        assert_eq!(status(&db,name).unwrap()["status"],"ready");
        {
            let connection = db.lock().unwrap();
            let mut changed = ui_store::get(&connection,KIND,name).unwrap().unwrap();
            changed["oauth"]["scope"] = json!("different-scope");
            ui_store::put(&connection,KIND,name,changed).unwrap();
        }
        assert_eq!(status(&db,name).unwrap()["status"],"not_connected");
        assert!(authorize(&db,name,&mut row(&db,name).unwrap()).is_err());
        disconnect(&db,name).unwrap();
        assert_eq!(status(&db,name).unwrap()["status"],"not_connected");
        assert!(authorize(&db,name,&mut row(&db,name).unwrap()).is_err());
    }
    #[test]
    fn public_client_loopback_handoff_uses_pkce_and_keeps_tokens_private() {
        use std::io::{Read,Write};
        let listener=TcpListener::bind(("127.0.0.1",0)).unwrap();
        let port=listener.local_addr().unwrap().port();drop(listener);
        let connection=rusqlite::Connection::open_in_memory().unwrap();
        connection.execute_batch("CREATE TABLE native_chat_turns(sequence INTEGER PRIMARY KEY,chat_id TEXT,request_id TEXT,message TEXT,answer TEXT,status TEXT);").unwrap();
        crate::webui::initialize(&connection).unwrap();
        let name="plugin-fixture-public";
        assert!(public_declaration(&json!({"client_id":"public-client","callback_url":format!("http://127.0.0.1:{port}/callback"),"callback_port":"bad"})).is_err());
        let declaration=public_declaration(&json!({"client_id":"public-client","callback_url":format!("http://127.0.0.1:{port}/callback")})).unwrap();
        assert_eq!(declaration["client_secret_required"],false);
        ui_store::put(&connection,KIND,name,json!({"name":name,"transport":"http","url":"https://api.githubcopilot.com/mcp/",
            "enabled":false,"connected":false,"oauth":declaration,"origin":"plugin","plugin":"fixture"})).unwrap();
        let db=crate::agent_host::DatabaseState::open(connection).unwrap();
        let fetch:Fetch=Arc::new(|url| {
            if url.contains("oauth-protected-resource") {Ok(json!({"resource":"https://api.githubcopilot.com/mcp","authorization_servers":["https://github.com/login/oauth"]}))}
            else {Ok(json!({"issuer":"https://github.com/login/oauth","authorization_endpoint":"https://github.com/login/oauth/authorize",
                "token_endpoint":"https://github.com/login/oauth/access_token","code_challenge_methods_supported":["S256"]}))}
        });
        let exchange:Exchange=Arc::new(|url,fields| {
            assert_eq!(url,"https://github.com/login/oauth/access_token");
            assert_eq!(fields["client_id"],"public-client");assert_eq!(fields["client_secret"],"");
            assert_eq!(fields["resource"],"https://api.githubcopilot.com/mcp/");
            assert!(fields["code_verifier"].as_str().unwrap().len()>30);
            Ok(json!({"access_token":"private-canary-token","expires_in":3600}))
        });
        let opened=start_with(&db,name,fetch,exchange,Arc::new(|_,_|panic!("registration not expected")),Arc::new(|_|Ok(None)),None,"http://127.0.0.1:12799/callback").unwrap();
        assert_eq!(status(&db,name).unwrap()["pending"],true);
        let authorization=Url::parse(opened["authorization_url"].as_str().unwrap()).unwrap();
        let params:std::collections::HashMap<_,_>=authorization.query_pairs().into_owned().collect();
        assert_eq!(params["code_challenge_method"],"S256");
        assert_eq!(params["resource"],"https://api.githubcopilot.com/mcp/");
        let request=|state:&str| {
            let mut socket=std::net::TcpStream::connect(("127.0.0.1",port)).unwrap();
            socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            write!(socket,"GET /callback?state={state}&code=fixture-code HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n").unwrap();
            let mut response=String::new();socket.read_to_string(&mut response).unwrap();response
        };
        assert!(request("wrong").starts_with("HTTP/1.1 400"));
        assert!(request(&params["state"]).starts_with("HTTP/1.1 200"));
        let state=status(&db,name).unwrap(); assert_eq!(state["status"],"ready"); assert_eq!(state["pending"],false);
        let mut config=row(&db,name).unwrap(); authorize(&db,name,&mut config).unwrap();
        assert_eq!(config["headers"]["Authorization"],"Bearer private-canary-token");
        assert!(!row(&db,name).unwrap().to_string().contains("private-canary-token"));
        assert!(!opened.to_string().contains("private-canary-token"));
        let stale=params["state"].clone();
        assert!(finish_with(&db,name,&format!("/callback?state={stale}&code=again"),&|_,_|panic!("replay must not exchange")).is_err());
    }
    #[test]
    fn expired_grant_refreshes_only_for_its_bound_resource() {
        let connection=rusqlite::Connection::open_in_memory().unwrap();
        connection.execute_batch("CREATE TABLE native_chat_turns(sequence INTEGER PRIMARY KEY,chat_id TEXT,request_id TEXT,message TEXT,answer TEXT,status TEXT);").unwrap();
        crate::webui::initialize(&connection).unwrap();
        let name="plugin-fixture-refresh";let id=grant(name);
        ui_credentials::oauth_store(&connection,&id,&json!({"access_token":"old-token","refresh_token":"private-refresh","expires_in":0}),true,0).unwrap();
        let authority=ui_credentials::oauth_authority(&connection,&id).unwrap().unwrap();
        ui_store::put(&connection,KIND,name,json!({"name":name,"transport":"http","url":"https://api.githubcopilot.com/mcp",
            "oauth":{"client_id":"public","callback_url":"http://127.0.0.1:12799/callback","client_secret_required":false},
            "oauth_binding":{"resource":"https://api.githubcopilot.com/mcp","client_id":"public","issuer":"https://github.com/login/oauth",
                "token_endpoint":"https://github.com/login/oauth/access_token","oauth":{"client_id":"public","callback_url":"http://127.0.0.1:12799/callback","client_secret_required":false},"authorization_id":authority}})).unwrap();
        let db=crate::agent_host::DatabaseState::open(connection).unwrap();
        let mut config=row(&db,name).unwrap();
        authorize_with(&db,name,&mut config,&|url,fields|{
            assert_eq!(url,"https://github.com/login/oauth/access_token");assert_eq!(fields["grant_type"],"refresh_token");
            assert_eq!(fields["refresh_token"],"private-refresh");assert_eq!(fields["resource"],"https://api.githubcopilot.com/mcp");
            Ok(json!({"access_token":"new-token","refresh_token":"rotated-refresh","expires_in":3600}))
        }).unwrap();
        assert_eq!(config["headers"]["Authorization"],"Bearer new-token");
        assert!(!row(&db,name).unwrap().to_string().contains("private-refresh"));
    }
    #[test]
    fn cancelling_reconnect_prevents_a_late_callback_and_preserves_existing_grant() {
        let temporary=TcpListener::bind(("127.0.0.1",0)).unwrap();let port=temporary.local_addr().unwrap().port();drop(temporary);
        let connection=rusqlite::Connection::open_in_memory().unwrap();
        connection.execute_batch("CREATE TABLE native_chat_turns(sequence INTEGER PRIMARY KEY,chat_id TEXT,request_id TEXT,message TEXT,answer TEXT,status TEXT);").unwrap();
        crate::webui::initialize(&connection).unwrap();let name="plugin-fixture-reconnect";let id=grant(name);
        ui_credentials::oauth_store(&connection,&id,&json!({"access_token":"existing-token","expires_in":3600}),true,0).unwrap();
        let authority=ui_credentials::oauth_authority(&connection,&id).unwrap().unwrap();
        ui_store::put(&connection,KIND,name,json!({"name":name,"transport":"http","url":"https://api.githubcopilot.com/mcp",
            "enabled":true,"connected":true,"oauth":{"client_id":"public","callback_url":format!("http://127.0.0.1:{port}/callback")},
            "oauth_binding":{"resource":"https://api.githubcopilot.com/mcp","client_id":"public","issuer":"https://github.com/login/oauth",
                "token_endpoint":"https://github.com/login/oauth/access_token","oauth":{"client_id":"public","callback_url":format!("http://127.0.0.1:{port}/callback")},"authorization_id":authority}})).unwrap();
        let db=crate::agent_host::DatabaseState::open(connection).unwrap();
        let fetch:Fetch=Arc::new(|url|if url.contains("oauth-protected-resource") {Ok(json!({"resource":"https://api.githubcopilot.com/mcp","authorization_servers":["https://github.com/login/oauth"]}))}
            else {Ok(json!({"issuer":"https://github.com/login/oauth","authorization_endpoint":"https://github.com/login/oauth/authorize",
                "token_endpoint":"https://github.com/login/oauth/access_token","code_challenge_methods_supported":["S256"]}))});
        let opened=start_with(&db,name,fetch,Arc::new(|_,_|panic!("cancelled flow must not exchange")),Arc::new(|_,_|panic!("registration not expected")),Arc::new(|_|Ok(None)),None,"http://127.0.0.1:12799/callback").unwrap();
        let state:std::collections::HashMap<_,_>=Url::parse(opened["authorization_url"].as_str().unwrap()).unwrap().query_pairs().into_owned().collect();
        assert_eq!(status(&db,name).unwrap()["status"],"ready");assert_eq!(status(&db,name).unwrap()["pending"],true);
        assert_eq!(authorize(&db,name,&mut row(&db,name).unwrap()).unwrap_err().0,409);
        assert!(finish_with(&db,name,&format!("/callback?state={}&code=first",state["state"]),&|_,_| {
            cancel(&db,name).unwrap();
            Ok(json!({"access_token":"late-replacement","expires_in":3600}))
        }).is_err());
        assert_eq!(status(&db,name).unwrap()["pending"],false);
        assert!(finish_with(&db,name,&format!("/callback?state={}&code=late",state["state"]),&|_,_|panic!("cancelled flow must not exchange")).is_err());
        let mut config=row(&db,name).unwrap();authorize(&db,name,&mut config).unwrap();
        assert_eq!(config["headers"]["Authorization"],"Bearer existing-token");
    }
}
