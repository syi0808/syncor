use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::config::SyncorPaths;
use crate::error::{Result, SyncorError};
use crate::link::LinkInfo;
use crate::progress::{ItemTotal, Phase, PhaseGuard, ProgressReporter};
use crate::transport::{ConflictInfo, PullResult, PushResult, RemoteLinkInfo, SyncTransport};

/// Run a network-facing git command, forwarding stderr lines verbatim to
/// `reporter.log`. Stdout is discarded (none of our 5 network call sites
/// inspect the child's stdout — `push` uses a subsequent `rev-parse`,
/// `clone` produces empty stdout in practice, etc.). Discarding stdout
/// also avoids a potential deadlock if git ever fills the stdout pipe
/// buffer while we block reading stderr.
///
/// Returns `(exit_status, stderr_raw)` so callers can inspect stderr for
/// existing error-classification logic (e.g. push's "non-fast-forward"
/// detection).
fn run_git_streaming(
    dir: &Path,
    args: &[&str],
    reporter: &dyn ProgressReporter,
) -> Result<(std::process::ExitStatus, String)> {
    use std::io::BufReader;
    let mut child = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| SyncorError::Transport(format!("git spawn: {}", e)))?;

    let stderr = child.stderr.take().expect("piped stderr");
    // Live-parse stderr on a worker thread so `GitProgressParser` sees
    // progress updates as git emits them (rather than after `wait()`).
    // `thread::scope` lets the worker borrow `reporter: &dyn ProgressReporter`
    // without a `'static` bound. Stdout stays `Stdio::null()` so git can't
    // block on stdout backpressure while we read stderr (deadlock safety).
    let stderr_raw: String = std::thread::scope(|s| {
        let h = s.spawn(|| {
            let mut raw = String::new();
            let mut parser = crate::transport::git_progress::GitProgressParser::new(reporter);
            let mut buf: Vec<u8> = Vec::new();
            let mut reader = BufReader::new(stderr);
            let mut chunk = [0u8; 4096];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    Err(_) => break,
                }
                // Flush complete lines (split on \r or \n). A mid-UTF-8-char
                // boundary inside `buf` is safe: we only decode slices that
                // end at ASCII \r/\n, so multibyte chars are never split.
                while let Some(pos) = buf.iter().position(|&b| b == b'\r' || b == b'\n') {
                    let line = String::from_utf8_lossy(&buf[..pos]).into_owned();
                    raw.push_str(&line);
                    raw.push('\n');
                    let trimmed = line.trim();
                    if !trimmed.is_empty() {
                        parser.feed_line(trimmed);
                    }
                    buf.drain(..=pos);
                }
            }
            // Flush any trailing content without a terminator.
            if !buf.is_empty() {
                let line = String::from_utf8_lossy(&buf).into_owned();
                raw.push_str(&line);
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    parser.feed_line(trimmed);
                }
            }
            parser.finish();
            raw
        });
        h.join().unwrap_or_default()
    });

    let status = child
        .wait()
        .map_err(|e| SyncorError::Transport(format!("git wait: {}", e)))?;
    Ok((status, stderr_raw))
}

/// Retry a closure up to `max_attempts` times with exponential backoff.
fn retry_with_backoff<F, T>(
    max_attempts: u32,
    reporter: &dyn ProgressReporter,
    mut f: F,
) -> Result<T>
where
    F: FnMut() -> Result<T>,
{
    assert!(max_attempts >= 1, "max_attempts must be >= 1");
    let delays = [5, 15, 45];
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
                    attempt + 1,
                    max_attempts,
                    delay,
                    e
                );
                reporter.log(&format!(
                    "retry {}/{}: {} (waiting {}s)",
                    attempt + 1,
                    max_attempts,
                    e,
                    delay
                ));
                std::thread::sleep(std::time::Duration::from_secs(delay));
            }
        }
    }
    unreachable!("loop always returns")
}

/// Git-backed transport using the `git` CLI for operations that need
/// authentication (clone, fetch, push) and libgit2 for local-only work
/// (commit, staging).  This avoids the credential-helper issues that
/// plague libgit2 on macOS / Windows.
pub struct GitTransport {
    paths: SyncorPaths,
}

impl GitTransport {
    pub fn new(paths: SyncorPaths) -> Self {
        Self { paths }
    }

    fn repo_dir(&self, link: &LinkInfo) -> PathBuf {
        self.paths.link_repo_dir(&link.id)
    }

