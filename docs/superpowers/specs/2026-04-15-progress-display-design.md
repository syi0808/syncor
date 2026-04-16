# Progress display for long CLI operations

**Date:** 2026-04-15
**Status:** Design — pending approval
**Scope:** Add user-facing progress feedback to `syncor push`, `syncor pull`, and `syncor connect`. Cover the whole pipeline (local chkpt save/restore + git network). Ship in three independent stages: spinners → counters → bytes/%. No behavior change to the sync protocol itself.

## Problem

Long commands appear frozen. Concretely:

- `transport::git::push/pull/init_remote` all call `Command::new("git").output()`, which blocks until completion and captures `stdout`/`stderr`. The user sees nothing until the command exits. For repos with many objects or slow networks this can mean tens of seconds of dead screen.
- `SavePipeline::run` iterates `changed_files`, reading each file, hashing (XXH3-128), LZ4-compressing, and appending to a `PackWriter`. For 1 GB of dotfiles this is the dominant latency. Zero feedback.
- `RestorePipeline::run` iterates a manifest, reads blobs from packs, and writes each file. Same issue.
- `retry_with_backoff` sleeps up to 45 s on transient failures but only emits `tracing::warn!`, which is not visible in default CLI output. From the user's perspective the command just hangs.

No crate in the workspace currently depends on `indicatif` or any other progress library.

## Goals

1. Every long-running phase of `push` / `pull` / `connect` emits visible progress to the terminal.
2. Granularity scales to what each phase can cheaply report:
   - Phases with a known item count **and** known total bytes show a byte/percent bar.
   - Phases with a known item count only show a count bar.
   - Phases with unknown total show a spinner.
3. Graceful degradation in non-TTY environments (CI, pipes, Claude Code terminal emulation): no control characters, final summary line still printed.
4. `syncor-core` remains free of UI dependencies; only the CLI depends on `indicatif`.
5. Retries under `retry_with_backoff` surface to the user, not just to `tracing`.
6. Three-stage rollout so that each stage is independently mergeable and delivers value even if later stages are delayed.

## Non-goals

- Progress for `syncor status`, `syncor log`, `syncor resolve`, or daemon-initiated syncs in this design. Daemon progress is a separate surface (notifications / log file).
- Multi-line `MultiProgress` layouts running in parallel. Phases are sequential per command.
- Changes to `chkpt-core`'s API. This design works around the absence of progress callbacks by observing the for-loops that already live in `syncor-core`.
- Parsing of every possible `git --progress` variant across every git version. Parser failures degrade to spinner + raw log; they do not fail the command.
- Localization of progress messages (English only).

## Technical feasibility summary

`chkpt-core` 0.3.1 exposes no progress hooks on `PackWriter`, `PackSet`, or `scan_workspace`. However, the save and restore loops that drive the per-file work live in `syncor-core` itself (`sync/save.rs`, `sync/restore.rs`), so per-file ticks can be emitted from those loops directly. `scan_workspace` is atomic (returns `Vec<ScannedFile>`) so the scan phase is spinner-only. `PackWriter::finish_with_options` is opaque so pack-chunking is spinner-only.

Git emits structured progress on `stderr` under `--progress`. The format is stable enough across modern git versions (2.20+) to be parsed with a single regex. Unparseable lines fall through to a log channel; the command still completes on `exit_code`.

## Design

### Architecture

