use std::sync::Mutex;
use syncor_core::progress::{ItemTotal, Phase, ProgressReporter};
use syncor_core::transport::git_progress::GitProgressParser;

#[derive(Default)]
struct RecordingReporter {
    events: Mutex<Vec<String>>,
}

impl ProgressReporter for RecordingReporter {
    fn phase_start(&self, phase: Phase, total: ItemTotal) {
        self.events.lock().unwrap().push(match total {
            ItemTotal::Unknown => format!("start {:?} unknown", phase),
            ItemTotal::Count(n) => format!("start {:?} count={}", phase, n),
            ItemTotal::Bytes { items, bytes } => {
                format!("start {:?} items={} bytes={}", phase, items, bytes)
            }
        });
    }
    fn phase_tick(&self, items: u64, bytes: u64) {
        self.events
            .lock()
            .unwrap()
            .push(format!("tick items+{} bytes+{}", items, bytes));
    }
    fn phase_end(&self, phase: Phase) {
        self.events.lock().unwrap().push(format!("end {:?}", phase));
    }
    fn log(&self, msg: &str) {
        self.events.lock().unwrap().push(format!("log {}", msg));
    }
}

fn feed(parser: &mut GitProgressParser<'_>, s: &str) {
    // Split on \r and \n, feed each line.
    for line in s.split(['\r', '\n']) {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            parser.feed_line(trimmed);
        }
    }
    parser.finish();
}

#[test]
fn parses_push_fixture() {
    let r = RecordingReporter::default();
    let mut p = GitProgressParser::new(&r);
    let data = include_str!("fixtures/git_output/push.txt");
    feed(&mut p, data);
    let events = r.events.lock().unwrap();
    // At minimum, we expect a GitEnumerate start, a GitWrite start,
    // and a terminal end.
    let starts: Vec<&String> = events.iter().filter(|e| e.starts_with("start")).collect();
    assert!(
        starts.iter().any(|e| e.contains("GitEnumerate")),
        "missing GitEnumerate start in events: {:?}",
        starts
    );
    assert!(
        starts.iter().any(|e| e.contains("GitWrite")),
        "missing GitWrite start in events: {:?}",
        starts
    );
    assert!(events.iter().any(|e| e.starts_with("end ")));
}

#[test]
fn parses_fetch_fixture() {
    let r = RecordingReporter::default();
    let mut p = GitProgressParser::new(&r);
    feed(&mut p, include_str!("fixtures/git_output/fetch.txt"));
    let events = r.events.lock().unwrap();
    let starts: Vec<&String> = events.iter().filter(|e| e.starts_with("start")).collect();
    assert!(
        starts
            .iter()
            .any(|e| e.contains("GitReceive") || e.contains("GitEnumerate")),
        "expected GitReceive or GitEnumerate in fetch fixture: {:?}",
        starts
    );
}

#[test]
fn parses_clone_fixture() {
    let r = RecordingReporter::default();
    let mut p = GitProgressParser::new(&r);
    feed(&mut p, include_str!("fixtures/git_output/clone.txt"));
    let events = r.events.lock().unwrap();
    let starts: Vec<&String> = events.iter().filter(|e| e.starts_with("start")).collect();
    assert!(
        starts.iter().any(|e| e.contains("GitReceive")),
        "expected GitReceive in clone fixture: {:?}",
        starts
    );
}

#[test]
fn unmatched_lines_become_logs() {
    let r = RecordingReporter::default();
    let mut p = GitProgressParser::new(&r);
    feed(&mut p, "something weird happened\n");
    let events = r.events.lock().unwrap();
    assert!(events.iter().any(|e| e.starts_with("log ")));
}

#[test]
fn remote_prefix_is_handled() {
    let r = RecordingReporter::default();
    let mut p = GitProgressParser::new(&r);
    feed(
        &mut p,
        "remote: Counting objects:  50% (1/2)\rremote: Counting objects: 100% (2/2), done.\n",
    );
    let events = r.events.lock().unwrap();
    assert!(events.iter().any(|e| e.contains("GitCount")));
}

#[test]
fn percent_ticks_accumulate_deltas() {
    let r = RecordingReporter::default();
    let mut p = GitProgressParser::new(&r);
    feed(
        &mut p,
        "Writing objects:  10% (1/10)\rWriting objects:  50% (5/10)\rWriting objects: 100% (10/10), done.\n",
    );
    let events = r.events.lock().unwrap();
    // Expect tick deltas: +1, +4, +5 (total = 10)
    let tick_sum: u64 = events
        .iter()
        .filter_map(|e| {
            e.strip_prefix("tick items+")
                .and_then(|s| s.split_whitespace().next())
                .and_then(|s| s.parse::<u64>().ok())
        })
        .sum();
    assert_eq!(tick_sum, 10, "events: {:?}", events);
}

#[test]
fn writing_objects_captures_bytes() {
    let r = RecordingReporter::default();
    let mut p = GitProgressParser::new(&r);
    feed(
        &mut p,
        "Writing objects:  50% (5/10), 1.00 KiB\r\
         Writing objects: 100% (10/10), 5.24 KiB | 5.24 MiB/s, done.\n",
    );
    let events = r.events.lock().unwrap();
    // Start should carry Bytes(items=10, bytes=..) OR Count(10) — Stage C makes
    // Writing use Bytes if byte group present on the first percent line.
    assert!(
        events
            .iter()
            .any(|e| e.starts_with("start GitWrite")
                && (e.contains("bytes=") || e.contains("count="))),
        "events: {:?}",
        events
    );
    // Tick deltas in bytes should sum to a value > 0
    let byte_sum: u64 = events
        .iter()
        .filter_map(|e| {
            e.strip_prefix("tick items+")?;
            let parts: Vec<_> = e.split("bytes+").collect();
            parts.get(1)?.parse::<u64>().ok()
        })
        .sum();
    assert!(
        byte_sum > 0,
        "expected nonzero byte tick, events: {:?}",
        events
    );
}
