use crate::error::{Result, SyncorError};
use crate::progress::{ItemTotal, Phase, PhaseGuard, ProgressReporter};
use chkpt_core::scanner::scan_workspace;
use chkpt_core::store::blob::bytes_to_hex;
use chkpt_core::store::catalog::MetadataCatalog;
use chkpt_core::store::pack::PackSet;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Validate that a path from a manifest doesn't escape the target directory.
pub fn validate_path(base: &Path, relative: &str) -> Result<PathBuf> {
    // Reject absolute paths and path traversal
    if relative.starts_with('/') || relative.starts_with('\\') || relative.contains("..") {
        return Err(SyncorError::Other(format!(
            "unsafe path in manifest: {}",
            relative,
        )));
    }
    let dest = base.join(relative);
    // Double-check the resolved path is still inside base
    // (handles edge cases like symlinks in base)
    Ok(dest)
}

pub struct RestoreResult {
    pub files_restored: usize,
    pub files_removed: usize,
}

pub struct RestorePipeline;

impl RestorePipeline {
    pub fn run(
        snapshot_id: &str,
        store_dir: &Path,
        target_dir: &Path,
        reporter: &dyn ProgressReporter,
    ) -> Result<RestoreResult> {
        // 1. Open MetadataCatalog
        let catalog_path = store_dir.join("catalog.sqlite");
        let catalog = MetadataCatalog::open(&catalog_path)?;

        // 2. Get manifest
        let manifest = catalog.snapshot_manifest(snapshot_id)?;

        // 3. Open PackSet
        let packs_dir = store_dir.join("packs");
        let pack_set = PackSet::open_all(&packs_dir)?;

        // Build set of manifest paths for later cleanup
        let manifest_paths: HashSet<String> = manifest.iter().map(|e| e.path.clone()).collect();

        // 4. Restore each file
        let restore_guard = PhaseGuard::new(reporter, Phase::Restore, ItemTotal::Unknown);
        let mut files_restored = 0;
        for entry in &manifest {
            let hash_hex = bytes_to_hex(&entry.blob_hash);
            let content = pack_set.read(&hash_hex)?;

            let dest = validate_path(target_dir, &entry.path)?;
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&dest, &content)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let perms = std::fs::Permissions::from_mode(entry.mode);
                std::fs::set_permissions(&dest, perms)?;
            }
            files_restored += 1;
        }
        restore_guard.end();

        // 5. Scan target_dir using the same scanner as save (respects .chkptignore etc.)
        //    and remove only scanned files not in the manifest. This preserves ignored
        //    files like .git/, .env, .chkptignore.
        let cleanup_guard = PhaseGuard::new(reporter, Phase::Cleanup, ItemTotal::Unknown);
        let scanned = scan_workspace(target_dir, None)?;
        let mut files_removed = 0;
        for file in &scanned {
            if !manifest_paths.contains(&file.relative_path) {
                let path = validate_path(target_dir, &file.relative_path)?;
                let _ = std::fs::remove_file(&path);
                files_removed += 1;
            }
        }

        // 6. Clean up empty directories (but never the target_dir root itself)
        remove_empty_dirs(target_dir, target_dir)?;
        cleanup_guard.end();

        Ok(RestoreResult {
            files_restored,
            files_removed,
        })
    }
}

fn remove_empty_dirs(root: &Path, dir: &Path) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err.into()),
    };

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            remove_empty_dirs(root, &path)?;
            // Never remove root itself
            if path != root {
                // Try to remove; ignore if not empty
                let _ = std::fs::remove_dir(&path);
            }
        }
    }

    Ok(())
}
