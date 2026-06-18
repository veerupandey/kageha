//! Authenticated service client implements the same ports as the embedded vault.
use crate::{
    crypto::{check, err},
    *,
};
use kageha_contracts::{execution::Control, ErrorCode, Result};
use serde_json::{json, Value};
use std::{path::Path, time::Duration};
use zeroize::Zeroize;
pub struct Client {
    http: reqwest::Client,
    base: String,
    token: SecretBytes,
}
impl Client {
    pub fn unix(socket: &Path, token: SecretBytes) -> Result<Self> {
        let http = reqwest::Client::builder()
            .unix_socket(socket.to_path_buf())
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(25))
            .build()
            .map_err(|_| err(ErrorCode::Unavailable, "Credential IPC client unavailable"))?;
        Ok(Self {
            http,
            base: "http://localhost".into(),
            token,
        })
    }
    pub fn hosted(base: &str, identity_pem: &[u8], ca_pem: &[u8]) -> Result<Self> {
        let url = reqwest::Url::parse(base)
            .map_err(|_| err(ErrorCode::Invalid, "Invalid credential endpoint"))?;
        if url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(err(
                ErrorCode::Invalid,
                "Credential network service requires a clean HTTPS endpoint",
            ));
        }
        let identity = reqwest::Identity::from_pem(identity_pem).map_err(|_| {
            err(
                ErrorCode::Invalid,
                "Invalid credential workload TLS identity",
            )
        })?;
        let roots = reqwest::Certificate::from_pem_bundle(ca_pem)
            .map_err(|_| err(ErrorCode::Invalid, "Invalid credential TLS trust roots"))?;
        if roots.is_empty() {
            return Err(err(
                ErrorCode::Invalid,
                "Credential TLS trust roots required",
            ));
        }
        let mut builder = reqwest::Client::builder()
            .identity(identity)
            .tls_built_in_root_certs(false)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(25));
        for root in roots {
            builder = builder.add_root_certificate(root);
        }
        let http = builder.build().map_err(|_| {
            err(
                ErrorCode::Unavailable,
                "Credential HTTPS client unavailable",
            )
        })?;
        Ok(Self {
            http,
            base: base.trim_end_matches('/').into(),
            token: SecretBytes::new(Vec::new()),
        })
    }
    async fn request(&self, path: &str, mut body: Value, control: &Control<'_>) -> Result<Value> {
        check(control)?;
        let mutation = matches!(path, "/v1/put" | "/v1/revoke" | "/v1/oauth")
            || match path {
                "/v1/control" => body["call"]["action"] != "export_record",
                "/v1/transfer" => matches!(
                    body["call"]["action"].as_str(),
                    Some("freeze" | "import" | "activate")
                ),
                "/v1/replacements" => {
                    !matches!(body["action"].as_str(), Some("binding" | "status"))
                }
                _ => false,
            };
        let mut builder = self.http.post(format!("{}{path}", self.base)).json(&body);
        if !self.token.expose().is_empty() {
            builder = builder.bearer_auth(
                std::str::from_utf8(self.token.expose())
                    .map_err(|_| err(ErrorCode::Invalid, "Invalid credential client token"))?,
            );
        }
        crate::wire::clear(&mut body);
        let limit = if path == "/v1/transfer" {
            64 * 1024 * 1024
        } else {
            256 * 1024
        };
        let operation = async {
            let mut response=builder.send().await.map_err(|_|err(ErrorCode::Uncertain,"Credential service response unavailable; query the original operation ID before retrying a mutation"))?;
            let status = response.status();
            let mut bytes = zeroize::Zeroizing::new(Vec::new());
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| err(ErrorCode::Uncertain, "Credential response interrupted"))?
            {
                if bytes.len() + chunk.len() > limit {
                    return Err(err(ErrorCode::Invalid, "Credential response exceeds limit"));
                }
                bytes.extend_from_slice(&chunk);
            }
            if status == reqwest::StatusCode::NO_CONTENT {
                return Ok(Value::Null);
            }
            if !status.is_success() {
                return Err(err(
                    match status.as_u16() {
                        401 | 403 => ErrorCode::Unauthorized,
                        404 => ErrorCode::NotFound,
                        400 => ErrorCode::Invalid,
                        409 => ErrorCode::Conflict,
                        504 => ErrorCode::Deadline,
                        _ => ErrorCode::Unavailable,
                    },
                    "Credential service rejected the operation",
                ));
            }
            serde_json::from_slice(&bytes).map_err(|_| {
                err(
                    ErrorCode::VerificationFailed,
                    "Invalid credential service response",
                )
            })
        };
        tokio::select! {result=operation=>result,_=async{loop{if control.check().is_err(){break;}tokio::time::sleep(Duration::from_millis(10)).await;}}=>{if mutation{return Err(err(ErrorCode::Uncertain,"Credential request may have dispatched; query the original operation ID before retrying"));}check(control)?;Err(err(ErrorCode::Cancelled,"Credential request cancelled"))}}
    }
    fn query(access: &CredentialAccess, reference: &CredentialRef) -> Value {
        json!({"reference":reference,"purpose":access.purpose,"destination":access.destination})
    }
}
#[async_trait::async_trait]
impl CredentialResolver for Client {
    async fn resolve(
        &self,
        a: &CredentialAccess,
        r: &CredentialRef,
        c: &Control<'_>,
    ) -> Result<SecretLease> {
        let mut value = self.request("/v1/resolve", Self::query(a, r), c).await?;
        let metadata: CredentialMetadata = serde_json::from_value(value["metadata"].clone())
            .map_err(|_| err(ErrorCode::VerificationFailed, "Invalid credential metadata"))?;
        if metadata.reference != *r {
            return Err(err(
                ErrorCode::VerificationFailed,
                "Credential service returned a different reference",
            ));
        }
        let Value::String(mut encoded) = value["secret_base64"].take() else {
            return Err(err(
                ErrorCode::VerificationFailed,
                "Invalid credential encoding",
            ));
        };
        let decoded = data_encoding::BASE64.decode(encoded.as_bytes());
        encoded.zeroize();
        let secret = SecretBytes::new(
            decoded
                .map_err(|_| err(ErrorCode::VerificationFailed, "Invalid credential encoding"))?,
        );
        let valid_until_ms = value["valid_until_ms"]
            .as_u64()
            .ok_or_else(|| err(ErrorCode::VerificationFailed, "Invalid credential lease"))?;
        if valid_until_ms <= crate::crypto::now_ms()
            || valid_until_ms > crate::crypto::now_ms() + 60000
        {
            return Err(err(
                ErrorCode::VerificationFailed,
                "Invalid credential lease expiry",
            ));
        }
        Ok(SecretLease {
            secret,
            metadata,
            valid_until_ms,
        })
    }
    async fn validate_lease(
        &self,
        a: &CredentialAccess,
        l: &SecretLease,
        c: &Control<'_>,
    ) -> Result<()> {
        let mut q = Self::query(a, &l.metadata.reference);
        q["metadata"] = json!(l.metadata);
        q["valid_until_ms"] = json!(l.valid_until_ms);
        self.request("/v1/check", q, c).await?;
        Ok(())
    }
}
#[async_trait::async_trait]
impl CredentialAdmin for Client {
    async fn metadata(
        &self,
        a: &CredentialAccess,
        r: &CredentialRef,
        c: &Control<'_>,
    ) -> Result<CredentialMetadata> {
        serde_json::from_value(self.request("/v1/metadata", Self::query(a, r), c).await?)
            .map_err(|_| err(ErrorCode::VerificationFailed, "Invalid credential metadata"))
    }
    async fn put(
        &self,
        a: &CredentialAccess,
        w: CredentialWrite,
        c: &Control<'_>,
    ) -> Result<CredentialOperation> {
        let mut q = Self::query(a, &w.reference);
        q["operation_id"] = json!(w.operation_id);
        q["expected_revision"] = json!(w.expected_revision);
        q["kind"] = json!(w.kind);
        q["expires_at_ms"] = json!(w.expires_at_ms);
        q["secret_base64"] = json!(data_encoding::BASE64.encode(w.secret.expose()));
        let result = self.request("/v1/put", q, c).await?;
        serde_json::from_value(result).map_err(|_| {
            err(
                ErrorCode::VerificationFailed,
                "Invalid credential operation receipt",
            )
        })
    }
    async fn revoke(
        &self,
        a: &CredentialAccess,
        r: &CredentialRef,
        rev: u64,
        id: &str,
        delete: bool,
        c: &Control<'_>,
    ) -> Result<CredentialOperation> {
        let mut q = Self::query(a, r);
        q["operation_id"] = json!(id);
        q["expected_revision"] = json!(rev);
        q["delete"] = json!(delete);
        serde_json::from_value(self.request("/v1/revoke", q, c).await?).map_err(|_| {
            err(
                ErrorCode::VerificationFailed,
                "Invalid credential operation receipt",
            )
        })
    }
    async fn operation(
        &self,
        a: &CredentialAccess,
        r: &CredentialRef,
        id: &str,
        c: &Control<'_>,
    ) -> Result<Option<CredentialOperation>> {
        let mut q = Self::query(a, r);
        q["operation_id"] = json!(id);
        serde_json::from_value(self.request("/v1/operation", q, c).await?).map_err(|_| {
            err(
                ErrorCode::VerificationFailed,
                "Invalid credential operation receipt",
            )
        })
    }
}

