//! Explicit obligations checked against host-owned evidence, not final-answer prose.
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FieldCheck {
    pub selector: String,
    pub property: String,
    pub expected: Value,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Verification {
    /// The submit is fenced until a separately observed, identity-bound site
    /// confirmation is recorded. Delivery alone never satisfies this proof.
    BrowserSubmission { target_id: String, document_id: String, source_url: String },
    BrowserPostcondition { target_id: String, document_id: String, selector: String, text: String },
    BrowserFields { target_id: String, document_id: String, assertions: Vec<FieldCheck> },
    /// A completed local credential rotation, not a site reset confirmation.
    CredentialRotation { origin: String, account: String, candidate: String },
    Command { command: String },
    RemoteFile { host_id: String, path: String, sha256: String },
    /// Exact computer input, where no browser postcondition is available.
    ComputerAction { arguments: Value },
}
impl Verification {
    /// Adding field assertions strengthens a proof; changing its target or
    /// dropping assertions changes what it establishes and must be disclosed.
    pub fn covers(&self, prior:&Self)->bool {
        match (self,prior) {
            (Self::BrowserFields{target_id,document_id,assertions},Self::BrowserFields{target_id:old_target,document_id:old_document,assertions:old}) =>
                target_id==old_target && document_id==old_document && old.iter().all(|a|assertions.contains(a)),
            _=>self==prior,
        }
    }
    pub fn valid(&self) -> bool {
        let text = |s: &str, max| !s.trim().is_empty() && s.len() <= max && !s.contains('\0');
        match self {
            Self::BrowserSubmission { target_id, document_id, source_url } =>
                text(target_id,256) && text(document_id,256) && text(source_url,2048)
                    && source_url.starts_with("https://"),
            Self::BrowserPostcondition { target_id, document_id, selector, text: expected } =>
                text(target_id,256) && text(document_id,256) && text(selector,512) && text(expected,512),
            Self::BrowserFields { target_id, document_id, assertions } => text(target_id,256) && text(document_id,256)
                && !assertions.is_empty() && assertions.len() <= 32
                && assertions.iter().all(|a| text(&a.selector,512)
                    && matches!(a.property.as_str(),"value"|"checked"|"text"|"validity.valid")
                    && !a.expected.is_null() && a.expected.to_string().len() <= 4096)
                && assertions.iter().enumerate().all(|(i,a)| !assertions[..i].contains(a)),
            Self::Command { command } => text(command,16384),
            Self::RemoteFile { host_id, path, sha256 } => text(host_id,128) && text(path,4096)
                && !path.chars().any(char::is_control)
                && path.split('/').all(|part| !matches!(part, "" | "." | ".."))
                && sha256.len() == 64 && sha256.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
            Self::CredentialRotation { origin, account, candidate } =>
                text(origin,2048) && text(account,320) && text(candidate,128),
            Self::ComputerAction { arguments } => arguments.is_object() && arguments.to_string().len() <= 16384,
        }
    }
}
