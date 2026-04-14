// Two local mirrors of the same remote link receive identical content
// after a single engine.restore_latest() call.

use std::fs;
use std::path::Path;
use syncor_core::config::SyncorPaths;
use syncor_core::link::{LinkId, LinkInfo, LinkMode};
use syncor_core::sync::engine::SyncEngine;
use syncor_core::transport::git::GitTransport;
use tempfile::TempDir;

/// Walk `dir` recursively and return a sorted list of (relative path, bytes)
/// pairs for every regular file underneath.
fn collect_files(dir: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(root: &Path, current: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        for entry in fs::read_dir(current).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let ty = entry.file_type().unwrap();
            if ty.is_dir() {
                walk(root, &path, out);
            } else if ty.is_file() {
                let rel = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                let bytes = fs::read(&path).unwrap();
                out.push((rel, bytes));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[test]
fn multi_mount_pull_mirrors_identical_content() {
    // --- Setup: bare git remote, Machine A workspace + data dir, Machine B
    //     data dir and two local mirror dirs (b1, b2). ---
    let remote_dir = TempDir::new().unwrap();
    let workspace_a = TempDir::new().unwrap();
    let data_dir_a = TempDir::new().unwrap();
    let data_dir_b = TempDir::new().unwrap();
    let b1 = TempDir::new().unwrap();
    let b2 = TempDir::new().unwrap();

    git2::Repository::init_bare(remote_dir.path()).unwrap();
    let remote_url = remote_dir.path().to_str().unwrap().to_string();
    let link_name = "multi-mount-link";

    // --- Machine A: create a non-trivial file tree (one top-level file and
    //     one nested file) so the fan-out code exercises create_dir_all. ---
    fs::write(
        workspace_a.path().join("settings.json"),
        "{\"theme\":\"dark\"}\n",
    )
    .unwrap();
    let nested_dir = workspace_a.path().join("agents").join("tools");
    fs::create_dir_all(&nested_dir).unwrap();
    fs::write(
        nested_dir.join("helper.md"),
        "# Helper\n\nSome nested content.\n",
    )
    .unwrap();

    let link_a = LinkInfo {
        id: LinkId::from_parts(&remote_url, link_name),
        name: link_name.to_string(),
        repo: remote_url.clone(),
        local_dirs: vec![workspace_a.path().to_path_buf()],
        mode: LinkMode::Push,
        poll_interval_secs: None,
    };

    let paths_a = SyncorPaths::with_home(data_dir_a.path());
    let transport_a = GitTransport::new(paths_a.clone());
    let engine_a = SyncEngine::new(paths_a, Box::new(transport_a));
    engine_a.init_link(&link_a).unwrap();
    let push_result = engine_a.push(&link_a).unwrap();
    assert!(push_result.pushed, "Machine A push should succeed");

    // --- Machine B: same link id/repo/name, two local_dirs, Pull mode. ---
    let link_b = LinkInfo {
        id: LinkId::from_parts(&remote_url, link_name),
        name: link_name.to_string(),
        repo: remote_url.clone(),
        local_dirs: vec![b1.path().to_path_buf(), b2.path().to_path_buf()],
        mode: LinkMode::Pull,
        poll_interval_secs: None,
    };
    assert_eq!(
        link_a.id, link_b.id,
        "LinkId::from_parts must be deterministic across machines"
    );

    let paths_b = SyncorPaths::with_home(data_dir_b.path());
    let transport_b = GitTransport::new(paths_b.clone());
    let engine_b = SyncEngine::new(paths_b, Box::new(transport_b));
    engine_b.init_link(&link_b).unwrap();

    // --- The fan-out: a single restore_latest call populates both mounts. ---
    let result = engine_b.restore_latest(&link_b).unwrap();
    assert!(result.restored, "restore_latest should report restored");
    assert!(
        result.files_restored > 0,
        "at least one file should be restored"
    );
    assert_eq!(
        result.per_mount.len(),
        2,
        "per_mount should have one entry per local_dirs entry"
    );
    for outcome in &result.per_mount {
        assert!(
            outcome.error.is_none(),
            "mount {:?} errored: {:?}",
            outcome.dir,
            outcome.error
        );
        assert!(
            outcome.files_restored > 0,
            "mount {:?} should restore at least one file",
            outcome.dir
        );
    }

    // --- Assertions: both mirrors have byte-identical trees matching A. ---
    let a_files = collect_files(workspace_a.path());
    let b1_files = collect_files(b1.path());
    let b2_files = collect_files(b2.path());

    assert_eq!(
        a_files, b1_files,
        "b1 should be byte-for-byte identical to Machine A's workspace"
    );
    assert_eq!(
        a_files, b2_files,
        "b2 should be byte-for-byte identical to Machine A's workspace"
    );
    assert_eq!(b1_files, b2_files, "both mounts should be identical");

    // Sanity: the nested file made it through (exercising create_dir_all).
    assert!(
        b1.path().join("agents").join("tools").join("helper.md").exists(),
        "nested file should exist in b1"
    );
    assert!(
        b2.path().join("agents").join("tools").join("helper.md").exists(),
        "nested file should exist in b2"
    );
}
