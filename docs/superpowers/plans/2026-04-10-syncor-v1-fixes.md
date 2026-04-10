# Syncor v1 Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix critical bugs and missing implementations found in the v1 code review — stabilize LinkId hashing, restore file permissions, update FileIndex on pull, add transport retry, fix unsafe daemon stop, fix non-UTF-8 panic.

**Architecture:** Targeted fixes to existing files. No new modules or structural changes. Each task is independent and can be committed separately.

**Tech Stack:** Rust, chkpt-core (XXH3 hashing), fs4, libc

**Spec:** `docs/superpowers/specs/2026-04-10-syncor-design.md`

---

## File Map

```
Modified files:
├── crates/syncor-core/src/link.rs              # Task 1: stable LinkId hash
├── crates/syncor-core/src/sync/catalog_merge.rs # Task 2: non-UTF-8 path fix
├── crates/syncor-core/src/sync/restore.rs       # Task 3: file permission restore
├── crates/syncor-core/src/sync/engine.rs        # Task 4: FileIndex update on pull
├── crates/syncor-core/src/transport/git.rs      # Task 5: retry + fetch error fix
├── crates/syncor-cli/src/main.rs                # Task 6: safe daemon stop
└── tests (various)                              # Updated for each task
```

---

### Task 1: Stable LinkId with XXH3

**Problem:** `DefaultHasher` output is not stable across Rust versions. LinkId is persisted in `links.toml` and used for filesystem paths — a compiler upgrade could orphan all link data.

**Files:**
- Modify: `crates/syncor-core/src/link.rs:9-17`
- Modify: `crates/syncor-core/tests/link_test.rs`

**NOTE: This is a breaking change.** Existing `links.toml` entries and `~/.local/share/syncor/<link-id>/` directories will be orphaned because the hash output changes from 16-char to 32-char hex. Acceptable for pre-release software. Users must re-link after this update.

- [ ] **Step 1: Replace DefaultHasher with XXH3-128**

Replace `link.rs` lines 9-17:

```rust
    pub fn from_parts(repo: &str, name: &str) -> Self {
        use chkpt_core::store::blob::{bytes_to_hex, hash_content_bytes};
        let input = format!("{}\0{}", repo, name);
        let hash = hash_content_bytes(input.as_bytes());
        Self(bytes_to_hex(&hash))
    }
```

This uses chkpt-core's XXH3-128 which is deterministic across platforms and compiler versions. The `\0` separator prevents `("ab", "cd")` from colliding with `("a", "bcd")`.

- [ ] **Step 2: Write test for hash stability**

Add to `crates/syncor-core/tests/link_test.rs`:

```rust
#[test]
fn link_id_is_stable_xxh3() {
    let id = LinkId::from_parts("https://github.com/user/repo.git", "dotfiles");
    // XXH3-128 produces 32-char hex string
    assert_eq!(id.as_str().len(), 32);
    // Deterministic
    let id2 = LinkId::from_parts("https://github.com/user/repo.git", "dotfiles");
    assert_eq!(id.as_str(), id2.as_str());
}
```

- [ ] **Step 3: Run all tests**

Run: `cargo test --workspace`
Expected: All pass (existing tests check determinism/equality, not specific values). Some test link IDs will change internally but assertions still hold.

- [ ] **Step 4: Commit**

```bash
git add crates/syncor-core/src/link.rs crates/syncor-core/tests/link_test.rs
git commit -m "fix: use XXH3-128 for stable LinkId across Rust versions"
```

---

### Task 2: Fix non-UTF-8 path panic in catalog_merge

**Problem:** `remote_path.to_str().unwrap()` panics on non-UTF-8 paths (possible on Linux).

**Files:**
- Modify: `crates/syncor-core/src/sync/catalog_merge.rs:8-11`

- [ ] **Step 1: Fix the unwrap**

Replace line 8-11 of `catalog_merge.rs`:

