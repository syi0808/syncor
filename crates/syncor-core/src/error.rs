use thiserror::Error;

#[derive(Error, Debug)]
pub enum SyncorError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("chkpt error: {0}")]
    Chkpt(#[from] chkpt_core::error::ChkpttError),

    #[error("git error: {0}")]
    Git(String),

    #[error("config error: {0}")]
    Config(String),

    #[error("link not found: {0}")]
    LinkNotFound(String),

    #[error("link already exists: {0}")]
    LinkAlreadyExists(String),

    #[error("transport error: {0}")]
    Transport(String),

    #[error("conflict: {0}")]
    Conflict(String),

    #[error("lock held by another process")]
    LockHeld,

    #[error("daemon not running")]
    DaemonNotRunning,

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, SyncorError>;
