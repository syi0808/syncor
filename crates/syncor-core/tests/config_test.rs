use std::path::PathBuf;

use syncor_core::config::{LinksRegistry, SyncorConfig, SyncorPaths};
use syncor_core::error::SyncorError;
use syncor_core::link::{LinkId, LinkInfo, LinkMode};
use tempfile::TempDir;

fn make_pull_link(repo: &str, name: &str, dir: &str) -> LinkInfo {
    LinkInfo {
        id: LinkId::from_parts(repo, name),
        name: name.to_string(),
        repo: repo.to_string(),
        local_dirs: vec![PathBuf::from(dir)],
        mode: LinkMode::Pull,
        poll_interval_secs: None,
    }
}

fn make_push_link(repo: &str, name: &str, dir: &str) -> LinkInfo {
    LinkInfo {
        mode: LinkMode::Push,
        ..make_pull_link(repo, name, dir)
    }
}

#[test]
fn paths_use_xdg_layout() {
    let home = TempDir::new().unwrap();
    let paths = SyncorPaths::with_home(home.path());
    assert!(paths.config_dir().ends_with("syncor"));
    assert!(paths.data_dir().ends_with("syncor"));
    assert!(paths.config_file().ends_with("config.toml"));
    assert!(paths.links_file().ends_with("links.toml"));
}

#[test]
fn config_default_values() {
    let config = SyncorConfig::default();
    assert_eq!(config.debounce_secs, 2);
    assert_eq!(config.default_poll_interval_secs, 60);
}

#[test]
fn config_roundtrip() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("config.toml");
    let config = SyncorConfig::default();
    config.save(&path).unwrap();
    let loaded = SyncorConfig::load(&path).unwrap();
    assert_eq!(config.debounce_secs, loaded.debounce_secs);
}

#[test]
fn links_registry_add_and_get() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("links.toml");
    let mut registry = LinksRegistry::new();
    let info = LinkInfo {
        id: LinkId::from_parts("repo", "dotfiles"),
        name: "dotfiles".to_string(),
        repo: "repo".to_string(),
        local_dirs: vec!["/home/user/dotfiles".into()],
        mode: LinkMode::Push,
        poll_interval_secs: None,
    };
    registry.add(info.clone()).unwrap();
    registry.save(&path).unwrap();

    let loaded = LinksRegistry::load(&path).unwrap();
    let found = loaded.get_by_name("dotfiles").unwrap();
    assert_eq!(found.repo, "repo");
}

#[test]
fn links_registry_rejects_duplicate_dir() {
    let mut registry = LinksRegistry::new();
    let info1 = LinkInfo {
        id: LinkId::from_parts("repo-a", "name1"),
        name: "name1".to_string(),
        repo: "repo-a".to_string(),
        local_dirs: vec!["/same/dir".into()],
        mode: LinkMode::Push,
        poll_interval_secs: None,
    };
    let info2 = LinkInfo {
        id: LinkId::from_parts("repo-b", "name2"),
        name: "name2".to_string(),
        repo: "repo-b".to_string(),
        local_dirs: vec!["/same/dir".into()],
        mode: LinkMode::Push,
        poll_interval_secs: None,
    };
    registry.add(info1).unwrap();
    assert!(registry.add(info2).is_err());
}

#[test]
fn add_mount_extends_pull_link() {
    let mut reg = LinksRegistry::new();
    let link = make_pull_link("repo", "dotfiles", "/tmp/a");
    reg.add(link.clone()).unwrap();

    reg.add_mount(&link.id, PathBuf::from("/tmp/b")).unwrap();
    let got = reg.get_by_id(&link.id).unwrap();
    assert_eq!(got.local_dirs.len(), 2);
    assert!(got.local_dirs.contains(&PathBuf::from("/tmp/a")));
    assert!(got.local_dirs.contains(&PathBuf::from("/tmp/b")));
}

#[test]
fn add_mount_rejects_push_link() {
    let mut reg = LinksRegistry::new();
    let link = make_push_link("repo", "dotfiles", "/tmp/a");
    reg.add(link.clone()).unwrap();

    let err = reg.add_mount(&link.id, PathBuf::from("/tmp/b")).unwrap_err();
    assert!(matches!(err, SyncorError::MultiMountNotAllowed(_)),
        "expected MultiMountNotAllowed, got {err:?}");
}

#[test]
fn add_mount_rejects_dir_owned_by_other_link() {
    let mut reg = LinksRegistry::new();
    let a = make_pull_link("repo-a", "x", "/tmp/shared");
    let b = make_pull_link("repo-b", "y", "/tmp/b");
    reg.add(a.clone()).unwrap();
    reg.add(b.clone()).unwrap();

    let err = reg.add_mount(&b.id, PathBuf::from("/tmp/shared")).unwrap_err();
    assert!(matches!(err, SyncorError::LinkAlreadyExists(_)));
}

#[test]
fn add_mount_is_idempotent_for_existing_dir_in_same_link() {
    let mut reg = LinksRegistry::new();
    let link = make_pull_link("repo", "x", "/tmp/a");
    reg.add(link.clone()).unwrap();

    reg.add_mount(&link.id, PathBuf::from("/tmp/a")).unwrap();
    assert_eq!(reg.get_by_id(&link.id).unwrap().local_dirs.len(), 1);
}

#[test]
fn remove_mount_non_last_keeps_link() {
    let mut reg = LinksRegistry::new();
    let link = make_pull_link("repo", "x", "/tmp/a");
    reg.add(link.clone()).unwrap();
    reg.add_mount(&link.id, PathBuf::from("/tmp/b")).unwrap();

    let r = reg.remove_mount(&link.id, &PathBuf::from("/tmp/a")).unwrap();
    assert!(!r.last_mount_removed);
    let got = reg.get_by_id(&link.id).unwrap();
    assert_eq!(got.local_dirs, vec![PathBuf::from("/tmp/b")]);
}

#[test]
fn remove_mount_last_reports_last_mount_removed() {
    let mut reg = LinksRegistry::new();
    let link = make_pull_link("repo", "x", "/tmp/a");
    reg.add(link.clone()).unwrap();

    let r = reg.remove_mount(&link.id, &PathBuf::from("/tmp/a")).unwrap();
    assert!(r.last_mount_removed);
    // Link is still present in registry; caller decides whether to remove().
    assert!(reg.get_by_id(&link.id).is_some());
    assert_eq!(reg.get_by_id(&link.id).unwrap().local_dirs.len(), 0);
}

#[test]
fn remove_mount_unknown_dir_returns_link_not_found() {
    let mut reg = LinksRegistry::new();
    let link = make_pull_link("repo", "x", "/tmp/a");
    reg.add(link.clone()).unwrap();

    let err = reg.remove_mount(&link.id, &PathBuf::from("/tmp/nope")).unwrap_err();
    assert!(matches!(err, SyncorError::LinkNotFound(_)));
}