```
┌────────────────────────────────────────────────────────────┐
│ syncor-cli (main.rs)                                       │
│   fn make_reporter(no_progress_flag) -> Arc<dyn Reporter>  │
│     - stderr is TTY & !no_progress  → TerminalReporter     │
│     - else                           → NullReporter        │
└────────────────┬───────────────────────────────────────────┘
                 │ Arc<dyn ProgressReporter>
                 ▼
┌────────────────────────────────────────────────────────────┐
│ syncor-core::progress  (NEW module)                        │
│   trait ProgressReporter: Send + Sync                      │
│   enum  Phase { ... }                                      │
│   enum  ItemTotal { Unknown, Count(u64),                   │
│                     Bytes { items, bytes } }               │
│   struct NullReporter                                      │
└────────────────┬───────────────────────────────────────────┘
                 │ reporter.phase_start / tick / end / log
                 ▼
┌────────────────────────────────────────────────────────────┐
│ syncor-core::sync (save, restore, engine)                  │
│ syncor-core::transport::git (push, pull, clone)            │
│   - save/restore loops emit per-file ticks                 │
│   - git commands switch from .output() to .spawn() +       │
│     streamed stderr, fed through GitProgressParser         │
└────────────────────────────────────────────────────────────┘
```

**Dependency direction**: core stays free of `indicatif`; only the CLI imports it. Tests and non-CLI consumers use `NullReporter`.

### `ProgressReporter` trait

```rust
// syncor-core/src/progress.rs
pub trait ProgressReporter: Send + Sync {
    fn phase_start(&self, phase: Phase, total: ItemTotal);
    fn phase_tick(&self, items_delta: u64, bytes_delta: u64);
    fn phase_end(&self, phase: Phase);
    fn log(&self, msg: &str);
}

pub enum ItemTotal {
    Unknown,
    Count(u64),
    Bytes { items: u64, bytes: u64 },
}

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

pub struct NullReporter;
impl ProgressReporter for NullReporter { /* no-op */ }
```

`phase_tick` identifies its phase implicitly via the most recent `phase_start`. The reporter is responsible for its own phase-state tracking; callers guarantee `start → tick* → end` ordering per phase.

### Propagation

Pipeline signatures gain a reporter reference:

```rust
// before
SavePipeline::run(workspace, store_dir, message)

// after
SavePipeline::run(workspace, store_dir, message, reporter: &dyn ProgressReporter)
```

Same change for `RestorePipeline::run` and for `Engine`'s push/pull entry points, which store `Arc<dyn ProgressReporter>` and pass `&*arc` down. All `SyncTransport` methods that may invoke network git gain a `reporter: &dyn ProgressReporter` parameter: `push`, `pull`, `init_remote`, `list_remote_links` (uses `git clone --depth=1` for manifest discovery during `syncor connect`), and `has_remote_changes` (uses `git fetch` during change polling). Non-network transport methods are unchanged.

Existing call sites in `engine.rs` update to thread `reporter` through. Tests that build pipelines directly pass `&NullReporter`.

### Phase-to-total mapping

**Push pipeline** (`Engine::push` → `SavePipeline::run` → `GitTransport::push`):

| # | Phase           | Total source                                      | Tick granularity        |
|---|-----------------|---------------------------------------------------|-------------------------|
| 1 | `Scan`          | `Unknown` (scan_workspace is atomic)              | none; end logs count    |
| 2 | `DetectChanges` | `Count(scanned.len())`                            | per file compared       |
| 3 | `Hash`          | `Bytes { items: changed_files.len(), bytes: Σsz }` | per file `(1, sz)`      |
| 4 | `PackFinish`    | `Unknown`                                         | spinner only            |
| 5 | `Catalog`       | `Unknown`                                         | spinner only            |
| 6 | `GitStage`      | `Unknown`                                         | spinner only            |
| 7 | `GitCommit`     | `Unknown`                                         | spinner only            |
| 8 | `GitEnumerate`  | From git stderr (`Count(total)`)                  | per parsed line         |
| 9 | `GitCompress`   | From git stderr (`Count(total)`)                  | per parsed line         |
|10 | `GitWrite`      | From git stderr (`Count(total)`, bytes in C)      | per parsed line         |

**Pull pipeline** (`Engine::pull` → `GitTransport::pull` → `RestorePipeline::run`):

