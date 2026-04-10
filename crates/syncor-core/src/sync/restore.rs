use crate::error::Result;
use chkpt_core::store::blob::bytes_to_hex;
use chkpt_core::store::catalog::MetadataCatalog;
use chkpt_core::store::pack::PackSet;
use std::collections::HashSet;
use std::path::Path;

pub struct RestoreResult {
    pub files_restored: usize,
    pub files_removed: usize,
}

pub struct RestorePipeline;

impl RestorePipeline {
    pub fn run(snapshot_id: &str, store_dir: &Path, target_dir: &Path) -> Result<RestoreResult> {
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
        let mut files_restored = 0;
        for entry in &manifest {
            let hash_hex = bytes_to_hex(&entry.blob_hash);
            let content = pack_set.read(&hash_hex)?;

            let dest = target_dir.join(&entry.path);
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

        // 5. Walk target_dir and remove files not in manifest
        let mut files_removed = 0;
        remove_extra_files(target_dir, target_dir, &manifest_paths, &mut files_removed)?;

        // 6. Clean up empty directories (but never the target_dir root itself)
        remove_empty_dirs(target_dir, target_dir)?;

        Ok(RestoreResult {
            files_restored,
            files_removed,
        })
    }
}

fn remove_extra_files(
    root: &Path,
    dir: &Path,
    manifest_paths: &HashSet<String>,
    files_removed: &mut usize,
) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err.into()),
    };

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;

        if file_type.is_dir() {
            remove_extra_files(root, &path, manifest_paths, files_removed)?;
        } else {
            // Compute relative path from root
            let relative = path
                .strip_prefix(root)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();

            if !manifest_paths.contains(&relative) {
                std::fs::remove_file(&path)?;
                *files_removed += 1;
            }
        }
    }

    Ok(())
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