    /// Run a git CLI command in a given directory.
    fn git(dir: &Path, args: &[&str]) -> Result<String> {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .map_err(|e| SyncorError::Transport(format!("failed to run git: {}", e)))?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            Err(SyncorError::Transport(format!(
                "git {} failed: {}",
                args.join(" "),
                stderr.trim()
            )))
        }
    }

    /// Run git command, returning Ok(stdout) or Ok("") on failure (non-fatal).
    fn git_ok(dir: &Path, args: &[&str]) -> String {
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default()
    }

    fn primary_branch(repo_dir: &Path) -> String {
        // Check current branch via symbolic-ref first
        let out = Self::git_ok(repo_dir, &["symbolic-ref", "--short", "HEAD"]);
        let branch = out.trim();
        if !branch.is_empty() {
            return branch.to_string();
        }
        // Fallback: check which branches exist
        let out = Self::git_ok(repo_dir, &["branch", "--list", "main"]);
        if out.contains("main") {
            return "main".to_string();
        }
        let out = Self::git_ok(repo_dir, &["branch", "--list", "master"]);
        if out.contains("master") {
            return "master".to_string();
        }
        "main".to_string()
    }
}

impl SyncTransport for GitTransport {
    fn init_remote(&self, link: &LinkInfo, reporter: &dyn ProgressReporter) -> Result<()> {
        let repo_dir = self.repo_dir(link);

        if repo_dir.join(".git").exists() {
            return Ok(());
        }

        std::fs::create_dir_all(&repo_dir)?;

        // Try clone first; fall back to init for empty repos.
        // No outer PhaseGuard: GitProgressParser emits granular phase events
        // (Enumerate → Count → Compress → Receive → Resolve) as git streams;
        // an outer guard would be immediately preempted.
        let clone_result = run_git_streaming(
            &repo_dir,
            &["clone", "--progress", &link.repo, "."],
            reporter,
        );

        let cloned = matches!(clone_result, Ok((status, _)) if status.success());
        if !cloned {
            // Init + add remote for empty repos.
            Self::git(&repo_dir, &["init"])?;
            Self::git(&repo_dir, &["remote", "add", "origin", &link.repo])?;
        }

        // Create store dirs.
        let store_base = repo_dir.join("stores").join(&link.name);
        std::fs::create_dir_all(store_base.join("packs"))?;
        std::fs::create_dir_all(store_base.join("trees"))?;

        // .gitignore
        let gitignore_path = repo_dir.join(".gitignore");
        if !gitignore_path.exists() {
            std::fs::write(&gitignore_path, "index.bin\n*.sqlite-wal\n*.sqlite-shm\n")?;
        }

        // Only create initial commit for freshly init'd repos (not cloned ones).
        if !cloned {
            let has_commits = Self::git_ok(&repo_dir, &["rev-parse", "HEAD"]);
            if has_commits.is_empty() {
                Self::git(&repo_dir, &["add", "-A"])?;
                Self::git(
                    &repo_dir,
                    &["commit", "-m", "init syncor repo", "--allow-empty"],
                )?;
                let _ = Self::git(&repo_dir, &["branch", "-M", "main"]);
            }
        }

        Ok(())
    }

    fn push(
        &self,
        link: &LinkInfo,
        _store_path: &Path,
        reporter: &dyn ProgressReporter,
    ) -> Result<PushResult> {
        let repo_dir = self.repo_dir(link);
        let branch = Self::primary_branch(&repo_dir);

        // Stage all.
        {
            let _guard = PhaseGuard::new(reporter, Phase::GitStage, ItemTotal::Unknown);
            Self::git(&repo_dir, &["add", "-A"])?;
        }

        // Check if there's anything to commit.
        let status = Self::git_ok(&repo_dir, &["status", "--porcelain"]);
        if status.trim().is_empty() {
            // Nothing changed — still "success" but with current HEAD as revision.
            let rev = Self::git_ok(&repo_dir, &["rev-parse", "HEAD"]);
            return Ok(PushResult::Success {
                revision: rev.trim().to_string(),
            });
        }

        {
            let _guard = PhaseGuard::new(reporter, Phase::GitCommit, ItemTotal::Unknown);
            Self::git(&repo_dir, &["commit", "-m", "syncor push"])?;
        }

        // Push via CLI (uses system credential helpers). GitProgressParser
        // emits its own phase events during streaming, so no outer PhaseGuard.
        retry_with_backoff(3, reporter, || {
            let (status, stderr_raw) = run_git_streaming(
                &repo_dir,
                &["push", "--progress", "-u", "origin", &branch],
                reporter,
            )?;

            if status.success() {
                let rev = Self::git_ok(&repo_dir, &["rev-parse", "HEAD"]);
                Ok(PushResult::Success {
                    revision: rev.trim().to_string(),
                })
            } else if stderr_raw.contains("non-fast-forward") || stderr_raw.contains("rejected") {
                Ok(PushResult::Conflict {
                    details: ConflictInfo {
                        message: stderr_raw.trim().to_string(),
                    },
                })
            } else {
                Err(SyncorError::Transport(format!(
                    "push failed: {}",
                    stderr_raw.trim()
                )))
            }
        })
    }

