use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};

use syncor_core::config::{LinksRegistry, SyncorConfig, SyncorPaths};
use syncor_core::daemon::manager::DaemonManager;
use syncor_core::link::{LinkId, LinkInfo, LinkMode};
use syncor_core::progress::ProgressReporter;
use syncor_core::sync::engine::SyncEngine;
use syncor_core::sync::state::StateDb;
use syncor_core::transport::git::GitTransport;
use syncor_core::transport::SyncTransport;

mod progress;

use progress::make_reporter;

// ---------------------------------------------------------------------------
// CLI definition
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(name = "syncor", version, about = "Cross-machine directory sync")]
struct Cli {
    /// Disable progress bars/spinners (also disabled when stderr is not a TTY)
    #[arg(long, global = true)]
    no_progress: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Link a local directory to a remote repo
    Link {
        /// Local directory to sync
        dir: PathBuf,
        /// Git repository URL
        #[arg(long)]
        repo: Option<String>,
        /// Human-readable name for this link
        #[arg(long)]
        name: Option<String>,
    },
    /// Connect to an existing remote link
    Connect {
        /// Git repository URL
        repo: String,
        /// Remote directory name to connect
        #[arg(long)]
        dir: Option<String>,
        /// Local path to sync to
        #[arg(long, value_name = "PATH")]
        to: Option<PathBuf>,
    },
    /// Show status of linked directories
    Status {
        /// Specific directory to check (all if omitted)
        dir: Option<PathBuf>,
    },
    /// Remove a link by directory
    Unlink {
        /// Local directory to unlink
        dir: PathBuf,
    },
    /// Remove a link by name
    Disconnect {
        /// Name of the link to disconnect
        name: String,
        /// Skip confirmation when the link has more than one mount.
        #[arg(long)]
        force: bool,
    },
    /// Resolve conflicts for a linked directory
    Resolve {
        /// Local directory with conflicts
        dir: PathBuf,
    },
    /// Show sync log for a linked directory
    Log {
        /// Local directory to show log for
        dir: PathBuf,
    },
    /// Manage configuration
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Manage the background daemon
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },
    /// Push local changes to remote
    Push {
        /// Directory to push (all push-mode links if omitted)
        dir: Option<PathBuf>,
    },
    /// Pull remote changes to local
    Pull {
        /// Directory to pull (all pull-mode links if omitted)
        dir: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Set a configuration value
    Set {
        /// Configuration key (debounce_secs, default_poll_interval_secs)
        key: String,
        /// Value to set
        value: String,
    },
}

#[derive(Subcommand)]
enum DaemonAction {
    /// Start the daemon
    Start,
    /// Stop the daemon
    Stop,
    /// Show daemon status
    Status,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn load_paths() -> Result<SyncorPaths> {
    let paths = SyncorPaths::new();
    paths
        .ensure_dirs()
        .context("failed to create syncor directories")?;
    Ok(paths)
}

fn load_registry(paths: &SyncorPaths) -> Result<LinksRegistry> {
    LinksRegistry::load(&paths.links_file()).context("failed to load links registry")
}

fn save_registry(paths: &SyncorPaths, registry: &LinksRegistry) -> Result<()> {
    registry
        .save(&paths.links_file())
        .context("failed to save links registry")
}

fn find_link_by_dir<'a>(registry: &'a LinksRegistry, dir: &PathBuf) -> Result<&'a LinkInfo> {
    let canonical = std::fs::canonicalize(dir)
        .with_context(|| format!("cannot resolve path: {}", dir.display()))?;
    registry
        .get_by_dir(&canonical)
        .with_context(|| format!("no link found for directory: {}", canonical.display()))
}

fn open_state_db(paths: &SyncorPaths) -> Result<StateDb> {
    let db_path = paths.link_state_db();
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    StateDb::open(&db_path).context("failed to open state database")
}

fn make_engine(paths: &SyncorPaths) -> SyncEngine {
    let transport = GitTransport::new(paths.clone());
    SyncEngine::new(paths.clone(), Box::new(transport))
}

enum RollbackKind<'a> {
    /// A new link was registered; undo fully: remove registry row, repo dir,
    /// lock file, state DB row.
    NewLink { info: &'a LinkInfo },
    /// An existing link gained a mount; undo just that mount. Leaves other
    /// mounts, the clone, state, and lock intact.
    MountAdded {
        id: &'a LinkId,
        dir: &'a std::path::Path,
    },
}

