use syncor_core::sync::save::SavePipeline;
use syncor_core::sync::restore::RestorePipeline;
use tempfile::TempDir;
use std::fs;

#[test]
fn restore_recreates_files_from_snapshot() {
    let workspace = TempDir::new().unwrap();
    let store = TempDir::new().unwrap();

    fs::write(workspace.path().join("a.txt"), "hello").unwrap();
    fs::write(workspace.path().join("b.txt"), "world").unwrap();
    let save_result = SavePipeline::run(workspace.path(), store.path(), None).unwrap();

    fs::remove_file(workspace.path().join("a.txt")).unwrap();
    fs::remove_file(workspace.path().join("b.txt")).unwrap();

    let result = RestorePipeline::run(
        &save_result.snapshot_id,
        store.path(),
        workspace.path(),
    ).unwrap();

    assert_eq!(result.files_restored, 2);
    assert_eq!(fs::read_to_string(workspace.path().join("a.txt")).unwrap(), "hello");
    assert_eq!(fs::read_to_string(workspace.path().join("b.txt")).unwrap(), "world");
}

#[test]
fn restore_removes_extra_files() {
    let workspace = TempDir::new().unwrap();
    let store = TempDir::new().unwrap();

    fs::write(workspace.path().join("keep.txt"), "keep").unwrap();
    let save_result = SavePipeline::run(workspace.path(), store.path(), None).unwrap();

    fs::write(workspace.path().join("extra.txt"), "extra").unwrap();

    RestorePipeline::run(&save_result.snapshot_id, store.path(), workspace.path()).unwrap();

    assert!(!workspace.path().join("extra.txt").exists());
    assert!(workspace.path().join("keep.txt").exists());
}
