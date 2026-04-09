// crates/syncor-core/tests/conflict_test.rs
use syncor_core::sync::conflict::{
    detect_conflicts, ConflictResolver, KeepLocalResolver,
    FileAction, Conflict, Resolution, ManifestMap,
};

fn map(entries: &[(&str, [u8; 16])]) -> ManifestMap {
    entries.iter().map(|(p, h)| (p.to_string(), *h)).collect()
}

#[test]
fn no_change() {
    let a = [1u8; 16];
    let base = map(&[("f.txt", a)]);
    let local = map(&[("f.txt", a)]);
    let remote = map(&[("f.txt", a)]);
    let actions = detect_conflicts(&base, &local, &remote);
    assert!(actions.is_empty());
}

#[test]
fn remote_changed_auto_apply() {
    let a = [1u8; 16];
    let b = [2u8; 16];
    let base = map(&[("f.txt", a)]);
    let local = map(&[("f.txt", a)]);
    let remote = map(&[("f.txt", b)]);
    let actions = detect_conflicts(&base, &local, &remote);
    assert_eq!(actions.len(), 1);
    assert!(matches!(actions[0], FileAction::ApplyRemote { .. }));
}

#[test]
fn local_changed_auto_keep() {
    let a = [1u8; 16];
    let b = [2u8; 16];
    let base = map(&[("f.txt", a)]);
    let local = map(&[("f.txt", b)]);
    let remote = map(&[("f.txt", a)]);
    let actions = detect_conflicts(&base, &local, &remote);
    assert!(actions.is_empty());
}

#[test]
fn both_changed_differently_is_conflict() {
    let a = [1u8; 16];
    let b = [2u8; 16];
    let c = [3u8; 16];
    let base = map(&[("f.txt", a)]);
    let local = map(&[("f.txt", b)]);
    let remote = map(&[("f.txt", c)]);
    let actions = detect_conflicts(&base, &local, &remote);
    assert_eq!(actions.len(), 1);
    assert!(matches!(actions[0], FileAction::Conflict { .. }));
}

#[test]
fn remote_added_auto_apply() {
    let b = [2u8; 16];
    let base = map(&[]);
    let local = map(&[]);
    let remote = map(&[("new.txt", b)]);
    let actions = detect_conflicts(&base, &local, &remote);
    assert_eq!(actions.len(), 1);
    assert!(matches!(actions[0], FileAction::ApplyRemote { .. }));
}

#[test]
fn both_added_differently_is_conflict() {
    let b = [2u8; 16];
    let c = [3u8; 16];
    let base = map(&[]);
    let local = map(&[("f.txt", b)]);
    let remote = map(&[("f.txt", c)]);
    let actions = detect_conflicts(&base, &local, &remote);
    assert_eq!(actions.len(), 1);
    assert!(matches!(actions[0], FileAction::Conflict { .. }));
}

#[test]
fn local_deleted_remote_changed_is_conflict() {
    let a = [1u8; 16];
    let b = [2u8; 16];
    let base = map(&[("f.txt", a)]);
    let local = map(&[]);
    let remote = map(&[("f.txt", b)]);
    let actions = detect_conflicts(&base, &local, &remote);
    assert_eq!(actions.len(), 1);
    assert!(matches!(actions[0], FileAction::Conflict { .. }));
}

#[test]
fn remote_deleted_auto_apply() {
    let a = [1u8; 16];
    let base = map(&[("f.txt", a)]);
    let local = map(&[("f.txt", a)]);
    let remote = map(&[]);
    let actions = detect_conflicts(&base, &local, &remote);
    assert_eq!(actions.len(), 1);
    assert!(matches!(actions[0], FileAction::DeleteLocal { .. }));
}
