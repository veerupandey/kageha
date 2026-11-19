//! Salted readback commitments for convergent field actions. No input values are journaled.
use crate::{
    action_journal::Action,
    actions::{self, Page},
    cdp::{Cdp, Result},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use zeroize::Zeroize;

#[derive(Clone, Copy, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all="snake_case")]
pub enum Comparison { #[default] Text, NumericDecimalV1 }
impl Comparison {
    fn is_text(&self)->bool {matches!(self,Self::Text)}
    fn value(self,value:&Value)->Result<Value> {
        match self {Self::Text=>Ok(value.clone()),Self::NumericDecimalV1=>
            crate::numeric::canonical(value.as_str().ok_or("numeric_value_unavailable")?).map(Value::String).ok_or("numeric_value_unavailable")}
    }
}

#[derive(Clone, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub backend_node_id: u64,
    pub identity_digest: String,
    pub salt: String,
    pub value_digest: String,
    #[serde(default,skip_serializing_if="Comparison::is_text")]
    pub comparison: Comparison,
    pub url_digest: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Probe {
    pub salt: String,
    pub value_digest: String,
    #[serde(default,skip_serializing_if="Comparison::is_text")]
    pub comparison: Comparison,
    pub document_id: String,
}
impl Probe {
    pub fn valid(&self) -> bool {
        (16..=64).contains(&self.salt.len())
            && self.salt.bytes().all(|b| b.is_ascii_alphanumeric())
            && self.value_digest.len() == 64
            && self.value_digest.bytes().all(|b| b.is_ascii_hexdigit())
            && !self.document_id.is_empty()
            && self.document_id.len() <= 128
    }
}
pub fn digest(salt: &str, value: &Value) -> String {
    let mut bytes = serde_json::to_vec(&(salt, value)).unwrap();
    let hash = data_encoding::HEXLOWER.encode(&Sha256::digest(&bytes));
    bytes.zeroize();
    hash
}
fn identity(node: &Value) -> Value {
    json!([node["role"], node["name"], node["id"], node["input_type"]])
}
impl Evidence {
    pub fn new(backend: u64, node: &Value, url: &Value, value: &Value, comparison:Comparison) -> Self {
        let salt = Uuid::new_v4().simple().to_string();
        let normalized=comparison.value(value).unwrap_or_else(|_|value.clone());
        Self {
            backend_node_id: backend,
            identity_digest: digest(&salt, &identity(node)),
            value_digest: digest(&salt, &normalized),
            comparison,
            url_digest: digest(&salt, url),
            salt,
        }
    }
    pub async fn matches(
        &self,
        cdp: &mut Cdp,
        page: &Page,
        snapshot: &Value,
        action: Action,
    ) -> Result<bool> {
        if digest(&self.salt, &snapshot["url"]) != self.url_digest {
            return Err("receipt_url_changed");
        }
        let node = actions::observed_identity(snapshot, &page.frame, self.backend_node_id)?;
        if digest(&self.salt, &identity(node)) != self.identity_digest {
            return Err("receipt_field_changed");
        }
        let object = page.object(cdp, self.backend_node_id).await?;
        if self.comparison==Comparison::NumericDecimalV1 && !page.test_object(cdp,&object,"function(){return this instanceof HTMLInputElement && this.type==='number';}",json!([])).await? {return Err("receipt_field_changed");}
        let mut value = state(cdp, page, &object, action, true).await?;
        let mut matched = digest(&self.salt, &self.comparison.value(&value)?) == self.value_digest;
        if !matched && action == Action::Select {
            if let Ok(mut labels) = select_labels(cdp, page, &object).await {
                matched = digest(&self.salt, &labels) == self.value_digest;
                erase(&mut labels);
            }
        }
        erase(&mut value);
        Ok(matched)
    }
}
async fn select_labels(cdp: &mut Cdp, page: &Page, object: &str) -> Result<Value> {
    let result = cdp.call(Some(&page.session), "Runtime.callFunctionOn", json!({
        "objectId": object,
        "functionDeclaration": r#"function() {
            if (!(this instanceof HTMLSelectElement)) throw Error('not select');
            return Array.from(this.selectedOptions, o => (o.text || o.textContent || o.label || '').trim()).sort();
        }"#,
        "arguments": [],
        "returnByValue": true,
    })).await?;
    if result.get("exceptionDetails").is_some() {
        return Err("input_readback_unavailable");
    }
    result["result"].get("value").cloned().ok_or("input_readback_unavailable")
}
fn erase(value: &mut Value) {
    match value {
        Value::String(v) => v.zeroize(),
        Value::Array(v) => v.iter_mut().for_each(erase),
        _ => {}
    }
}
async fn state(
    cdp: &mut Cdp,
    page: &Page,
    object: &str,
    action: Action,
    allow_secure: bool,
) -> Result<Value> {
    let mode = match action {
        Action::Fill => "fill",
        Action::Select => "select",
        Action::Check => "check",
        _ => return Err("input_not_convergent"),
    };
    let result=cdp.call(Some(&page.session),"Runtime.callFunctionOn",json!({"objectId":object,
        "functionDeclaration":r#"function(mode, secure) {
            if (!this.isConnected || this.ownerDocument !== document
                || !(this.checkVisibility({opacityProperty:true, visibilityProperty:true})
                    || mode === 'check' && this instanceof HTMLInputElement && Array.from(this.labels || []).some(label =>
                        label.control === this && label.checkVisibility({opacityProperty:true,visibilityProperty:true})))) throw Error('unobserved field');
            if (mode === 'fill' && (this instanceof HTMLTextAreaElement || this instanceof HTMLInputElement
                && ['text','search','email','tel','url','number',...(secure?['password']:[])].includes(this.type))) return this.value;
            if (mode === 'select' && this instanceof HTMLSelectElement) return Array.from(this.selectedOptions,o=>o.value).sort();
            if (mode === 'check' && this instanceof HTMLInputElement && ['checkbox','radio'].includes(this.type) && !this.indeterminate) return this.checked;
            throw Error('unsupported field');
        }"#,"arguments":[{"value":mode},{"value":allow_secure}],"returnByValue":true})).await?;
    if result.get("exceptionDetails").is_some() {
        return Err("input_readback_unavailable");
    }
    result["result"]
        .get("value")
        .cloned()
        .ok_or("input_readback_unavailable")
}
pub async fn probe(
    cdp: &mut Cdp,
    page: &Page,
    snapshot: &Value,
    selector: &str,
    expected: &Probe,
) -> Result<Value> {
    if !expected.valid() || page.document != expected.document_id {
        return Err("receipt_document_changed");
    }
    let backend = actions::resolve_target(cdp, page, snapshot, selector).await?;
    actions::observed_identity(snapshot, &page.frame, backend)?;
    let object = page.object(cdp, backend).await?;
    if expected.comparison==Comparison::NumericDecimalV1 && !page.test_object(cdp,&object,"function(){return this instanceof HTMLInputElement && this.type==='number';}",json!([])).await? {return Err("receipt_field_changed");}
    let mut value = state(cdp, page, &object, Action::Fill, false).await?;
    let matched = digest(&expected.salt, &expected.comparison.value(&value)?) == expected.value_digest;
    erase(&mut value);
    page.check_document(cdp).await?;
    Ok(
        json!({"value_matches":matched,"backend_node_id":backend,"document_id":page.document,
        "verification":"field_value_readback","input_sent":false}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn commitment_is_salted_and_has_no_value() {
        let value = json!("private user value");
        let node = json!({"role":"textbox","name":"City","id":"city"});
        let a = Evidence::new(7, &node, &json!("https://example.com"), &value, Comparison::Text);
        let b = Evidence::new(7, &node, &json!("https://example.com"), &value, Comparison::Text);
        assert_ne!(a.value_digest, b.value_digest);
        assert_eq!(a.value_digest, digest(&a.salt, &value));
        assert!(!serde_json::to_string(&a)
            .unwrap()
            .contains("private user value"));
    }
}
