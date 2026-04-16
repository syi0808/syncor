use std::io::IsTerminal;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use indicatif::{ProgressBar, ProgressStyle};
use syncor_core::progress::{ItemTotal, Phase, ProgressReporter};

/// Format a byte count using 1024-scaled units (B / KiB / MiB / GiB / TiB).
/// Kept self-contained to avoid pulling in another crate.
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{} B", bytes);
    }
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{:.1} {}", value, UNITS[unit])
}

pub fn make_reporter(no_progress_flag: bool, verbose: bool) -> Arc<dyn ProgressReporter> {
    let disabled = no_progress_flag
        || std::env::var_os("SYNCOR_NO_PROGRESS").is_some()
        || !std::io::stderr().is_terminal();
    if disabled {
        Arc::new(NullCliReporter { verbose })
    } else {
        Arc::new(TerminalReporter::new(verbose))
    }
}

/// Fallback reporter for non-TTY / --no-progress. Prints retry warnings
/// unconditionally; other log messages only when `verbose` is set.
pub struct NullCliReporter {
    verbose: bool,
}

impl ProgressReporter for NullCliReporter {
    fn phase_start(&self, _: Phase, _: ItemTotal) {}
    fn phase_tick(&self, _: u64, _: u64) {}
    fn phase_end(&self, _: Phase) {}
    fn log(&self, msg: &str) {
        // Retry warnings always surface; other lines only under --verbose.
        if self.verbose || msg.starts_with("retry ") {
            eprintln!("{}", msg);
        }
    }
}

enum BarMode {
    Spinner,
    Count,
    Bytes { items_total: u64, items_done: u64 },
}

struct ActiveBar {
    phase: Phase,
    bar: ProgressBar,
    mode: BarMode,
    /// Set to true the first time `phase_tick` moves this bar. A phase that
    /// ends without ticking was preempted (e.g. the outer GitWrite guard is
    /// immediately replaced by the parser's GitEnumerate phase) and should
    /// not emit a "✓ {label}" line. Spinner-mode phases print unconditionally
    /// since they have no fill state to indicate activity.
    ticked: bool,
}

pub struct TerminalReporter {
    active: Mutex<Option<ActiveBar>>,
    verbose: bool,
}

impl TerminalReporter {
    pub fn new(verbose: bool) -> Self {
        Self {
            active: Mutex::new(None),
            verbose,
        }
    }

    fn spinner_style() -> ProgressStyle {
        ProgressStyle::with_template("{spinner:.cyan} {msg} ({elapsed})")
            .expect("static template")
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"])
    }

    fn count_style() -> ProgressStyle {
        ProgressStyle::with_template(
            "{spinner:.cyan} {msg} [{bar:30.cyan/blue}] {pos}/{len} ({percent}%)",
        )
        .expect("static template")
        .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"])
        .progress_chars("█▓▒░ ")
    }

    fn bytes_style() -> ProgressStyle {
        ProgressStyle::with_template(
            "{spinner:.cyan} {msg} [{bar:30.cyan/blue}] {bytes}/{total_bytes} ETA {eta}",
        )
        .expect("static template")
        .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"])
        .progress_chars("█▓▒░ ")
    }
}

impl Default for TerminalReporter {
    fn default() -> Self {
        Self::new(false)
    }
}

impl ProgressReporter for TerminalReporter {
    fn phase_start(&self, phase: Phase, total: ItemTotal) {
        let mut slot = self.active.lock().unwrap();
        if let Some(prev) = slot.take() {
            prev.bar.finish_and_clear();
        }
        let (bar, mode) = match total {
            ItemTotal::Unknown => {
                let b = ProgressBar::new_spinner();
                b.set_style(Self::spinner_style());
                b.enable_steady_tick(Duration::from_millis(100));
                (b, BarMode::Spinner)
            }
            ItemTotal::Count(total) => {
                let b = ProgressBar::new(total);
                b.set_style(Self::count_style());
                b.enable_steady_tick(Duration::from_millis(100));
                (b, BarMode::Count)
            }
            ItemTotal::Bytes { items, bytes } => {
                let b = ProgressBar::new(bytes);
                b.set_style(Self::bytes_style());
                b.enable_steady_tick(Duration::from_millis(100));
                (
                    b,
                    BarMode::Bytes {
                        items_total: items,
                        items_done: 0,
                    },
                )
            }
        };
        bar.set_message(phase.label());
        *slot = Some(ActiveBar {
            phase,
            bar,
            mode,
            ticked: false,
        });
    }

    fn phase_tick(&self, items: u64, bytes: u64) {
        let mut slot = self.active.lock().unwrap();
        if let Some(active) = slot.as_mut() {
            match &mut active.mode {
                BarMode::Spinner => {}
                BarMode::Count => active.bar.inc(items),
                BarMode::Bytes {
                    items_total,
                    items_done,
                } => {
                    active.bar.inc(bytes);
                    *items_done += items;
                    active.bar.set_message(format!(
                        "{} ({}/{} files)",
                        active.phase.label(),
                        items_done,
                        items_total,
                    ));
                }
            }
            active.ticked = true;
        }
    }

    fn phase_end(&self, phase: Phase) {
        let mut slot = self.active.lock().unwrap();
        if let Some(active) = slot.take() {
            if active.phase == phase {
                active.bar.finish_and_clear();
                // Only print the "✓" line when the phase actually did work.
                // Spinner-mode phases (Unknown) always print since they have
                // no fill state; Count/Bytes phases print only if they ticked,
                // which suppresses the noise from preempted outer guards.
                let should_announce = matches!(active.mode, BarMode::Spinner) || active.ticked;
                if should_announce {
                    eprintln!("✓ {}", phase.label());
                }
            } else {
                // Idempotent: some other phase is active; leave it running.
                *slot = Some(active);
            }
        }
    }

    fn log(&self, msg: &str) {
        // Retry warnings always surface; other log lines only under --verbose.
        if !self.verbose && !msg.starts_with("retry ") {
            return;
        }
        let slot = self.active.lock().unwrap();
        if let Some(active) = slot.as_ref() {
            active.bar.println(msg);
        } else {
            eprintln!("{}", msg);
        }
    }
}
