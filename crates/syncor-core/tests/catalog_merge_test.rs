use chkpt_core::store::catalog::{BlobLocation, CatalogSnapshot, ManifestEntry, MetadataCatalog};
use chkpt_core::store::snapshot::SnapshotStats;
use chrono::Utc;
use syncor_core::sync::catalog_merge::merge_catalogs;
use tempfile::TempDir;

fn make_snapshot(catalog: &MetadataCatalog, id: &str, files: &[(&str, [u8; 16])]) {
    let manifest: Vec<ManifestEntry> = files
        .iter()
        .map(|(path, hash)| ManifestEntry {
            path: path.to_string(),
            blob_hash: *hash,
            size: 100,
            mode: 0o644,
        })
        .collect();

    let blob_locs: Vec<([u8; 16], BlobLocation)> = files
        .iter()
        .map(|(_, hash)| {
            (
                *hash,
                BlobLocation {
                    pack_hash: Some("pack1".to_string()),
                    size: 100,
                },
            )
        })
        .collect();

    catalog.bulk_upsert_blob_locations(&blob_locs).unwrap();

    let snapshot = CatalogSnapshot {
        id: id.to_string(),
        created_at: Utc::now(),
        message: None,
        parent_snapshot_id: None,
        manifest_snapshot_id: None,
        root_tree_hash: None,
        stats: SnapshotStats {
            total_files: files.len() as u64,
            total_bytes: files.len() as u64 * 100,
            new_objects: files.len() as u64,
        },
    };
    catalog.insert_snapshot(&snapshot, &manifest).unwrap();
}

#[test]
fn merge_adds_remote_snapshots_to_local() {
    let local_dir = TempDir::new().unwrap();
    let remote_dir = TempDir::new().unwrap();

    let local_path = local_dir.path().join("catalog.db");
    let remote_path = remote_dir.path().join("catalog.db");

    let local_catalog = MetadataCatalog::open(&local_path).unwrap();
    let remote_catalog = MetadataCatalog::open(&remote_path).unwrap();

    let hash_a = [1u8; 16];
    let hash_b = [2u8; 16];

    make_snapshot(&local_catalog, "snap-local", &[("local.txt", hash_a)]);
    make_snapshot(&remote_catalog, "snap-remote", &[("remote.txt", hash_b)]);

    // Close catalogs before merging (drop connections)
    drop(local_catalog);
    drop(remote_catalog);

    merge_catalogs(&local_path, &remote_path).unwrap();

    // Reopen and verify
    let merged = MetadataCatalog::open(&local_path).unwrap();
    let snapshots = merged.list_snapshots(None).unwrap();
    let ids: Vec<&str> = snapshots.iter().map(|s| s.id.as_str()).collect();

    assert!(
        ids.contains(&"snap-local"),
        "local snapshot should be present"
    );
    assert!(
        ids.contains(&"snap-remote"),
        "remote snapshot should be merged in"
    );
    assert_eq!(snapshots.len(), 2);
}

#[test]
fn merge_is_idempotent() {
    let local_dir = TempDir::new().unwrap();
    let remote_dir = TempDir::new().unwrap();

    let local_path = local_dir.path().join("catalog.db");
    let remote_path = remote_dir.path().join("catalog.db");

    let local_catalog = MetadataCatalog::open(&local_path).unwrap();
    let remote_catalog = MetadataCatalog::open(&remote_path).unwrap();

    let hash_a = [1u8; 16];
    let hash_b = [2u8; 16];

    make_snapshot(&local_catalog, "snap-local", &[("local.txt", hash_a)]);
    make_snapshot(&remote_catalog, "snap-remote", &[("remote.txt", hash_b)]);

    drop(local_catalog);
    drop(remote_catalog);

    // Merge twice — should not fail or duplicate
    merge_catalogs(&local_path, &remote_path).unwrap();
    merge_catalogs(&local_path, &remote_path).unwrap();

    let merged = MetadataCatalog::open(&local_path).unwrap();
    let snapshots = merged.list_snapshots(None).unwrap();
    assert_eq!(snapshots.len(), 2, "no duplicates after double merge");
}

#[test]
fn merge_also_includes_blob_index() {
    let local_dir = TempDir::new().unwrap();
    let remote_dir = TempDir::new().unwrap();

    let local_path = local_dir.path().join("catalog.db");
    let remote_path = remote_dir.path().join("catalog.db");

    let local_catalog = MetadataCatalog::open(&local_path).unwrap();
    let remote_catalog = MetadataCatalog::open(&remote_path).unwrap();

    let hash_remote = [9u8; 16];

    make_snapshot(&remote_catalog, "snap-r", &[("file.txt", hash_remote)]);

    drop(local_catalog);
    drop(remote_catalog);

    merge_catalogs(&local_path, &remote_path).unwrap();

    let merged = MetadataCatalog::open(&local_path).unwrap();
    let loc = merged.blob_location(&hash_remote).unwrap();
    assert!(loc.is_some(), "blob_index entry should be merged");
    assert_eq!(loc.unwrap().pack_hash, Some("pack1".to_string()));
}

#[test]
fn merge_empty_remote_is_noop() {
    let local_dir = TempDir::new().unwrap();
    let remote_dir = TempDir::new().unwrap();

    let local_path = local_dir.path().join("catalog.db");
    let remote_path = remote_dir.path().join("catalog.db");

    let local_catalog = MetadataCatalog::open(&local_path).unwrap();
    let remote_catalog = MetadataCatalog::open(&remote_path).unwrap();

    let hash_a = [1u8; 16];
    make_snapshot(&local_catalog, "snap-local", &[("local.txt", hash_a)]);

    drop(local_catalog);
    drop(remote_catalog);

    merge_catalogs(&local_path, &remote_path).unwrap();

    let merged = MetadataCatalog::open(&local_path).unwrap();
    let snapshots = merged.list_snapshots(None).unwrap();
    assert_eq!(snapshots.len(), 1, "local snapshot should still be present");
}

#[cfg(unix)]
#[test]
fn merge_rejects_non_utf8_path() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    use std::path::PathBuf;

    let dir = TempDir::new().unwrap();
    let local_path = dir.path().join("local.sqlite");
    let local = MetadataCatalog::open(&local_path).unwrap();
    drop(local);

    let bad_name = OsStr::from_bytes(&[0xff, 0xfe]);
    let bad_path: PathBuf = dir.path().join(bad_name);

    let result = merge_catalogs(&local_path, &bad_path);
    assert!(result.is_err(), "should return error for non-UTF-8 path");
}
