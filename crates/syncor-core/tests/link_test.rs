use syncor_core::link::{LinkId, LinkInfo, LinkState, LinkMode};

#[test]
fn link_id_is_deterministic() {
    let id1 = LinkId::from_parts("my-repo", "dotfiles");
    let id2 = LinkId::from_parts("my-repo", "dotfiles");
    assert_eq!(id1, id2);
}

#[test]
fn link_id_differs_for_different_inputs() {
    let id1 = LinkId::from_parts("repo-a", "dotfiles");
    let id2 = LinkId::from_parts("repo-b", "dotfiles");
    assert_ne!(id1, id2);
}

#[test]
fn link_info_roundtrip_serde() {
    let info = LinkInfo {
        id: LinkId::from_parts("my-repo", "dotfiles"),
        name: "dotfiles".to_string(),
        repo: "my-repo".to_string(),
        local_dir: "/home/user/dotfiles".into(),
        mode: LinkMode::Push,
        poll_interval_secs: None,
    };
    let serialized = toml::to_string(&info).unwrap();
    let deserialized: LinkInfo = toml::from_str(&serialized).unwrap();
    assert_eq!(info.name, deserialized.name);
    assert_eq!(info.repo, deserialized.repo);
}

#[test]
fn link_state_default_is_idle() {
    let state = LinkState::default();
    assert!(matches!(state, LinkState::Idle));
}
