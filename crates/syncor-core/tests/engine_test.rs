use std::fs;
use syncor_core::config::SyncorPaths;
use syncor_core::link::{LinkId, LinkInfo, LinkMode};
use syncor_core::sync::engine::SyncEngine;
use syncor_core::transport::git::GitTransport;
use tempfile::TempDir;

fn setup() -> (TempDir, TempDir, TempDir, LinkInfo) {
    let workspace = TempDir::new().unwrap();
    let remote_dir = TempDir::new().unwrap();
    let data_dir = TempDir::new().unwrap();
    git2::Repository::init_bare(remote_dir.path()).unwrap();
    let remote_url = remote_dir.path().to_str().unwrap().to_string();
    let link = LinkInfo {
        id: LinkId::from_parts(&remote_url, "test"),
        name: "test".to_string(),
        repo: remote_url,
        local_dir: workspace.path().to_path_buf(),
        mode: LinkMode::Push,
        poll_interval_secs: None,
    };
    (workspace, remote_dir, data_dir, link)
}

#[test]
fn push_syncs_new_files() {
    let (workspace, _remote, data_dir, link) = setup();
    fs::write(workspace.path().join("hello.txt"), "hello").unwrap();

    let paths = SyncorPaths::with_home(data_dir.path());
    let transport = GitTransport::new(paths.clone());
    let engine = SyncEngine::new(paths, Box::new(transport));

    engine.init_link(&link).unwrap();
    let result = engine.push(&link).unwrap();
    assert!(result.snapshot_id.is_some());
}

#[test]
fn pull_restores_remote_files() {
    // Machine A pushes, Machine B pulls
    let (workspace_a, _remote, data_dir_a, link_a) = setup();
    let workspace_b = TempDir::new().unwrap();
    let data_dir_b = TempDir::new().unwrap();

    fs::write(workspace_a.path().join("shared.txt"), "shared content").unwrap();

    let paths_a = SyncorPaths::with_home(data_dir_a.path());
    let transport_a = GitTransport::new(paths_a.clone());
    let engine_a = SyncEngine::new(paths_a, Box::new(transport_a));
    engine_a.init_link(&link_a).unwrap();
    engine_a.push(&link_a).unwrap();

    let mut link_b = link_a.clone();
    link_b.local_dir = workspace_b.path().to_path_buf();
    link_b.mode = LinkMode::Pull;

    let paths_b = SyncorPaths::with_home(data_dir_b.path());
    let transport_b = GitTransport::new(paths_b.clone());
    let engine_b = SyncEngine::new(paths_b, Box::new(transport_b));
    engine_b.init_link(&link_b).unwrap();
    // init_link clones the repo, so pull() returns UpToDate.
    // Use restore_latest() for initial connect.
    let result = engine_b.restore_latest(&link_b).unwrap();
    assert!(result.restored);

    assert_eq!(
        fs::read_to_string(workspace_b.path().join("shared.txt")).unwrap(),
        "shared content"
    );
}

#[test]
fn restore_latest_updates_file_index() {
    let (workspace_a, _remote, data_dir_a, link_a) = setup();
    let workspace_b = TempDir::new().unwrap();
    let data_dir_b = TempDir::new().unwrap();

    fs::write(workspace_a.path().join("data.txt"), "some data").unwrap();
    let paths_a = SyncorPaths::with_home(data_dir_a.path());
    let transport_a = GitTransport::new(paths_a.clone());
    let engine_a = SyncEngine::new(paths_a, Box::new(transport_a));
    engine_a.init_link(&link_a).unwrap();
    engine_a.push(&link_a).unwrap();

    let mut link_b = link_a.clone();
    link_b.local_dir = workspace_b.path().to_path_buf();
    link_b.mode = LinkMode::Pull;
    let paths_b = SyncorPaths::with_home(data_dir_b.path());
    let transport_b = GitTransport::new(paths_b.clone());
    let engine_b = SyncEngine::new(paths_b.clone(), Box::new(transport_b));
    engine_b.init_link(&link_b).unwrap();
    engine_b.restore_latest(&link_b).unwrap();

    // Verify index.bin exists and has entries
    let store_dir = paths_b
        .link_repo_dir(&link_b.id)
        .join("stores")
        .join(&link_b.name);
    let index_path = store_dir.join("index.bin");
    assert!(
        index_path.exists(),
        "index.bin should exist after restore_latest"
    );

    let index = chkpt_core::index::FileIndex::open(&index_path).unwrap();
    let paths_in_index = index.all_paths().unwrap();
    assert!(
        paths_in_index.contains(&"data.txt".to_string()),
        "index should contain data.txt"
    );
}
