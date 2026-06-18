//! Framework-independent encrypted credential storage. Hosting is optional.
pub use kageha_contracts::credentials::*;
#[cfg(feature = "client")]
pub mod client;
pub mod crypto;
pub mod keys;
pub mod oauth;
pub mod recovery;
pub mod registry;
pub mod repository;
#[cfg(feature = "service")]
pub mod service;
pub mod transfer;
pub mod vault;
#[cfg(any(feature = "service", feature = "client"))]
mod wire;
pub use vault::Vault;
#[cfg(feature = "client")]
pub mod config;
pub mod sources;
