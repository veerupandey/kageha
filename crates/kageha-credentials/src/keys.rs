use crate::{
    crypto::{check, err},
    KeyProtector, SecretBytes,
};
use kageha_contracts::{execution::Control, ErrorCode, Result};
use std::{collections::BTreeMap, path::PathBuf};
/// Explicit operator mappings; no implicit path lookup and no key-file creation.
pub struct FileKeys(pub BTreeMap<String, PathBuf>);
fn read_key(path: &std::path::Path) -> Result<SecretBytes> {
    use std::{
        io::Read,
        os::unix::fs::{MetadataExt, OpenOptionsExt},
    };
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| {
            err(
                ErrorCode::Unavailable,
                "Credential key is unavailable; vault remains locked",
            )
        })?;
    let meta = file.metadata().map_err(|_| {
        err(
            ErrorCode::Unavailable,
            "Credential key metadata unavailable",
        )
    })?;
    if !meta.is_file() || meta.mode() & 0o077 != 0 || meta.uid() != unsafe { libc::geteuid() } {
        return Err(err(
            ErrorCode::Unauthorized,
            "Credential key file must be owned by this user and private",
        ));
    }
    let mut key = zeroize::Zeroizing::new(Vec::new());
    file.take(33)
        .read_to_end(&mut key)
        .map_err(|_| err(ErrorCode::Unavailable, "Credential key read failed"))?;
    if key.len() != 32 {
        return Err(err(
            ErrorCode::Invalid,
            "Credential key file must contain exactly 32 raw bytes",
        ));
    }
    Ok(SecretBytes::new(std::mem::take(&mut *key)))
}
#[async_trait::async_trait]
impl KeyProtector for FileKeys {
    async fn unlock(&self, key_id: &str, control: &Control<'_>) -> Result<SecretBytes> {
        check(control)?;
        let path = self.0.get(key_id).cloned().ok_or_else(|| {
            err(
                ErrorCode::Unavailable,
                "Credential key mapping unavailable; vault remains locked",
            )
        })?;
        let task = tokio::task::spawn_blocking(move || read_key(&path));
        let result = crate::crypto::controlled(control, async {
            task.await
                .map_err(|_| err(ErrorCode::Unavailable, "Credential key worker unavailable"))?
        })
        .await?;
        check(control)?;
        Ok(result)
    }
}
/// Intended for embedded callers and disposable tests. Ownership and persistence belong to the caller.
pub struct SuppliedKey {
    id: String,
    key: SecretBytes,
}
impl SuppliedKey {
    pub fn new(id: &str, key: SecretBytes) -> Self {
        Self { id: id.into(), key }
    }
}
#[async_trait::async_trait]
impl KeyProtector for SuppliedKey {
    async fn unlock(&self, id: &str, control: &Control<'_>) -> Result<SecretBytes> {
        check(control)?;
        if id != self.id {
            return Err(err(ErrorCode::Unavailable, "Credential key unavailable"));
        }
        Ok(SecretBytes::new(self.key.expose().to_vec()))
    }
}
// Legacy macOS Keychain ignores per-item authentication UI flags. Serialize
// this crate's interactive provisioning and background reads while setting its
// process-local interaction policy, restoring the previous policy on return.
#[cfg(target_os = "macos")]
static KEYCHAIN_ACCESS: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Take the process-wide Keychain lock for a non-interactive read, waiting briefly for
/// another read to finish. The lock guards no data, only the process-local interaction
/// policy, so a panic in another thread (poisoning) must not lock the vault for good.
/// Gives up quickly: an interactive authorization can hold it while a prompt is open.
#[cfg(target_os = "macos")]
fn acquire_keychain(wait: std::time::Duration) -> Option<std::sync::MutexGuard<'static, ()>> {
    acquire_lock(&KEYCHAIN_ACCESS, wait)
}

