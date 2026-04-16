use std::io::IsTerminal;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use indicatif::{ProgressBar, ProgressStyle};
use syncor_core::progress::{ItemTotal, Phase, ProgressReporter};

/// Format a byte count using 1024-scaled units (B / KiB / MiB / GiB / TiB).
///
/// Stage A: helper prepared for push/pull summary output. The Stage A summary
/// only has a count-based form (files restored, snapshot id); Stage C (Task C4)
/// enriches push summaries with `bytes_compressed` and will consume this.
/// Kept self-contained to avoid pulling in another crate.
#[allow(dead_code)] // consumed by Stage C summary enrichment
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

pub fn make_reporter(no_progress_flag: bool) -> Arc<dyn ProgressReporter> {
    let disabled = no_progress_flag
        || std::env::var_os("SYNCOR_NO_PROGRESS").is_some()
        || !std::io::stderr().is_terminal();
    if disabled {
        Arc::new(NullCliReporter)
    } else {
        Arc::new(TerminalReporter::new())
    }
}

/// Fallback reporter for non-TTY / --no-progress. Prints completion lines only.
pub struct NullCliReporter;

impl ProgressReporter for NullCliReporter {
    fn phase_start(&self, _: Phase, _: ItemTotal) {}
    fn phase_tick(&self, _: u64, _: u64) {}
    fn phase_end(&self, _: Phase) {}
    fn log(&self, msg: &str) {
        // retry warnings and similar are worth seeing even without a bar
        eprintln!("{}", msg);
    }
}

struct ActiveBar {
    phase: Phase,
    bar: ProgressBar,
}

pub struct TerminalReporter {
    active: Mutex<Option<ActiveBar>>,
}

impl TerminalReporter {
    pub fn new() -> Self {
        Self {
            active: Mutex::new(None),
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
}

impl Default for TerminalReporter {
    fn default() -> Self {
        Self::new()
    }
}

impl ProgressReporter for TerminalReporter {
    fn phase_start(&self, phase: Phase, total: ItemTotal) {
        let mut slot = self.active.lock().unwrap();
        if let Some(prev) = slot.take() {
            prev.bar.finish_and_clear();
        }
        let bar = match total {
            ItemTotal::Unknown => {
                let b = ProgressBar::new_spinner();
                b.set_style(Self::spinner_style());
                b.enable_steady_tick(Duration::from_millis(100));
                b
            }
            ItemTotal::Count(total) => {
                let b = ProgressBar::new(total);
                b.set_style(Self::count_style());
                b.enable_steady_tick(Duration::from_millis(100));
                b
            }
            ItemTotal::Bytes { .. } => {
                // Stage B: render byte totals as a count fallback. Stage C
                // replaces this with a byte/ETA bar.
                let b = ProgressBar::new_spinner();
                b.set_style(Self::spinner_style());
                b.enable_steady_tick(Duration::from_millis(100));
                b
            }
        };
        bar.set_message(phase.label());
        *slot = Some(ActiveBar { phase, bar });
    }

    fn phase_tick(&self, items: u64, _bytes: u64) {
        let slot = self.active.lock().unwrap();
        if let Some(active) = slot.as_ref() {
            // For Count bars: inc items. For Unknown spinners: inc is a no-op
            // on a spinner-style bar but also harmless. Bytes handled in Stage C.
            active.bar.inc(items);
        }
    }

    fn phase_end(&self, phase: Phase) {
        let mut slot = self.active.lock().unwrap();
        if let Some(active) = slot.take() {
            if active.phase == phase {
                active.bar.finish_and_clear();
                eprintln!("✓ {}", phase.label());
            } else {
                // Idempotent: some other phase is active; leave it running.
                *slot = Some(active);
            }
        }
    }

    fn log(&self, msg: &str) {
        let slot = self.active.lock().unwrap();
        if let Some(active) = slot.as_ref() {
            active.bar.println(msg);
        } else {
            eprintln!("{}", msg);
        }
    }
}
