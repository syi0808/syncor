use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use syncor_core::progress::{ItemTotal, NullReporter, Phase, PhaseGuard, ProgressReporter};

#[test]
fn null_reporter_accepts_all_calls() {
    let r = NullReporter;
    r.phase_start(Phase::Hash, ItemTotal::Unknown);
    r.phase_tick(3, 100);
    r.log("retry 1/3: fetch failed");
    r.phase_end(Phase::Hash);
}

#[test]
fn null_reporter_is_object_safe() {
    let r: Box<dyn ProgressReporter> = Box::new(NullReporter);
    r.phase_start(Phase::Scan, ItemTotal::Count(5));
    r.phase_end(Phase::Scan);
}

#[derive(Default)]
struct CountingReporter {
    starts: AtomicUsize,
    ends: AtomicUsize,
    logs: Mutex<Vec<String>>,
}

impl ProgressReporter for CountingReporter {
    fn phase_start(&self, _: Phase, _: ItemTotal) {
        self.starts.fetch_add(1, Ordering::SeqCst);
    }
    fn phase_tick(&self, _: u64, _: u64) {}
    fn phase_end(&self, _: Phase) {
        self.ends.fetch_add(1, Ordering::SeqCst);
    }
    fn log(&self, msg: &str) {
        self.logs.lock().unwrap().push(msg.to_string());
    }
}

#[test]
fn phase_guard_ends_on_drop() {
    let r = CountingReporter::default();
    {
        let _g = PhaseGuard::new(&r, Phase::Hash, ItemTotal::Unknown);
    }
    assert_eq!(r.starts.load(Ordering::SeqCst), 1);
    assert_eq!(r.ends.load(Ordering::SeqCst), 1);
}

#[test]
fn phase_guard_end_is_idempotent() {
    let r = CountingReporter::default();
    {
        let g = PhaseGuard::new(&r, Phase::Hash, ItemTotal::Unknown);
        g.end();
    }
    // .end() on success + Drop on scope exit should total exactly 1 phase_end
    assert_eq!(r.ends.load(Ordering::SeqCst), 1);
}
