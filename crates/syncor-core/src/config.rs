use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Result, SyncorError};
use crate::link::{LinkId, LinkInfo};

// ---------------------------------------------------------------------------
// SyncorPaths
// ---------------------------------------------------------------------------

/// Canonical filesystem layout for Syncor's config and data directories.
///
/// Follows an XDG-like convention:
///   config: $HOME/.config/syncor/
///   data:   $HOME/.local/share/syncor/
#[derive(Debug, Clone)]
pub struct SyncorPaths {
    config_dir: PathBuf,
    data_dir: PathBuf,
}

impl SyncorPaths {
    /// Derive paths from the system home directory (uses `dirs` crate).
    pub fn new() -> Self {
        let home = dirs::home_dir().expect("unable to resolve home directory");
        Self::with_home(&home)
    }

    /// Derive paths from an explicit home directory — useful in tests.
    pub fn with_home(home: &Path) -> Self {
        let config_dir = home.join(".config").join("syncor");
        let data_dir = home.join(".local").join("share").join("syncor");
        Self {
            config_dir,
            data_dir,
        }
    }

    // --- Config-dir paths ---

    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    pub fn links_file(&self) -> PathBuf {
        self.config_dir.join("links.toml")
    }

    // --- Data-dir paths ---

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Unix-domain socket for daemon IPC.
    pub fn socket_path(&self) -> PathBuf {
        self.data_dir.join("syncor.sock")
    }

    /// PID file used to track the running daemon.
    pub fn pid_file(&self) -> PathBuf {
        self.data_dir.join("syncor.pid")
    }

    /// Log file for the daemon.
    pub fn log_file(&self) -> PathBuf {
        self.data_dir.join("syncor.log")
    }

    /// Root directory that holds per-link working directories.
    pub fn link_dir(&self) -> PathBuf {
        self.data_dir.join("links")
    }

    /// Working directory for a specific link's repo checkout.
    pub fn link_repo_dir(&self, link_id: &LinkId) -> PathBuf {
        self.link_dir().join(link_id.as_str())
    }

    /// SQLite database that holds link state / checkpoints.
    pub fn link_state_db(&self) -> PathBuf {
        self.data_dir.join("state.db")
    }

    /// Advisory lock file for a specific link.
    pub fn link_lock_file(&self, link_id: &LinkId) -> PathBuf {
        self.link_dir().join(format!("{}.lock", link_id.as_str()))
    }

    /// Create all required directories, returning an error on failure.
    pub fn ensure_dirs(&self) -> Result<()> {
        std::fs::create_dir_all(&self.config_dir)?;
        std::fs::create_dir_all(&self.data_dir)?;
        std::fs::create_dir_all(self.link_dir())?;
        Ok(())
    }
}

impl Default for SyncorPaths {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// SyncorConfig
// ---------------------------------------------------------------------------

/// Top-level application configuration (stored in `config.toml`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncorConfig {
    /// How long (seconds) to debounce filesystem events before acting.
    pub debounce_secs: u64,
    /// Default poll interval (seconds) for links that don't override it.
    pub default_poll_interval_secs: u64,
}

impl Default for SyncorConfig {
    fn default() -> Self {
        Self {
            debounce_secs: 2,
            default_poll_interval_secs: 60,
        }
    }
}

impl SyncorConfig {
    /// Load config from a TOML file.  If the file does not exist the default
    /// configuration is returned.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(path)?;
        let cfg: Self = toml::from_str(&raw).map_err(|e| SyncorError::Config(e.to_string()))?;
        Ok(cfg)
    }

    /// Persist the configuration to a TOML file, creating parent dirs as needed.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let raw = toml::to_string_pretty(self).map_err(|e| SyncorError::Config(e.to_string()))?;
        std::fs::write(path, raw)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// LinksRegistry
// ---------------------------------------------------------------------------

/// On-disk representation of the links file.
#[derive(Debug, Default, Serialize, Deserialize)]
struct LinksFile {
    #[serde(default)]
    links: Vec<LinkInfo>,
}

/// In-memory registry of all configured links, backed by `links.toml`.
#[derive(Debug, Default)]
pub struct LinksRegistry {
    /// Primary index: id → info.
    by_id: HashMap<String, LinkInfo>,
}

impl LinksRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load from a TOML file.  Returns an empty registry if the file does not
    /// exist.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::new());
        }
        let raw = std::fs::read_to_string(path)?;
        let file: LinksFile =
            toml::from_str(&raw).map_err(|e| SyncorError::Config(e.to_string()))?;
        let mut registry = Self::new();
        for info in file.links {
            registry.by_id.insert(info.id.as_str().to_owned(), info);
        }
        Ok(registry)
    }

    /// Persist the registry to a TOML file.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut links: Vec<&LinkInfo> = self.by_id.values().collect();
        // Stable ordering for deterministic output.
        links.sort_by_key(|l| l.id.as_str());
        let file = LinksFile {
            links: links.into_iter().cloned().collect(),
        };
        let raw = toml::to_string_pretty(&file).map_err(|e| SyncorError::Config(e.to_string()))?;
        std::fs::write(path, raw)?;
        Ok(())
    }

    /// Add a new link.  Returns an error if:
    /// - A link with the same id already exists.
    /// - Another link already points at the same `local_dir` (one-dir one-link).
    pub fn add(&mut self, info: LinkInfo) -> Result<()> {
        // Check id uniqueness.
        if self.by_id.contains_key(info.id.as_str()) {
            return Err(SyncorError::LinkAlreadyExists(format!(
                "id {} already registered",
                info.id
            )));
        }
        // Enforce the one-dir / one-link constraint.
        if self.get_by_dir(&info.local_dir).is_some() {
            return Err(SyncorError::LinkAlreadyExists(format!(
                "directory {} is already managed by another link",
                info.local_dir.display()
            )));
        }
        self.by_id.insert(info.id.as_str().to_owned(), info);
        Ok(())
    }

    /// Remove a link by its id.  Returns an error if not found.
    pub fn remove(&mut self, id: &LinkId) -> Result<()> {
        self.by_id
            .remove(id.as_str())
            .ok_or_else(|| SyncorError::LinkNotFound(id.to_string()))?;
        Ok(())
    }

    /// Look up a link by its human-readable name.
    pub fn get_by_name(&self, name: &str) -> Option<&LinkInfo> {
        self.by_id.values().find(|l| l.name == name)
    }

    /// Look up a link by its `local_dir`.
    pub fn get_by_dir(&self, dir: &Path) -> Option<&LinkInfo> {
        self.by_id.values().find(|l| l.local_dir == dir)
    }

    /// Look up a link by its `LinkId`.
    pub fn get_by_id(&self, id: &LinkId) -> Option<&LinkInfo> {
        self.by_id.get(id.as_str())
    }

    /// Iterate over all registered links.
    pub fn iter(&self) -> impl Iterator<Item = &LinkInfo> {
        self.by_id.values()
    }
}
