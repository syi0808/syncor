use crate::config::SyncorPaths;
use crate::error::{Result, SyncorError};
use crate::link::LinkInfo;
use crate::sync::catalog_merge::{checkpoint_wal, merge_catalogs};
use crate::sync::conflict::{detect_conflicts, FileAction, ManifestMap};
use crate::sync::save::SavePipeline;
use crate::sync::state::{ConflictRecord, StateDb, SyncState};
use crate::transport::{PullResult, PushResult, SyncTransport};
use chkpt_core::store::blob::bytes_to_hex;
use chkpt_core::store::catalog::MetadataCatalog;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// LinkLock — per-link advisory filesystem lock using fs4
// ---------------------------------------------------------------------------

use fs4::fs_std::FileExt;
use std::fs::File;

pub struct LinkLock {
    _file: File,
}

impl LinkLock {
    pub fn acquire(paths: &SyncorPaths, link: &LinkInfo) -> Result<Self> {
        let lock_path = paths.link_lock_file(&link.id);
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = File::options()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)?;
        file.try_lock_exclusive()
            .map_err(|_| SyncorError::LockHeld)?;
        Ok(Self { _file: file })
    }
}

// ---------------------------------------------------------------------------
// SyncEngine
// ---------------------------------------------------------------------------

pub struct SyncEngine {
    paths: SyncorPaths,
    transport: Box<dyn SyncTransport + Send + Sync>,
}

#[derive(Debug)]
pub struct PushSyncResult {
    pub snapshot_id: Option<String>,
    pub pushed: bool,
}

#[derive(Debug)]
pub struct PullSyncResult {
    pub restored: bool,
    pub files_restored: usize,
}

impl SyncEngine {
    pub fn new(paths: SyncorPaths, transport: Box<dyn SyncTransport + Send + Sync>) -> Self {
        Self { paths, transport }
    }

    /// Scan workspace and update the FileIndex so the next push skips unchanged files.
    fn update_file_index(&self, link: &LinkInfo, store_dir: &std::path::Path) -> Result<()> {
        use chkpt_core::index::{FileEntry, FileIndex};
        use chkpt_core::scanner::scan_workspace;
        use chkpt_core::store::blob::hash_path_bytes;

        let index_path = store_dir.join("index.bin");
        let mut index = FileIndex::open(&index_path)?;

        let scanned = scan_workspace(&link.local_dir, None)?;
        let mut entries = Vec::new();
        for file in &scanned {
            let hash = hash_path_bytes(&file.absolute_path, file.is_symlink)?;
            entries.push(FileEntry {
                path: file.relative_path.clone(),
                blob_hash: hash,
                size: file.size,
                mtime_secs: file.mtime_secs,
                mtime_nanos: file.mtime_nanos,
                inode: file.inode,
                mode: file.mode,
            });
        }

        let scanned_paths: std::collections::HashSet<&str> =
            scanned.iter().map(|f| f.relative_path.as_str()).collect();
        let all_indexed = index.all_paths()?;
        let removed: Vec<String> = all_indexed
            .into_iter()
            .filter(|p| !scanned_paths.contains(p.as_str()))
            .collect();

        index.apply_changes(&removed, &entries)?;
        Ok(())
    }

    fn store_dir(&self, link: &LinkInfo) -> PathBuf {
        self.paths
            .link_repo_dir(&link.id)
            .join("stores")
            .join(&link.name)
    }

    fn state_db(&self) -> Result<StateDb> {
        let path = self.paths.link_state_db();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        StateDb::open(path)
    }

    pub fn init_link(&self, link: &LinkInfo) -> Result<()> {
        self.transport.init_remote(link)?;
        let link_dir = self.paths.link_dir();
        std::fs::create_dir_all(&link_dir)?;
        Ok(())
    }

    /// Ensure syncor.toml in the repo dir lists this link.
    fn ensure_syncor_toml(&self, link: &LinkInfo) -> Result<()> {
        let repo_dir = self.paths.link_repo_dir(&link.id);
        let toml_path = repo_dir.join("syncor.toml");

        #[derive(serde::Serialize, serde::Deserialize, Default)]
        struct SyncorManifest {
            #[serde(default)]
            links: Vec<SyncorManifestLink>,
        }
        #[derive(serde::Serialize, serde::Deserialize)]
        struct SyncorManifestLink {
            name: String,
            #[serde(default)]
            created_at: Option<String>,
        }

        let mut manifest = if toml_path.exists() {
            let content = std::fs::read_to_string(&toml_path)?;
            toml::from_str(&content).unwrap_or_default()
        } else {
            SyncorManifest::default()
        };

        if !manifest.links.iter().any(|l| l.name == link.name) {
            manifest.links.push(SyncorManifestLink {
                name: link.name.clone(),
                created_at: Some(chrono::Utc::now().to_rfc3339()),
            });
            let content = toml::to_string_pretty(&manifest)
                .map_err(|e| SyncorError::Config(e.to_string()))?;
            std::fs::write(&toml_path, content)?;
        }

        Ok(())
    }