fn ticket(value: Value) -> Result<RefreshTicket> {
    serde_json::from_value(value).map_err(|_| {
        err(
            ErrorCode::VerificationFailed,
            "Invalid OAuth refresh ticket",
        )
    })
}
fn operation(value: Value) -> Result<CredentialOperation> {
    serde_json::from_value(value).map_err(|_| {
        err(
            ErrorCode::VerificationFailed,
            "Invalid OAuth refresh receipt",
        )
    })
}
#[async_trait::async_trait]
impl CredentialRefresh for Client {
    async fn begin_refresh(
        &self,
        a: &CredentialAccess,
        r: &CredentialRef,
        revision: u64,
        id: &str,
        c: &Control<'_>,
    ) -> Result<RefreshTicket> {
        let reply=self.request("/v1/oauth",json!({"query":Self::query(a,r),"call":{"action":"begin","expected_revision":revision,"operation_id":id}}),c).await?;
        ticket(reply["ticket"].clone())
    }
    async fn dispatch_refresh(
        &self,
        a: &CredentialAccess,
        t: &RefreshTicket,
        id: &str,
        c: &Control<'_>,
    ) -> Result<(RefreshTicket, SecretBytes)> {
        let mut reply=self.request("/v1/oauth",json!({"query":Self::query(a,&t.reference),"call":{"action":"dispatch","ticket":t,"operation_id":id}}),c).await?;
        let result = ticket(reply["ticket"].clone())?;
        let Value::String(mut value) = reply["grant_base64"].take() else {
            return Err(err(
                ErrorCode::VerificationFailed,
                "Invalid OAuth refresh grant",
            ));
        };
        let decoded = data_encoding::BASE64.decode(value.as_bytes());
        value.zeroize();
        Ok((
            result,
            SecretBytes::new(decoded.map_err(|_| {
                err(
                    ErrorCode::VerificationFailed,
                    "Invalid OAuth refresh encoding",
                )
            })?),
        ))
    }
    async fn complete_refresh(
        &self,
        a: &CredentialAccess,
        t: &RefreshTicket,
        id: &str,
        grant: SecretBytes,
        expiry: Option<u64>,
        c: &Control<'_>,
    ) -> Result<CredentialOperation> {
        let reply=self.request("/v1/oauth",json!({"query":Self::query(a,&t.reference),"call":{"action":"complete","ticket":t,"operation_id":id,"grant_base64":data_encoding::BASE64.encode(grant.expose()),"expires_at_ms":expiry}}),c).await?;
        operation(reply["operation"].clone())
    }
    async fn recover_refresh(
        &self,
        a: &CredentialAccess,
        r: &CredentialRef,
        revision: u64,
        id: &str,
        c: &Control<'_>,
    ) -> Result<CredentialOperation> {
        let reply=self.request("/v1/oauth",json!({"query":Self::query(a,r),"call":{"action":"recover","expected_revision":revision,"operation_id":id}}),c).await?;
        operation(reply["operation"].clone())
    }
}

