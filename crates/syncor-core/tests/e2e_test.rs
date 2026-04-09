use std::fs;
use syncor_core::config::SyncorPaths;
use syncor_core::link::{LinkId, LinkInfo, LinkMode};
use syncor_core::sync::engine::SyncEngine;
use syncor_core::transport::git::GitTransport;
use tempfile::TempDir;

/// Full round-trip: Machine A links and pushes, Machine B connects and pulls.
#[test]
fn e2e_push_pull_roundtrip() {
    // --- Setup: bare git remote, two workspaces, two data dirs ---
    let remote_dir = TempDir::new().unwrap();
    let workspace_a = TempDir::new().unwrap();
    let data_dir_a = TempDir::new().unwrap();
    let workspace_b = TempDir::new().unwrap();
    let data_dir_b = TempDir::new().unwrap();

    git2::Repository::init_bare(remote_dir.path()).unwrap();
    let remote_url = remote_dir.path().to_str().unwrap().to_string();

    // --- Machine A: create files ---
    fs::write(workspace_a.path().join("config.yaml"), "version: 1\nenv: prod\n").unwrap();
    let scripts_dir = workspace_a.path().join("scripts");
    fs::create_dir_all(&scripts_dir).unwrap();
    fs::write(scripts_dir.join("deploy.sh"), "#!/bin/sh\necho 'deploying'\n").unwrap();

    // --- Machine A: create LinkInfo with Push mode ---
    let link_a = LinkInfo {
        id: LinkId::from_parts(&remote_url, "e2e-link"),
        name: "e2e-link".to_string(),
        repo: remote_url.clone(),
        local_dir: workspace_a.path().to_path_buf(),
        mode: LinkMode::Push,
        poll_interval_secs: None,
    };

    // --- Machine A: SyncEngine, init_link, push ---
    let paths_a = SyncorPaths::with_home(data_dir_a.path());
    let transport_a = GitTransport::new(paths_a.clone());
    let engine_a = SyncEngine::new(paths_a, Box::new(transport_a));
    engine_a.init_link(&link_a).unwrap();
    let push_result = engine_a.push(&link_a).unwrap();
    assert!(push_result.pushed, "Machine A initial push should succeed");
    assert!(push_result.snapshot_id.is_some(), "push should produce a snapshot id");

    // --- Machine B: same link id/repo/name, different local_dir, Pull mode ---
    let link_b = LinkInfo {
        id: LinkId::from_parts(&remote_url, "e2e-link"),
        name: "e2e-link".to_string(),
        repo: remote_url.clone(),
        local_dir: workspace_b.path().to_path_buf(),
        mode: LinkMode::Pull,
        poll_interval_secs: None,
    };

    // --- Machine B: separate SyncEngine, init_link, pull ---
    let paths_b = SyncorPaths::with_home(data_dir_b.path());
    let transport_b = GitTransport::new(paths_b.clone());
    let engine_b = SyncEngine::new(paths_b, Box::new(transport_b));
    engine_b.init_link(&link_b).unwrap();
    let pull_result = engine_b.pull(&link_b).unwrap();
    assert!(pull_result.restored, "Machine B initial pull should restore files");
    assert!(pull_result.files_restored >= 2, "at least 2 files should be restored");

    // --- Assert files match ---
    let config_b = workspace_b.path().join("config.yaml");
    assert!(config_b.exists(), "config.yaml should exist on Machine B");
    assert_eq!(
        fs::read_to_string(&config_b).unwrap(),
        "version: 1\nenv: prod\n",
        "config.yaml content should match"
    );

    let deploy_b = workspace_b.path().join("scripts").join("deploy.sh");
    assert!(deploy_b.exists(), "scripts/deploy.sh should exist on Machine B");
    assert_eq!(
        fs::read_to_string(&deploy_b).unwrap(),
        "#!/bin/sh\necho 'deploying'\n",
        "deploy.sh content should match"
    );

    // --- Machine A: modify config.yaml, push again ---
    fs::write(
        workspace_a.path().join("config.yaml"),
        "version: 2\nenv: staging\n",
    )
    .unwrap();

    let push_result2 = engine_a.push(&link_a).unwrap();
    assert!(push_result2.pushed, "Machine A second push should succeed");

    // --- Machine B: pull again, assert updated content ---
    let pull_result2 = engine_b.pull(&link_b).unwrap();
    assert!(pull_result2.restored, "Machine B second pull should restore updated file");

    assert_eq!(
        fs::read_to_string(workspace_b.path().join("config.yaml")).unwrap(),
        "version: 2\nenv: staging\n",
        "config.yaml should reflect updated content after second pull"
    );

    // scripts/deploy.sh should be unchanged
    assert_eq!(
        fs::read_to_string(workspace_b.path().join("scripts").join("deploy.sh")).unwrap(),
        "#!/bin/sh\necho 'deploying'\n",
        "deploy.sh should remain unchanged after second pull"
    );
}
