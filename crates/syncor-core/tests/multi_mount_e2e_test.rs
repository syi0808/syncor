// Two local mirrors of the same remote link receive identical content
// after a single engine.restore_latest() call.

use std::fs;
use std::path::Path;
use syncor_core::config::{LinksRegistry, SyncorPaths};
use syncor_core::link::{LinkId, LinkInfo, LinkMode};
use syncor_core::sync::engine::SyncEngine;
use syncor_core::sync::state::StateDb;
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

/// When `engine.init_link` fails for a newly-registered link, the CLI's
/// `RollbackKind::NewLink` path undoes every side-effect: the registry row is
/// removed, the repo clone is deleted, the lock file is gone, and any state-DB
/// row for that link id is cleared. This test reproduces that sequence inline
/// (the CLI's `rollback()` helper is private) and asserts the registry + FS +
/// state DB are all clean afterwards.
#[test]
fn failed_init_link_leaves_registry_clean() {
    let home = TempDir::new().unwrap();
    let dir_b = TempDir::new().unwrap();

    let paths = SyncorPaths::with_home(home.path());
    paths.ensure_dirs().unwrap();

    // Empty registry on disk.
    let mut registry = LinksRegistry::load(&paths.links_file()).unwrap();

    // A repo URL that GitTransport::init_remote will reject. The transport
    // first tries `git clone <url> .` (which fails for any nonexistent URL),
    // then falls back to `git init` + `git remote add origin <url>`. To force
    // the fallback to fail too, we use a URL that begins with `-`, which the
    // git CLI parses as a flag and rejects with a non-zero exit. This is more
    // deterministic than `file:///nonexistent/...` (where `git remote add`
    // happily accepts the string and init_link succeeds end-to-end).
    let bad_url = "-not-a-real-url".to_string();
    let link_name = "bad-link";
    let info = LinkInfo {
        id: LinkId::from_parts(&bad_url, link_name),
        name: link_name.to_string(),
        repo: bad_url.clone(),
        local_dirs: vec![dir_b.path().to_path_buf()],
        mode: LinkMode::Pull,
        poll_interval_secs: None,
    };
    let id = info.id.clone();

    // Simulate cmd_connect's new-link path: add to registry and persist before
    // attempting remote initialisation.
    registry.add(info.clone()).unwrap();
    registry.save(&paths.links_file()).unwrap();

    let transport = GitTransport::new(paths.clone());
    let engine = SyncEngine::new(paths.clone(), Box::new(transport));

    // The init_link call must fail against the bogus URL.
    let init_result = engine.init_link(&info);
    assert!(
        init_result.is_err(),
        "init_link against a nonexistent file:// URL should fail; got {:?}",
        init_result
    );

    // --- Manually replicate RollbackKind::NewLink (the CLI helper is private). ---
    registry.remove(&info.id).unwrap();
    registry.save(&paths.links_file()).unwrap();
    let _ = std::fs::remove_dir_all(paths.link_repo_dir(&info.id));
    let _ = std::fs::remove_file(paths.link_lock_file(&info.id));
    if let Ok(db) = StateDb::open(paths.link_state_db()) {
        let _ = db.delete_state(info.id.as_str());
    }

    // --- Assertions: registry, FS, and state DB are all clean for this id. ---
    let reloaded = LinksRegistry::load(&paths.links_file()).unwrap();
    assert!(
        reloaded.get_by_id(&id).is_none(),
        "registry should not contain the failed link after rollback"
    );

    assert!(
        !paths.link_repo_dir(&id).exists(),
        "link repo dir {:?} should not exist after rollback",
        paths.link_repo_dir(&id)
    );

    let db = StateDb::open(paths.link_state_db()).unwrap();
    assert!(
        db.get_sync_state(id.as_str()).unwrap().is_none(),
        "state DB should have no sync_state row for the failed link id"
    );
}

