//! The credential vault as the learning pipeline's [`SecretSource`] (spec
//! autonomous-agent-loop, task 23.2; design section 15 "Safety"; Requirements 15.6 and
//! 19.6).
//!
//! [`VaultSecrets`] reads the values held in the credential vault
//! (`ui_credentials::vault_values`: each published, active credential's value, or the
//! string fields of a compound one; at least 8 characters). The reflection worker passes
//! it to `SqliteLearning::store_lessons` and to the skill promoter, which drop a lesson
//! or skill holding a value; the Run_Journal worker redacts values from entries.
//!
//! Values stay in memory only. A source keeps the values it last read while the vault's
//! published references are unchanged (`ui_credentials::vault_generation`) and for at
//! most [`CACHE_FOR`], so the journal worker does not decrypt the vault for every entry.
//! [`Zeroizing`] clears them when they are replaced or the source is dropped. Errors
//! never hold a value.
use crate::{agent_host::DatabaseState, webui::Database};
use kageha_learning::secrets::SecretSource;
use std::{
    io,
    sync::{Arc, Mutex, Weak},
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

/// Longest time a source reuses the values it read.
pub(crate) const CACHE_FOR: Duration = Duration::from_secs(60);

/// The Host's credential vault as a [`SecretSource`].
pub(crate) struct VaultSecrets {
    db: Weak<DatabaseState>,
    cache: Mutex<Option<Cached>>,
}

struct Cached {
    generation: u64,
    read: Instant,
    values: Vec<Zeroizing<String>>,
}

impl VaultSecrets {
    pub(crate) fn new(db: &Database) -> Self {
        Self { db: Arc::downgrade(db), cache: Mutex::new(None) }
    }
}

impl SecretSource for VaultSecrets {
    fn values(&self) -> io::Result<Vec<Zeroizing<String>>> {
        let db = self.db.upgrade().ok_or_else(|| io::Error::other("The host database is closed"))?;
        let connection = db.lock().map_err(|_| io::Error::other("database unavailable"))?;
        let unavailable =
            |(_, message): (u16, String)| io::Error::other(format!("The credential vault could not be read: {message}"));
        let generation = crate::ui_credentials::vault_generation(&connection).map_err(unavailable)?;
        let mut cache = self.cache.lock().unwrap_or_else(|poison| poison.into_inner());
        if let Some(cached) = cache.as_ref().filter(|cached| cached.generation == generation && cached.read.elapsed() < CACHE_FOR) {
            return Ok(cached.values.clone());
        }
        *cache = None;
        let values = crate::ui_credentials::vault_values(&connection).map_err(unavailable)?;
        *cache = Some(Cached { generation, read: Instant::now(), values: values.clone() });
        Ok(values)
    }
}

#[cfg(test)]
#[path = "ui_learning_secrets_tests.rs"]
mod excluded_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_vault_has_no_values_and_a_closed_database_is_an_error() {
        let fixture = crate::audited_session_tests::Fixture::new();
        let source = VaultSecrets::new(&fixture.db);
        assert!(source.values().unwrap().is_empty());
        let closed = VaultSecrets { db: Weak::new(), cache: Mutex::new(None) };
        assert!(closed.values().is_err());
    }
}