| # | Phase          | Total source                                       | Tick granularity        |
|---|----------------|----------------------------------------------------|-------------------------|
| 1 | `GitEnumerate` | From git stderr                                    | per parsed line         |
| 2 | `GitCompress`  | From git stderr                                    | per parsed line         |
| 3 | `GitReceive`   | From git stderr (`Count(total)`, bytes in C)       | per parsed line         |
| 4 | `GitResolve`   | From git stderr                                    | per parsed line         |
| 5 | `Merge`        | `Unknown`                                          | spinner only            |
| 6 | `Restore`      | `Bytes { items: manifest.len(), bytes: Σsz }`      | per file `(1, sz)`      |
| 7 | `Cleanup`      | `Unknown`                                          | spinner (scan + remove) |

### Git stderr streaming and parsing

Transport operations that hit the network (`git push`, `git fetch`, `git clone`) change shape:

```rust
let mut child = Command::new("git")
    .args(["push", "--progress", "-u", "origin", &branch])
    .current_dir(&repo_dir)
    .env("GIT_TERMINAL_PROMPT", "0")
    .stderr(Stdio::piped())
    .stdout(Stdio::piped())
    .spawn()?;

let stderr = child.stderr.take().expect("piped");
let reporter_cl = Arc::clone(&reporter_arc);
let parse_thread = std::thread::spawn(move || {
    GitProgressParser::new(reporter_cl).drain(stderr);
});

let status = child.wait()?;
let _ = parse_thread.join();
// stdout is captured normally for rev-parse / error context
```

Local one-shot git calls (`git rev-parse`, `git symbolic-ref`, `git add`, `git commit`, `git merge`, `git status`, `git branch`) keep the existing `Command::output()` / `git_ok` pattern. The five network-facing call sites that switch to `.spawn()` + streamed stderr are: `push` (git.rs:188), `fetch` inside `pull` (git.rs:227), `fetch` inside `has_remote_changes` (git.rs:328), `clone` inside `init_remote` (git.rs:127), and `clone` inside `list_remote_links` (git.rs:279).

**Parser** (`syncor-core/src/transport/git_progress.rs`):

- Reads stderr with a custom reader that splits on both `\r` and `\n` (git uses `\r` for in-place line updates).
- Single regex:
  ```
  ^(?:remote:\s+)?
  (?P<phase>Enumerating|Counting|Compressing|Writing|Receiving|Resolving)
  \s+(?:objects|deltas):\s+
  (?:(?P<pct>\d+)%\s+\((?P<cur>\d+)/(?P<total>\d+)\)|(?P<count>\d+))
  (?:,\s*(?P<bytes>[\d.]+)\s*(?P<unit>[KMG]i?B))?
  ```
- Maintains current phase in `State::{Idle, Phase(P)}`. Phase changes trigger `phase_end` (old) + `phase_start` (new). Each percent line triggers `phase_tick` with `cur - last_cur` and `bytes_parsed - last_bytes`.
- Unmatched lines route to `reporter.log(raw)`. In `--verbose` mode these are printed under the bar; otherwise held at `tracing::debug!`.
- Parser failure safety: if no line ever matches during a git push/fetch, `TerminalReporter` still shows the `GitWrite` / `GitReceive` spinner started at `Command::spawn()` time, so the user never sees a frozen screen. Command success/failure is still determined by `child.wait()` exit status.

### CLI rendering (`TerminalReporter`)

`syncor-cli/src/progress.rs` (new):

```rust
pub fn make_reporter(no_progress: bool) -> Arc<dyn ProgressReporter> {
    let disabled = no_progress
        || std::env::var_os("SYNCOR_NO_PROGRESS").is_some()
        || !std::io::stderr().is_terminal();
    if disabled {
        Arc::new(NullReporter::with_summary_channel(...))
    } else {
        Arc::new(TerminalReporter::new())
    }
}
```

Global CLI flag `--no-progress` added to the root clap struct. `SYNCOR_NO_PROGRESS=1` environment variable also disables.