/// When attempting to add a second mount to an already-connected pull link, a
/// failure during `engine.restore_latest_to` must be rolled back via
/// `RollbackKind::MountAdded`: the new mount is removed from the registry but
/// the existing mount, repo clone, and state-DB row are left intact.
///
/// We induce restore failure by making the second mount's parent directory
/// read-only (chmod 0o555), so the restore pipeline cannot create the target
/// subtree. A scope guard restores permissions to 0o755 regardless of test
/// outcome so the tempdir can be cleaned up.
#[cfg(unix)]
#[test]
fn failed_add_mount_preserves_existing_mount_and_state() {
    use std::os::unix::fs::PermissionsExt;

    // Scope guard: restore perms on drop so tempdir cleanup succeeds even on
    // panic.
    struct PermGuard {
        path: std::path::PathBuf,
    }
    impl Drop for PermGuard {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(
                &self.path,
                std::fs::Permissions::from_mode(0o755),
            );
        }
    }

    // --- Setup: bare git remote, Machine A workspace + data dir, Machine B
    //     data dir and one mount dir (b1). ---
    let remote_dir = TempDir::new().unwrap();
    let workspace_a = TempDir::new().unwrap();
    let data_dir_a = TempDir::new().unwrap();
    let data_dir_b = TempDir::new().unwrap();
    let b1 = TempDir::new().unwrap();
    // A scratch tempdir to host the read-only parent; the tempdir itself must
    // stay writable so Drop cleanup can remove the readonly child.
    let scratch = TempDir::new().unwrap();

    git2::Repository::init_bare(remote_dir.path()).unwrap();
    let remote_url = remote_dir.path().to_str().unwrap().to_string();
    let link_name = "rollback-add-mount-link";

    // --- Machine A: push a single file so Machine B has something to restore.
    fs::write(workspace_a.path().join("hello.txt"), b"hello multi-mount\n").unwrap();

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

    // --- Machine B: initial connect with single mount b1 (succeeds). ---
    let paths_b = SyncorPaths::with_home(data_dir_b.path());
    paths_b.ensure_dirs().unwrap();

    let link_b_initial = LinkInfo {
        id: LinkId::from_parts(&remote_url, link_name),
        name: link_name.to_string(),
        repo: remote_url.clone(),
        local_dirs: vec![b1.path().to_path_buf()],
        mode: LinkMode::Pull,
        poll_interval_secs: None,
    };
    let id = link_b_initial.id.clone();

    let mut registry = LinksRegistry::load(&paths_b.links_file()).unwrap();
    registry.add(link_b_initial.clone()).unwrap();
    registry.save(&paths_b.links_file()).unwrap();

    let transport_b = GitTransport::new(paths_b.clone());
    let engine_b = SyncEngine::new(paths_b.clone(), Box::new(transport_b));
    engine_b.init_link(&link_b_initial).unwrap();
    let restore = engine_b.restore_latest(&link_b_initial).unwrap();
    assert!(restore.restored, "initial restore into b1 should succeed");

    // Confirm precondition: b1 has the expected file, state DB row exists,
    // repo dir exists.
    assert_eq!(
        fs::read(b1.path().join("hello.txt")).unwrap(),
        b"hello multi-mount\n",
        "b1 should contain the file A pushed"
    );
    {
        let db = StateDb::open(paths_b.link_state_db()).unwrap();
        assert!(
            db.get_sync_state(id.as_str()).unwrap().is_some(),
            "state DB should have sync_state row after initial restore"
        );
    }
    assert!(
        paths_b.link_repo_dir(&id).exists(),
        "link repo dir should exist after initial init_link"
    );

    // --- Prepare the failing second mount. ---
    let readonly_parent = scratch.path().join("readonly");
    std::fs::create_dir(&readonly_parent).unwrap();
    let b2 = readonly_parent.join("target");
    // Install the scope guard BEFORE chmod so we always restore perms, even
    // if subsequent setup panics.
    let _perm_guard = PermGuard {
        path: readonly_parent.clone(),
    };
    std::fs::set_permissions(
        &readonly_parent,
        std::fs::Permissions::from_mode(0o555),
    )
    .unwrap();

    // --- Simulate cmd_connect's add-mount branch. ---
    // 1. Register the new mount.
    registry.add_mount(&id, b2.clone()).unwrap();
    registry.save(&paths_b.links_file()).unwrap();

    // 2. Attempt the restore into b2 — expect failure because the parent is
    //    read-only and restore cannot create b2 or write into it.
    let info_after = registry.get_by_id(&id).expect("just added mount").clone();
    let restore_err = engine_b.restore_latest_to(&info_after, &b2);
    assert!(
        restore_err.is_err(),
        "restore_latest_to into a read-only parent should fail; got {:?}",
        restore_err
    );

    // 3. Manually run RollbackKind::MountAdded cleanup: remove only the new
    //    mount from the registry; leave repo_dir, state, and lock untouched.
    registry.remove_mount(&id, &b2).unwrap();
    registry.save(&paths_b.links_file()).unwrap();

    // --- Assertions: (a) registry has only b1, (b) repo dir intact,
    //     (c) state DB row intact, (d) b1 contents unchanged.
    let reloaded = LinksRegistry::load(&paths_b.links_file()).unwrap();
    let reloaded_info = reloaded
        .get_by_id(&id)
        .expect("link should still be registered after mount rollback");
    assert_eq!(
        reloaded_info.local_dirs,
        vec![b1.path().to_path_buf()],
        "registry should only contain the original mount b1 after rollback"
    );

    assert!(
        paths_b.link_repo_dir(&id).exists(),
        "link repo dir should still exist after mount-add rollback"
    );

    let db = StateDb::open(paths_b.link_state_db()).unwrap();
    assert!(
        db.get_sync_state(id.as_str()).unwrap().is_some(),
        "state DB sync_state row should be preserved after mount-add rollback"
    );

    assert_eq!(
        fs::read(b1.path().join("hello.txt")).unwrap(),
        fs::read(workspace_a.path().join("hello.txt")).unwrap(),
        "b1 contents should be byte-identical to A's workspace file"
    );

    // _perm_guard drops here and restores 0o755 on readonly_parent so scratch
    // tempdir can be cleaned up.
}