fn rollback(paths: &SyncorPaths, registry: &mut LinksRegistry, kind: RollbackKind) {
    match kind {
        RollbackKind::NewLink { info } => {
            let _ = registry.remove(&info.id);
            let _ = save_registry(paths, registry);
            let _ = std::fs::remove_dir_all(paths.link_repo_dir(&info.id));
            let _ = std::fs::remove_file(paths.link_lock_file(&info.id));
            if let Ok(db) = StateDb::open(paths.link_state_db()) {
                let _ = db.delete_state(info.id.as_str());
            }
        }
        RollbackKind::MountAdded { id, dir } => {
            let _ = registry.remove_mount(id, dir);
            let _ = save_registry(paths, registry);
            // Intentionally: do NOT touch link_repo_dir, lock file, or state.
        }
    }
}

// ---------------------------------------------------------------------------
// Command implementations
// ---------------------------------------------------------------------------

fn cmd_link(
    dir: PathBuf,
    repo: Option<String>,
    name: Option<String>,
    no_progress: bool,
) -> Result<()> {
    let paths = load_paths()?;
    let mut registry = load_registry(&paths)?;
    let reporter = make_reporter(no_progress);
    let reporter_ref: &dyn ProgressReporter = &*reporter;

    let canonical = std::fs::canonicalize(&dir)
        .with_context(|| format!("cannot resolve path: {}", dir.display()))?;

    let repo_url = match repo {
        Some(r) => r,
        None => {
            let input: String = dialoguer::Input::new()
                .with_prompt("Git repository URL")
                .interact_text()?;
            input
        }
    };

    let link_name = match name {
        Some(n) => n,
        None => {
            let default_name = canonical
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "default".to_string());
            let input: String = dialoguer::Input::new()
                .with_prompt("Link name")
                .default(default_name)
                .interact_text()?;
            input
        }
    };

    let id = LinkId::from_parts(&repo_url, &link_name);
    let info = LinkInfo {
        id,
        name: link_name.clone(),
        repo: repo_url.clone(),
        local_dirs: vec![canonical.clone()],
        mode: LinkMode::Push,
        poll_interval_secs: None,
    };

    registry
        .add(info.clone())
        .context("failed to register link")?;
    save_registry(&paths, &registry)?;

    let engine = make_engine(&paths);
    if let Err(e) = engine.init_link(&info) {
        eprintln!("Failed to initialise remote: {e}");
        rollback(&paths, &mut registry, RollbackKind::NewLink { info: &info });
        bail!("link failed: could not initialise remote");
    }

    println!("Linked {} -> {}", canonical.display(), repo_url);
    println!("Performing initial push...");

    match engine.push(&info, reporter_ref) {
        Ok(result) => {
            if result.pushed {
                println!(
                    "Initial push complete (snapshot: {})",
                    result.snapshot_id.as_deref().unwrap_or("n/a")
                );
            } else {
                println!("Nothing to push.");
            }
        }
        Err(e) => {
            eprintln!("Initial push failed: {e}");
            eprintln!("Rolling back link registration...");
            rollback(&paths, &mut registry, RollbackKind::NewLink { info: &info });
            bail!("link failed: initial push could not complete");
        }
    }

    Ok(())
}

