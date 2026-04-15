pub mod git;

use crate::error::Result;
use crate::link::LinkInfo;
use crate::progress::ProgressReporter;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct RemoteLinkInfo {
    pub name: String,
    pub created_at: String,
}

#[derive(Debug)]
pub struct ConflictInfo {
    pub message: String,
}

#[derive(Debug)]
pub enum PushResult {
    Success { revision: String },
    Conflict { details: ConflictInfo },
}

#[derive(Debug)]
pub enum PullResult {
    Success { revision: String },
    UpToDate,
    Conflict { details: ConflictInfo },
}

pub trait SyncTransport: Send + Sync {
    fn init_remote(&self, link: &LinkInfo, reporter: &dyn ProgressReporter) -> Result<()>;
    fn push(
        &self,
        link: &LinkInfo,
        store_path: &Path,
        reporter: &dyn ProgressReporter,
    ) -> Result<PushResult>;
    fn pull(
        &self,
        link: &LinkInfo,
        store_path: &Path,
        reporter: &dyn ProgressReporter,
    ) -> Result<PullResult>;
    fn list_remote_links(
        &self,
        repo: &str,
        reporter: &dyn ProgressReporter,
    ) -> Result<Vec<RemoteLinkInfo>>;
    fn has_remote_changes(&self, link: &LinkInfo, reporter: &dyn ProgressReporter) -> Result<bool>;
}
