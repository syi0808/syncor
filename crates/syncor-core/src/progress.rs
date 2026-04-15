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

pub trait ProgressReporter: Send + Sync {
    fn phase_start(&self, phase: Phase, total: ItemTotal);
    fn phase_tick(&self, items_delta: u64, bytes_delta: u64);
    fn phase_end(&self, phase: Phase);
    fn log(&self, msg: &str);
}

/// No-op reporter for tests and non-CLI consumers.
pub struct NullReporter;

impl ProgressReporter for NullReporter {
    fn phase_start(&self, _: Phase, _: ItemTotal) {}
    fn phase_tick(&self, _: u64, _: u64) {}
    fn phase_end(&self, _: Phase) {}
    fn log(&self, _: &str) {}
}

/// RAII guard that pairs `phase_start` with `phase_end`. On drop, emits
/// `phase_end` if `.end()` was not called. Reporter implementations must
/// tolerate a redundant `phase_end` call on an already-closed phase as a
/// no-op.
pub struct PhaseGuard<'a> {
    reporter: &'a dyn ProgressReporter,
    phase: Phase,
    ended: bool,
}

impl<'a> PhaseGuard<'a> {
    pub fn new(reporter: &'a dyn ProgressReporter, phase: Phase, total: ItemTotal) -> Self {
        reporter.phase_start(phase, total);
        Self {
            reporter,
            phase,
            ended: false,
        }
    }

    pub fn tick(&self, items: u64, bytes: u64) {
        self.reporter.phase_tick(items, bytes);
    }

    pub fn end(mut self) {
        self.reporter.phase_end(self.phase);
        self.ended = true;
    }
}

impl Drop for PhaseGuard<'_> {
    fn drop(&mut self) {
        if !self.ended {
            self.reporter.phase_end(self.phase);
        }
    }
}
