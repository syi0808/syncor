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

fn parse_bytes(value: &str, unit: &str) -> u64 {
    let v: f64 = value.parse().unwrap_or(0.0);
    let mult: f64 = match unit {
        "KiB" | "KB" => 1024.0,
        "MiB" | "MB" => 1024.0 * 1024.0,
        "GiB" | "GB" => 1024.0 * 1024.0 * 1024.0,
        _ => 1.0,
    };
    (v * mult) as u64
}

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

        // Parse optional running byte count (only present on Writing/Receiving lines).
        let bytes_now: Option<u64> = match (caps.name("bytes"), caps.name("unit")) {
            (Some(b), Some(u)) => Some(parse_bytes(b.as_str(), u.as_str())),
            _ => None,
        };

        let tracks_bytes = matches!(phase, Phase::GitWrite | Phase::GitReceive);

        // Determine the ItemTotal to use when starting/switching into this phase.
        // For Write/Receive: if bytes are present on this (first) line, use Bytes
        // with an estimated total; otherwise fall back to Count(total).
        let total_for_start = if tracks_bytes {
            match bytes_now {
                Some(b) => {
                    let bytes_estimate = b * total / cur.max(1);
                    ItemTotal::Bytes {
                        items: total,
                        bytes: bytes_estimate,
                    }
                }
                None => ItemTotal::Count(total),
            }
        } else {
            ItemTotal::Count(total)
        };

        self.switch_to(phase, total_for_start);
        if let State::Active {
            last_cur,
            last_bytes,
            ..
        } = &mut self.state
        {
            let items_delta = cur.saturating_sub(*last_cur);
            let bytes_delta = if tracks_bytes {
                match bytes_now {
                    Some(b) => {
                        let d = b.saturating_sub(*last_bytes);
                        *last_bytes = b;
                        d
                    }
                    None => 0,
                }
            } else {
                0
            };
            if items_delta > 0 || bytes_delta > 0 {
                self.reporter.phase_tick(items_delta, bytes_delta);
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
