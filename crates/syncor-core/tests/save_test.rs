use syncor_core::sync::save::SavePipeline;
use tempfile::TempDir;
use std::fs;

#[test]
fn save_creates_snapshot_for_new_files() {
    let workspace = TempDir::new().unwrap();
    let store = TempDir::new().unwrap();

    fs::write(workspace.path().join("hello.txt"), "hello world").unwrap();
    fs::write(workspace.path().join("data.bin"), vec![0u8; 1024]).unwrap();
    fs::create_dir(workspace.path().join("sub")).unwrap();
    fs::write(workspace.path().join("sub/nested.txt"), "nested").unwrap();

    let result = SavePipeline::run(workspace.path(), store.path(), None).unwrap();
    assert!(!result.snapshot_id.is_empty());
    assert_eq!(result.files_scanned, 3);
    assert!(result.files_hashed > 0);
    assert!(store.path().join("packs").exists());
    assert!(store.path().join("catalog.sqlite").exists());
}

#[test]
fn save_is_incremental_via_index() {
    let workspace = TempDir::new().unwrap();
    let store = TempDir::new().unwrap();

    fs::write(workspace.path().join("a.txt"), "aaa").unwrap();

    let r1 = SavePipeline::run(workspace.path(), store.path(), None).unwrap();
    assert!(r1.files_hashed > 0);

    let r2 = SavePipeline::run(workspace.path(), store.path(), None).unwrap();
    assert_eq!(r2.files_hashed, 0);
}

#[test]
fn save_detects_modified_file() {
    let workspace = TempDir::new().unwrap();
    let store = TempDir::new().unwrap();

    fs::write(workspace.path().join("a.txt"), "version1").unwrap();
    SavePipeline::run(workspace.path(), store.path(), None).unwrap();

    fs::write(workspace.path().join("a.txt"), "version2").unwrap();
    let r2 = SavePipeline::run(workspace.path(), store.path(), None).unwrap();
    assert_eq!(r2.files_hashed, 1);
}
