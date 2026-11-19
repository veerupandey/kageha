//! Operator-owned OAuth mutual-TLS identity, scoped to one MCP server.
use crate::{server,ui_credentials,ui_settings,ui_store::{self,ApiResult},webui::Database};
use serde_json::{json,Value};
use sha2::{Digest,Sha256};
use zeroize::Zeroizing;

fn hash(pem:&str)->String {format!("{:x}",Sha256::digest(pem.as_bytes()))}
fn callback(raw:&str)->bool {
    let Ok(url)=url::Url::parse(raw) else {return false};
    url.scheme()=="http" && url.host_str().and_then(|v|v.parse::<std::net::IpAddr>().ok()).is_some_and(|v|v.is_loopback())
        && url.port().is_some() && url.path()!="/" && url.query().is_none() && url.fragment().is_none()
        && url.username().is_empty() && url.password().is_none()
}
/// Replacing an identity revokes the old grant before publishing the new binding.
pub(crate) fn provision(db:&Database,payload:&Value)->ApiResult<Value> {
    let name=payload["name"].as_str().filter(|s|server::identifier(s)).ok_or((400,"Invalid MCP server name".into()))?;
    let revision=payload["expected_revision"].as_u64().ok_or((400,"Expected server revision is required".into()))?;
    let client=payload["client_id"].as_str().filter(|s|!s.is_empty() && s.len()<=1024 && !s.chars().any(char::is_control)).ok_or((400,"Invalid OAuth client ID".into()))?;
    let method=payload["auth_method"].as_str().unwrap_or("tls_client_auth");
    if !matches!(method,"tls_client_auth"|"self_signed_tls_client_auth") {return Err((400,"Invalid mutual TLS client authentication method".into()));}
    let pem=Zeroizing::new(payload["identity_pem"].as_str().filter(|s|!s.is_empty() && s.len()<=48*1024).ok_or((400,"Mutual TLS certificate and private key are required".into()))?.to_owned());
    kageha_integrations::validate_mutual_tls_identity(pem.as_bytes()).map_err(|_|(400,"Invalid mutual TLS identity".into()))?;
    let mut connection=db.lock().map_err(ui_store::db_error)?;
    let tx=connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(ui_store::db_error)?;
    let mut config=ui_store::get(&tx,"config:mcp:servers",name).map_err(ui_store::db_error)?
        .filter(|v|v["name"]==name && v["revision"].as_u64()==Some(revision)).ok_or((409,"MCP server changed or is unavailable".into()))?;
    let endpoint=config["url"].as_str().and_then(|s|url::Url::parse(s).ok()).filter(|u|u.scheme()=="https" && u.host_str().is_some() && u.username().is_empty() && u.password().is_none() && u.query().is_none() && u.fragment().is_none())
        .ok_or((400,"Mutual TLS requires an HTTPS MCP endpoint".into()))?;
    if !matches!(config["transport"].as_str(),Some("http"|"streamable_http")) {return Err((400,"Mutual TLS requires an HTTP MCP server".into()));}
    let callback=payload["callback_url"].as_str().or_else(||config["oauth"]["callback_url"].as_str()).filter(|s|callback(s)).ok_or((400,"A loopback OAuth callback URL is required".into()))?;
    let scope=config["oauth"]["scope"].clone();
    let mut oauth=json!({"client_id":client,"callback_url":callback,"client_auth_method":method,"client_secret_required":false,"mtls_identity_sha256":hash(&pem)});
    if !scope.is_null() {oauth["scope"]=scope;}
    let id=format!("mcp-{name}");
    ui_credentials::delete(&tx,&format!("oauth:{id}:grant"))?;
    if ui_credentials::pending_get(&tx,&id)?.is_some() {ui_credentials::pending_delete(&tx,&id)?;}
    let encoded=Zeroizing::new(data_encoding::BASE64.encode(pem.as_bytes()));
    ui_settings::save_secret(&tx,&format!("mcp:{name}:oauth-mtls-identity"),encoded.as_str())?;
    config["oauth"]=oauth;config["oauth_binding"]=Value::Null;
    config["enabled"]=json!(false);config["connected"]=json!(false);config["tools"]=json!([]);
    let updated=ui_store::put(&tx,"config:mcp:servers",name,config).map_err(ui_store::db_error)?;
    tx.commit().map_err(ui_store::db_error)?;
    let _=endpoint;
    Ok(json!({"ok":true,"name":name,"revision":updated["revision"],"configured":true}))
}
/// Resolve the identity for this exact current server and immutable public binding.
pub(crate) fn identity(db:&Database,name:&str,config:&Value)->ApiResult<Zeroizing<String>> {
    let expected=config["oauth"]["mtls_identity_sha256"].as_str().filter(|s|s.len()==64).ok_or((409,"Mutual TLS identity is not configured".into()))?;
    let connection=db.lock().map_err(ui_store::db_error)?;
    let current=ui_store::get(&connection,"config:mcp:servers",name).map_err(ui_store::db_error)?.ok_or((409,"MCP server is unavailable".into()))?;
    if current["name"]!=name || current["revision"]!=config["revision"] || current["url"]!=config["url"] || current["oauth"]!=config["oauth"] {
        return Err((409,"Mutual TLS configuration changed".into()));
    }
    decode_identity(&connection,name,expected)
}
fn decode_identity(connection:&rusqlite::Connection,name:&str,expected:&str)->ApiResult<Zeroizing<String>> {
    let encoded=Zeroizing::new(ui_settings::secret(connection,&format!("mcp:{name}:oauth-mtls-identity"))?);
    let bytes=Zeroizing::new(data_encoding::BASE64.decode(encoded.as_bytes()).map_err(|_|(409,"Mutual TLS identity is invalid".into()))?);
    let pem=Zeroizing::new(std::str::from_utf8(&bytes).map_err(|_|(409,"Mutual TLS identity is invalid".into()))?.to_owned());
    if pem.is_empty() || hash(&pem)!=expected {return Err((409,"Mutual TLS identity changed or is unavailable".into()));}
    Ok(pem)
}
pub(crate) fn identity_available(connection:&rusqlite::Connection,name:&str,config:&Value)->bool {
    config["oauth"]["mtls_identity_sha256"].as_str().filter(|value|value.len()==64)
        .is_some_and(|expected|decode_identity(connection,name,expected).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::{BufRead,BufReader,Read,Write},process::{Command,Stdio},sync::atomic::AtomicBool};
    fn run(dir:&std::path::Path,args:&[&str]) {
        let status=Command::new("openssl").current_dir(dir).args(args).stdout(Stdio::null()).stderr(Stdio::null()).status().unwrap();
        assert!(status.success(),"openssl {:?}",args);
    }
    struct Server(std::process::Child);
    impl Drop for Server {fn drop(&mut self){let _=self.0.kill();let _=self.0.wait();}}
    #[test]
    fn mutual_tls_real_oauth_and_mcp_end_to_end() {
        use std::os::unix::fs::PermissionsExt;
        let root=tempfile::tempdir().unwrap();let dir=root.path();
        run(dir,&["req","-x509","-newkey","rsa:2048","-nodes","-keyout","ca.key","-out","ca.crt","-subj","/CN=Fixture CA","-days","1"]);
        std::fs::write(dir.join("server.ext"),"subjectAltName=IP:127.0.0.1\nextendedKeyUsage=serverAuth\n").unwrap();
        std::fs::write(dir.join("client.ext"),"extendedKeyUsage=clientAuth\n").unwrap();
        for (name,cn,ext) in [("server","fixture-server","server.ext"),("client","fixture-client","client.ext"),("other","other-client","client.ext")] {
            run(dir,&["req","-newkey","rsa:2048","-nodes","-keyout",&format!("{name}.key"),"-out",&format!("{name}.csr"),"-subj",&format!("/CN={cn}")]);
            run(dir,&["x509","-req","-in",&format!("{name}.csr"),"-CA","ca.crt","-CAkey","ca.key","-CAcreateserial","-out",&format!("{name}.crt"),"-days","1","-extfile",ext]);
            if name!="server" {
                let mut pem=std::fs::read(dir.join(format!("{name}.crt"))).unwrap();
                pem.extend_from_slice(&std::fs::read(dir.join(format!("{name}.key"))).unwrap());
                std::fs::write(dir.join(format!("{name}.pem")),pem).unwrap();
            }
        }
        let log=dir.join("requests.log");
        let script=std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../kageha-mcp/tests/fixtures/mtls_server.cjs");
        let child=Command::new("node").arg(script).arg(dir).arg(&log).stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn().unwrap();
        let mut server=Server(child);let mut line=String::new();
        BufReader::new(server.0.stdout.take().unwrap()).read_line(&mut line).unwrap();
        let port:u16=line.trim().parse().unwrap();let origin=format!("https://127.0.0.1:{port}");
        let key=dir.join("vault.key");std::fs::write(&key,[42u8;32]).unwrap();std::fs::set_permissions(&key,std::fs::Permissions::from_mode(0o600)).unwrap();
        let output=Command::new(std::env::current_exe().unwrap()).args(["--ignored","--exact","ui_mcp_mtls::tests::mutual_tls_child","--nocapture"])
            .env("KAGEHA_CA_BUNDLE",dir.join("ca.crt")).env("KAGEHA_CREDENTIAL_KEY_FILE",&key).env("KAGEHA_HOME",dir.join("home"))
            .env("KAGEHA_TEST_MTLS_ORIGIN",&origin).env("KAGEHA_TEST_MTLS_DIR",dir).env("KAGEHA_TEST_MTLS_LOG",&log).output().unwrap();
        assert!(output.status.success(),"child failed: {}\n{}",String::from_utf8_lossy(&output.stdout),String::from_utf8_lossy(&output.stderr));
        let entries:Vec<Value>=std::fs::read_to_string(&log).unwrap().lines().map(|line|serde_json::from_str(line).unwrap()).collect();
        assert!(entries.iter().filter(|v|v["path"]=="/mtls-token").count()>=2,"authorization and refresh must both use the mTLS alias: {entries:?}");
        assert!(entries.iter().any(|v|v["path"]=="/mcp" && v["method"]=="POST"));
        assert!(entries.iter().filter(|v|v["path"]=="/mtls-token").all(|v|v["identity"]=="fixture-client"));
        assert!(entries.iter().any(|v|v["path"]=="/mcp" && v["method"]=="POST" && v["identity"]=="other-client"),
            "the separate identity must be offered independently");
        assert!(!entries.iter().any(|v|v["path"]=="/ordinary-token"));
    }
    #[test]
    #[ignore = "spawned with isolated TLS trust and credential vault"]
    fn mutual_tls_child() {
        let origin=std::env::var("KAGEHA_TEST_MTLS_ORIGIN").unwrap();
        let dir=std::path::PathBuf::from(std::env::var_os("KAGEHA_TEST_MTLS_DIR").unwrap());
        let trust=crate::ui_mcp_oauth::test_trust_loopback(&format!("{origin}/")).unwrap();
        let connection=rusqlite::Connection::open(dir.join("state.sqlite")).unwrap();
        connection.execute_batch("CREATE TABLE native_chat_turns(sequence INTEGER PRIMARY KEY,chat_id TEXT,request_id TEXT,message TEXT,answer TEXT,status TEXT);").unwrap();
        crate::webui::initialize(&connection).unwrap();
        let row=ui_store::put(&connection,"config:mcp:servers","fixture",json!({"name":"fixture","transport":"http","url":format!("{origin}/mcp"),"enabled":false,"connected":false,"tools":[]})).unwrap();
        let db=crate::agent_host::DatabaseState::open(connection).unwrap();
        let callback_listener=std::net::TcpListener::bind(("127.0.0.1",0)).unwrap();
        let callback_port=callback_listener.local_addr().unwrap().port();drop(callback_listener);
        let pem=std::fs::read_to_string(dir.join("client.pem")).unwrap();
        assert!(provision(&db,&json!({"name":"fixture","expected_revision":row["revision"].as_u64().unwrap()+1,"client_id":"fixture-client","callback_url":format!("http://127.0.0.1:{callback_port}/callback"),"identity_pem":pem})).is_err());
        let configured=provision(&db,&json!({"name":"fixture","expected_revision":row["revision"],"client_id":"fixture-client","callback_url":format!("http://127.0.0.1:{callback_port}/callback"),"identity_pem":pem})).unwrap();
        assert_eq!(configured["configured"],true);
        let saved={let c=db.lock().unwrap();ui_store::get(&c,"config:mcp:servers","fixture").unwrap().unwrap()};
        assert!(!saved.to_string().contains("PRIVATE KEY"));
        assert!(!db.lock().unwrap().query_row("SELECT EXISTS(SELECT 1 FROM ui_document_history WHERE body LIKE '%PRIVATE KEY%')",[],|r|r.get::<_,bool>(0)).unwrap());
        let encoded=data_encoding::BASE64.encode(pem.as_bytes());
        assert!(!db.lock().unwrap().query_row("SELECT EXISTS(SELECT 1 FROM ui_document_history WHERE body LIKE ?)",[format!("%{encoded}%")],|r|r.get::<_,bool>(0)).unwrap());
        let opened=crate::ui_mcp_oauth::start(&db,"fixture").unwrap();
        let auth=url::Url::parse(opened["authorization_url"].as_str().unwrap()).unwrap();
        let state=auth.query_pairs().find(|(key,_)|key=="state").unwrap().1.to_string();
        let mut socket=std::net::TcpStream::connect(("127.0.0.1",callback_port)).unwrap();
        socket.set_read_timeout(Some(std::time::Duration::from_secs(15))).unwrap();
        write!(socket,"GET /callback?state={state}&code=fixture HTTP/1.1\r\nHost: 127.0.0.1:{callback_port}\r\nConnection: close\r\n\r\n").unwrap();
        let mut response=String::new();socket.read_to_string(&mut response).unwrap();assert!(response.starts_with("HTTP/1.1 200"),"{response}");
        assert_eq!(crate::ui_mcp_oauth::status(&db,"fixture").unwrap()["status"],"ready");
        let revision={let c=db.lock().unwrap();ui_store::get(&c,"config:mcp:servers","fixture").unwrap().unwrap()["revision"].as_u64().unwrap()};
        crate::ui_mcp::activate(&db,"fixture",revision).unwrap();
        let (config,tool)=crate::ui_mcp::resolve(&db,&kageha_mcp::tool_id("fixture","lookup")).unwrap();
        let result=crate::ui_mcp::invoke(&db,"fixture-session","fixture-turn","fixture-call",&config,&tool,json!({}),&AtomicBool::new(false)).unwrap();
        assert!(result.to_string().contains("certificate-bound result"));
        // A second client sharing the endpoint and session must use its own
        // certificate, never the first client's pooled TLS connection.
        let other=std::fs::read_to_string(dir.join("other.pem")).unwrap();
        let raw=json!({"id":"other-server","transport":"http","url":format!("{origin}/mcp"),"headers":{"Authorization":"Bearer fixture-token"}});
        let other_config:kageha_mcp::Config=serde_json::from_value(raw.clone()).unwrap();
        let other_config=other_config.with_mutual_tls_identity(&other).unwrap();
        let pool=kageha_mcp::Pool::open(&dir.join("isolation.sqlite")).unwrap();
        let cancel=AtomicBool::new(false);
        let control=kageha_contracts::execution::Control{cancelled:&cancel,deadline:None};
        let context=kageha_contracts::execution::ToolContext{session:"shared-session".into(),turn:"fixture-turn".into()};
        assert!(pool.exchange(&other_config,&context,"tools/list",json!({}),None,&control).is_err());
        let good_config:kageha_mcp::Config=serde_json::from_value(raw).unwrap();
        let good_config=good_config.with_mutual_tls_identity(&pem).unwrap();
        assert!(pool.exchange(&good_config,&context,"tools/list",json!({}),None,&control).is_ok());
        let foreign=crate::native_io::http_mutual_tls("https://127.0.0.1:1/","GET",&[],vec![],&AtomicBool::new(false),std::time::Duration::from_secs(1),1024,pem.as_bytes(),&origin);
        assert!(foreign.is_err());
        let revision={let c=db.lock().unwrap();ui_store::get(&c,"config:mcp:servers","fixture").unwrap().unwrap()["revision"].as_u64().unwrap()};
        assert!(provision(&db,&json!({"name":"fixture","expected_revision":revision,"client_id":"fixture-client","identity_pem":"not a key"})).is_err());
        assert_eq!(crate::ui_mcp_oauth::status(&db,"fixture").unwrap()["status"],"ready");
        provision(&db,&json!({"name":"fixture","expected_revision":revision,"client_id":"fixture-client","identity_pem":other})).unwrap();
        assert!(identity(&db,"fixture",&config).is_err());
        assert!(crate::ui_mcp::resolve(&db,&kageha_mcp::tool_id("fixture","lookup")).is_err());
        assert_eq!(crate::ui_mcp_oauth::status(&db,"fixture").unwrap()["status"],"not_connected");
        drop(trust);
    }
}
