use crate::SecretBytes;
use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    XChaCha20Poly1305, XNonce,
};
use kageha_contracts::{ErrorCode, Result, ServiceError};
pub(crate) fn err(code: ErrorCode, message: &str) -> ServiceError {
    ServiceError::new(code, message)
}
pub fn random<const N: usize>() -> Result<[u8; N]> {
    let mut bytes = [0; N];
    getrandom::getrandom(&mut bytes)
        .map_err(|_| err(ErrorCode::Unavailable, "Credential entropy unavailable"))?;
    Ok(bytes)
}
pub fn seal(key: &[u8], aad: &[u8], bytes: &[u8]) -> Result<Vec<u8>> {
    let cipher = XChaCha20Poly1305::new_from_slice(key)
        .map_err(|_| err(ErrorCode::Invalid, "Invalid credential key"))?;
    let nonce = random::<24>()?;
    let ciphertext = cipher
        .encrypt(XNonce::from_slice(&nonce), Payload { msg: bytes, aad })
        .map_err(|_| err(ErrorCode::Unavailable, "Credential encryption failed"))?;
    let mut value = nonce.to_vec();
    value.extend(ciphertext);
    Ok(value)
}
pub fn open(key: &[u8], aad: &[u8], bytes: &[u8]) -> Result<SecretBytes> {
    if bytes.len() < 40 {
        return Err(err(
            ErrorCode::VerificationFailed,
            "Credential authentication failed",
        ));
    }
    let cipher = XChaCha20Poly1305::new_from_slice(key)
        .map_err(|_| err(ErrorCode::Invalid, "Invalid credential key"))?;
    let value = cipher
        .decrypt(
            XNonce::from_slice(&bytes[..24]),
            Payload {
                msg: &bytes[24..],
                aad,
            },
        )
        .map_err(|_| {
            err(
                ErrorCode::VerificationFailed,
                "Credential authentication failed",
            )
        })?;
    Ok(SecretBytes::new(value))
}
pub(crate) fn json<T: serde::Serialize>(value: &T) -> Result<Vec<u8>> {
    serde_json::to_vec(value).map_err(|_| err(ErrorCode::Invalid, "Invalid credential structure"))
}
pub(crate) fn parse<T: serde::de::DeserializeOwned>(value: &[u8]) -> Result<T> {
    serde_json::from_slice(value).map_err(|_| {
        err(
            ErrorCode::VerificationFailed,
            "Invalid credential structure",
        )
    })
}
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
pub(crate) fn check(control: &kageha_contracts::execution::Control<'_>) -> Result<()> {
    control.check().map_err(|e| {
        err(
            if e.kind() == std::io::ErrorKind::TimedOut {
                ErrorCode::Deadline
            } else {
                ErrorCode::Cancelled
            },
            "Credential operation interrupted",
        )
    })
}

pub(crate) async fn controlled<T>(
    control: &kageha_contracts::execution::Control<'_>,
    future: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    check(control)?;
    tokio::select! {result=future=>result,_=async{loop{if control.check().is_err(){break;}tokio::time::sleep(std::time::Duration::from_millis(5)).await;}}=>{check(control)?;Err(err(ErrorCode::Cancelled,"Credential operation cancelled"))}}
}
