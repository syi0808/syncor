use std::fs;
use syncor_core::config::SyncorPaths;
use syncor_core::link::{LinkId, LinkInfo, LinkMode};
use syncor_core::transport::git::GitTransport;
use syncor_core::transport::{PushResult, SyncTransport};
use tempfile::TempDir;

fn create_bare_remote(dir: &std::path::Path) -> String {
    git2::Repository::init_bare(dir).unwrap();
    dir.to_str().unwrap().to_string()
}

#[test]
fn init_remote_clones_and_creates_structure() {
    let remote_dir = TempDir::new().unwrap();
    let data_dir = TempDir::new().unwrap();
    let remote_url = create_bare_remote(remote_dir.path());

    let paths = SyncorPaths::with_home(data_dir.path());
    let transport = GitTransport::new(paths.clone());

    let link = LinkInfo {
        id: LinkId::from_parts(&remote_url, "test"),
        name: "test".to_string(),
        repo: remote_url,
        local_dirs: vec!["/tmp/test".into()],
        mode: LinkMode::Push,
        poll_interval_secs: None,
    };

    transport.init_remote(&link).unwrap();

    let repo_dir = paths.link_repo_dir(&link.id);
    assert!(repo_dir.exists());
}

#[test]
fn push_commits_store_files() {
    let remote_dir = TempDir::new().unwrap();
    let data_dir = TempDir::new().unwrap();
    let remote_url = create_bare_remote(remote_dir.path());

    let paths = SyncorPaths::with_home(data_dir.path());
    let transport = GitTransport::new(paths.clone());

    let link = LinkInfo {
        id: LinkId::from_parts(&remote_url, "mylink"),
        name: "mylink".to_string(),
        repo: remote_url,
        local_dirs: vec!["/tmp/test".into()],
        mode: LinkMode::Push,
        poll_interval_secs: None,
    };

    transport.init_remote(&link).unwrap();

    let store_dir = paths
        .link_repo_dir(&link.id)
        .join("stores")
        .join(&link.name);
    fs::create_dir_all(&store_dir).unwrap();
    fs::write(store_dir.join("test.txt"), "data").unwrap();

    let result = transport.push(&link, &store_dir).unwrap();
    assert!(matches!(result, PushResult::Success { .. }));
}
