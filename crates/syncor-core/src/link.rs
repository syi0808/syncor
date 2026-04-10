use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;

/// A stable, deterministic identifier for a link derived from (repo, name).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LinkId(String);

impl LinkId {
    /// Create a deterministic ID from repo and name components.
    pub fn from_parts(repo: &str, name: &str) -> Self {
        use chkpt_core::store::blob::{bytes_to_hex, hash_content_bytes};
        let input = format!("{}\0{}", repo, name);
        let hash = hash_content_bytes(input.as_bytes());
        Self(bytes_to_hex(&hash))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for LinkId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// How the link syncs changes between the local directory and the repo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LinkMode {
    /// Local changes are pushed to the repo.
    Push,
    /// Repo changes are pulled to the local directory.
    Pull,
}

/// All persistent metadata about a single link.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkInfo {
    pub id: LinkId,
    pub name: String,
    pub repo: String,
    pub local_dir: PathBuf,
    pub mode: LinkMode,
    pub poll_interval_secs: Option<u64>,
}

/// Runtime state of a link (not persisted, tracked in-memory / SQLite).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum LinkState {
    #[default]
    Idle,
    Syncing,
    Conflict,
    Error(String),
}