#[cfg(target_os = "macos")]
fn acquire_lock(lock: &std::sync::Mutex<()>, wait: std::time::Duration) -> Option<std::sync::MutexGuard<'_, ()>> {
    let deadline = std::time::Instant::now() + wait;
    loop {
        match lock.try_lock() {
            Ok(guard) => return Some(guard),
            Err(std::sync::TryLockError::Poisoned(poisoned)) => return Some(poisoned.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => {
                if std::time::Instant::now() >= deadline {
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
    }
}
#[cfg(target_os = "macos")]
pub struct MacKeychain {
    pub service: String,
}
#[cfg(target_os = "macos")]
impl MacKeychain {
    /// Explicit, user-started authorization only. Never called by background reads.
    pub async fn authorize(&self, key_id: &str) -> Result<SecretBytes> {
        let service = self.service.clone();
        let account = key_id.to_owned();
        tokio::task::spawn_blocking(move || {
            let _access = KEYCHAIN_ACCESS.try_lock().map_err(|_|err(ErrorCode::Unavailable,"Another Keychain operation is in progress"))?;
            let value = security_framework::passwords::get_generic_password(&service,&account)
                .map(SecretBytes::new)
                .map_err(|_|err(ErrorCode::Unavailable,"Keychain authorization was denied or unavailable. Saved credentials were not changed."))?;
            if value.expose().len()!=32 { return Err(err(ErrorCode::VerificationFailed,"Invalid Keychain credential key")); }
            Ok(value)
        }).await.map_err(|_|err(ErrorCode::Unavailable,"Keychain worker unavailable"))?
    }
    /// Explicit initialization only. Never replace an existing key or fall back to disk.
    pub async fn provision(&self, key_id: &str) -> Result<()> {
        let service = self.service.clone();
        let account = key_id.to_owned();
        tokio::task::spawn_blocking(move || {
            let _access = KEYCHAIN_ACCESS.lock().map_err(|_|err(ErrorCode::Unavailable,"Keychain worker unavailable"))?;
            match security_framework::passwords::get_generic_password(&service, &account) {
                Ok(value) => {
                    let value = SecretBytes::new(value);
                    if value.expose().len() != 32 {
                        return Err(err(
                            ErrorCode::VerificationFailed,
                            "Invalid Keychain credential key",
                        ));
                    }
                    Ok(())
                }
                Err(e) if e.code() == -25300 => {
                    let key = SecretBytes::new(crate::crypto::random::<32>()?.to_vec());
                    // Add-only: concurrent initialization must never overwrite an existing master key.
                    let keychain = security_framework::os::macos::keychain::SecKeychain::default()
                        .map_err(|_| err(ErrorCode::Unavailable, "Keychain unavailable"))?;
                    match keychain.add_generic_password(&service, &account, key.expose()) {
                        Ok(()) => Ok(()),
                        Err(e) if e.code() == -25299 => {
                            let stored = security_framework::passwords::get_generic_password(
                                &service, &account,
                            )
                            .map(SecretBytes::new)
                            .map_err(|_| err(ErrorCode::Unavailable, "Keychain key unavailable"))?;
                            if stored.expose().len() != 32 {
                                return Err(err(
                                    ErrorCode::VerificationFailed,
                                    "Invalid Keychain credential key",
                                ));
                            }
                            Ok(())
                        }
                        Err(_) => Err(err(
                            ErrorCode::Unavailable,
                            "Cannot provision Keychain credential key",
                        )),
                    }
                }
                Err(_) => Err(err(
                    ErrorCode::Unavailable,
                    "Keychain unavailable; vault remains locked",
                )),
            }
        })
        .await
        .map_err(|_| err(ErrorCode::Unavailable, "Keychain worker unavailable"))?
    }
}
#[cfg(target_os = "macos")]
#[async_trait::async_trait]
impl KeyProtector for MacKeychain {
    async fn unlock(&self, id: &str, control: &Control<'_>) -> Result<SecretBytes> {
        check(control)?;
        let service = self.service.clone();
        let account = id.to_owned();
        let task = tokio::task::spawn_blocking(move || {
            let _access = acquire_keychain(std::time::Duration::from_secs(2))
                .ok_or_else(|| err(ErrorCode::Unavailable, "Keychain worker unavailable"))?;
            use security_framework::os::macos::keychain::SecKeychain;
            let interaction = SecKeychain::user_interaction_allowed()
                .map_err(|_|err(ErrorCode::Unavailable,"Keychain interaction policy unavailable"))?;
            let _no_ui = if interaction {
                Some(SecKeychain::disable_user_interaction()
                    .map_err(|_|err(ErrorCode::Unavailable,"Cannot suppress background Keychain prompts"))?)
            } else { None };
            let locked = |status| err(ErrorCode::Unavailable,
                &format!("Saved credentials are locked. Open Settings > Models and choose Unlock saved credentials. Your chats remain available. (Keychain status {status})"));
            // Use the same lookup as explicit authorization. The process-local
            // no-UI guard still rejects any read that needs an operator prompt.
            security_framework::passwords::get_generic_password(&service, &account)
                .map(SecretBytes::new)
                .map_err(|error| locked(error.code()))
        });
        let value = crate::crypto::controlled(control, async {
            task.await
                .map_err(|_| err(ErrorCode::Unavailable, "Keychain worker unavailable"))?
        })
        .await?;
        if value.expose().len() != 32 {
            return Err(err(
                ErrorCode::VerificationFailed,
                "Invalid Keychain credential key",
            ));
        }
        check(control)?;
        Ok(value)
    }
}

#[cfg(all(test, target_os = "macos"))]
mod noninteractive_keychain_tests {
    use super::*;
    #[tokio::test]
    async fn missing_key_fails_closed_without_provisioning_or_prompting() {
        let interaction_before = security_framework::os::macos::keychain::SecKeychain::user_interaction_allowed().unwrap();
        let keychain = MacKeychain { service: format!("com.kageha.test.absent.{}", std::process::id()) };
        let cancelled = std::sync::atomic::AtomicBool::new(false);
        let control = Control { cancelled: &cancelled, deadline: Some(std::time::Instant::now()+std::time::Duration::from_secs(2)) };
        let error = keychain.unlock("nonexistent-master", &control).await.err().expect("No key may be generated on resolve");
        assert_eq!(error.code, ErrorCode::Unavailable);
        assert!(error.message.contains("chats remain available"));
        assert_eq!(security_framework::os::macos::keychain::SecKeychain::user_interaction_allowed().unwrap(), interaction_before);
    }
}

#[cfg(all(test, target_os = "macos"))]
mod keychain_lock_tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    #[test]
    fn a_second_reader_waits_for_the_first_instead_of_failing() {
        let lock = Arc::new(Mutex::new(()));
        let first = lock.lock().unwrap();
        let other = lock.clone();
        let waiter = std::thread::spawn(move || {
            let started = Instant::now();
            let acquired = acquire_lock(&other, Duration::from_secs(2)).is_some();
            (acquired, started.elapsed())
        });
        std::thread::sleep(Duration::from_millis(100));
        drop(first);
        let (acquired, waited) = waiter.join().unwrap();
        assert!(acquired, "the waiter must get the lock once it is released");
        assert!(waited >= Duration::from_millis(80), "it must actually have waited: {waited:?}");
    }

    #[test]
    fn a_lock_that_stays_held_is_reported_after_the_wait() {
        let lock = Mutex::new(());
        let _held = lock.lock().unwrap();
        let started = Instant::now();
        assert!(acquire_lock(&lock, Duration::from_millis(60)).is_none());
        assert!(started.elapsed() >= Duration::from_millis(60));
    }

    #[test]
    fn a_poisoned_lock_is_still_usable() {
        let lock = Arc::new(Mutex::new(()));
        let poisoner = lock.clone();
        let result = std::thread::spawn(move || {
            let _guard = poisoner.lock().unwrap();
            panic!("poison the lock");
        })
        .join();
        assert!(result.is_err());
        assert!(lock.is_poisoned());
        assert!(acquire_lock(&lock, Duration::from_millis(50)).is_some());
    }
}
