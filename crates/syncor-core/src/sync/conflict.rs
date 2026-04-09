use std::collections::{HashMap, HashSet};

/// A map from file path to its MD5/blake3/etc. hash (16 bytes).
pub type ManifestMap = HashMap<String, [u8; 16]>;

/// A conflict between base, local, and remote versions of a file.
#[derive(Debug, Clone, PartialEq)]
pub struct Conflict {
    pub path: String,
    pub base_hash: Option<[u8; 16]>,
    pub local_hash: Option<[u8; 16]>,
    pub remote_hash: Option<[u8; 16]>,
}

/// How a conflict was resolved.
#[derive(Debug, Clone, PartialEq)]
pub enum Resolution {
    KeepLocal,
    KeepRemote,
    Merged(Vec<u8>),
    Skip,
}

/// An action to take for a given file after three-point comparison.
#[derive(Debug, Clone, PartialEq)]
pub enum FileAction {
    /// Pull the remote version of this file locally.
    ApplyRemote { path: String, remote_hash: [u8; 16] },
    /// Delete the local copy (remote deleted it, local was unchanged).
    DeleteLocal { path: String },
    /// Both sides changed in incompatible ways; needs manual resolution.
    Conflict(Conflict),
}

/// Trait for resolving conflicts programmatically.
pub trait ConflictResolver {
    fn resolve(&self, conflict: &Conflict) -> Resolution;
}

/// Always keeps the local version when a conflict is detected.
pub struct KeepLocalResolver;

impl ConflictResolver for KeepLocalResolver {
    fn resolve(&self, _conflict: &Conflict) -> Resolution {
        Resolution::KeepLocal
    }
}

/// Three-point merge: compare base, local, and remote manifests and return
/// the list of actions that need to be taken.
///
/// Match table (base, local, remote):
/// (A, A, A) → no-op
/// (A, A, B) → ApplyRemote
/// (A, B, A) → no-op (keep local)
/// (A, B, B) → no-op (both same non-base change)
/// (A, B, C) → Conflict
/// (-, B, -) → no-op (local added)
/// (-, -, B) → ApplyRemote
/// (-, B, C) → Conflict (both added differently)
/// (A, -, A) → no-op (local deleted, remote unchanged)
/// (A, A, -) → DeleteLocal (remote deleted, local unchanged)
/// (A, -, -) → no-op (both deleted)
/// (A, B, -) → Conflict (local changed, remote deleted)
/// (A, -, B) → Conflict (local deleted, remote changed)
pub fn detect_conflicts(
    base: &ManifestMap,
    local: &ManifestMap,
    remote: &ManifestMap,
) -> Vec<FileAction> {
    let mut actions = Vec::new();

    // Collect all paths across all three manifests.
    let all_paths: HashSet<&String> = base
        .keys()
        .chain(local.keys())
        .chain(remote.keys())
        .collect();

    for path in all_paths {
        let b = base.get(path);
        let l = local.get(path);
        let r = remote.get(path);

        let action = match (b, l, r) {
            // (A, A, A) — no change
            (Some(bh), Some(lh), Some(rh)) if bh == lh && lh == rh => None,

            // (A, A, B) — remote changed, local untouched → apply remote
            (Some(bh), Some(lh), Some(rh)) if bh == lh && lh != rh => {
                Some(FileAction::ApplyRemote {
                    path: path.clone(),
                    remote_hash: *rh,
                })
            }

            // (A, B, A) — local changed, remote untouched → keep local (no-op)
            (Some(bh), Some(_lh), Some(rh)) if bh == rh => None,

            // (A, B, B) — both changed to same value → no-op
            (Some(_bh), Some(lh), Some(rh)) if lh == rh => None,

            // (A, B, C) — all three differ → conflict
            (Some(bh), Some(lh), Some(rh)) => Some(FileAction::Conflict(Conflict {
                path: path.clone(),
                base_hash: Some(*bh),
                local_hash: Some(*lh),
                remote_hash: Some(*rh),
            })),

            // (-, B, -) — local added, remote doesn't have it → no-op
            (None, Some(_lh), None) => None,

            // (-, -, B) — remote added, local doesn't have it → apply remote
            (None, None, Some(rh)) => Some(FileAction::ApplyRemote {
                path: path.clone(),
                remote_hash: *rh,
            }),

            // (-, B, C) — both added with different content → conflict
            (None, Some(lh), Some(rh)) if lh != rh => Some(FileAction::Conflict(Conflict {
                path: path.clone(),
                base_hash: None,
                local_hash: Some(*lh),
                remote_hash: Some(*rh),
            })),

            // (-, B, B) — both added with same content → no-op
            (None, Some(_lh), Some(_rh)) => None,

            // (A, -, A) — local deleted, remote unchanged → keep deletion (no-op)
            (Some(bh), None, Some(rh)) if bh == rh => None,

            // (A, -, B) — local deleted, remote changed → conflict
            (Some(bh), None, Some(rh)) => Some(FileAction::Conflict(Conflict {
                path: path.clone(),
                base_hash: Some(*bh),
                local_hash: None,
                remote_hash: Some(*rh),
            })),

            // (A, A, -) — remote deleted, local unchanged → delete local
            (Some(bh), Some(lh), None) if bh == lh => {
                Some(FileAction::DeleteLocal { path: path.clone() })
            }

            // (A, B, -) — local changed, remote deleted → conflict
            (Some(bh), Some(lh), None) => Some(FileAction::Conflict(Conflict {
                path: path.clone(),
                base_hash: Some(*bh),
                local_hash: Some(*lh),
                remote_hash: None,
            })),

            // (A, -, -) — both deleted → no-op
            (Some(_bh), None, None) => None,

            // (-, -, -) — shouldn't happen, but handle gracefully
            (None, None, None) => None,
        };

        if let Some(a) = action {
            actions.push(a);
        }
    }

    actions
}
