use crate::error::Result;
use chkpt_core::index::{FileEntry, FileIndex};
use chkpt_core::scanner::scan_workspace;
use chkpt_core::store::blob::{bytes_to_hex, hash_content_bytes, read_or_mmap};
use chkpt_core::store::catalog::{BlobLocation, CatalogSnapshot, ManifestEntry, MetadataCatalog};
use chkpt_core::store::pack::PackWriter;
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
    pub fn run(workspace: &Path, store_dir: &Path, message: Option<&str>) -> Result<SaveResult> {
        // 1. Ensure store directories
        let packs_dir = store_dir.join("packs");
        std::fs::create_dir_all(&packs_dir)?;
        std::fs::create_dir_all(store_dir.join("trees"))?;

        // 2. Scan workspace
        let scanned = scan_workspace(workspace, None)?;
        let files_scanned = scanned.len();

        // 3. Load FileIndex
        let index_path = store_dir.join("index.bin");
        let mut file_index = FileIndex::open(&index_path)?;

        // 4. Compare scanned files against index to find changed files
        let mut changed_files = Vec::new();
        for sf in &scanned {
            if let Ok(Some(entry)) = file_index.get(&sf.relative_path) {
                // Check if size or mtime changed
                if entry.size == sf.size
                    && entry.mtime_secs == sf.mtime_secs
                    && entry.mtime_nanos == sf.mtime_nanos
                {
                    continue;
                }
            }
            changed_files.push(sf);
        }

        // 5. Hash and compress changed files, add to pack
        let mut pack_writer = PackWriter::new(&packs_dir)?;
        let mut blob_locations: Vec<([u8; 16], BlobLocation)> = Vec::new();
        let mut new_entries: Vec<FileEntry> = Vec::new();
        let mut bytes_compressed: u64 = 0;
        let files_hashed = changed_files.len();

        for sf in &changed_files {
            let content = read_or_mmap(&sf.absolute_path)?;
            let hash_bytes = hash_content_bytes(content.as_ref());
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

        // 6. Finish pack if not empty
        let pack_hash = if !pack_writer.is_empty() {
            Some(pack_writer.finish()?)
        } else {
            // Drop the empty pack writer without finishing
            drop(pack_writer);
            None
        };

        // 7. Update blob locations with pack hash and upsert to catalog
        if let Some(ref ph) = pack_hash {
            for (_, loc) in &mut blob_locations {
                loc.pack_hash = Some(ph.clone());
            }
        }

        let catalog_path = store_dir.join("catalog.sqlite");
        let catalog = MetadataCatalog::open(&catalog_path)?;

        if !blob_locations.is_empty() {
            catalog.bulk_upsert_blob_locations(&blob_locations)?;
        }

        // 8. Build manifest: unchanged from index + newly hashed
        let mut manifest: Vec<ManifestEntry> = Vec::new();
        let mut total_bytes: u64 = 0;

        // Build a set of changed paths for quick lookup
        let changed_paths: std::collections::HashSet<&str> = changed_files
            .iter()
            .map(|sf| sf.relative_path.as_str())
            .collect();

        // Add unchanged files from index
        for sf in &scanned {
            if !changed_paths.contains(sf.relative_path.as_str()) {
                if let Ok(Some(entry)) = file_index.get(&sf.relative_path) {
                    manifest.push(ManifestEntry {
                        path: entry.path.clone(),
                        blob_hash: entry.blob_hash,
                        size: entry.size,
                        mode: entry.mode,
                    });
                    total_bytes += entry.size;
                }
            }
        }

        // Add newly hashed files
        for entry in &new_entries {
            manifest.push(ManifestEntry {
                path: entry.path.clone(),
                blob_hash: entry.blob_hash,
                size: entry.size,
                mode: entry.mode,
            });
            total_bytes += entry.size;
        }

        // 9. Create CatalogSnapshot with UUIDv7, insert_snapshot
        let snapshot_id = Uuid::now_v7().to_string();
        let parent_snapshot = catalog.latest_snapshot()?;

        let snapshot = CatalogSnapshot {
            id: snapshot_id.clone(),
            created_at: Utc::now(),
            message: message.map(|s| s.to_string()),
            parent_snapshot_id: parent_snapshot.map(|s| s.id),
            manifest_snapshot_id: None,
            root_tree_hash: None,
            stats: SnapshotStats {
                total_files: files_scanned as u64,
                total_bytes,
                new_objects: files_hashed as u64,
            },
        };

        catalog.insert_snapshot(&snapshot, &manifest)?;

        // 10. Update FileIndex with apply_changes
        // Remove paths that are no longer in workspace
        let scanned_paths: std::collections::HashSet<String> =
            scanned.iter().map(|sf| sf.relative_path.clone()).collect();
        let index_paths = file_index.all_paths()?;
        let removed_paths: Vec<String> = index_paths
            .into_iter()
            .filter(|p| !scanned_paths.contains(p))
            .collect();

        // Build all entries to upsert (unchanged keep their existing entry, changed get new)
        let entries_to_upsert: Vec<FileEntry> = new_entries;

        file_index.apply_changes(&removed_paths, &entries_to_upsert)?;

        Ok(SaveResult {
            snapshot_id,
            files_scanned,
            files_hashed,
            bytes_compressed,
        })
    }
}