fn cmd_connect(
    repo: String,
    dir_name: Option<String>,
    to: Option<PathBuf>,
    no_progress: bool,
) -> Result<()> {
    let paths = load_paths()?;
    let mut registry = load_registry(&paths)?;
    let reporter = make_reporter(no_progress);
    let reporter_ref: &dyn ProgressReporter = &*reporter;

    let transport = GitTransport::new(paths.clone());
    let remote_links = transport
        .list_remote_links(&repo, reporter_ref)
        .context("failed to list remote links")?;

    if remote_links.is_empty() {
        bail!("No links found in remote repository: {repo}");
    }

    let selected_name = match dir_name {
        Some(n) => {
            if !remote_links.iter().any(|l| l.name == n) {
                bail!(
                    "Remote link '{}' not found. Available: {}",
                    n,
                    remote_links
                        .iter()
                        .map(|l| l.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            n
        }
        None => {
            if remote_links.len() == 1 {
                remote_links[0].name.clone()
            } else {
                let names: Vec<&str> = remote_links.iter().map(|l| l.name.as_str()).collect();
                let selection = dialoguer::Select::new()
                    .with_prompt("Select directory to connect")
                    .items(&names)
                    .default(0)
                    .interact()?;
                names[selection].to_string()
            }
        }
    };

    let local_dir = match to {
        Some(p) => {
            std::fs::create_dir_all(&p)?;
            std::fs::canonicalize(&p)?
        }
        None => {
            let cwd = std::env::current_dir()?;
            let target = cwd.join(&selected_name);
            std::fs::create_dir_all(&target)?;
            std::fs::canonicalize(&target)?
        }
    };

    let id = LinkId::from_parts(&repo, &selected_name);

    match registry.get_by_id(&id).cloned() {
        Some(existing) if existing.mode == LinkMode::Pull => {
            if existing.local_dirs.contains(&local_dir) {
                println!(
                    "{} is already mounted for link {}.",
                    local_dir.display(),
                    existing.name
                );
                return Ok(());
            }
            registry
                .add_mount(&id, local_dir.clone())
                .context("failed to register mount")?;
            save_registry(&paths, &registry)?;

            let engine = make_engine(&paths);
            let info_after = registry.get_by_id(&id).expect("just added").clone();
            if let Err(e) = engine.restore_latest_to(&info_after, &local_dir) {
                eprintln!("Initial restore failed: {e}");
                eprintln!("Rolling back mount registration...");
                rollback(
                    &paths,
                    &mut registry,
                    RollbackKind::MountAdded {
                        id: &id,
                        dir: &local_dir,
                    },
                );
                bail!(
                    "connect failed: could not restore into {}",
                    local_dir.display()
                );
            }

            println!(
                "Mounted {} for link {} (now {} mount(s))",
                local_dir.display(),
                existing.name,
                info_after.local_dirs.len()
            );
            Ok(())
        }
        Some(existing) => {
            // Push mode — reject. We use bail! (rather than SyncorError::LinkAlreadyExists)
            // because the user-facing message is clearer and the spec's concern is that
            // the push-single-mount invariant is preserved, which this achieves.
            bail!(
                "link {} already exists as {:?}-mode; multi-mount is pull-only",
                existing.name,
                existing.mode
            );
        }
        None => {
            let info = LinkInfo {
                id: id.clone(),
                name: selected_name.clone(),
                repo: repo.clone(),
                local_dirs: vec![local_dir.clone()],
                mode: LinkMode::Pull,
                poll_interval_secs: None,
            };
            registry
                .add(info.clone())
                .context("failed to register link")?;
            save_registry(&paths, &registry)?;

            let engine = make_engine(&paths);
            if let Err(e) = engine.init_link(&info) {
                eprintln!("Failed to initialise remote: {e}");
                rollback(&paths, &mut registry, RollbackKind::NewLink { info: &info });
                bail!("connect failed: could not initialise remote");
            }

            println!("Connected {} <- {}", local_dir.display(), repo);
            println!("Performing initial restore...");

            match engine.restore_latest(&info) {
                Ok(result) => {
                    if result.restored {
                        println!("Restored {} file(s).", result.files_restored);
                    } else {
                        println!("No snapshots to restore.");
                    }
                    Ok(())
                }
                Err(e) => {
                    eprintln!("Initial restore failed: {e}");
                    eprintln!("Rolling back link registration...");
                    rollback(&paths, &mut registry, RollbackKind::NewLink { info: &info });
                    bail!("connect failed: initial restore could not complete");
                }
            }
        }
    }
}

fn cmd_status(dir: Option<PathBuf>) -> Result<()> {
    let paths = load_paths()?;
    let registry = load_registry(&paths)?;
    let db = open_state_db(&paths)?;

    let links: Vec<&LinkInfo> = match &dir {
        Some(d) => {
            let canonical = std::fs::canonicalize(d)
                .with_context(|| format!("cannot resolve path: {}", d.display()))?;
            match registry.get_by_dir(&canonical) {
                Some(link) => vec![link],
                None => bail!("no link found for directory: {}", canonical.display()),
            }
        }
        None => registry.iter().collect(),
    };

    if links.is_empty() {
        println!("No links configured. Use 'syncor link' to get started.");
        return Ok(());
    }

    for link in &links {
        let mode_str = match link.mode {
            LinkMode::Push => "push",
            LinkMode::Pull => "pull",
        };
        let has_conflicts = db.has_conflicts(link.id.as_str()).unwrap_or(false);
        let state = db.get_sync_state(link.id.as_str()).ok().flatten();
        let last_sync = state
            .as_ref()
            .and_then(|s| s.last_sync_at.as_deref())
            .unwrap_or("never");

        println!("  {} [{}]", link.name, mode_str);
        println!("    repo: {}", link.repo);
        println!("    last sync: {}", last_sync);
        println!("    mounts:");
        for dir in &link.local_dirs {
            println!("      - {}", dir.display());
        }
        if has_conflicts {
            for dir in &link.local_dirs {
                println!("    !! CONFLICTS — run 'syncor resolve {}'", dir.display());
            }
        }
        println!();
    }

    Ok(())
}

fn cmd_unlink(dir: PathBuf) -> Result<()> {
    let paths = load_paths()?;
    let mut registry = load_registry(&paths)?;
    let link = find_link_by_dir(&registry, &dir)?.clone();

    let canonical = std::fs::canonicalize(&dir)
        .with_context(|| format!("cannot resolve path: {}", dir.display()))?;

    let result = registry
        .remove_mount(&link.id, &canonical)
        .context("failed to remove mount")?;
    save_registry(&paths, &registry)?;

    if result.last_mount_removed {
        // Final mount — clean up link-wide state as before.
        let _ = registry.remove(&link.id);
        save_registry(&paths, &registry)?;
        if let Ok(db) = StateDb::open(paths.link_state_db()) {
            let _ = db.delete_state(link.id.as_str());
        }
        let repo_dir = paths.link_repo_dir(&link.id);
        if repo_dir.exists() {
            let _ = std::fs::remove_dir_all(&repo_dir);
            println!("  Removed repo dir: {}", repo_dir.display());
        }
        let lock_file = paths.link_lock_file(&link.id);
        if lock_file.exists() {
            let _ = std::fs::remove_file(&lock_file);
            println!("  Removed lock file: {}", lock_file.display());
        }
        println!("Unlinked {} ({})", link.name, canonical.display());
    } else {
        let remaining = link.local_dirs.len().saturating_sub(1);
        println!(
            "Unmounted {} from link {} (still mounted at {} other dir(s))",
            canonical.display(),
            link.name,
            remaining
        );
    }

    Ok(())
}

fn cmd_disconnect(name: String, force: bool) -> Result<()> {
    let paths = load_paths()?;
    let mut registry = load_registry(&paths)?;

    let link = registry
        .get_by_name(&name)
        .with_context(|| format!("no link found with name: {name}"))?
        .clone();

    if link.local_dirs.len() > 1 && !force {
        let prompt = format!(
            "Link {} has {} mounts:\n  - {}\nDisconnect all? [y/N]",
            link.name,
            link.local_dirs.len(),
            link.local_dirs
                .iter()
                .map(|d| d.display().to_string())
                .collect::<Vec<_>>()
                .join("\n  - ")
        );
        let confirmed = dialoguer::Confirm::new()
            .with_prompt(prompt)
            .default(false)
            .interact()?;
        if !confirmed {
            bail!("disconnect cancelled");
        }
    }

    registry.remove(&link.id).context("failed to remove link")?;
    save_registry(&paths, &registry)?;

    if let Ok(db) = StateDb::open(paths.link_state_db()) {
        let _ = db.delete_state(link.id.as_str());
    }
    let repo_dir = paths.link_repo_dir(&link.id);
    if repo_dir.exists() {
        let _ = std::fs::remove_dir_all(&repo_dir);
        println!("  Removed repo dir: {}", repo_dir.display());
    }
    let lock_file = paths.link_lock_file(&link.id);
    if lock_file.exists() {
        let _ = std::fs::remove_file(&lock_file);
        println!("  Removed lock file: {}", lock_file.display());
    }

    let mounts_str = link
        .local_dirs
        .iter()
        .map(|d| d.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    println!(
        "Disconnected {} ({} mount(s): {})",
        link.name,
        link.local_dirs.len(),
        mounts_str
    );
    Ok(())
}

fn cmd_resolve(dir: PathBuf) -> Result<()> {
    let paths = load_paths()?;
    let registry = load_registry(&paths)?;
    let link = find_link_by_dir(&registry, &dir)?.clone();
    let db = open_state_db(&paths)?;

    let conflicts = db
        .list_conflicts(link.id.as_str())
        .context("failed to load conflicts")?;

    if conflicts.is_empty() {
        println!("No conflicts for {}.", link.name);
        return Ok(());
    }

    println!("Found {} conflict(s) for {}:", conflicts.len(), link.name);

    // For each conflict, let user choose local or remote
    let choices = &["Keep local", "Use remote"];
    let store_dir = paths
        .link_repo_dir(&link.id)
        .join("stores")
        .join(&link.name);

    for conflict in &conflicts {
        println!("\n  File: {}", conflict.file_path);
        if let Some(ref lh) = conflict.local_hash {
            println!("    local hash:  {}", &lh[..lh.len().min(16)]);
        }
        if let Some(ref rh) = conflict.remote_hash {
            println!("    remote hash: {}", &rh[..rh.len().min(16)]);
        }

        let selection = dialoguer::Select::new()
            .with_prompt("Resolution")
            .items(choices)
            .default(0)
            .interact()?;

        if selection == 1 {
            // Apply remote version
            if let Some(ref remote_hash) = conflict.remote_hash {
                let pack_dir = store_dir.join("packs");
                if pack_dir.exists() {
                    let pack_set = chkpt_core::store::pack::PackSet::open_all(&pack_dir)
                        .context("failed to open pack set")?;
                    let content = pack_set
                        .read(remote_hash)
                        .context("failed to read remote blob")?;
                    let file_path = link.primary_dir().join(&conflict.file_path);
                    if let Some(parent) = file_path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::write(&file_path, content)?;
                    println!("    -> Applied remote version.");
                } else {
                    println!("    -> Pack directory not found, skipping.");
                }
            } else {
                // Remote deleted the file
                let file_path = link.primary_dir().join(&conflict.file_path);
                let _ = std::fs::remove_file(&file_path);
                println!("    -> Removed local file (remote deleted).");
            }
        } else {
            println!("    -> Kept local version.");
        }
    }

    db.clear_conflicts(link.id.as_str())
        .context("failed to clear conflicts")?;
    println!("\nAll conflicts resolved for {}.", link.name);

    Ok(())
}

fn cmd_log(dir: PathBuf) -> Result<()> {
    let paths = load_paths()?;
    let registry = load_registry(&paths)?;
    let link = find_link_by_dir(&registry, &dir)?;
    let db = open_state_db(&paths)?;

    let entries = db
        .list_log(link.id.as_str(), Some(50))
        .context("failed to load sync log")?;

    if entries.is_empty() {
        println!("No sync history for {}.", link.name);
        return Ok(());
    }

    println!("Sync log for {} (most recent first):\n", link.name);
    for entry in &entries {
        let msg = entry.message.as_deref().unwrap_or("");
        println!(
            "  [{}] {} — {} {}",
            entry.created_at, entry.action, entry.status, msg
        );
    }

    Ok(())
}

fn cmd_config_set(key: String, value: String) -> Result<()> {
    let paths = load_paths()?;
    let mut config = SyncorConfig::load(&paths.config_file()).context("failed to load config")?;

    match key.as_str() {
        "debounce_secs" => {
            config.debounce_secs = value.parse().context("invalid value for debounce_secs")?;
        }
        "default_poll_interval_secs" => {
            config.default_poll_interval_secs = value
                .parse()
                .context("invalid value for default_poll_interval_secs")?;
        }
        _ => bail!(
            "unknown config key: {key}. Valid keys: debounce_secs, default_poll_interval_secs"
        ),
    }

    config
        .save(&paths.config_file())
        .context("failed to save config")?;
    println!("Set {} = {}", key, value);

    Ok(())
}

async fn cmd_daemon_start() -> Result<()> {
    let paths = load_paths()?;

    if DaemonManager::is_running(&paths) {
        println!("Daemon is already running.");
        return Ok(());
    }

    DaemonManager::cleanup_stale(&paths);

    let config = SyncorConfig::load(&paths.config_file()).context("failed to load config")?;

    println!("Starting daemon...");
    let manager = DaemonManager::new(paths.clone(), config);
    manager.run().await.context("daemon exited with error")?;

    Ok(())
}

fn cmd_daemon_stop() -> Result<()> {
    let paths = load_paths()?;

    if !DaemonManager::is_running(&paths) {
        println!("Daemon is not running.");
        return Ok(());
    }

    // Validate via IPC socket before sending signal
    let sock = paths.socket_path();
    if sock.exists() {
        match std::os::unix::net::UnixStream::connect(&sock) {
            Ok(_stream) => {
                // Socket connectable = likely our daemon (not 100% — another process
                // could theoretically listen on the same path, but unlikely in practice).
                let pid_file = paths.pid_file();
                if let Ok(content) = std::fs::read_to_string(&pid_file) {
                    if let Ok(pid) = content.trim().parse::<i32>() {
                        unsafe { libc::kill(pid, libc::SIGTERM) };
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
    } else {
        println!("Daemon stopped.");
    }
    DaemonManager::cleanup_stale(&paths);

    Ok(())
}

fn cmd_daemon_status() -> Result<()> {
    let paths = load_paths()?;

    if DaemonManager::is_running(&paths) {
        let pid_str = std::fs::read_to_string(paths.pid_file()).unwrap_or_default();
        println!("Daemon is running (pid {}).", pid_str.trim());
    } else {
        println!("Daemon is not running.");
    }

    Ok(())
}

fn cmd_push(dir: Option<PathBuf>, no_progress: bool) -> Result<()> {
    let paths = load_paths()?;
    let registry = load_registry(&paths)?;
    let engine = make_engine(&paths);
    let reporter = make_reporter(no_progress);
    let reporter_ref: &dyn ProgressReporter = &*reporter;

    let links: Vec<LinkInfo> = match dir {
        Some(d) => vec![find_link_by_dir(&registry, &d)?.clone()],
        None => registry
            .iter()
            .filter(|l| l.mode == LinkMode::Push)
            .cloned()
            .collect(),
    };

    if links.is_empty() {
        println!("No push-mode links found.");
        return Ok(());
    }

    for link in &links {
        match engine.push(link, reporter_ref) {
            Ok(result) => {
                if result.pushed {
                    println!(
                        "✓ Push complete: {} (snapshot {})",
                        link.name,
                        result.snapshot_id.as_deref().unwrap_or("n/a")
                    );
                } else {
                    println!("✓ {}: nothing to push.", link.name);
                }
            }
            Err(e) => eprintln!("✗ {}: {e}", link.name),
        }
    }

    Ok(())
}

fn cmd_pull(dir: Option<PathBuf>, no_progress: bool) -> Result<()> {
    let paths = load_paths()?;
    let registry = load_registry(&paths)?;
    let engine = make_engine(&paths);
    let reporter = make_reporter(no_progress);
    let reporter_ref: &dyn ProgressReporter = &*reporter;

    let links: Vec<LinkInfo> = match dir {
        Some(d) => vec![find_link_by_dir(&registry, &d)?.clone()],
        None => registry
            .iter()
            .filter(|l| l.mode == LinkMode::Pull)
            .cloned()
            .collect(),
    };

    if links.is_empty() {
        println!("No pull-mode links found.");
        return Ok(());
    }

    for link in &links {
        match engine.pull(link, reporter_ref) {
            Ok(result) => {
                if result.restored {
                    println!(
                        "✓ Pull complete: {} ({} files restored)",
                        link.name, result.files_restored
                    );
                } else {
                    println!("✓ {}: already up to date.", link.name);
                }
            }
            Err(e) => eprintln!("✗ {}: {e}", link.name),
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();

    let no_progress = cli.no_progress;
    match cli.command {
        Commands::Link { dir, repo, name } => cmd_link(dir, repo, name, no_progress),
        Commands::Connect { repo, dir, to } => cmd_connect(repo, dir, to, no_progress),
        Commands::Status { dir } => cmd_status(dir),
        Commands::Unlink { dir } => cmd_unlink(dir),
        Commands::Disconnect { name, force } => cmd_disconnect(name, force),
        Commands::Resolve { dir } => cmd_resolve(dir),
        Commands::Log { dir } => cmd_log(dir),
        Commands::Config { action } => match action {
            ConfigAction::Set { key, value } => cmd_config_set(key, value),
        },
        Commands::Daemon { action } => match action {
            DaemonAction::Start => cmd_daemon_start().await,
            DaemonAction::Stop => cmd_daemon_stop(),
            DaemonAction::Status => cmd_daemon_status(),
        },
        Commands::Push { dir } => cmd_push(dir, no_progress),
        Commands::Pull { dir } => cmd_pull(dir, no_progress),
    }
}
