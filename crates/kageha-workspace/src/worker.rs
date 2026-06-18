//! Portable worker workspace policy. Hosts retain the admitted policy in run specs.
mod manager;
pub use manager::{Manager, Snapshot};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Options {
    pub isolation: String,
    pub keep_worktree: bool,
    pub base_ref: String,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            isolation: "shared".into(),
            keep_worktree: true,
            base_ref: "HEAD".into(),
        }
    }
}
impl Options {
    pub fn parse(value: &Value) -> io::Result<Self> {
        let mut options = Self::default();
        if let Some(v) = value.get("isolation") {
            options.isolation = match v.as_str() {
                Some("worktree" | "git" | "wt") => "worktree",
                Some("shared" | "none") => "shared",
                _ => {
                    return Err(io::Error::other(
                        "Unsupported specialist workspace isolation",
                    ))
                }
            }
            .into();
        }
        if let Some(v) = value.get("keep_worktree") {
            options.keep_worktree = v
                .as_bool()
                .ok_or_else(|| io::Error::other("keep_worktree must be boolean"))?;
        }
        if let Some(v) = value.get("base_ref") {
            options.base_ref = v
                .as_str()
                .filter(|s| {
                    !s.is_empty()
                        && s.len() <= 256
                        && !s.starts_with('-')
                        && !s.chars().any(char::is_control)
                })
                .ok_or_else(|| io::Error::other("Invalid worker base_ref"))?
                .into();
        }
        Ok(options)
    }
    pub fn isolated(&self) -> bool {
        self.isolation == "worktree"
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn isolation_is_explicit_validated_and_retained_by_default() {
        for alias in ["worktree", "git", "wt"] {
            let o = Options::parse(&json!({"isolation":alias})).unwrap();
            assert!(o.isolated());
            assert!(o.keep_worktree);
        }
        assert_eq!(Options::parse(&json!({})).unwrap(), Options::default());
        for bad in [
            json!({"isolation":"typo"}),
            json!({"keep_worktree":"false"}),
            json!({"base_ref":"--help"}),
        ] {
            assert!(Options::parse(&bad).is_err());
        }
    }
}
