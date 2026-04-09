use syncor_core::sync::state::{ConflictRecord, StateDb, SyncLogEntry, SyncState};
use tempfile::TempDir;

#[test]
fn state_db_creates_tables() {
    let dir = TempDir::new().unwrap();
    let db = StateDb::open(dir.path().join("state.db")).unwrap();
    db.get_sync_state("nonexistent").unwrap();
}

#[test]
fn upsert_and_get_sync_state() {
    let dir = TempDir::new().unwrap();
    let db = StateDb::open(dir.path().join("state.db")).unwrap();
    let state = SyncState {
        link_id: "abc".to_string(),
        last_local_snapshot: Some("snap-1".to_string()),
        last_remote_revision: Some("rev-1".to_string()),
        last_synced_snapshot_id: Some("snap-1".to_string()),
        last_sync_at: Some("2026-04-10T00:00:00Z".to_string()),
    };
    db.upsert_sync_state(&state).unwrap();
    let loaded = db.get_sync_state("abc").unwrap().unwrap();
    assert_eq!(loaded.last_local_snapshot.as_deref(), Some("snap-1"));
}

#[test]
fn insert_and_list_conflicts() {
    let dir = TempDir::new().unwrap();
    let db = StateDb::open(dir.path().join("state.db")).unwrap();
    let conflict = ConflictRecord {
        link_id: "abc".to_string(),
        file_path: "config.yaml".to_string(),
        local_hash: Some("aaa".to_string()),
        remote_hash: Some("bbb".to_string()),
        base_hash: Some("ccc".to_string()),
    };
    db.insert_conflict(&conflict).unwrap();
    let conflicts = db.list_conflicts("abc").unwrap();
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].file_path, "config.yaml");
}

#[test]
fn clear_conflicts_for_link() {
    let dir = TempDir::new().unwrap();
    let db = StateDb::open(dir.path().join("state.db")).unwrap();
    let conflict = ConflictRecord {
        link_id: "abc".to_string(),
        file_path: "f.txt".to_string(),
        local_hash: None,
        remote_hash: Some("x".to_string()),
        base_hash: Some("y".to_string()),
    };
    db.insert_conflict(&conflict).unwrap();
    db.clear_conflicts("abc").unwrap();
    assert!(db.list_conflicts("abc").unwrap().is_empty());
}

#[test]
fn has_conflicts_returns_true_when_present() {
    let dir = TempDir::new().unwrap();
    let db = StateDb::open(dir.path().join("state.db")).unwrap();
    assert!(!db.has_conflicts("abc").unwrap());
    let conflict = ConflictRecord {
        link_id: "abc".to_string(),
        file_path: "f.txt".to_string(),
        local_hash: None,
        remote_hash: Some("x".to_string()),
        base_hash: None,
    };
    db.insert_conflict(&conflict).unwrap();
    assert!(db.has_conflicts("abc").unwrap());
}

#[test]
fn append_and_list_sync_log() {
    let dir = TempDir::new().unwrap();
    let db = StateDb::open(dir.path().join("state.db")).unwrap();
    db.append_log("abc", "push", "success", None).unwrap();
    db.append_log("abc", "pull", "error", Some("network timeout"))
        .unwrap();
    let logs = db.list_log("abc", Some(10)).unwrap();
    assert_eq!(logs.len(), 2);
    assert_eq!(logs[0].action, "pull"); // most recent first
}
