// LinkLock is already implemented in crate::sync::engine.
// Re-export it so callers can import from the daemon module if desired.
pub use crate::sync::engine::LinkLock;

// ---------------------------------------------------------------------------
// SyncJob — describes a unit of work the daemon should execute
// ---------------------------------------------------------------------------

/// A job dispatched to a daemon worker.
#[derive(Debug, Clone)]
pub enum SyncJob {
    /// Push local changes for the given link to the remote.
    Push { link_id: String },
    /// Pull remote changes for the given link to the local directory.
    Pull { link_id: String },
}
