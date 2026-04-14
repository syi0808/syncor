use serde::de::{self, Deserializer};
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
#[derive(Debug, Clone, Serialize)]
pub struct LinkInfo {
    pub id: LinkId,
    pub name: String,
    pub repo: String,
    pub local_dirs: Vec<PathBuf>,
    pub mode: LinkMode,
    pub poll_interval_secs: Option<u64>,
}

impl LinkInfo {
    /// Convenience: returns the first mount. Caller-safe by invariant
    /// (`local_dirs` is non-empty after registry validation) but panics
    /// if violated — use only in paths already gated by registry add/add_mount.
    pub fn primary_dir(&self) -> &std::path::Path {
        &self.local_dirs[0]
    }
}

#[derive(Deserialize)]
struct RawLinkInfo {
    id: LinkId,
    name: String,
    repo: String,
    #[serde(default)]
    local_dir: Option<PathBuf>,
    #[serde(default)]
    local_dirs: Option<Vec<PathBuf>>,
    mode: LinkMode,
    #[serde(default)]
    poll_interval_secs: Option<u64>,
}

impl<'de> Deserialize<'de> for LinkInfo {
    fn deserialize<D>(de: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawLinkInfo::deserialize(de)?;
        let local_dirs = match (raw.local_dir, raw.local_dirs) {
            (Some(_), Some(_)) => {
                return Err(de::Error::custom(
                    "link entry has both `local_dir` and `local_dirs`; use `local_dirs` only",
                ))
            }
            (Some(d), None) => vec![d],
            (None, Some(v)) => v,
            (None, None) => {
                return Err(de::Error::custom(
                    "link entry is missing `local_dirs` (or legacy `local_dir`)",
                ))
            }
        };
        Ok(LinkInfo {
            id: raw.id,
            name: raw.name,
            repo: raw.repo,
            local_dirs,
            mode: raw.mode,
            poll_interval_secs: raw.poll_interval_secs,
        })
    }
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
