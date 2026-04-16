//! Parses `git --progress` stderr into ProgressReporter events.
//!
//! Git writes progress lines separated by `\r` (in-place updates) and `\n`
//! (phase transitions). A single regex extracts phase name, current/total
//! counts, and optional bytes. Unmatched lines route to `reporter.log`.

use once_cell::sync::Lazy;
use regex::Regex;

use crate::progress::{ItemTotal, Phase, ProgressReporter};

static LINE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"^(?:remote:\s+)?(?P<phase>Enumerating|Counting|Compressing|Writing|Receiving|Resolving)\s+(?:objects|deltas):\s+(?:(?P<pct>\d+)%\s+\((?P<cur>\d+)/(?P<total>\d+)\)|(?P<count>\d+))(?:,\s*(?P<bytes>[\d.]+)\s*(?P<unit>[KMG]i?B))?"
    ).expect("static regex")
});

fn phase_for(name: &str) -> Phase {
    match name {
        "Enumerating" => Phase::GitEnumerate,
        "Counting" => Phase::GitCount,
        "Compressing" => Phase::GitCompress,
        "Writing" => Phase::GitWrite,
        "Receiving" => Phase::GitReceive,
        "Resolving" => Phase::GitResolve,
        _ => unreachable!("regex restricts phase values"),
    }
}

enum State {
    Idle,
    Active {
        phase: Phase,
        last_cur: u64,
        // Reserved for Stage C (bytes accounting); unused in Stage B.
        #[allow(dead_code)]
        last_bytes: u64,
    },
}

pub struct GitProgressParser<'a> {
    reporter: &'a dyn ProgressReporter,
    state: State,
}

impl<'a> GitProgressParser<'a> {
    pub fn new(reporter: &'a dyn ProgressReporter) -> Self {
        Self {
            reporter,
            state: State::Idle,
        }
    }

    pub fn feed_line(&mut self, line: &str) {
        let Some(caps) = LINE_RE.captures(line) else {
            self.reporter.log(line);
            return;
        };
        let phase_name = caps.name("phase").unwrap().as_str();
        let phase = phase_for(phase_name);

        // If we match a "short count" variant (e.g. "Enumerating objects: 26, done.")
        if let Some(count) = caps.name("count") {
            let n: u64 = count.as_str().parse().unwrap_or(0);
            self.switch_to(phase, ItemTotal::Count(n));
            // Fast-forward: a count-only line is a terminal snapshot for that phase.
            if let State::Active { last_cur, .. } = &mut self.state {
                let delta = n.saturating_sub(*last_cur);
                if delta > 0 {
                    self.reporter.phase_tick(delta, 0);
                }
                *last_cur = n;
            }
            return;
        }

        // Percent variant: cur/total known
        let cur: u64 = caps.name("cur").unwrap().as_str().parse().unwrap_or(0);
        let total: u64 = caps.name("total").unwrap().as_str().parse().unwrap_or(0);

        // Stage B: bytes captured in Stage C. We still read the bytes group but
        // always pass 0 to phase_tick in Stage B. The capture is wired for C.
        let _bytes_opt: Option<u64> = caps.name("bytes").map(|_| 0);

        self.switch_to(phase, ItemTotal::Count(total));
        if let State::Active { last_cur, .. } = &mut self.state {
            let delta = cur.saturating_sub(*last_cur);
            if delta > 0 {
                self.reporter.phase_tick(delta, 0);
            }
            *last_cur = cur;
        }
    }

    /// Called when the stderr stream reaches EOF.
    pub fn finish(&mut self) {
        if let State::Active { phase, .. } = std::mem::replace(&mut self.state, State::Idle) {
            self.reporter.phase_end(phase);
        }
    }

    fn switch_to(&mut self, new_phase: Phase, total: ItemTotal) {
        let need_start = match &self.state {
            State::Idle => true,
            State::Active { phase, .. } if *phase != new_phase => true,
            _ => false,
        };
        if need_start {
            if let State::Active { phase, .. } = std::mem::replace(&mut self.state, State::Idle) {
                self.reporter.phase_end(phase);
            }
            self.reporter.phase_start(new_phase, total);
            self.state = State::Active {
                phase: new_phase,
                last_cur: 0,
                last_bytes: 0,
            };
        }
    }
}
