use std::path::{Path, PathBuf};

use git2::{
    BranchType, IndexAddOption, PushOptions, RemoteCallbacks, Repository, Signature,
};

use crate::config::SyncorPaths;
use crate::error::{Result, SyncorError};
use crate::link::LinkInfo;
use crate::transport::{ConflictInfo, PullResult, PushResult, RemoteLinkInfo, SyncTransport};

/// Git-backed transport using libgit2.
///
/// Because `git2::Repository` is !Send + !Sync we store only [`SyncorPaths`]
/// and open the repository on every call.
pub struct GitTransport {
    paths: SyncorPaths,
}

impl GitTransport {
    pub fn new(paths: SyncorPaths) -> Self {
        Self { paths }
    }

    /// Working-tree directory for this link's clone.
    fn repo_dir(&self, link: &LinkInfo) -> PathBuf {
        self.paths.link_repo_dir(&link.id)
    }

    /// Create a signature for commits.
    fn sig() -> std::result::Result<Signature<'static>, git2::Error> {
        Signature::now("syncor", "syncor@localhost")
    }

    /// Determine the primary branch name (main or master).
    fn primary_branch(repo: &Repository) -> String {
        // Try main first, then master.
        if repo.find_branch("main", BranchType::Local).is_ok() {
            return "main".to_string();
        }
        if repo.find_branch("master", BranchType::Local).is_ok() {
            return "master".to_string();
        }
        // Check remote branches
        if repo
            .find_branch("origin/main", BranchType::Remote)
            .is_ok()
        {
            return "main".to_string();
        }
        if repo
            .find_branch("origin/master", BranchType::Remote)
            .is_ok()
        {
            return "master".to_string();
        }
        "main".to_string()
    }
}

impl SyncTransport for GitTransport {
    fn init_remote(&self, link: &LinkInfo) -> Result<()> {
        let repo_dir = self.repo_dir(link);
        std::fs::create_dir_all(&repo_dir)?;

        let repo = match Repository::clone(&link.repo, &repo_dir) {
            Ok(r) => r,
            Err(_) => {
                // Clone may fail on an empty bare repo. Init locally and add
                // remote instead.
                let r = Repository::init(&repo_dir)?;
                r.remote("origin", &link.repo)?;
                r
            }
        };

        // Create stores/<name>/packs and stores/<name>/trees directories.
        let store_base = repo_dir.join("stores").join(&link.name);
        std::fs::create_dir_all(store_base.join("packs"))?;
        std::fs::create_dir_all(store_base.join("trees"))?;

        // Create .gitignore excluding binary / WAL files.
        let gitignore_path = repo_dir.join(".gitignore");
        if !gitignore_path.exists() {
            std::fs::write(
                &gitignore_path,
                "index.bin\n*.sqlite-wal\n*.sqlite-shm\n",
            )?;
        }

        // Make an initial commit if the repo has no commits yet.
        if repo.head().is_err() {
            let mut index = repo.index()?;
            index.add_all(["*"].iter(), IndexAddOption::DEFAULT, None)?;
            let oid = index.write_tree()?;
            index.write()?;
            let tree = repo.find_tree(oid)?;
            let sig = Self::sig()?;
            repo.commit(Some("HEAD"), &sig, &sig, "init syncor repo", &tree, &[])?;

            // Ensure the branch is named "main".
            let head = repo.head()?;
            if let Some(name) = head.shorthand() {
                if name != "main" {
                    // Rename the branch to "main".
                    let mut branch = repo.find_branch(name, BranchType::Local)?;
                    branch.rename("main", true)?;
                }
            }
        }

        Ok(())
    }

    fn push(&self, link: &LinkInfo, _store_path: &Path) -> Result<PushResult> {
        let repo_dir = self.repo_dir(link);
        let repo = Repository::open(&repo_dir)?;
        let branch_name = Self::primary_branch(&repo);

        // Stage all changes.
        let mut index = repo.index()?;
        index.add_all(["*"].iter(), IndexAddOption::DEFAULT, None)?;
        let oid = index.write_tree()?;
        index.write()?;

        let tree = repo.find_tree(oid)?;
        let sig = Self::sig()?;

        // Build parent commit list.
        let parent_commit = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
        let parents: Vec<&git2::Commit<'_>> = parent_commit.iter().collect();

        let commit_oid =
            repo.commit(Some("HEAD"), &sig, &sig, "syncor push", &tree, &parents)?;

        // Push to origin.
        let mut remote = repo.find_remote("origin")?;
        let refspec = format!("refs/heads/{branch_name}:refs/heads/{branch_name}");

        let push_err = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
        {
            let push_err = push_err.clone();
            let mut callbacks = RemoteCallbacks::new();
            callbacks.push_update_reference(move |_ref, status| {
                if let Some(msg) = status {
                    *push_err.lock().unwrap() = Some(msg.to_string());
                }
                Ok(())
            });

            let mut push_opts = PushOptions::new();
            push_opts.remote_callbacks(callbacks);

            if let Err(e) = remote.push(&[&refspec], Some(&mut push_opts)) {
                let msg = e.to_string();
                if msg.contains("non-fast-forward") {
                    return Ok(PushResult::Conflict {
                        details: ConflictInfo { message: msg },
                    });
                }
                return Err(SyncorError::Git(e));
            }
        }

        let push_err = push_err.lock().unwrap().take();
        if let Some(err_msg) = push_err {
            if err_msg.contains("non-fast-forward") {
                return Ok(PushResult::Conflict {
                    details: ConflictInfo { message: err_msg },
                });
            }
            return Err(SyncorError::Transport(err_msg));
        }

        Ok(PushResult::Success {
            revision: commit_oid.to_string(),
        })
    }