**Rendering rules**:
- Only one active bar / spinner at a time. `phase_start` closes the previous bar with `finish_and_clear`, prints a one-line `✓ <summary>`, then starts the new bar.
- Style selection by `ItemTotal`:
  - `Unknown` → spinner: `⠋ <phase label> ({elapsed})`
  - `Count(n)` → `{spinner} <label> [{bar:30}] {pos}/{len} ({percent}%)`
  - `Bytes { .. }` → `{spinner} <label> [{bar:30}] {bytes}/{total_bytes} ({pos}/{len}) ETA {eta}`
- `reporter.log(msg)` during an active bar: `bar.println(msg)` so messages scroll above the bar without breaking rendering.
- On error: active bar is closed with `finish_and_clear`; the error is printed via the normal CLI error path. A `Drop` guard on the reporter ensures cleanup even on panic.

**Final summary** (printed in both TTY and non-TTY modes):

```
✓ Push complete: snapshot abc123ef
  87 files hashed, 120.0 MB → 58.4 MB (48% ratio)
  26 objects pushed in 4.1s
```

### Retry visibility

`retry_with_backoff` gains a `reporter: &dyn ProgressReporter` parameter. Each retry logs via `reporter.log` in addition to the existing `tracing::warn!`:

```
retry 2/3: fetch failed, waiting 15s
```

This surfaces above the active spinner. The spinner continues running during the sleep, so the user sees motion.

### Error paths

- Any pipeline error: caller closes the active phase via `reporter.phase_end(current)` before returning the error. This is enforced by a `PhaseGuard` RAII helper inside each pipeline so that `?`-propagation does not leak stale bars:
  ```rust
  struct PhaseGuard<'a> { reporter: &'a dyn ProgressReporter, phase: Phase, ended: bool }
  impl<'a> PhaseGuard<'a> {
      fn new(reporter: &'a dyn ProgressReporter, phase: Phase, total: ItemTotal) -> Self {
          reporter.phase_start(phase, total);
          Self { reporter, phase, ended: false }
      }
      fn end(mut self) { self.reporter.phase_end(self.phase); self.ended = true; }
  }
  impl Drop for PhaseGuard<'_> {
      fn drop(&mut self) { if !self.ended { self.reporter.phase_end(self.phase); } }
  }
  ```
  `phase_end` is therefore idempotent from the reporter's perspective: explicit `.end()` on success, Drop fallback on `?`-early-return or panic. Reporter implementations must tolerate a late `phase_end` on an already-closed phase as a no-op.
- Conflict returns (`PushResult::Conflict`, `PullResult::Conflict`): phase ends normally; conflict message printed via normal CLI error path, not via the reporter.
- Panic: `TerminalReporter`'s `Drop` calls `MultiProgress::clear()` so the terminal is left in a usable state.

## Three-stage rollout

### Stage A — infrastructure + spinners (parity with option 1)

Ship:
- `syncor-core::progress` module with trait, `Phase` enum, only `ItemTotal::Unknown`, `NullReporter`.
- `syncor-cli::progress` module with `TerminalReporter`, TTY detection, `--no-progress` flag, `SYNCOR_NO_PROGRESS` env var.
- `indicatif = "0.17"` added to workspace dependencies.
- Reporter parameter threaded through `SavePipeline`, `RestorePipeline`, `Engine`, `SyncTransport` impls.
- Every phase emits `phase_start(..., Unknown)` and `phase_end`. No counts, no bytes.
- Network git commands switch from `.output()` to `.spawn()` with streamed stderr; for this stage, stderr lines are routed verbatim to `reporter.log()` (the bar is still a spinner; no parsing yet).
- `retry_with_backoff` uses the reporter.
- Final summary line printed in both modes.

Value: every command shows motion. No frozen screens.

Out of scope for Stage A: counts, bytes, git line parsing.

### Stage B — counters (parity with option 2)

Ship:
- `ItemTotal::Count(u64)` activated in `TerminalReporter` (count bar style).
- `SavePipeline`: `Hash` phase uses `Count(changed_files.len())`, ticks per file.
- `RestorePipeline`: `Restore` phase uses `Count(manifest.len())`, ticks per file.
- `DetectChanges` uses `Count(scanned.len())`.
- **`GitProgressParser` introduced** — regex, `\r`-split reader, state machine, golden-fixture tests. This stage uses only `cur/total` counts from parsed lines; byte parsing lands in Stage C.
- `--verbose` flag wired to surface `reporter.log` output.

