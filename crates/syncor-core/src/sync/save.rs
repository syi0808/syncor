use crate::error::Result;
use chkpt_core::index::{FileEntry, FileIndex};
use chkpt_core::scanner::scan_workspace;
use chkpt_core::store::blob::{bytes_to_hex, hash_content_bytes};
use chkpt_core::store::catalog::{BlobLocation, CatalogSnapshot, ManifestEntry, MetadataCatalog};
use chkpt_core::store::pack::{PackFinishOptions, PackWriter};
use chkpt_core::store::snapshot::SnapshotStats;
use chrono::Utc;
use std::io::Write;
use std::path::Path;
use uuid::Uuid;

pub struct SaveResult {
    pub snapshot_id: String,
    pub files_scanned: usize,
    pub files_hashed: usize,
    pub bytes_compressed: u64,
}

pub struct SavePipeline;

impl SavePipeline {
    pub fn run(workspace: &Path, store_dir: &Path, _message: Option<&str>) -> Result<SaveResult> {
        // 1. Ensure store directories exist
        let packs_dir = store_dir.join("packs");
        let trees_dir = store_dir.join("trees");
        let catalog_path = store_dir.join("catalog.sqlite");
        let index_path = store_dir.join("index.bin");
        std::fs::create_dir_all(&packs_dir)?;
        std::fs::create_dir_all(&trees_dir)?;

        // 2. Scan workspace
        let scanned = scan_workspace(workspace, None)?;
        let files_scanned = scanned.len();

        // 3. Load file index for incremental detection
        let mut index = FileIndex::open(&index_path)?;

        // 4. Find changed files (compare against index by size + mtime)
        let mut changed_files = Vec::new();
        for sf in &scanned {
            if let Ok(Some(entry)) = index.get(&sf.relative_path) {
                if entry.size == sf.size
                    && entry.mtime_secs == sf.mtime_secs
                    && entry.mtime_nanos == sf.mtime_nanos
                {
                    continue;
                }
            }
            changed_files.push(sf);
        }
        let files_hashed = changed_files.len();

        // Check if any files were removed since last index
        let scanned_paths: std::collections::HashSet<&str> =
            scanned.iter().map(|f| f.relative_path.as_str()).collect();
        let all_indexed = index.all_paths()?;
        let removed_paths: Vec<String> = all_indexed
            .into_iter()
            .filter(|p| !scanned_paths.contains(p.as_str()))
            .collect();

        // If nothing changed and nothing removed, skip snapshot creation
        if files_hashed == 0 && removed_paths.is_empty() {
            let catalog = MetadataCatalog::open(&catalog_path)?;
            let latest = catalog.latest_snapshot()?;
            return Ok(SaveResult {
                snapshot_id: latest.map(|s| s.id).unwrap_or_default(),
                files_scanned,
                files_hashed: 0,
                bytes_compressed: 0,
            });
        }

        // 5. Hash, compress, and pack changed files
        let mut pack_writer = PackWriter::new(&packs_dir)?;
        let mut blob_locations: Vec<([u8; 16], BlobLocation)> = Vec::new();
        let mut new_entries: Vec<FileEntry> = Vec::new();
        let mut bytes_compressed: u64 = 0;

        for sf in &changed_files {
            // Use read_path_bytes for symlink-aware reading (symlinks to dirs
            // would cause EISDIR with read_or_mmap).
            let content =
                chkpt_core::store::blob::read_path_bytes(&sf.absolute_path, sf.is_symlink)?;
            let hash_bytes = hash_content_bytes(&content);
            let hash_hex = bytes_to_hex(&hash_bytes);

            // LZ4 compress
            let mut encoder = lz4_flex::frame::FrameEncoder::new(Vec::new());
            encoder
                .write_all(content.as_ref())
                .map_err(crate::error::SyncorError::Io)?;
            let compressed = encoder
                .finish()
                .map_err(|e| crate::error::SyncorError::Other(e.to_string()))?;
            let compressed_len = compressed.len() as u64;
            bytes_compressed += compressed_len;

            pack_writer.add_pre_compressed(hash_hex.clone(), compressed)?;

            blob_locations.push((
                hash_bytes,
                BlobLocation {
                    pack_hash: None, // Will be set after finish
                    size: sf.size,
                },
            ));

            new_entries.push(FileEntry {
                path: sf.relative_path.clone(),
                blob_hash: hash_bytes,
                size: sf.size,
                mtime_secs: sf.mtime_secs,
                mtime_nanos: sf.mtime_nanos,
                inode: sf.inode,
                mode: sf.mode,
            });
        }

        // 6. Finish pack — chunk at 50 MB to stay under GitHub's 100 MB file limit
        let pack_hash = if !pack_writer.is_empty() {
            Some(pack_writer.finish_with_options(PackFinishOptions {
                chunk_bytes: Some(50_000_000),
            })?)
        } else {
            drop(pack_writer);
            None
        };

        // 7. Set pack_hash on blob locations
        if let Some(ref ph) = pack_hash {
            for (_, loc) in blob_locations.iter_mut() {
                loc.pack_hash = Some(ph.clone());
            }
        }

        // 8. Build manifest from index (unchanged) + new entries
        let mut manifest: Vec<ManifestEntry> = Vec::new();
        for sf in &scanned {
            if let Ok(Some(entry)) = index.get(&sf.relative_path) {
                if !new_entries.iter().any(|e| e.path == sf.relative_path) {
                    manifest.push(ManifestEntry {
                        path: entry.path.clone(),
                        blob_hash: entry.blob_hash,
                        size: entry.size,
                        mode: entry.mode,
                    });
                }
            }
        }
        for entry in &new_entries {
            manifest.push(ManifestEntry {
                path: entry.path.clone(),
                blob_hash: entry.blob_hash,
                size: entry.size,
                mode: entry.mode,
            });
        }
        manifest.sort_by(|a, b| a.path.cmp(&b.path));

        // 9. Open catalog and insert snapshot
        let catalog = MetadataCatalog::open(&catalog_path)?;
        let parent = catalog.latest_snapshot()?;

        if !blob_locations.is_empty() {
            catalog.bulk_upsert_blob_locations(&blob_locations)?;
        }

        let snapshot_id = Uuid::now_v7().to_string();
        let snapshot = CatalogSnapshot {
            id: snapshot_id.clone(),
            created_at: Utc::now(),
            message: None,
            parent_snapshot_id: parent.as_ref().map(|p| p.id.clone()),
            manifest_snapshot_id: None,
            root_tree_hash: None,
            stats: SnapshotStats {
                total_files: manifest.len() as u64,
                total_bytes: manifest.iter().map(|e| e.size).sum(),
                new_objects: files_hashed as u64,
            },
        };
        catalog.insert_snapshot(&snapshot, &manifest)?;

        // 10. Update file index
        index.apply_changes(&removed_paths, &new_entries)?;

        Ok(SaveResult {
            snapshot_id,
            files_scanned,
            files_hashed,
            bytes_compressed,
        })
    }
}