    fn pull(&self, link: &LinkInfo, _store_path: &Path) -> Result<PullResult> {
        let repo_dir = self.repo_dir(link);
        let repo = Repository::open(&repo_dir)?;
        let branch_name = Self::primary_branch(&repo);

        // Fetch from origin.
        let mut remote = repo.find_remote("origin")?;
        remote.fetch(&[&branch_name], None, None)?;

        // Find the fetch head.
        let fetch_head = repo.find_reference(&format!("refs/remotes/origin/{branch_name}"))?;
        let fetch_commit = fetch_head.peel_to_commit()?;

        // Compare with local HEAD.
        let local_head = match repo.head() {
            Ok(h) => h.peel_to_commit()?,
            Err(_) => {
                // No local commits yet — just set HEAD.
                repo.set_head(&format!("refs/heads/{branch_name}"))?;
                // Create the branch pointing at fetch_commit.
                repo.branch(&branch_name, &fetch_commit, true)?;
                repo.checkout_head(Some(
                    git2::build::CheckoutBuilder::new().force(),
                ))?;
                return Ok(PullResult::Success {
                    revision: fetch_commit.id().to_string(),
                });
            }
        };

        if local_head.id() == fetch_commit.id() {
            return Ok(PullResult::UpToDate);
        }

        // Check if we can fast-forward.
        let analysis = repo.merge_analysis(&[&repo.find_annotated_commit(fetch_commit.id())?])?;

        if analysis.0.is_fast_forward() {
            // Fast-forward: move the branch ref and checkout.
            let refname = format!("refs/heads/{branch_name}");
            repo.reference(
                &refname,
                fetch_commit.id(),
                true,
                "syncor pull fast-forward",
            )?;
            repo.set_head(&refname)?;
            repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))?;

            Ok(PullResult::Success {
                revision: fetch_commit.id().to_string(),
            })
        } else {
            Ok(PullResult::Conflict {
                details: ConflictInfo {
                    message: "cannot fast-forward; manual resolution needed".to_string(),
                },
            })
        }
    }

    fn list_remote_links(&self, repo_url: &str) -> Result<Vec<RemoteLinkInfo>> {
        // Clone to a temp dir and read syncor.toml.
        let tmp = tempfile::tempdir()?;
        let _repo = match Repository::clone(repo_url, tmp.path()) {
            Ok(r) => r,
            Err(_) => return Ok(vec![]),
        };

        let toml_path = tmp.path().join("syncor.toml");
        if !toml_path.exists() {
            return Ok(vec![]);
        }

        let contents = std::fs::read_to_string(&toml_path)?;

        // Minimal parse — just extract link names from [[links]] entries.
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

        let manifest: Manifest = toml::from_str(&contents)
            .map_err(|e| SyncorError::Config(e.to_string()))?;

        Ok(manifest
            .links
            .into_iter()
            .map(|l| RemoteLinkInfo {
                name: l.name,
                created_at: l.created_at.unwrap_or_default(),
            })
            .collect())
    }

    fn has_remote_changes(&self, link: &LinkInfo) -> Result<bool> {
        let repo_dir = self.repo_dir(link);
        let repo = Repository::open(&repo_dir)?;
        let branch_name = Self::primary_branch(&repo);

        // Fetch.
        let mut remote = repo.find_remote("origin")?;
        remote.fetch(&[&branch_name], None, None)?;

        // Compare local HEAD with remote HEAD.
        let local_oid = repo.head().ok().and_then(|h| h.target());
        let remote_oid = repo
            .find_reference(&format!("refs/remotes/origin/{branch_name}"))
            .ok()
            .and_then(|r| r.target());

        match (local_oid, remote_oid) {
            (Some(local), Some(remote)) => Ok(local != remote),
            (None, Some(_)) => Ok(true),
            _ => Ok(false),
        }
    }
}