Value: users see "42/87 files" for local work and "12/26 objects" for git. Remaining work is estimable.

Rationale for separating the parser from Stage A: parser reliability is the riskiest piece. Decoupling it means Stage A can ship even if the parser needs more iteration.

### Stage C — bytes and ETA (parity with option 3)

Ship:
- `ItemTotal::Bytes { items, bytes }` activated in `TerminalReporter` (byte bar style with ETA).
- `SavePipeline::Hash`: total bytes = `Σ changed_files.iter().map(|f| f.size)`, tick per file `(1, sf.size)`.
- `RestorePipeline::Restore`: total bytes = `Σ manifest.iter().map(|e| e.size)`, tick per file `(1, entry.size)`.
- Git parser extended to capture and tick the byte portion of `Writing objects` / `Receiving objects` lines.

Value: meaningful ETA for sync of large directories (dotfiles with media, Claude workspace snapshots, etc.).

### Invariants preserved across stages

- After Stage A, every call site that Stage B/C touches already has a reporter parameter. Stage B/C only extend the trait's variants and the pipelines' internal logic; they do not ripple through signatures.
- The parser is introduced atomically in Stage B; Stage C only extends it with one additional capture.
- Each stage is mergeable without the next. If Stage B's parser work slips, Stage A is already live.

## Testing

- `syncor-core/src/progress.rs`: unit tests for `NullReporter` (no-ops do not panic; trait object compiles).
- `syncor-core/tests/fixtures/git_output/`: capture real stderr from `git push --progress`, `git fetch --progress`, `git clone --progress` against a local test repo. Commit the fixtures.
- `syncor-core/tests/git_progress_parser_test.rs` (Stage B): feed each fixture through the parser, assert the emitted `(Phase, ItemTotal, tick sequence)` stream matches a golden JSON.
- Parser fuzz: small permutations (extra whitespace, `remote:` prefix present/absent, bytes in `KiB` vs `MiB`, missing `done.` trailer). Assert graceful degradation to spinner on total-format corruption.
- E2E for each stage: run `syncor link` / `push` / `connect` / `pull` against a local bare repo on the filesystem and visually confirm output in TTY mode and with `SYNCOR_NO_PROGRESS=1`.
- Stage C e2e: create a 20 MB random file, push it, observe byte bar progression and final compression ratio in the summary.

## Risks and mitigations

- **Git output format drift across versions**: mitigated by fallback to spinner + raw log on parse miss. Fixture tests catch regressions against known versions; unknown versions degrade safely.
- **Progress overhead on fast paths**: per-file `phase_tick` is one atomic increment in `TerminalReporter` and a no-op in `NullReporter`. indicatif throttles redraws internally. Measured impact expected to be sub-millisecond on typical workloads; will be spot-checked in Stage C e2e.
- **Reporter + panic = terminal corruption**: mitigated by `Drop` guard on `TerminalReporter` and `PhaseGuard` RAII inside pipelines.
- **Concurrent access** (future daemon): `ProgressReporter: Send + Sync`. `TerminalReporter` uses `indicatif::MultiProgress` internally which is concurrency-safe. Not exercised in this design but the trait does not preclude it.
- **stderr parser blocking on `child.wait()`**: the parser runs on a dedicated thread that consumes stderr to EOF. Joining after `wait` returns is sound because git closes stderr on exit.

## Out-of-scope follow-ups

- Daemon / watch-mode progress surface (log file, desktop notifications).
- `syncor status` remote change detection progress.
- chkpt-core upstream: add progress callbacks to `scan_workspace` and `PackWriter::finish_with_options` so the scan and pack-chunking phases can become bar'd.
- Structured event emission (JSON lines) for machine consumers.
