use std::fs;
use std::path::Path;
use syncor_core::sync::restore::{validate_path, RestorePipeline};
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
    assert!(
        mode & 0o100 != 0,
        "execute bit should be preserved, got {:o}",
        mode
    );
}

#[test]
fn restore_rejects_path_traversal() {
    let base = Path::new("/tmp/safe_dir");

    // Path traversal with ..
    assert!(validate_path(base, "../../etc/passwd").is_err());
    assert!(validate_path(base, "foo/../../../etc/passwd").is_err());

    // Absolute paths
    assert!(validate_path(base, "/etc/passwd").is_err());
    assert!(validate_path(base, "\\etc\\passwd").is_err());

    // Normal paths should succeed
    assert!(validate_path(base, "foo/bar.txt").is_ok());
    assert!(validate_path(base, "file.txt").is_ok());
}

#[test]
fn restore_preserves_ignored_files() {
    let workspace = TempDir::new().unwrap();
    let store = TempDir::new().unwrap();

    // Create files including an "ignored" one
    fs::write(workspace.path().join("tracked.txt"), "tracked").unwrap();
    fs::write(workspace.path().join(".env"), "SECRET=123").unwrap();
    fs::write(workspace.path().join(".chkptignore"), ".env\n").unwrap();

    let save_result = SavePipeline::run(workspace.path(), store.path(), None).unwrap();

    // Add another tracked file, then restore
    fs::write(workspace.path().join("extra.txt"), "extra").unwrap();
    RestorePipeline::run(&save_result.snapshot_id, store.path(), workspace.path()).unwrap();

    // .env should still exist (it was ignored by scanner)
    assert!(
        workspace.path().join(".env").exists(),
        ".env should be preserved"
    );
    // extra.txt should be removed (it was tracked but not in snapshot)
    assert!(!workspace.path().join("extra.txt").exists());
    // tracked.txt should exist
    assert!(workspace.path().join("tracked.txt").exists());
}
