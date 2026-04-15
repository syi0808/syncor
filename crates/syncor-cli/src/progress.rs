// Items defined here are wired into command handlers by Task A13; silence
// dead-code warnings in the meantime without touching the plan-specified API.
#![allow(dead_code)]

use std::io::IsTerminal;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use indicatif::{ProgressBar, ProgressStyle};
use syncor_core::progress::{ItemTotal, Phase, ProgressReporter};

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
            ItemTotal::Count(_) | ItemTotal::Bytes { .. } => {
                // Stage A: always spinner; Stage B/C extend this arm.
                let b = ProgressBar::new_spinner();
                b.set_style(Self::spinner_style());
                b.enable_steady_tick(Duration::from_millis(100));
                b
            }
        };
        bar.set_message(phase.label());
        *slot = Some(ActiveBar { phase, bar });
    }

    fn phase_tick(&self, _items: u64, _bytes: u64) {
        // Stage A: spinner animates on its own via steady_tick. No-op here.
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