#[async_trait::async_trait]
impl CredentialControl for Client {
    async fn revoke_workload(
        &self,
        a: &CredentialAccess,
        p: &str,
        v: u64,
        id: &str,
        c: &Control<'_>,
    ) -> Result<CredentialOperation> {
        operation(self.request("/v1/control",json!({"query":Self::query(a,&CredentialRef{namespace:"system".into(),id:"vault".into()}),"call":{"action":"revoke_workload","principal":p,"expected_revision":v,"operation_id":id}}),c).await?)
    }
    async fn binding_generation(&self, c: &Control<'_>) -> Result<u64> {
        self.request("/v1/info", Value::Null, c).await?["generation"]
            .as_u64()
            .ok_or_else(|| {
                err(
                    ErrorCode::VerificationFailed,
                    "Credential service generation missing",
                )
            })
    }
    async fn authentication_generation(&self, c: &Control<'_>) -> Result<u64> {
        self.request("/v1/info", Value::Null, c).await?["authentication_generation"]
            .as_u64()
            .ok_or_else(|| {
                err(
                    ErrorCode::VerificationFailed,
                    "Credential service authentication generation missing",
                )
            })
    }
    async fn fence(
        &self,
        a: &CredentialAccess,
        r: &CredentialRef,
        expected: u64,
        id: &str,
        c: &Control<'_>,
    ) -> Result<CredentialOperation> {
        let value=self.request("/v1/control",json!({"query":Self::query(a,r),"call":{"action":"fence","expected_revision":expected,"operation_id":id}}),c).await?;
        operation(value)
    }
    async fn fail_refresh(
        &self,
        a: &CredentialAccess,
        t: &RefreshTicket,
        id: &str,
        c: &Control<'_>,
    ) -> Result<CredentialOperation> {
        let value=self.request("/v1/control",json!({"query":Self::query(a,&t.reference),"call":{"action":"fail_refresh","ticket":t,"operation_id":id}}),c).await?;
        operation(value)
    }
    async fn export_record(
        &self,
        a: &CredentialAccess,
        r: &CredentialRef,
        c: &Control<'_>,
    ) -> Result<(CredentialMetadata, SecretBytes)> {
        let mut value = self
            .request(
                "/v1/control",
                json!({"query":Self::query(a,r),"call":{"action":"export_record"}}),
                c,
            )
            .await?;
        let Value::String(mut encoded) = value["secret_base64"].take() else {
            return Err(err(
                ErrorCode::VerificationFailed,
                "Credential export encoding missing",
            ));
        };
        let decoded = data_encoding::BASE64.decode(encoded.as_bytes());
        encoded.zeroize();
        let metadata = serde_json::from_value(value["metadata"].clone()).map_err(|_| {
            err(
                ErrorCode::VerificationFailed,
                "Credential export metadata missing",
            )
        })?;
        Ok((
            metadata,
            SecretBytes::new(decoded.map_err(|_| {
                err(
                    ErrorCode::VerificationFailed,
                    "Invalid credential export encoding",
                )
            })?),
        ))
    }
    async fn audits(
        &self,
        a: &CredentialAccess,
        namespace: &str,
        after: u64,
        c: &Control<'_>,
    ) -> Result<Vec<CredentialAudit>> {
        let value=self.request("/v1/audit",json!({"namespace":namespace,"purpose":a.purpose,"destination":a.destination,"after":after}),c).await?;
        serde_json::from_value(value).map_err(|_| {
            err(
                ErrorCode::VerificationFailed,
                "Invalid credential audit response",
            )
        })
    }
}
impl Client {
    async fn transfer(&self, a: &CredentialAccess, call: Value, c: &Control<'_>) -> Result<Value> {
        self.request("/v1/transfer",json!({"query":Self::query(a,&CredentialRef{namespace:"system".into(),id:"vault".into()}),"call":call}),c).await
    }
}
#[async_trait::async_trait]
impl CredentialTransfer for Client {
    async fn backend_state(
        &self,
        a: &CredentialAccess,
        c: &Control<'_>,
    ) -> Result<CredentialBackendState> {
        serde_json::from_value(self.transfer(a, json!({"action":"state"}), c).await?).map_err(
            |_| {
                err(
                    ErrorCode::VerificationFailed,
                    "Invalid credential backend state",
                )
            },
        )
    }
    async fn export_checkpoint(
        &self,
        a: &CredentialAccess,
        c: &Control<'_>,
    ) -> Result<CredentialCheckpoint> {
        let mut value = self.transfer(a, json!({"action":"export"}), c).await?;
        let wire = serde_json::from_value::<crate::wire::Checkpoint>(value.clone());
        crate::wire::clear(&mut value);
        wire.map_err(|_| {
            err(
                ErrorCode::VerificationFailed,
                "Invalid credential checkpoint",
            )
        })?
        .decode()
    }
    async fn freeze(
        &self,
        a: &CredentialAccess,
        expected: u64,
        owner: &str,
        c: &Control<'_>,
    ) -> Result<CredentialBackendState> {
        serde_json::from_value(
            self.transfer(
                a,
                json!({"action":"freeze","expected_generation":expected,"owner":owner}),
                c,
            )
            .await?,
        )
        .map_err(|_| {
            err(
                ErrorCode::VerificationFailed,
                "Invalid credential fence state",
            )
        })
    }
    async fn import_checkpoint(
        &self,
        a: &CredentialAccess,
        expected: u64,
        owner: &str,
        checkpoint: CredentialCheckpoint,
        c: &Control<'_>,
    ) -> Result<CredentialBackendState> {
        let wire = crate::wire::Checkpoint::encode(checkpoint);
        serde_json::from_value(self.transfer(a,json!({"action":"import","expected_generation":expected,"owner":owner,"checkpoint":wire}),c).await?).map_err(|_|err(ErrorCode::VerificationFailed,"Invalid credential import state"))
    }
    async fn activate(
        &self,
        a: &CredentialAccess,
        expected: u64,
        owner: &str,
        c: &Control<'_>,
    ) -> Result<CredentialBackendState> {
        serde_json::from_value(
            self.transfer(
                a,
                json!({"action":"activate","expected_generation":expected,"owner":owner}),
                c,
            )
            .await?,
        )
        .map_err(|_| {
            err(
                ErrorCode::VerificationFailed,
                "Invalid credential activation state",
            )
        })
    }
}