    /// Restore the latest snapshot to the local directory.
    /// Used for initial connect when the repo already has data.
    pub fn restore_latest(&self, link: &LinkInfo) -> Result<PullSyncResult> {
        let store_dir = self.store_dir(link);
        let catalog_path = store_dir.join("catalog.sqlite");

        if !catalog_path.exists() {
            return Ok(PullSyncResult {
                restored: false,
                files_restored: 0,
            });
        }

        let catalog = MetadataCatalog::open(&catalog_path)?;
        let latest = match catalog.latest_snapshot()? {
            Some(s) => s,
            None => {
                return Ok(PullSyncResult {
                    restored: false,
                    files_restored: 0,
                })
            }
        };

        use crate::sync::restore::RestorePipeline;
        let result = RestorePipeline::run(&latest.id, &store_dir, &link.local_dir)?;

        self.update_file_index(link, &store_dir)?;

        let db = self.state_db()?;
        let state = SyncState {
            link_id: link.id.as_str().to_string(),
            last_local_snapshot: Some(latest.id.clone()),
            last_remote_revision: None,
            last_synced_snapshot_id: Some(latest.id),
            last_sync_at: Some(chrono::Utc::now().to_rfc3339()),
        };
        db.upsert_sync_state(&state)?;

        Ok(PullSyncResult {
            restored: true,
            files_restored: result.files_restored,
        })
    }

    pub fn push(&self, link: &LinkInfo) -> Result<PushSyncResult> {
        let _lock = LinkLock::acquire(&self.paths, link)?;

        let store_dir = self.store_dir(link);

        // Save current workspace state into the store
        let save_result = SavePipeline::run(&link.local_dir, &store_dir, None)?;

        // Ensure syncor.toml lists this link
        self.ensure_syncor_toml(link)?;

        // Checkpoint WAL before pushing so git sees a single-file DB
        let catalog_path = store_dir.join("catalog.sqlite");
        if catalog_path.exists() {
            checkpoint_wal(&catalog_path)?;
        }

        // Push store to remote via transport
        let push_result = self.transport.push(link, &store_dir)?;

        let db = self.state_db()?;
        match push_result {
            PushResult::Success { revision } => {
                let state = SyncState {
                    link_id: link.id.as_str().to_string(),
                    last_local_snapshot: Some(save_result.snapshot_id.clone()),
                    last_remote_revision: Some(revision),
                    last_synced_snapshot_id: Some(save_result.snapshot_id.clone()),
                    last_sync_at: Some(chrono::Utc::now().to_rfc3339()),
                };
                db.upsert_sync_state(&state)?;
                db.append_log(link.id.as_str(), "push", "success", None)?;

                Ok(PushSyncResult {
                    snapshot_id: Some(save_result.snapshot_id),
                    pushed: true,
                })
            }
            PushResult::Conflict { details } => {
                db.append_log(link.id.as_str(), "push", "conflict", Some(&details.message))?;
                Err(SyncorError::Conflict(details.message))
            }
        }
    }