    fn pull(
        &self,
        link: &LinkInfo,
        _store_path: &Path,
        reporter: &dyn ProgressReporter,
    ) -> Result<PullResult> {
        let repo_dir = self.repo_dir(link);
        let branch = Self::primary_branch(&repo_dir);

        // Record current HEAD before fetch.
        let head_before = Self::git_ok(&repo_dir, &["rev-parse", "HEAD"]);

        // Fetch via CLI. GitProgressParser emits granular phase events; no
        // outer guard (would be immediately preempted).
        retry_with_backoff(3, reporter, || {
            let (status, stderr_raw) = run_git_streaming(
                &repo_dir,
                &["fetch", "--progress", "origin", &branch],
                reporter,
            )?;

            if status.success() {
                Ok(())
            } else {
                Err(SyncorError::Transport(format!(
                    "fetch failed: {}",
                    stderr_raw.trim()
                )))
            }
        })?;

        // Compare local vs remote.
        let remote_ref = format!("origin/{}", branch);
        let remote_head = Self::git_ok(&repo_dir, &["rev-parse", &remote_ref]);

        if head_before.trim() == remote_head.trim() && !head_before.is_empty() {
            return Ok(PullResult::UpToDate);
        }

        // Try fast-forward merge.
        let merge_output = {
            let _guard = PhaseGuard::new(reporter, Phase::Merge, ItemTotal::Unknown);
            Command::new("git")
                .args(["merge", "--ff-only", &remote_ref])
                .current_dir(&repo_dir)
                .env("GIT_TERMINAL_PROMPT", "0")
                .output()
                .map_err(|e| SyncorError::Transport(format!("merge exec: {}", e)))?
        };

        if merge_output.status.success() {
            let rev = Self::git_ok(&repo_dir, &["rev-parse", "HEAD"]);
            Ok(PullResult::Success {
                revision: rev.trim().to_string(),
            })
        } else {
            // If merge --ff-only fails, it's a conflict (diverged histories).
            Ok(PullResult::Conflict {
                details: ConflictInfo {
                    message: "cannot fast-forward; manual resolution needed".to_string(),
                },
            })
        }
    }

    fn list_remote_links(
        &self,
        repo_url: &str,
        reporter: &dyn ProgressReporter,
    ) -> Result<Vec<RemoteLinkInfo>> {
        let tmp = tempfile::tempdir()?;

        // Parser owns the phase lifecycle; no outer guard.
        let clone_result = run_git_streaming(
            tmp.path(),
            &["clone", "--progress", "--depth=1", repo_url, "."],
            reporter,
        );

        match clone_result {
            Ok((status, _)) if status.success() => {}
            _ => return Ok(vec![]),
        }

        let toml_path = tmp.path().join("syncor.toml");
        if !toml_path.exists() {
            return Ok(vec![]);
        }

        let contents = std::fs::read_to_string(&toml_path)?;

        #[derive(serde::Deserialize)]
        struct Manifest {
            #[serde(default)]
            links: Vec<ManifestLink>,
        }
        #[derive(serde::Deserialize)]
        struct ManifestLink {
            name: String,
            #[serde(default)]
            created_at: Option<String>,
        }

        let manifest: Manifest =
            toml::from_str(&contents).map_err(|e| SyncorError::Config(e.to_string()))?;

        Ok(manifest
            .links
            .into_iter()
            .map(|l| RemoteLinkInfo {
                name: l.name,
                created_at: l.created_at.unwrap_or_default(),
            })
            .collect())
    }

    fn has_remote_changes(&self, link: &LinkInfo, reporter: &dyn ProgressReporter) -> Result<bool> {
        let repo_dir = self.repo_dir(link);
        let branch = Self::primary_branch(&repo_dir);

        // Fetch with retry. Parser owns the phase lifecycle.
        retry_with_backoff(3, reporter, || {
            let (status, stderr_raw) = run_git_streaming(
                &repo_dir,
                &["fetch", "--progress", "origin", &branch],
                reporter,
            )?;

            if status.success() {
                Ok(())
            } else {
                Err(SyncorError::Transport(format!(
                    "fetch failed: {}",
                    stderr_raw.trim()
                )))
            }
        })?;

        let local = Self::git_ok(&repo_dir, &["rev-parse", "HEAD"]);
        let remote = Self::git_ok(&repo_dir, &["rev-parse", &format!("origin/{}", branch)]);

        Ok(local.trim() != remote.trim())
    }
}
