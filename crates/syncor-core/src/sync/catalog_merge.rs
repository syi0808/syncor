use crate::error::Result;
use rusqlite::Connection;
use std::path::Path;

pub fn merge_catalogs(local_path: &Path, remote_path: &Path) -> Result<()> {
    let conn = Connection::open(local_path)?;
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
    let remote_str = remote_path.to_str().ok_or_else(|| {
        crate::error::SyncorError::Other("non-UTF-8 path for remote catalog".into())
    })?;
    conn.execute(
        "ATTACH DATABASE ?1 AS remote",
        [remote_str],
    )?;

    conn.execute_batch(
        "
        INSERT OR IGNORE INTO blob_index (blob_hash, pack_hash, size)
            SELECT blob_hash, pack_hash, size FROM remote.blob_index;

        INSERT OR IGNORE INTO snapshots (id, created_at, message, parent_snapshot_id, manifest_snapshot_id, root_tree_hash, total_files, total_bytes, new_objects)
            SELECT id, created_at, message, parent_snapshot_id, manifest_snapshot_id, root_tree_hash, total_files, total_bytes, new_objects FROM remote.snapshots;

        INSERT OR IGNORE INTO snapshot_files (snapshot_id, path, blob_hash, size, mode)
            SELECT snapshot_id, path, blob_hash, size, mode FROM remote.snapshot_files;
        ",
    )?;

    conn.execute_batch("DETACH DATABASE remote;")?;
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
    Ok(())
}

pub fn checkpoint_wal(catalog_path: &Path) -> Result<()> {
    let conn = Connection::open(catalog_path)?;
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
    Ok(())
}