    pub fn pull(&self, link: &LinkInfo) -> Result<PullSyncResult> {
        let _lock = LinkLock::acquire(&self.paths, link)?;

        let store_dir = self.store_dir(link);
        let catalog_path = store_dir.join("catalog.sqlite");

        // Pull remote changes via transport
        let pull_result = self.transport.pull(link, &store_dir)?;

        match pull_result {
            PullResult::UpToDate => Ok(PullSyncResult {
                restored: false,
                files_restored: 0,
            }),
            PullResult::Conflict { details } => Err(SyncorError::Conflict(details.message)),
            PullResult::Success { revision } => {
                // The catalog in the repo dir (store_dir) is the remote catalog.
                // We keep a local copy under the link dir for merging.
                let local_catalog_path = self
                    .paths
                    .link_dir()
                    .join(link.id.as_str())
                    .join("catalog.sqlite");
                let remote_catalog_path = catalog_path.clone();

                if !local_catalog_path.exists() {
                    if let Some(parent) = local_catalog_path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::copy(&remote_catalog_path, &local_catalog_path)?;
                } else {
                    merge_catalogs(&local_catalog_path, &remote_catalog_path)?;
                }

                let catalog = MetadataCatalog::open(&local_catalog_path)?;
                let latest_remote = catalog
                    .latest_snapshot()?
                    .ok_or_else(|| SyncorError::Other("no snapshots in remote catalog".into()))?;

                let db = self.state_db()?;
                let sync_state = db.get_sync_state(link.id.as_str())?;

                // Build remote manifest map
                let remote_map: ManifestMap = catalog
                    .snapshot_manifest(&latest_remote.id)?
                    .into_iter()
                    .map(|e| (e.path, e.blob_hash))
                    .collect();

                // Build base manifest map from last synced snapshot
                let base_map: ManifestMap = match sync_state
                    .as_ref()
                    .and_then(|s| s.last_synced_snapshot_id.as_deref())
                {
                    Some(base_id) => catalog
                        .snapshot_manifest(base_id)
                        .unwrap_or_default()
                        .into_iter()
                        .map(|e| (e.path, e.blob_hash))
                        .collect(),
                    None => std::collections::HashMap::new(),
                };

                // Scan local workspace to build local manifest map
                let local_map: ManifestMap = {
                    use chkpt_core::scanner::scan_workspace;
                    use chkpt_core::store::blob::hash_path_bytes;
                    let scanned = scan_workspace(&link.local_dir, None)?;
                    let mut map = std::collections::HashMap::new();
                    for file in &scanned {
                        let hash = hash_path_bytes(&file.absolute_path, file.is_symlink)?;
                        map.insert(file.relative_path.clone(), hash);
                    }
                    map
                };

                // Three-point conflict detection
                let actions = detect_conflicts(&base_map, &local_map, &remote_map);

                // Check for conflicts
                let conflicts: Vec<_> = actions
                    .iter()
                    .filter_map(|a| {
                        if let FileAction::Conflict(c) = a {
                            Some(c.clone())
                        } else {
                            None
                        }
                    })
                    .collect();

                if !conflicts.is_empty() {
                    for c in &conflicts {
                        db.insert_conflict(&ConflictRecord {
                            link_id: link.id.as_str().to_string(),
                            file_path: c.path.clone(),
                            local_hash: c.local_hash.map(|h| bytes_to_hex(&h)),
                            remote_hash: c.remote_hash.map(|h| bytes_to_hex(&h)),
                            base_hash: c.base_hash.map(|h| bytes_to_hex(&h)),
                        })?;
                    }
                    db.append_log(
                        link.id.as_str(),
                        "pull",
                        "conflict",
                        Some(&format!("{} conflicts", conflicts.len())),
                    )?;
                    return Err(SyncorError::Conflict(format!(
                        "{} file(s) in conflict. Run 'syncor resolve' to fix.",
                        conflicts.len()
                    )));
                }

                let remote_modes: std::collections::HashMap<String, u32> = catalog
                    .snapshot_manifest(&latest_remote.id)?
                    .into_iter()
                    .map(|e| (e.path, e.mode))
                    .collect();

                // Apply non-conflicting actions
                let pack_set =
                    chkpt_core::store::pack::PackSet::open_all(&store_dir.join("packs"))?;
                let mut files_restored = 0;
                for action in &actions {
                    match action {
                        FileAction::ApplyRemote { path, remote_hash } => {
                            let hash_hex = bytes_to_hex(remote_hash);
                            let content = pack_set.read(&hash_hex)?;
                            let file_path = link.local_dir.join(path);
                            if let Some(parent) = file_path.parent() {
                                std::fs::create_dir_all(parent)?;
                            }
                            std::fs::write(&file_path, content)?;
                            #[cfg(unix)]
                            if let Some(&mode) = remote_modes.get(path) {
                                use std::os::unix::fs::PermissionsExt;
                                let perms = std::fs::Permissions::from_mode(mode);
                                std::fs::set_permissions(&file_path, perms)?;
                            }
                            files_restored += 1;
                        }
                        FileAction::DeleteLocal { path } => {
                            let file_path = link.local_dir.join(path);
                            let _ = std::fs::remove_file(&file_path);
                        }
                        FileAction::Conflict(_) => {} // already handled above
                    }
                }

                self.update_file_index(link, &store_dir)?;

                // Copy merged catalog back to repo dir
                checkpoint_wal(&local_catalog_path)?;
                std::fs::copy(&local_catalog_path, &remote_catalog_path)?;

                let state = SyncState {
                    link_id: link.id.as_str().to_string(),
                    last_local_snapshot: Some(latest_remote.id.clone()),
                    last_remote_revision: Some(revision),
                    last_synced_snapshot_id: Some(latest_remote.id),
                    last_sync_at: Some(chrono::Utc::now().to_rfc3339()),
                };
                db.upsert_sync_state(&state)?;
                db.append_log(link.id.as_str(), "pull", "success", None)?;

                Ok(PullSyncResult {
                    restored: true,
                    files_restored,
                })
            }
        }
    }
}
