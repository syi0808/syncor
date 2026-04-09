use crate::error::Result;
use rusqlite::{params, Connection};
use std::path::Path;

pub struct StateDb {
    conn: Connection,
}

#[derive(Debug, Clone)]
pub struct SyncState {
    pub link_id: String,
    pub last_local_snapshot: Option<String>,
    pub last_remote_revision: Option<String>,
    pub last_synced_snapshot_id: Option<String>,
    pub last_sync_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ConflictRecord {
    pub link_id: String,
    pub file_path: String,
    pub local_hash: Option<String>,
    pub remote_hash: Option<String>,
    pub base_hash: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SyncLogEntry {
    pub id: i64,
    pub link_id: String,
    pub action: String,
    pub status: String,
    pub message: Option<String>,
    pub created_at: String,
}

impl StateDb {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let conn = Connection::open(path)?;

        // Enable WAL mode for better concurrency
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sync_state (
                link_id TEXT PRIMARY KEY,
                last_local_snapshot TEXT,
                last_remote_revision TEXT,
                last_synced_snapshot_id TEXT,
                last_sync_at TEXT
            );

            CREATE TABLE IF NOT EXISTS conflicts (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                link_id TEXT NOT NULL,
                file_path TEXT NOT NULL,
                local_hash TEXT,
                remote_hash TEXT,
                base_hash TEXT
            );

            CREATE TABLE IF NOT EXISTS sync_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                link_id TEXT NOT NULL,
                action TEXT NOT NULL,
                status TEXT NOT NULL,
                message TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );",
        )?;

        Ok(Self { conn })
    }

    pub fn get_sync_state(&self, link_id: &str) -> Result<Option<SyncState>> {
        let mut stmt = self.conn.prepare(
            "SELECT link_id, last_local_snapshot, last_remote_revision, last_synced_snapshot_id, last_sync_at
             FROM sync_state WHERE link_id = ?1",
        )?;

        let mut rows = stmt.query(params![link_id])?;
        if let Some(row) = rows.next()? {
            Ok(Some(SyncState {
                link_id: row.get(0)?,
                last_local_snapshot: row.get(1)?,
                last_remote_revision: row.get(2)?,
                last_synced_snapshot_id: row.get(3)?,
                last_sync_at: row.get(4)?,
            }))
        } else {
            Ok(None)
        }
    }

    pub fn upsert_sync_state(&self, state: &SyncState) -> Result<()> {
        self.conn.execute(
            "INSERT INTO sync_state (link_id, last_local_snapshot, last_remote_revision, last_synced_snapshot_id, last_sync_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(link_id) DO UPDATE SET
                last_local_snapshot = excluded.last_local_snapshot,
                last_remote_revision = excluded.last_remote_revision,
                last_synced_snapshot_id = excluded.last_synced_snapshot_id,
                last_sync_at = excluded.last_sync_at",
            params![
                state.link_id,
                state.last_local_snapshot,
                state.last_remote_revision,
                state.last_synced_snapshot_id,
                state.last_sync_at,
            ],
        )?;
        Ok(())
    }

    pub fn insert_conflict(&self, conflict: &ConflictRecord) -> Result<()> {
        self.conn.execute(
            "INSERT INTO conflicts (link_id, file_path, local_hash, remote_hash, base_hash)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                conflict.link_id,
                conflict.file_path,
                conflict.local_hash,
                conflict.remote_hash,
                conflict.base_hash,
            ],
        )?;
        Ok(())
    }

    pub fn list_conflicts(&self, link_id: &str) -> Result<Vec<ConflictRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT link_id, file_path, local_hash, remote_hash, base_hash
             FROM conflicts WHERE link_id = ?1",
        )?;

        let records = stmt.query_map(params![link_id], |row| {
            Ok(ConflictRecord {
                link_id: row.get(0)?,
                file_path: row.get(1)?,
                local_hash: row.get(2)?,
                remote_hash: row.get(3)?,
                base_hash: row.get(4)?,
            })
        })?;

        let mut result = Vec::new();
        for record in records {
            result.push(record?);
        }
        Ok(result)
    }

    pub fn has_conflicts(&self, link_id: &str) -> Result<bool> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM conflicts WHERE link_id = ?1",
            params![link_id],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    pub fn clear_conflicts(&self, link_id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM conflicts WHERE link_id = ?1", params![link_id])?;
        Ok(())
    }

    pub fn append_log(
        &self,
        link_id: &str,
        action: &str,
        status: &str,
        message: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO sync_log (link_id, action, status, message)
             VALUES (?1, ?2, ?3, ?4)",
            params![link_id, action, status, message],
        )?;
        Ok(())
    }

    pub fn list_log(&self, link_id: &str, limit: Option<usize>) -> Result<Vec<SyncLogEntry>> {
        let limit_val = limit.unwrap_or(100) as i64;
        let mut stmt = self.conn.prepare(
            "SELECT id, link_id, action, status, message, created_at
             FROM sync_log WHERE link_id = ?1
             ORDER BY id DESC LIMIT ?2",
        )?;

        let entries = stmt.query_map(params![link_id, limit_val], |row| {
            Ok(SyncLogEntry {
                id: row.get(0)?,
                link_id: row.get(1)?,
                action: row.get(2)?,
                status: row.get(3)?,
                message: row.get(4)?,
                created_at: row.get(5)?,
            })
        })?;

        let mut result = Vec::new();
        for entry in entries {
            result.push(entry?);
        }
        Ok(result)
    }

    pub fn delete_state(&self, link_id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM sync_state WHERE link_id = ?1",
            params![link_id],
        )?;
        Ok(())
    }
}
