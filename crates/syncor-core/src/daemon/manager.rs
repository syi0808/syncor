use std::fs;
use std::io::Write;

use crate::config::{LinksRegistry, SyncorConfig, SyncorPaths};
use crate::error::Result;

// ---------------------------------------------------------------------------
// DaemonManager
// ---------------------------------------------------------------------------

/// Manages the lifecycle of the Syncor daemon process.
///
/// Responsibilities:
/// - Write / remove the PID file.
/// - Load the links registry on startup.
/// - Run a command loop (IPC commands forwarded here once the IPC server is
///   available; for now a placeholder channel is used).
/// - Clean up stale PID / socket files left by crashed processes.
pub struct DaemonManager {
    paths: SyncorPaths,
    config: SyncorConfig,
}

impl DaemonManager {
    pub fn new(paths: SyncorPaths, config: SyncorConfig) -> Self {
        Self { paths, config }
    }

    /// Run the daemon.
    ///
    /// 1. Write the PID file.
    /// 2. Load the links registry.
    /// 3. Enter the command loop (placeholder channel until the IPC server is wired in).
    /// 4. Remove the PID file on exit.
    pub async fn run(&self) -> Result<()> {
        // Ensure data directories exist.
        self.paths.ensure_dirs()?;

        // --- Write PID file ---
        let pid = std::process::id();
        let pid_path = self.paths.pid_file();
        {
            let mut f = fs::File::create(&pid_path)?;
            writeln!(f, "{}", pid)?;
        }
        tracing::info!("daemon started (pid {})", pid);

        // --- Load links registry ---
        let registry = LinksRegistry::load(&self.paths.links_file())?;
        let link_count = registry.iter().count();
        tracing::info!("loaded {} link(s) from registry", link_count);

        // --- Command loop (placeholder) ---
        // The real IPC server (daemon/server.rs) will send commands over a
        // tokio channel once it is implemented.  For now we just park the
        // task and log that we are ready.
        tracing::info!(
            "daemon ready (debounce={}s, poll={}s)",
            self.config.debounce_secs,
            self.config.default_poll_interval_secs,
        );

        // Use a one-shot channel as a placeholder for the future shutdown
        // signal that the IPC server / signal handler will send.
        let (_tx, rx) = tokio::sync::oneshot::channel::<()>();
        let _ = rx.await; // park until shutdown signal arrives

        // --- Cleanup ---
        let _ = fs::remove_file(&pid_path);
        tracing::info!("daemon exited cleanly");
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Static helpers — do not require an instance
    // -----------------------------------------------------------------------

    /// Returns `true` if a daemon process described by the PID file is alive.
    pub fn is_running(paths: &SyncorPaths) -> bool {
        let pid_path = paths.pid_file();
        if !pid_path.exists() {
            return false;
        }

        // Read PID from file.
        let pid: i32 = match fs::read_to_string(&pid_path)
            .ok()
            .and_then(|s| s.trim().parse().ok())
        {
            Some(p) => p,
            None => return false,
        };

        // Use `kill(pid, 0)` — succeeds (returns 0) if the process exists.
        (unsafe { libc::kill(pid, 0) }) == 0
    }

    /// Remove a stale PID file and Unix-domain socket that were left behind
    /// by a previously crashed daemon.
    pub fn cleanup_stale(paths: &SyncorPaths) {
        let pid_path = paths.pid_file();
        let sock_path = paths.socket_path();

        if pid_path.exists() {
            let _ = fs::remove_file(&pid_path);
            tracing::debug!("removed stale pid file: {}", pid_path.display());
        }
        if sock_path.exists() {
            let _ = fs::remove_file(&sock_path);
            tracing::debug!("removed stale socket: {}", sock_path.display());
        }
    }
}