/// Explicit transport credentials per workload. Network identities still come from TLS/IPC authentication.
pub struct RoutedClients {
    pub workloads: std::collections::BTreeMap<String, std::sync::Arc<Client>>,
    pub management_principal: String,
}
impl RoutedClients {
    fn for_access(&self, a: &CredentialAccess) -> Result<&Client> {
        self.workloads
            .get(&a.principal)
            .map(AsRef::as_ref)
            .ok_or_else(|| {
                err(
                    ErrorCode::Unauthorized,
                    "Credential workload has no authenticated client",
                )
            })
    }
    fn management(&self) -> Result<&Client> {
        self.workloads
            .get(&self.management_principal)
            .map(AsRef::as_ref)
            .ok_or_else(|| {
                err(
                    ErrorCode::Unauthorized,
                    "Credential management client is missing",
                )
            })
    }
}
#[async_trait::async_trait]
impl CredentialResolver for RoutedClients {
    async fn resolve(
        &self,
        a: &CredentialAccess,
        r: &CredentialRef,
        c: &Control<'_>,
    ) -> Result<SecretLease> {
        let s = self.for_access(a)?;
        s.resolve(a, r, c).await
    }
    async fn validate_lease(
        &self,
        a: &CredentialAccess,
        l: &SecretLease,
        c: &Control<'_>,
    ) -> Result<()> {
        let s = self.for_access(a)?;
        s.validate_lease(a, l, c).await
    }
}
#[async_trait::async_trait]
impl CredentialAdmin for RoutedClients {
    async fn metadata(
        &self,
        a: &CredentialAccess,
        r: &CredentialRef,
        c: &Control<'_>,
    ) -> Result<CredentialMetadata> {
        let s = self.for_access(a)?;
        s.metadata(a, r, c).await
    }
    async fn put(
        &self,
        a: &CredentialAccess,
        w: CredentialWrite,
        c: &Control<'_>,
    ) -> Result<CredentialOperation> {
        let s = self.for_access(a)?;
        s.put(a, w, c).await
    }
    async fn revoke(
        &self,
        a: &CredentialAccess,
        r: &CredentialRef,
        v: u64,
        id: &str,
        delete: bool,
        c: &Control<'_>,
    ) -> Result<CredentialOperation> {
        let s = self.for_access(a)?;
        s.revoke(a, r, v, id, delete, c).await
    }
    async fn operation(
        &self,
        a: &CredentialAccess,
        r: &CredentialRef,
        id: &str,
        c: &Control<'_>,
    ) -> Result<Option<CredentialOperation>> {
        let s = self.for_access(a)?;
        s.operation(a, r, id, c).await
    }
}
#[async_trait::async_trait]
impl CredentialRefresh for RoutedClients {
    async fn begin_refresh(
        &self,
        a: &CredentialAccess,
        r: &CredentialRef,
        v: u64,
        id: &str,
        c: &Control<'_>,
    ) -> Result<RefreshTicket> {
        let s = self.for_access(a)?;
        s.begin_refresh(a, r, v, id, c).await
    }
    async fn dispatch_refresh(
        &self,
        a: &CredentialAccess,
        t: &RefreshTicket,
        id: &str,
        c: &Control<'_>,
    ) -> Result<(RefreshTicket, SecretBytes)> {
        let s = self.for_access(a)?;
        s.dispatch_refresh(a, t, id, c).await
    }
    async fn complete_refresh(
        &self,
        a: &CredentialAccess,
        t: &RefreshTicket,
        id: &str,
        g: SecretBytes,
        expiry: Option<u64>,
        c: &Control<'_>,
    ) -> Result<CredentialOperation> {
        let s = self.for_access(a)?;
        s.complete_refresh(a, t, id, g, expiry, c).await
    }
    async fn recover_refresh(
        &self,
        a: &CredentialAccess,
        r: &CredentialRef,
        v: u64,
        id: &str,
        c: &Control<'_>,
    ) -> Result<CredentialOperation> {
        let s = self.for_access(a)?;
        s.recover_refresh(a, r, v, id, c).await
    }
}
#[async_trait::async_trait]
impl CredentialControl for RoutedClients {
    async fn revoke_workload(
        &self,
        a: &CredentialAccess,
        p: &str,
        v: u64,
        id: &str,
        c: &Control<'_>,
    ) -> Result<CredentialOperation> {
        self.for_access(a)?.revoke_workload(a, p, v, id, c).await
    }
    async fn binding_generation(&self, c: &Control<'_>) -> Result<u64> {
        let s = self.management()?;
        s.binding_generation(c).await
    }
    async fn authentication_generation(&self, c: &Control<'_>) -> Result<u64> {
        let s = self.management()?;
        s.authentication_generation(c).await
    }
    async fn fence(
        &self,
        a: &CredentialAccess,
        r: &CredentialRef,
        v: u64,
        id: &str,
        c: &Control<'_>,
    ) -> Result<CredentialOperation> {
        let s = self.for_access(a)?;
        s.fence(a, r, v, id, c).await
    }
    async fn fail_refresh(
        &self,
        a: &CredentialAccess,
        t: &RefreshTicket,
        id: &str,
        c: &Control<'_>,
    ) -> Result<CredentialOperation> {
        let s = self.for_access(a)?;
        s.fail_refresh(a, t, id, c).await
    }
    async fn export_record(
        &self,
        a: &CredentialAccess,
        r: &CredentialRef,
        c: &Control<'_>,
    ) -> Result<(CredentialMetadata, SecretBytes)> {
        let s = self.for_access(a)?;
        s.export_record(a, r, c).await
    }
    async fn audits(
        &self,
        a: &CredentialAccess,
        n: &str,
        after: u64,
        c: &Control<'_>,
    ) -> Result<Vec<CredentialAudit>> {
        let s = self.for_access(a)?;
        s.audits(a, n, after, c).await
    }
}
#[async_trait::async_trait]
impl CredentialTransfer for RoutedClients {
    async fn backend_state(
        &self,
        a: &CredentialAccess,
        c: &Control<'_>,
    ) -> Result<CredentialBackendState> {
        serde_json::from_value(
            self.for_access(a)?
                .transfer(a, json!({"action":"state"}), c)
                .await?,
        )
        .map_err(|_| {
            err(
                ErrorCode::VerificationFailed,
                "Invalid credential backend state",
            )
        })
    }
    async fn export_checkpoint(
        &self,
        a: &CredentialAccess,
        c: &Control<'_>,
    ) -> Result<CredentialCheckpoint> {
        let mut value = self
            .for_access(a)?
            .transfer(a, json!({"action":"export"}), c)
            .await?;
        let wire = serde_json::from_value::<crate::wire::Checkpoint>(value.clone());
        crate::wire::clear(&mut value);
        wire.map_err(|_| {
            err(
                ErrorCode::VerificationFailed,
                "Invalid credential checkpoint",
            )
        })?
        .decode()
    }
    async fn freeze(
        &self,
        a: &CredentialAccess,
        expected: u64,
        owner: &str,
        c: &Control<'_>,
    ) -> Result<CredentialBackendState> {
        serde_json::from_value(
            self.for_access(a)?
                .transfer(
                    a,
                    json!({"action":"freeze","expected_generation":expected,"owner":owner}),
                    c,
                )
                .await?,
        )
        .map_err(|_| {
            err(
                ErrorCode::VerificationFailed,
                "Invalid credential fence state",
            )
        })
    }
    async fn import_checkpoint(
        &self,
        a: &CredentialAccess,
        expected: u64,
        owner: &str,
        checkpoint: CredentialCheckpoint,
        c: &Control<'_>,
    ) -> Result<CredentialBackendState> {
        for grant in &checkpoint.access_grants {
            if !self.workloads.contains_key(&grant.principal) {
                return Err(err(
                    ErrorCode::Unauthorized,
                    "Credential destination lacks a required authenticated workload connection",
                ));
            }
        }
        for (principal, client) in &self.workloads {
            if client.workload_identity(c).await? != *principal {
                return Err(err(
                    ErrorCode::Unauthorized,
                    "Credential connection authenticated as another workload",
                ));
            }
        }
        let wire = crate::wire::Checkpoint::encode(checkpoint);
        serde_json::from_value(self.for_access(a)?.transfer(a,json!({"action":"import","expected_generation":expected,"owner":owner,"checkpoint":wire}),c).await?).map_err(|_|err(ErrorCode::VerificationFailed,"Invalid credential import state"))
    }
    async fn activate(
        &self,
        a: &CredentialAccess,
        expected: u64,
        owner: &str,
        c: &Control<'_>,
    ) -> Result<CredentialBackendState> {
        serde_json::from_value(
            self.for_access(a)?
                .transfer(
                    a,
                    json!({"action":"activate","expected_generation":expected,"owner":owner}),
                    c,
                )
                .await?,
        )
        .map_err(|_| {
            err(
                ErrorCode::VerificationFailed,
                "Invalid credential activation state",
            )
        })
    }
}
impl Client {
    pub async fn replacement(
        &self,
        a: &CredentialAccess,
        action: serde_json::Value,
        c: &Control<'_>,
    ) -> Result<serde_json::Value> {
        let mut body = action;
        let object = body.as_object_mut().ok_or_else(|| {
            err(
                ErrorCode::Invalid,
                "Credential replacement action must be an object",
            )
        })?;
        object.insert("purpose".into(), json!(a.purpose));
        object.insert("destination".into(), json!(a.destination));
        self.request("/v1/replacements", body, c).await
    }
}
#[async_trait::async_trait]
impl CredentialMigration for Client {
    async fn prepare(
        &self,
        a: &CredentialAccess,
        destination: &str,
        id: &str,
        c: &Control<'_>,
    ) -> Result<CredentialMigrationStatus> {
        let value=self.replacement(a,json!({"action":"request","operation_id":id,"decision":{"id":id,"destination":destination,"allow_browser_transfer":false}}),c).await?;
        serde_json::from_value(value)
            .map_err(|_| err(ErrorCode::VerificationFailed, "Invalid replacement status"))
    }
    async fn status(
        &self,
        a: &CredentialAccess,
        id: &str,
        c: &Control<'_>,
    ) -> Result<CredentialMigrationStatus> {
        let value = self
            .replacement(a, json!({"action":"status","operation_id":id}), c)
            .await?;
        serde_json::from_value(value)
            .map_err(|_| err(ErrorCode::VerificationFailed, "Invalid replacement status"))
    }
}

impl Client {
    pub async fn workload_identity(&self, c: &Control<'_>) -> Result<String> {
        self.request("/v1/info", Value::Null, c).await?["workload"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| {
                err(
                    ErrorCode::VerificationFailed,
                    "Credential service did not report its authenticated workload identity",
                )
            })
    }
}
