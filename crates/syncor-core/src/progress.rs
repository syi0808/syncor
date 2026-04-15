//! Progress reporting trait + null/test implementations.
//!
//! Consumers (sync pipelines, transports) call `phase_start` → `phase_tick*` →
//! `phase_end` for each logical phase. `ItemTotal` tells the reporter what kind
//! of bar/spinner to draw; unknown totals render as spinners, counts as count
//! bars, and byte totals as byte/ETA bars.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Phase {
    Scan,
    DetectChanges,
    Hash,
    PackFinish,
    Catalog,
    GitStage,
    GitCommit,
    GitEnumerate,
    GitCount,
    GitCompress,
    GitWrite,
    GitReceive,
    GitResolve,
    Merge,
    Restore,
    Cleanup,
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Phase::Scan => "Scanning workspace",
            Phase::DetectChanges => "Detecting changes",
            Phase::Hash => "Hashing & packing",
            Phase::PackFinish => "Finalizing pack",
            Phase::Catalog => "Writing snapshot catalog",
            Phase::GitStage => "Staging changes",
            Phase::GitCommit => "Committing",
            Phase::GitEnumerate => "Enumerating objects",
            Phase::GitCount => "Counting objects",
            Phase::GitCompress => "Compressing objects",
            Phase::GitWrite => "Writing objects",
            Phase::GitReceive => "Receiving objects",
            Phase::GitResolve => "Resolving deltas",
            Phase::Merge => "Merging",
            Phase::Restore => "Restoring files",
            Phase::Cleanup => "Cleaning up",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ItemTotal {
    Unknown,
    Count(u64),
    Bytes { items: u64, bytes: u64 },
}
