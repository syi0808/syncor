use syncor_core::config::{LinksRegistry, SyncorConfig, SyncorPaths};
use syncor_core::link::{LinkId, LinkInfo, LinkMode};
use tempfile::TempDir;

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
