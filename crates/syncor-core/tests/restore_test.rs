use std::fs;
use syncor_core::sync::restore::RestorePipeline;
use syncor_core::sync::save::SavePipeline;
use tempfile::TempDir;

#[test]
fn restore_recreates_files_from_snapshot() {
    let workspace = TempDir::new().unwrap();
    let store = TempDir::new().unwrap();

    fs::write(workspace.path().join("a.txt"), "hello").unwrap();
    fs::write(workspace.path().join("b.txt"), "world").unwrap();
    let save_result = SavePipeline::run(workspace.path(), store.path(), None).unwrap();

    fs::remove_file(workspace.path().join("a.txt")).unwrap();
    fs::remove_file(workspace.path().join("b.txt")).unwrap();

    let result =
        RestorePipeline::run(&save_result.snapshot_id, store.path(), workspace.path()).unwrap();

    assert_eq!(result.files_restored, 2);
    assert_eq!(
        fs::read_to_string(workspace.path().join("a.txt")).unwrap(),
        "hello"
    );
    assert_eq!(
        fs::read_to_string(workspace.path().join("b.txt")).unwrap(),
        "world"
    );
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

#[cfg(unix)]
#[test]
fn restore_preserves_file_permissions() {
    use std::os::unix::fs::PermissionsExt;
    use syncor_core::sync::save::SavePipeline;

    let workspace = TempDir::new().unwrap();
    let store = TempDir::new().unwrap();

    let script_path = workspace.path().join("run.sh");
    fs::write(&script_path, "#!/bin/bash\necho hi").unwrap();
    fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755)).unwrap();

    let save_result = SavePipeline::run(workspace.path(), store.path(), None).unwrap();

    fs::remove_file(&script_path).unwrap();
    RestorePipeline::run(&save_result.snapshot_id, store.path(), workspace.path()).unwrap();

    let perms = fs::metadata(&script_path).unwrap().permissions();
    let mode = perms.mode() & 0o777;
    assert!(mode & 0o100 != 0, "execute bit should be preserved, got {:o}", mode);
}