```rust
    let remote_str = remote_path.to_str().ok_or_else(|| {
        crate::error::SyncorError::Other("non-UTF-8 path for remote catalog".into())
    })?;
    conn.execute("ATTACH DATABASE ?1 AS remote", [remote_str])?;
```

- [ ] **Step 2: Add test for non-UTF-8 path**

Add to `crates/syncor-core/tests/catalog_merge_test.rs`:

```rust
#[cfg(unix)]
#[test]
fn merge_rejects_non_utf8_path() {
    use std::os::unix::ffi::OsStrExt;
    use std::ffi::OsStr;
    use std::path::PathBuf;

    let dir = TempDir::new().unwrap();
    let local_path = dir.path().join("local.sqlite");
    let local = MetadataCatalog::open(&local_path).unwrap();
    drop(local);

    // Create a path with invalid UTF-8 byte
    let bad_name = OsStr::from_bytes(&[0xff, 0xfe]);
    let bad_path: PathBuf = dir.path().join(bad_name);

    let result = merge_catalogs(&local_path, &bad_path);
    assert!(result.is_err(), "should return error for non-UTF-8 path");
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test --package syncor-core --test catalog_merge_test`
Expected: All pass.

- [ ] **Step 4: Commit**

```bash
git add crates/syncor-core/src/sync/catalog_merge.rs crates/syncor-core/tests/catalog_merge_test.rs
git commit -m "fix: return error instead of panic on non-UTF-8 catalog path"
```

---

### Task 3: Restore file permissions

**Problem:** `RestorePipeline::run` writes files with default umask, losing execute bits. Dotfiles users will find their scripts broken after sync.

**Files:**
- Modify: `crates/syncor-core/src/sync/restore.rs:31-43`
- Modify: `crates/syncor-core/src/sync/engine.rs` (ApplyRemote action in pull, ~line 340)
- Modify: `crates/syncor-core/tests/restore_test.rs`

- [ ] **Step 1: Write test for permission preservation**

Add to `crates/syncor-core/tests/restore_test.rs`:

```rust
#[cfg(unix)]
#[test]
fn restore_preserves_file_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let workspace = TempDir::new().unwrap();
    let store = TempDir::new().unwrap();

    let script_path = workspace.path().join("run.sh");
    fs::write(&script_path, "#!/bin/bash\necho hi").unwrap();
    // Set executable permission
    fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755)).unwrap();

    let save_result = SavePipeline::run(workspace.path(), store.path(), None).unwrap();

    // Remove and restore
    fs::remove_file(&script_path).unwrap();
    RestorePipeline::run(&save_result.snapshot_id, store.path(), workspace.path()).unwrap();

    let perms = fs::metadata(&script_path).unwrap().permissions();
    let mode = perms.mode() & 0o777;
    // At minimum, the owner execute bit should be set
    assert!(mode & 0o100 != 0, "execute bit should be preserved, got {:o}", mode);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --package syncor-core --test restore_test restore_preserves`
Expected: FAIL — execute bit lost.

- [ ] **Step 3: Add permission restoration after write**

In `restore.rs`, after `std::fs::write(&dest, &content)?;` (line 41), add:

```rust
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let perms = std::fs::Permissions::from_mode(entry.mode);
                std::fs::set_permissions(&dest, perms)?;
            }
```

- [ ] **Step 4: Also fix permissions in engine.rs pull ApplyRemote**

In `engine.rs`, the `pull` method also writes files directly in the `ApplyRemote` action (~line 340). After `std::fs::write(&file_path, content)?;`, the manifest entry's mode is not available (only `remote_hash` is in `FileAction::ApplyRemote`). To fix this properly:

1. Look up the manifest entry for the file path to get the mode value from the remote manifest
2. Or: after writing all ApplyRemote files, scan them and set permissions from the remote manifest

Simplest approach: build a `HashMap<String, u32>` of `path -> mode` from the remote manifest before the action loop, then apply permissions after each write:

```rust
                // Build path-to-mode map from remote manifest
                let remote_modes: std::collections::HashMap<String, u32> = catalog
                    .snapshot_manifest(&latest_remote.id)?
                    .into_iter()
                    .map(|e| (e.path, e.mode))
                    .collect();
```

Then inside the `ApplyRemote` arm, after the write:

```rust
                        FileAction::ApplyRemote { path, remote_hash } => {
                            // ... existing write code ...
                            #[cfg(unix)]
                            if let Some(&mode) = remote_modes.get(path) {
                                use std::os::unix::fs::PermissionsExt;
                                let perms = std::fs::Permissions::from_mode(mode);
                                std::fs::set_permissions(&file_path, perms)?;
                            }
                            files_restored += 1;
                        }
```

- [ ] **Step 5: Run tests**

Run: `cargo test --package syncor-core --test restore_test`
Expected: All pass including the new permission test.

- [ ] **Step 6: Commit**

```bash
git add crates/syncor-core/src/sync/restore.rs crates/syncor-core/src/sync/engine.rs crates/syncor-core/tests/restore_test.rs
git commit -m "fix: restore file permissions (unix mode) after pull"
```

---

### Task 4: Update FileIndex after pull

**Problem:** After pulling files, the FileIndex (`index.bin`) is not updated. The next push will re-hash every pulled file, creating redundant snapshots.

**Files:**
- Modify: `crates/syncor-core/src/sync/engine.rs` (pull method, after applying actions)

- [ ] **Step 1: Add a private helper `update_file_index` to SyncEngine**

In `engine.rs`, add this private method to `SyncEngine`:

```rust
    /// Scan workspace and update the FileIndex in the store so the next push
    /// doesn't re-hash unchanged files.
    fn update_file_index(&self, link: &LinkInfo, store_dir: &std::path::Path) -> Result<()> {
        use chkpt_core::index::{FileEntry, FileIndex};
        use chkpt_core::scanner::scan_workspace;
        use chkpt_core::store::blob::hash_path_bytes;

        let index_path = store_dir.join("index.bin");
        let mut index = FileIndex::open(&index_path)?;

        let scanned = scan_workspace(&link.local_dir, None)?;
        let mut entries = Vec::new();
        for file in &scanned {
            let hash = hash_path_bytes(&file.absolute_path, file.is_symlink)?;
            entries.push(FileEntry {
                path: file.relative_path.clone(),
                blob_hash: hash,
                size: file.size,
                mtime_secs: file.mtime_secs,
                mtime_nanos: file.mtime_nanos,
                inode: file.inode,
                mode: file.mode,
            });
        }

        let scanned_paths: std::collections::HashSet<&str> =
            scanned.iter().map(|f| f.relative_path.as_str()).collect();
        let all_indexed = index.all_paths()?;
        let removed: Vec<String> = all_indexed
            .into_iter()
            .filter(|p| !scanned_paths.contains(p.as_str()))
            .collect();

        index.apply_changes(&removed, &entries)?;
        Ok(())
    }
```

- [ ] **Step 2: Call `update_file_index` in both `pull` and `restore_latest`**

In `pull()`, after applying actions and before "Copy merged catalog back", add:
```rust
                self.update_file_index(link, &store_dir)?;
```

In `restore_latest()`, after `RestorePipeline::run(...)` and before the state.db update, add:
```rust
        self.update_file_index(link, &store_dir)?;
```

- [ ] **Step 3: Write test to verify index is updated**

Add to `crates/syncor-core/tests/engine_test.rs`:

```rust
#[test]
fn restore_latest_updates_file_index() {
    let (workspace_a, _remote, data_dir_a, link_a) = setup();
    let workspace_b = TempDir::new().unwrap();
    let data_dir_b = TempDir::new().unwrap();

    // Machine A pushes
    fs::write(workspace_a.path().join("data.txt"), "some data").unwrap();
    let paths_a = SyncorPaths::with_home(data_dir_a.path());
    let transport_a = GitTransport::new(paths_a.clone());
    let engine_a = SyncEngine::new(paths_a, Box::new(transport_a));
    engine_a.init_link(&link_a).unwrap();
    engine_a.push(&link_a).unwrap();

    // Machine B restores
    let mut link_b = link_a.clone();
    link_b.local_dir = workspace_b.path().to_path_buf();
    link_b.mode = LinkMode::Pull;
    let paths_b = SyncorPaths::with_home(data_dir_b.path());
    let transport_b = GitTransport::new(paths_b.clone());
    let engine_b = SyncEngine::new(paths_b.clone(), Box::new(transport_b));
    engine_b.init_link(&link_b).unwrap();
    engine_b.restore_latest(&link_b).unwrap();

    // Verify index.bin exists and has entries
    let store_dir = paths_b.link_repo_dir(&link_b.id).join("stores").join(&link_b.name);
    let index_path = store_dir.join("index.bin");
    assert!(index_path.exists(), "index.bin should exist after restore_latest");

    let index = chkpt_core::index::FileIndex::open(&index_path).unwrap();
    let paths_in_index = index.all_paths().unwrap();
    assert!(paths_in_index.contains(&"data.txt".to_string()), "index should contain data.txt");
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test --package syncor-core --test engine_test`
Expected: All pass.

- [ ] **Step 5: Run all tests**

Run: `cargo test --workspace`
Expected: All pass.

- [ ] **Step 6: Commit**

```bash
git add crates/syncor-core/src/sync/engine.rs crates/syncor-core/tests/engine_test.rs
git commit -m "fix: update FileIndex after pull/restore to avoid redundant re-hashing"
```

---

### Task 5: Transport retry + fetch error handling

**Problem:** (I9) No retry logic for transient network errors. (I10) `has_remote_changes` silently ignores fetch failures.

**Files:**
- Modify: `crates/syncor-core/src/transport/git.rs`

- [ ] **Step 1: Add retry helper**

Add at the top of `git.rs`, after imports:

```rust
/// Retry a closure up to `max_attempts` times with exponential backoff.
/// `max_attempts` must be >= 1.
fn retry_with_backoff<F, T>(max_attempts: u32, mut f: F) -> Result<T>
where
    F: FnMut() -> Result<T>,
{
    assert!(max_attempts >= 1, "max_attempts must be >= 1");
    let delays = [5, 15, 45]; // seconds
    for attempt in 0..max_attempts {
        match f() {
            Ok(v) => return Ok(v),
            Err(e) => {
                let is_retryable = matches!(&e, SyncorError::Transport(msg) if
                    msg.contains("fetch failed") ||
                    msg.contains("push failed") ||
                    msg.contains("Could not resolve host") ||
                    msg.contains("Connection refused") ||
                    msg.contains("timed out")
                );
                if !is_retryable || attempt + 1 >= max_attempts {
                    return Err(e);
                }
                let delay = delays.get(attempt as usize).copied().unwrap_or(45);
                tracing::warn!(
                    "transport operation failed (attempt {}/{}), retrying in {}s: {}",
                    attempt + 1, max_attempts, delay, e
                );
                std::thread::sleep(std::time::Duration::from_secs(delay));
            }
        }
    }
    unreachable!("loop always returns")
}
```

- [ ] **Step 2: Wrap ONLY the git push CLI call in retry**

In `GitTransport::push`, wrap ONLY the `Command::new("git").args(["push", ...])` block (NOT the staging or commit) in `retry_with_backoff(3, || { ... })`. The staging and commit are local operations that should not be retried.

In `GitTransport::pull`, wrap ONLY the `Command::new("git").args(["fetch", ...])` block in `retry_with_backoff(3, || { ... })`. The merge is a local operation.

Note: `pull` already propagates fetch errors correctly. `has_remote_changes` is the one that silently ignores them.

- [ ] **Step 3: Fix `has_remote_changes` to propagate fetch errors**

Replace the silent `let _ = Command::new("git")...` in `has_remote_changes` with:

```rust
        let fetch_output = Command::new("git")
            .args(["fetch", "origin", &branch])
            .current_dir(&repo_dir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .map_err(|e| SyncorError::Transport(format!("fetch exec: {}", e)))?;

        if !fetch_output.status.success() {
            let stderr = String::from_utf8_lossy(&fetch_output.stderr);
            return Err(SyncorError::Transport(format!(
                "git fetch failed: {}",
                stderr.trim()
            )));
        }
```

- [ ] **Step 4: Run tests**

Run: `cargo test --workspace`
Expected: All pass (retry doesn't affect local test flows since they don't fail).

- [ ] **Step 5: Commit**

```bash
git add crates/syncor-core/src/transport/git.rs
git commit -m "fix: add retry with backoff for transport, propagate fetch errors"
```

---

### Task 6: Safe daemon stop

**Problem:** `daemon stop` sends SIGTERM to a PID without verifying the process is actually syncor. Could kill an unrelated process if the PID was recycled.

**Files:**
- Modify: `crates/syncor-cli/src/main.rs` (cmd_daemon_stop function)

- [ ] **Step 1: Read the current daemon stop implementation**

Read `crates/syncor-cli/src/main.rs` and find `cmd_daemon_stop`.

- [ ] **Step 2: Replace raw kill with IPC-based shutdown + PID validation**

Replace the `cmd_daemon_stop` function:

```rust
fn cmd_daemon_stop() -> Result<()> {
    let paths = load_paths()?;

    if !DaemonManager::is_running(&paths) {
        println!("Daemon is not running.");
        return Ok(());
    }

    // First try graceful shutdown via IPC socket
    let sock = paths.socket_path();
    if sock.exists() {
        match std::os::unix::net::UnixStream::connect(&sock) {
            Ok(_stream) => {
                // Socket connectable = likely our daemon (not 100% — another process
                // could theoretically listen on the same path, but unlikely in practice).
                // A more robust approach on Linux would check /proc/<pid>/cmdline.
                let pid_file = paths.pid_file();
                if let Ok(content) = std::fs::read_to_string(&pid_file) {
                    if let Ok(pid) = content.trim().parse::<i32>() {
                        unsafe { libc::kill(pid, libc::SIGTERM) };
                        // Wait briefly for shutdown
                        for _ in 0..10 {
                            std::thread::sleep(std::time::Duration::from_millis(200));
                            if !DaemonManager::is_running(&paths) {
                                break;
                            }
                        }
                    }
                }
            }
            Err(_) => {
                // Socket not connectable — stale files
                DaemonManager::cleanup_stale(&paths);
            }
        }
    }

    if DaemonManager::is_running(&paths) {
        println!("Daemon did not stop gracefully. Cleaning up stale files.");
        DaemonManager::cleanup_stale(&paths);
    } else {
        DaemonManager::cleanup_stale(&paths);
        println!("Daemon stopped.");
    }

    Ok(())
}
```

- [ ] **Step 3: Add `use std::os::unix::net::UnixStream` if not already imported**

- [ ] **Step 4: Run build**

Run: `cargo build --package syncor-cli`
Expected: Compiles.

- [ ] **Step 5: Commit**

```bash
git add crates/syncor-cli/src/main.rs
git commit -m "fix: validate daemon PID via socket before sending SIGTERM"
```

---

### Task 7: Final verification

- [ ] **Step 1: Run all tests**

Run: `cargo test --workspace`
Expected: All pass.

- [ ] **Step 2: Run clippy**

Run: `cargo clippy --workspace -- -D warnings`
Expected: No warnings.

- [ ] **Step 3: Run fmt**

Run: `cargo fmt --all`

- [ ] **Step 4: Commit if needed**

```bash
git add -A
git commit -m "chore: fix clippy warnings and formatting after v1 fixes"
```
